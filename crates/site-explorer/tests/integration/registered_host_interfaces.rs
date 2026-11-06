/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::error::Error;
use std::sync::Arc;

use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::default_config;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use mac_address::MacAddress;
use model::expected_machine::{ExpectedInterface, ExpectedInterfaceRole};
use model::machine::{DpuInitState, DpuInitStates, ManagedHostState};
use model::site_explorer::EthernetInterface;
use model::test_support::ManagedHostConfig;
use tonic::Request;

mod metrics;

/// Retains provisional links while public registration supplies controller-2 targets, allowing each test to choose when repair starts.
struct Bf4HostFixture {
    pool: PgPool,
    harness: TestHarness,
    host: TestManagedHost,
    explorer: TestSiteExplorer,
    requests: Vec<rpc::forge::MachineDiscoveryInfo>,
    targets: Vec<MacAddress>,
}

impl Bf4HostFixture {
    /// Builds an upgraded host with distinct factory MACs so controller-2 tests cannot pass through legacy selection.
    async fn new(pool: PgPool, dpu_count: usize) -> Result<Self, Box<dyn Error>> {
        // Configure both public discovery and Site Explorer to select controller 2.
        let mut runtime_config = default_config::get();
        runtime_config.dpu_host_pf_pci_controller_id = 2;
        let harness = TestHarness::builder(pool.clone())
            .with_api_builder_fn(move |builder| {
                builder.with_runtime_config(Arc::new(runtime_config))
            })
            .build()
            .await;
        let domain = harness.test_domain().await;
        let controller = harness.network_controller();
        let underlay = controller.create_underlay_segment(&domain).await;
        controller.create_admin_segment(&domain).await;
        let ingestion = harness.test_site_explorer(SiteExplorerConfig {
            enabled: Arc::new(true.into()),
            create_machines: Arc::new(true.into()),
            explorations_per_run: 10,
            dpu_host_pf_pci_controller_id: 2,
            retained_boot_interface_window: None,
            ..Default::default()
        });
        // Give factory MACs lower PCI paths so selecting a legacy candidate cannot pass these tests.
        let mut config = ManagedHostConfig::default().with_dpu_count(dpu_count);
        for (index, dpu) in config.dpus.iter_mut().enumerate() {
            dpu.override_hosts_uefi_device_path =
                Some(format!("PciRoot(0x0)/Pci(0x1,0x{index:x})").parse()?);
        }
        let (host, build_data) = harness
            .managed_host_builder(&ingestion, underlay)
            .with_config(config.clone())
            .build()
            .await;
        // Enrich proactively created rows with their existing Redfish IDs before capturing any repair baseline.
        ingestion.run_single_iteration().await?;
        // Park the provisional host in Ready so registration cannot repair links before all evidence is arranged.
        host.advance_state(ManagedHostState::Ready).await;

        // Keep the existing Redfish evidence and add exact targets with a different PCI ordering.
        let mut txn = pool.begin().await?;
        let mut report =
            db::explored_endpoints::find_by_ips(txn.as_mut(), vec![build_data.host_bmc_ip()])
                .await?
                .remove(0)
                .report;
        let mut requests = Vec::new();
        let mut targets = Vec::new();
        for (index, (dpu, dpu_config)) in host.dpus.iter().zip(&config.dpus).enumerate() {
            let target: MacAddress = format!("02:00:00:00:bf:{:02x}", index + 1).parse()?;
            assert_ne!(target, dpu_config.host_mac_address);
            assert!(
                db::machine_interface::find_by_mac_address(txn.as_mut(), target)
                    .await?
                    .is_empty()
            );
            targets.push(target);
            report.systems[0]
                .ethernet_interfaces
                .push(EthernetInterface {
                    id: Some(format!("BF4.Controller2.Dpu{index}")),
                    mac_address: Some(target), // Controller-2 inventory must create a different row.
                    uefi_device_path: Some(
                        if index == 0 {
                            "PciRoot(0x0)/Pci(0x10,0x0)"
                        } else {
                            "PciRoot(0x0)/Pci(0x2,0x0)"
                        }
                        .parse()?,
                    ),
                    ..Default::default()
                });

            // Refresh BF3 fixture classification on the next trusted BF4 phone-home.
            db::machine_topology::set_topology_update_needed(&mut txn, &dpu.id.into(), true)
                .await?;
            let registered_dpu = harness.find_machine(dpu.id.into()).await.remove(0);
            let caller = registered_dpu
                .bmc
                .as_ref()
                .and_then(|bmc| bmc.machine_interface_id)
                .expect("owned DPU BMC interface");
            let mut discovery = registered_dpu
                .status
                .expect("DPU status")
                .discovery_info
                .expect("DPU hardware");
            let hardware = discovery.dpu_info.as_mut().expect("DpuData");
            hardware.part_number = "900-9D4B4-CWAA-TSA".to_string();
            hardware.host_pfs = vec![
                // A valid controller-1 entry must not substitute for the configured controller.
                rpc::machine_discovery::DpuHostPf {
                    pci_controller_id: 1,
                    pf_id: 0,
                    mac_address: dpu_config.host_mac_address.to_string(),
                    physical_port_name: "c1pf0".to_string(),
                    ..Default::default()
                },
                // Controller 2 deliberately differs from the provisional factory MAC.
                rpc::machine_discovery::DpuHostPf {
                    pci_controller_id: 2,
                    pf_id: 0,
                    mac_address: target.to_string(),
                    physical_port_name: "c2pf0".to_string(),
                    ..Default::default()
                },
            ];
            requests.push(rpc::forge::MachineDiscoveryInfo {
                machine_interface_id: Some(caller),
                discovery_data: Some(rpc::DiscoveryData::Info(discovery)),
                discovery_reporter: rpc::forge::MachineDiscoveryReporter::DpuAgent as i32,
                ..Default::default()
            });
        }
        // Commit the Redfish evidence and topology refresh flags before submitting any registration request.
        sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
            .bind(build_data.host_bmc_ip())
            .bind(sqlx::types::Json(&report))
            .execute(txn.as_mut())
            .await?;
        txn.commit().await?;

        // Reconcile persisted evidence without refreshing endpoints or recreating machines.
        let explorer = harness.test_site_explorer(SiteExplorerConfig {
            enabled: Arc::new(true.into()),
            create_machines: Arc::new(false.into()),
            explorations_per_run: 0,
            dpu_host_pf_pci_controller_id: 2,
            retained_boot_interface_window: None,
            ..Default::default()
        });
        Ok(Self {
            pool,
            harness,
            host,
            explorer,
            requests,
            targets,
        })
    }

    /// Registers each DPU through DiscoverMachine and reloads its inventory so repair depends on committed agent data.
    async fn register_inventory(&self) -> Result<(), Box<dyn Error>> {
        for (dpu, request) in self.host.dpus.iter().zip(&self.requests) {
            // Ready protection lets all paired inventories commit before any host link changes.
            self.harness
                .api()
                .discover_machine(Request::new(request.clone()))
                .await?;
            // Read the accepted inventory again to prove the public operation persisted it.
            let persisted = self
                .harness
                .find_machine(dpu.id.into())
                .await
                .remove(0)
                .status
                .expect("registered DPU")
                .discovery_info
                .expect("hardware")
                .dpu_info
                .expect("DpuData");
            let Some(rpc::DiscoveryData::Info(reported)) = request.discovery_data.as_ref() else {
                panic!("fixture inventory")
            };
            assert_eq!(
                persisted.host_pfs,
                reported.dpu_info.as_ref().expect("DpuData").host_pfs
            );
        }
        Ok(())
    }

    /// Returns the host to DPUInit so the next Site Explorer pass may publish registered links.
    async fn initialize(&self) {
        self.host
            .advance_state(ManagedHostState::DPUInit {
                dpu_states: DpuInitStates {
                    states: self
                        .host
                        .dpus
                        .iter()
                        .map(|dpu| (dpu.id, DpuInitState::WaitingForNetworkConfig))
                        .collect(),
                },
            })
            .await;
    }

    /// Reloads committed host interfaces through FindMachinesByIds so association assertions observe persisted data.
    async fn host_status(&self) -> rpc::forge::MachineStatus {
        self.harness
            .find_machine(self.host.host.id.into())
            .await
            .remove(0)
            .status
            .expect("host status")
    }

    /// Reloads vendor interface IDs and desired selection through the public view so assertions cannot trust a setter's response.
    async fn boot_interfaces(
        &self,
    ) -> Result<rpc::forge::GetMachineBootInterfacesResponse, Box<dyn Error>> {
        Ok(self
            .harness
            .api()
            .get_machine_boot_interfaces(Request::new(
                rpc::forge::GetMachineBootInterfacesRequest {
                    machine_id: Some(self.host.host.id.into()),
                },
            ))
            .await?
            .into_inner())
    }

    /// Captures link rows, address and instance allocations, desired selection and host/DPU machine versions to detect partial publication after rollback.
    async fn persisted_repair_state(&self) -> Result<serde_json::Value, Box<dyn Error>> {
        // Keep the fixture's DPU IDs so a broken attachment cannot hide a sibling network-config write.
        let host_and_dpu_ids = self
            .host
            .dpus
            .iter()
            .map(|dpu| dpu.id.to_string())
            .chain(std::iter::once(self.host.host.id.to_string()))
            .collect::<Vec<_>>();
        // Include all owned rows so retained provisional interfaces and newly created exact rows are compared together.
        Ok(sqlx::query_scalar(
            "SELECT jsonb_build_object(
                'interfaces', (SELECT jsonb_agg(to_jsonb(mi) ORDER BY mi.id)
                    FROM machine_interfaces mi WHERE mi.machine_id = $1),
                'addresses', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id)
                    FROM machine_interface_addresses a JOIN machine_interfaces mi ON mi.id = a.interface_id
                    WHERE mi.machine_id = $1),
                'desired', (SELECT to_jsonb(b) FROM machine_boot_interfaces b WHERE b.machine_id = $1),
                'instances', (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.id)
                    FROM instances i WHERE i.machine_id = $1),
                'versions', (SELECT jsonb_agg(jsonb_build_object('id', m.id, 'version', m.version,
                    'network_config', m.network_config, 'network_config_version', m.network_config_version)
                    ORDER BY m.id) FROM machines m WHERE m.id = ANY($2)))"
        ).bind(self.host.host.id).bind(&host_and_dpu_ids).fetch_one(&self.pool).await?)
    }
}

/// Repairs both controller-2 links and selects the lower numeric PCI path while Reserved policy forces the new primary to reuse its DHCP donor.
#[sqlx_test]
async fn controller_two_repairs_every_dpu_with_reserved_dhcp_transfer(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // The old primary owns the only reusable DHCP allocation; both registered target rows are absent.
    let fixture = Bf4HostFixture::new(pool, 2).await?;
    fixture.register_inventory().await?;
    let before = fixture.host_status().await;
    let original = before
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("provisional primary");
    assert!(!original.address.is_empty());
    sqlx::query("UPDATE network_segments SET allocation_strategy = 'reserved' WHERE id = $1")
        .bind(original.segment_id)
        .execute(&fixture.pool)
        .await?;

    // Publish the configured links through the real background reconciliation transaction.
    fixture.initialize().await;
    fixture.explorer.run_single_iteration().await?;
    let repaired = fixture.host_status().await;
    let view = fixture.boot_interfaces().await?;

    // Keep both DPUs attached and capture every current Redfish ID, including the non-primary target.
    assert_eq!(repaired.associated_dpu_machine_ids.len(), 2);
    for (index, (dpu, target)) in fixture.host.dpus.iter().zip(&fixture.targets).enumerate() {
        let links = repaired
            .interfaces
            .iter()
            .filter(|interface| interface.attached_dpu_machine_id == Some(dpu.id))
            .collect::<Vec<_>>();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].mac_address, target.to_string());
        let boot = view
            .machine_interfaces
            .iter()
            .find(|interface| interface.interface_id == links[0].id)
            .expect("exact boot row");
        assert_eq!(
            boot.boot_interface_id.as_deref(),
            Some(format!("BF4.Controller2.Dpu{index}").as_str())
        );
    }
    let primaries = repaired
        .interfaces
        .iter()
        .filter(|interface| interface.primary_interface)
        .collect::<Vec<_>>();
    assert_eq!(primaries.len(), 1);
    assert_eq!(primaries[0].mac_address, fixture.targets[1].to_string());
    assert_eq!(primaries[0].address, original.address);
    let desired = view.reconciliation.expect("desired selection");
    assert_eq!(
        desired.selection_source,
        rpc::forge::BootInterfaceSelectionSource::RedfishUefiPci as i32
    );
    let target = desired.desired_boot_interface.expect("desired pair");
    assert_eq!(target.mac_address, fixture.targets[1].to_string());
    assert_eq!(target.interface_id.as_deref(), Some("BF4.Controller2.Dpu1"));
    assert!(
        db::host_boot_interface::not_ready_reason(&fixture.pool, &fixture.host.host.id, 2)
            .await?
            .is_none()
    );
    Ok(())
}

/// Rejects two registered DPUs claiming one MAC even when an ExpectedMachine declaration bypasses automatic candidate validation, preserving both DPU attachments.
#[sqlx_test]
async fn duplicate_registered_mac_preserves_declared_primary_and_both_dpu_links(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // Register the contradiction while Ready so both accepted inventories precede the first repair.
    let mut fixture = Bf4HostFixture::new(pool, 2).await?;
    // Make both DPUs claim an absent target so existing-row ownership checks cannot detect the contradiction.
    let Some(rpc::DiscoveryData::Info(second)) = fixture.requests[1].discovery_data.as_mut() else {
        panic!("fixture inventory")
    };
    second.dpu_info.as_mut().expect("DpuData").host_pfs[1].mac_address =
        fixture.targets[0].to_string();
    fixture.register_inventory().await?;
    let before = fixture.host_status().await;
    let primary = before
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("provisional primary");
    // Resolve selection through the declaration so automatic duplicate-candidate rejection is bypassed.
    let mut txn = fixture.pool.begin().await?;
    let mut expected =
        db::expected_machine::find_by_bmc_mac_address(txn.as_mut(), fixture.host.host.bmc_mac)
            .await?
            .expect("expected host");
    expected.data.interfaces.push(ExpectedInterface {
        mac_address: Some(primary.mac_address.parse()?),
        role: ExpectedInterfaceRole::Host,
        primary: Some(true), // The declaration must not waive DPU-assignment validation.
        ..Default::default()
    });
    db::expected_machine::update(&mut txn, &expected).await?;
    txn.commit().await?;
    fixture.initialize().await;
    let before_repair = fixture.persisted_repair_state().await?;

    // Reconciliation must reject the full assignment list before creating or reattaching any row.
    let mut txn = fixture.pool.begin().await?;
    let error = db::host_boot_interface::reconcile_pre_host_init(
        &mut txn,
        &fixture.host.host.id,
        2,
        false,
        None,
    )
    .await
    .expect_err("one MAC cannot identify two paired DPU links");
    assert!(
        matches!(error, db::DatabaseError::FailedPrecondition(message)
        if message.contains("multiple paired DPUs claim host-PF MAC"))
    );
    txn.rollback().await?;

    // A fresh persisted read must retain each DPU and the existing declared primary.
    assert_eq!(fixture.persisted_repair_state().await?, before_repair);
    let preserved = fixture.host_status().await;
    assert_eq!(preserved.interfaces, before.interfaces);
    assert_eq!(preserved.associated_dpu_machine_ids.len(), 2);
    Ok(())
}

/// Rolls back newly created links, primary promotion, DHCP transfer, Redfish IDs and versions when the final desired-target write fails, preventing a partially published boot configuration.
#[sqlx_test]
async fn late_desired_target_failure_rolls_back_registered_host_repair(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // Serialize this injected failure with tests that measure the process-wide failure counter.
    let _failure_count_window = carbide_instrument::testing::MetricsCapture::start();
    // Register distinct targets and capture the predecessor rows before permitting repair.
    let fixture = Bf4HostFixture::new(pool, 2).await?;
    fixture.register_inventory().await?;
    fixture.initialize().await;
    let before = fixture.persisted_repair_state().await?;
    // Fail the final publication after networking writes while recording that step outside rollback.
    sqlx::raw_sql(
        "CREATE SEQUENCE bf4_desired_writes;
         CREATE FUNCTION reject_bf4_desired_write() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM nextval('bf4_desired_writes');
             RAISE EXCEPTION 'forced BF4 desired target failure';
         END $$;
         CREATE TRIGGER reject_bf4_desired_write
             BEFORE INSERT OR UPDATE ON machine_boot_interfaces
             FOR EACH ROW EXECUTE FUNCTION reject_bf4_desired_write();",
    )
    .execute(&fixture.pool)
    .await?;

    // Exercise Site Explorer's rollback after the desired-target write follows all networking mutations.
    fixture.explorer.run_single_iteration().await?;

    // Sequence state survives rollback and proves the failure reached the final publication step.
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT is_called FROM bf4_desired_writes")
            .fetch_one(&fixture.pool)
            .await?
    );
    assert_eq!(fixture.persisted_repair_state().await?, before);
    assert_eq!(
        fixture.host_status().await.associated_dpu_machine_ids.len(),
        2
    );
    Ok(())
}

/// Retains a provisional row's static address and Redfish ID after replacing its DPU attachment, keeping operator-owned data available during BF4 repair.
#[sqlx_test]
async fn registered_repair_preserves_static_provisional_address_and_boot_id(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // Convert the provisional DHCP allocation to static while its row still identifies the DPU.
    let fixture = Bf4HostFixture::new(pool, 1).await?;
    fixture.register_inventory().await?;
    let before = fixture.host_status().await;
    let original = before
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("provisional primary");
    let address = original.address.first().expect("admin address").clone();
    fixture
        .harness
        .api()
        .assign_static_address(Request::new(rpc::forge::AssignStaticAddressRequest {
            interface_id: original.id,
            ip_address: address.clone(),
        }))
        .await?;
    let before_boot = fixture.boot_interfaces().await?;
    let original_boot_id = before_boot
        .machine_interfaces
        .iter()
        .find(|interface| interface.interface_id == original.id)
        .expect("provisional boot row")
        .boot_interface_id
        .clone();
    assert!(original_boot_id.is_some());

    // Create the registered row and detach the provisional link through the background transaction.
    fixture.initialize().await;
    fixture.explorer.run_single_iteration().await?;

    // Reload the retained row and allocation type through public APIs rather than trusting the assignment response.
    let repaired = fixture.host_status().await;
    let retained = repaired
        .interfaces
        .iter()
        .find(|interface| interface.id == original.id)
        .expect("retained provisional row");
    assert!(!retained.primary_interface);
    assert_eq!(retained.attached_dpu_machine_id, None);
    assert_eq!(retained.address, vec![address.clone()]);
    let allocations = fixture
        .harness
        .api()
        .find_interface_addresses(Request::new(rpc::forge::FindInterfaceAddressesRequest {
            interface_id: original.id,
        }))
        .await?
        .into_inner()
        .addresses;
    assert_eq!(allocations.len(), 1);
    assert_eq!(allocations[0].address, address);
    assert_eq!(allocations[0].allocation_type, "static");
    let after_boot = fixture.boot_interfaces().await?;
    assert_eq!(
        after_boot
            .machine_interfaces
            .iter()
            .find(|interface| interface.interface_id == original.id)
            .expect("retained boot row")
            .boot_interface_id,
        original_boot_id
    );
    let primary = repaired
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("registered primary");
    assert_eq!(primary.mac_address, fixture.targets[0].to_string());
    assert_eq!(
        primary.attached_dpu_machine_id,
        Some(fixture.host.dpus[0].id)
    );
    assert!(!primary.address.is_empty());
    Ok(())
}

/// Verifies that an unpaired BF4 DPU cannot claim an allocated host's MAC, protecting allocated host networking before postcommit repair.
///
/// Accepts the same DPU with an unowned target so an unrelated caller-identity rejection cannot satisfy this regression.
#[sqlx_test]
async fn unpaired_registration_preserves_another_hosts_allocated_interface(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // Promote the host through public discovery so the allocation references a stable host ID.
    let mut fixture = Bf4HostFixture::new(pool, 2).await?;
    let host_caller = fixture
        .harness
        .find_machine(fixture.host.host.id.into())
        .await
        .remove(0)
        .bmc
        .and_then(|bmc| bmc.machine_interface_id)
        .expect("host BMC interface");
    let promoted = fixture
        .harness
        .api()
        .discover_machine(Request::new(rpc::forge::MachineDiscoveryInfo {
            machine_interface_id: Some(host_caller),
            discovery_data: Some(rpc::DiscoveryData::Info(
                fixture.host.host.hardware_info().try_into()?,
            )),
            ..Default::default()
        }))
        .await?
        .into_inner();
    fixture.host.host.id = promoted.machine_id.expect("stable host ID").try_into()?;

    // Remove the second DPU's host link while retaining its owned discovery caller and stable hardware identity.
    let unpaired_dpu = fixture.host.dpus[1].id;
    let detached = sqlx::query(
        "UPDATE machine_interfaces SET attached_dpu_machine_id = NULL
         WHERE machine_id = $1 AND attached_dpu_machine_id = $2",
    )
    .bind(fixture.host.host.id)
    .bind(unpaired_dpu)
    .execute(&fixture.pool)
    .await?;
    assert_eq!(detached.rows_affected(), 1);

    // Construct a persisted instance in DPUInit so Ready-state protection cannot mask an attachment change.
    fixture.initialize().await;
    let allocation: carbide_uuid::instance::InstanceId = sqlx::query_scalar(
        r#"INSERT INTO instances (machine_id, operating_system_id, os_ipxe_script, network_config, nvlink_config)
           VALUES ($1, NULL, '#!ipxe', '{"interfaces": [], "auto_config": null}'::jsonb, '{"gpu_configs": []}'::jsonb)
           RETURNING id"#,
    )
    .bind(fixture.host.host.id)
    .fetch_one(&fixture.pool)
    .await?;
    // Reload the victim's association and allocation to establish the committed state that registration must preserve.
    let before = fixture.host_status().await;
    assert_eq!(
        before.associated_dpu_machine_ids,
        vec![fixture.host.dpus[0].id]
    );
    let victim_mac = before
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("allocated host primary")
        .mac_address
        .clone();
    let persisted_before = fixture.persisted_repair_state().await?;
    let allocation_before = fixture
        .harness
        .api()
        .find_instance_by_machine_id(Request::new(fixture.host.host.id.into()))
        .await?
        .into_inner();
    assert_eq!(allocation_before.instances.len(), 1);
    assert_eq!(allocation_before.instances[0].id, Some(allocation));

    // Change only the selected PF's MAC; the distinct factory MAC and caller still identify the unpaired DPU.
    let mut request = fixture.requests[1].clone();
    let Some(rpc::DiscoveryData::Info(discovery)) = request.discovery_data.as_mut() else {
        panic!("DPU discovery inventory")
    };
    let hardware = discovery.dpu_info.as_mut().expect("DpuData");
    assert_ne!(hardware.factory_mac_address, victim_mac);
    hardware.host_pfs[1].mac_address = victim_mac.clone(); // Controller 2 must claim the occupied row.

    // Submit registration through the public API and require the selected-MAC ownership rejection.
    let error = fixture
        .harness
        .api()
        .discover_machine(Request::new(request.clone()))
        .await
        .expect_err("an unpaired DPU cannot claim an allocated host interface");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains(&format!(
        "host-PF MAC {victim_mac} has conflicting ownership"
    )));

    // Reload networking and the instance independently so rollback is proved against committed rows.
    assert_eq!(fixture.persisted_repair_state().await?, persisted_before);
    assert_eq!(fixture.host_status().await.interfaces, before.interfaces);
    assert_eq!(
        fixture
            .harness
            .api()
            .find_instance_by_machine_id(Request::new(fixture.host.host.id.into()))
            .await?
            .into_inner(),
        allocation_before,
    );

    // Accept the same caller and factory identity with an unowned target to rule out an unrelated authorization failure.
    let Some(rpc::DiscoveryData::Info(discovery)) = request.discovery_data.as_mut() else {
        panic!("DPU discovery inventory")
    };
    discovery.dpu_info.as_mut().expect("DpuData").host_pfs[1].mac_address =
        fixture.targets[1].to_string();
    let accepted = fixture
        .harness
        .api()
        .discover_machine(Request::new(request))
        .await?
        .into_inner();
    assert_eq!(accepted.machine_id, Some(unpaired_dpu.into()));

    // Reload both machines to prove the accepted target pairs a different host without taking the allocated host's NIC.
    let accepted_dpu = fixture
        .harness
        .find_machine(unpaired_dpu.into())
        .await
        .remove(0)
        .status
        .expect("registered DPU");
    let accepted_host = accepted_dpu
        .associated_host_machine_id
        .expect("new host association");
    assert_ne!(accepted_host, fixture.host.host.id);
    let accepted_host = fixture
        .harness
        .find_machine(accepted_host.into())
        .await
        .remove(0)
        .status
        .expect("newly paired host");
    assert!(
        accepted_host
            .interfaces
            .iter()
            .any(
                |interface| interface.mac_address == fixture.targets[1].to_string()
                    && interface.attached_dpu_machine_id == Some(unpaired_dpu)
            )
    );
    assert_eq!(fixture.host_status().await.interfaces, before.interfaces);
    assert_eq!(
        fixture
            .harness
            .api()
            .find_instance_by_machine_id(Request::new(fixture.host.host.id.into()))
            .await?
            .into_inner(),
        allocation_before,
    );
    Ok(())
}

/// Preserves a registered BF4 host's absent or MAC-only desired target when Redfish omits its part number and Ready state or a persisted instance forbids repair.
/// Keeps a current compatibility baseline unchanged so ordinary legacy enrichment cannot silently advance verified boot intent before the BF4 guards run.
#[sqlx_test]
async fn missing_redfish_part_number_preserves_protected_registered_bf4_targets(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // Promote the fixture through public discovery so a later allocation references a stable host ID.
    let mut fixture = Bf4HostFixture::new(pool, 1).await?;
    let host_caller = fixture
        .harness
        .find_machine(fixture.host.host.id.into())
        .await
        .remove(0)
        .bmc
        .and_then(|bmc| bmc.machine_interface_id)
        .expect("host BMC interface");
    let promoted = fixture
        .harness
        .api()
        .discover_machine(Request::new(rpc::forge::MachineDiscoveryInfo {
            machine_interface_id: Some(host_caller),
            discovery_data: Some(rpc::DiscoveryData::Info(
                fixture.host.host.hardware_info().try_into()?,
            )),
            ..Default::default()
        }))
        .await?
        .into_inner();
    fixture.host.host.id = promoted.machine_id.expect("stable host ID").try_into()?;
    fixture.host.advance_state(ManagedHostState::Ready).await;
    fixture.register_inventory().await?;

    // Refresh every endpoint with the DPU part number omitted while keeping its identity and host-PF mapping registered.
    let explorer = fixture.harness.test_site_explorer(SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        create_machines: Arc::new(true.into()), // Exercise existing-host reconciliation before the after-commit BF4 guard.
        explorations_per_run: 10,
        dpu_host_pf_pci_controller_id: 2,
        retained_boot_interface_window: None,
        ..Default::default()
    });
    let endpoints = db::explored_endpoints::find_all(&fixture.pool).await?;
    let addresses = endpoints
        .iter()
        .map(|endpoint| endpoint.address)
        .collect::<Vec<_>>();
    explorer.insert_endpoints(
        endpoints
            .into_iter()
            .map(|endpoint| {
                let mut report = endpoint.report;
                if report.is_dpu() {
                    for chassis in &mut report.chassis {
                        chassis.part_number = None;
                    }
                }
                (endpoint.address, report)
            })
            .collect(),
    );
    let mut txn = fixture.pool.begin().await?;
    db::explored_endpoints::request_exploration_for_addresses(&addresses, txn.as_mut()).await?;
    txn.commit().await?;
    explorer.run_single_iteration().await?;

    // Reuse settled networking while testing the two different legacy writes and their independent BF4 guards.
    let primary = fixture
        .host_status()
        .await
        .interfaces
        .into_iter()
        .find(|interface| interface.primary_interface)
        .expect("provisional primary");
    let primary_mac: MacAddress = primary.mac_address.parse()?;
    assert!(
        fixture
            .boot_interfaces()
            .await?
            .machine_interfaces
            .iter()
            .find(|interface| interface.interface_id == primary.id)
            .expect("provisional boot row")
            .boot_interface_id
            .is_some()
    );
    for (scenario, allocated) in [
        // Missing targets must stay absent on Ready hosts rather than acquiring an assumed legacy baseline.
        ("Ready host with no desired target", false),
        // A committed instance must block same-MAC enrichment even if the controller has returned to DPUInit.
        ("allocated initializing host with a MAC-only target", true),
    ] {
        // Remove the desired row to model an upgraded host that predates durable boot intent.
        let mut txn = fixture.pool.begin().await?;
        sqlx::query("DELETE FROM machine_boot_interfaces WHERE machine_id = $1")
            .bind(fixture.host.host.id)
            .execute(txn.as_mut())
            .await?;
        if allocated {
            // Initialize the same MAC while Ready to retain a current compatibility baseline during the allocation case.
            db::machine_desired_boot_interface::initialize_if_unset(
                txn.as_mut(),
                &fixture.host.host.id,
                &model::machine_boot_interface::MachineBootInterfaceTarget::MacOnly(primary_mac),
                model::machine_boot_interface::BootInterfaceSelectionSource::LegacyUnknown,
            )
            .await?;
        }
        txn.commit().await?;
        if allocated {
            // Persist the allocation in DPUInit so the lifecycle check cannot hide a missing allocation guard.
            fixture.initialize().await;
            sqlx::query(
                r#"INSERT INTO instances (machine_id, operating_system_id, os_ipxe_script, network_config, nvlink_config)
                   VALUES ($1, NULL, '#!ipxe', '{"interfaces": [], "auto_config": null}'::jsonb, '{"gpu_configs": []}'::jsonb)"#,
            )
            .bind(fixture.host.host.id)
            .execute(&fixture.pool)
            .await?;
        }

        // Reload desired intent and allocations so the operation starts from the committed protected state.
        let before_boot = fixture.boot_interfaces().await?.reconciliation;
        if allocated {
            let baseline = before_boot
                .as_ref()
                .expect("MAC-only compatibility baseline");
            let target = baseline
                .desired_boot_interface
                .as_ref()
                .expect("desired MAC");
            assert_eq!(target.mac_address, primary.mac_address);
            assert_eq!(target.interface_id, None);
            assert_eq!(
                baseline.verified_version.as_deref(),
                Some(baseline.desired_version.as_str())
            );
            assert!(baseline.is_compatibility_baseline);
        } else {
            assert_eq!(before_boot, None);
        }
        let allocations = fixture
            .harness
            .api()
            .find_instance_by_machine_id(Request::new(fixture.host.host.id.into()))
            .await?
            .into_inner();
        assert_eq!(allocations.instances.len(), usize::from(allocated));
        let before = fixture.persisted_repair_state().await?;

        // Request a real refresh so create_machines revisits the already ingested host instead of skipping it.
        let mut txn = fixture.pool.begin().await?;
        db::explored_endpoints::request_exploration_for_addresses(&addresses, txn.as_mut()).await?;
        txn.commit().await?;
        let identified = explorer.run_single_iteration().await?;

        // Confirm pairing survives the incomplete Redfish report, then reload every protected write and public desired field.
        assert_eq!(identified.len(), 1, "{scenario}");
        assert_eq!(identified[0].0.dpus.len(), 1, "{scenario}");
        assert!(
            identified[0].0.dpus[0]
                .report
                .chassis
                .iter()
                .all(|chassis| chassis.part_number.is_none())
        );
        assert_eq!(
            fixture.persisted_repair_state().await?,
            before,
            "{scenario}"
        );
        assert_eq!(
            fixture.boot_interfaces().await?.reconciliation,
            before_boot,
            "{scenario}"
        );
    }
    Ok(())
}

/// Completes an Operator MacOnly target after exploration learns its ID even without the configured BF4 host-PF, preserving selection and networking instead of depending on automatic inventory.
#[sqlx_test]
async fn late_operator_interface_id_preserves_boot_authority_and_networking(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // The Operator NIC starts as an ID-less prediction while the provisional primary retains administrative traffic.
    let mut fixture = Bf4HostFixture::new(pool, 1).await?;
    // Keep valid controller-1 inventory but omit configured controller 2 so automatic BF4 selection remains unresolved.
    let Some(rpc::DiscoveryData::Info(discovery)) = fixture.requests[0].discovery_data.as_mut()
    else {
        panic!("fixture inventory")
    };
    discovery
        .dpu_info
        .as_mut()
        .expect("DpuData")
        .host_pfs
        .retain(|host_pf| host_pf.pci_controller_id != 2);
    fixture.register_inventory().await?;
    let operator_mac = fixture.targets[0];
    let host_machine_id = carbide_uuid::machine::MachineId::from(fixture.host.host.id);
    let bmc_address = fixture
        .harness
        .find_machine(fixture.host.host.id.into())
        .await
        .remove(0)
        .bmc
        .and_then(|bmc| bmc.ip)
        .expect("host BMC address")
        .parse()?;
    let mut txn = fixture.pool.begin().await?;
    let mut report = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![bmc_address])
        .await?
        .remove(0)
        .report;
    let resolved_report = report.clone();
    // Clear only the selected NIC's ID to model the host report before late exploration resolves it.
    for interface in report
        .systems
        .iter_mut()
        .flat_map(|system| &mut system.ethernet_interfaces)
    {
        if interface.mac_address == Some(operator_mac) {
            interface.id = None;
        }
    }
    sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
        .bind(bmc_address)
        .bind(sqlx::types::Json(&report))
        .execute(txn.as_mut())
        .await?;
    db::predicted_machine_interface::create(
        model::predicted_machine_interface::NewPredictedMachineInterface {
            machine_id: &host_machine_id,
            mac_address: operator_mac, // Select a different MAC from the provisional desired target.
            expected_network_segment_type: model::network_segment::NetworkSegmentType::HostInband,
            boot_interface_id: None, // The first exploration has not resolved this ID.
            primary_interface: false, // Boot-only intent must not move the networking primary.
        },
        txn.as_mut(),
    )
    .await?;
    txn.commit().await?;
    fixture.initialize().await;

    // Record boot-only authority through the public action, then reload its persisted MAC-only baseline.
    fixture
        .harness
        .api()
        .machine_setup(Request::new(rpc::forge::MachineSetupRequest {
            machine_id: Some(fixture.host.host.id.to_string()),
            boot_interface_mac: Some(operator_mac.to_string()),
            bmc_endpoint_request: None,
        }))
        .await?;
    let operator_selection = fixture
        .boot_interfaces()
        .await?
        .reconciliation
        .expect("operator selection");
    let initial_target = operator_selection
        .desired_boot_interface
        .as_ref()
        .expect("MAC-only target");
    assert_eq!(initial_target.mac_address, operator_mac.to_string());
    assert_eq!(initial_target.interface_id, None);
    assert_eq!(
        operator_selection.selection_source,
        rpc::forge::BootInterfaceSelectionSource::Operator as i32
    );
    assert!(operator_selection.selection_updated_at.is_some());
    let before = fixture.host_status().await;
    let before_network = fixture
        .harness
        .api()
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(fixture.host.dpus[0].id),
            },
        ))
        .await?
        .into_inner();

    // Queue the host's later report so real exploration must persist the predicted ID before background reconciliation can enrich intent.
    let explorer = fixture.harness.test_site_explorer(SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        create_machines: Arc::new(false.into()),
        explorations_per_run: 1,
        dpu_host_pf_pci_controller_id: 2,
        retained_boot_interface_window: None,
        ..Default::default()
    });
    // Seed unchanged endpoint reports because the same pass may also explore a queued DPU.
    explorer.insert_endpoints(
        db::explored_endpoints::find_all(&fixture.pool)
            .await?
            .into_iter()
            .map(|endpoint| (endpoint.address, endpoint.report))
            .collect(),
    );
    explorer.insert_endpoints(vec![(bmc_address, resolved_report)]);
    let mut txn = fixture.pool.begin().await?;
    db::explored_endpoints::request_exploration_for_addresses(&[bmc_address], txn.as_mut()).await?;
    txn.commit().await?;
    explorer.run_single_iteration().await?;

    // Reload public stores to prove the new pair retained Operator authority and the network response stayed unchanged.
    let view = fixture.boot_interfaces().await?;
    let prediction = view
        .predicted_interfaces
        .iter()
        .find(|prediction| prediction.mac_address == operator_mac.to_string())
        .expect("refreshed prediction");
    assert_eq!(
        prediction.boot_interface_id.as_deref(),
        Some("BF4.Controller2.Dpu0")
    );
    let enriched_selection = view.reconciliation.expect("enriched operator selection");
    let enriched_target = enriched_selection
        .desired_boot_interface
        .expect("completed pair");
    assert_eq!(enriched_target.mac_address, operator_mac.to_string());
    assert_eq!(
        enriched_target.interface_id.as_deref(),
        Some("BF4.Controller2.Dpu0")
    );
    assert_ne!(
        enriched_selection.desired_version,
        operator_selection.desired_version
    );
    assert_eq!(
        enriched_selection.selection_source,
        operator_selection.selection_source
    );
    assert_eq!(
        enriched_selection.selection_updated_at,
        operator_selection.selection_updated_at
    );
    let after = fixture.host_status().await;
    assert_eq!(after.interfaces, before.interfaces);
    let after_network = fixture
        .harness
        .api()
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(fixture.host.dpus[0].id),
            },
        ))
        .await?
        .into_inner();
    assert_eq!(
        after_network.managed_host_config,
        before_network.managed_host_config
    );
    assert_eq!(
        after_network.managed_host_config_version,
        before_network.managed_host_config_version
    );
    Ok(())
}

/// Keeps an ingested declared NIC usable for BF4 boot work before its first DHCP lease, so DPU registration cannot strand initial provisioning.
/// Transfers DHCP to a different registered PF before detachment and releases it after the declared NIC leases, preventing obsolete allocations from consuming pool capacity.
#[sqlx_test]
async fn declared_primary_prediction_survives_bf4_registration_before_dhcp(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // Only the declared integrated NIC remains unleased; the ordinary builder supplies the BMCs and provisional DPU link.
    let harness = TestHarness::builder(pool.clone())
        // Underlay, Admin, and HostInband each require a VLAN and VNI; the defaults provide only two of each.
        .with_resource_pools(
            ResourcePoolBuilder::default()
                .with_vlan_ids(1, 3)
                .with_vnis(10001, 10003)
                .build(),
        )
        .build()
        .await;
    let domain = harness.test_domain().await;
    let controller = harness.network_controller();
    let underlay = controller.create_underlay_segment(&domain).await;
    controller.create_admin_segment(&domain).await;
    let host_inband_segment = controller.create_host_inband_segment(&domain).await;
    let explorer = harness.test_site_explorer(SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        create_machines: Arc::new(true.into()),
        explorations_per_run: 10,
        retained_boot_interface_window: None,
        ..Default::default()
    });
    let declared_mac: MacAddress = "02:00:00:00:be:01".parse()?;
    let mut config = ManagedHostConfig::default();
    config.non_dpu_macs.push(declared_mac); // Redfish must expose the integrated NIC before it leases.
    config.expected_machine_data = Some(model::expected_machine::ExpectedMachineData {
        serial_number: config.serial.clone(),
        interfaces: vec![ExpectedInterface {
            mac_address: Some(declared_mac),
            role: ExpectedInterfaceRole::Host,
            primary: Some(true), // Keep declared boot intent separate from the provisional real primary.
            ..Default::default()
        }],
        ..Default::default()
    });
    let (host, build_data) = harness
        .managed_host_builder(&explorer, underlay)
        .with_config(config.clone())
        .build()
        .await;
    let dpu_id = host.dpus[0].id;

    // Reload ingestion's pending prediction and prove that no integrated-NIC DHCP row exists.
    let mut txn = pool.begin().await?;
    let prediction = db::predicted_machine_interface::find_by_mac_address(&mut txn, declared_mac)
        .await?
        .expect("ingestion must retain declared primary intent before DHCP");
    assert_eq!(prediction.machine_id, host.host.id.into());
    assert!(prediction.primary_interface);
    assert_eq!(
        prediction.expected_network_segment_type,
        model::network_segment::NetworkSegmentType::HostInband
    );
    assert!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), declared_mac)
            .await?
            .is_empty()
    );
    // Publish a distinct registered PF so repair must replace the leased provisional primary while the declared NIC is still a prediction.
    let registered_mac: MacAddress = "02:00:00:00:be:02".parse()?;
    assert_ne!(registered_mac, config.dpus[0].host_mac_address);
    let registered_boot_id = "BF4.RegisteredPF0";
    let mut report =
        db::explored_endpoints::find_by_ips(txn.as_mut(), vec![build_data.host_bmc_ip()])
            .await?
            .remove(0)
            .report;
    report.systems[0]
        .ethernet_interfaces
        .push(EthernetInterface {
            id: Some(registered_boot_id.to_string()),
            mac_address: Some(registered_mac), // Registration must replace the provisional MAC rather than reuse its row.
            uefi_device_path: Some("PciRoot(0x0)/Pci(0x2,0x0)".parse()?),
            ..Default::default()
        });
    sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
        .bind(build_data.host_bmc_ip())
        .bind(sqlx::types::Json(&report))
        .execute(txn.as_mut())
        .await?;
    // Allow the fixture's next phone-home to introduce BF4 classification without changing its machine identity.
    db::machine_topology::set_topology_update_needed(&mut txn, &dpu_id.into(), true).await?;
    // Commit the current Redfish evidence before registration so its repair can resolve the new PF.
    txn.commit().await?;
    // Retain the added PF during subsequent mock explorations so repair sees the same Redfish evidence.
    explorer.insert_endpoint_result(build_data.host_bmc_ip(), Ok(report));
    let before = harness
        .find_machine(host.host.id.into())
        .await
        .remove(0)
        .status
        .expect("host before BF4 registration");
    let provisional = before
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("leased provisional primary");
    assert_eq!(
        provisional.mac_address,
        config.dpus[0].host_mac_address.to_string()
    );
    assert!(!provisional.address.is_empty());

    // Register BF4 inventory through DiscoverMachine so its after-commit repair consumes the ingestion-created prediction.
    let registered_dpu = harness.find_machine(dpu_id.into()).await.remove(0);
    let caller = registered_dpu
        .bmc
        .as_ref()
        .and_then(|bmc| bmc.machine_interface_id)
        .expect("owned DPU BMC interface");
    let mut discovery = registered_dpu
        .status
        .expect("DPU status")
        .discovery_info
        .expect("DPU inventory");
    let hardware = discovery.dpu_info.as_mut().expect("DpuData");
    hardware.part_number = "900-9D4B4-CWAA-TSA".to_string(); // Enable registered BF4 repair and readiness.
    hardware.host_pfs = vec![rpc::machine_discovery::DpuHostPf {
        pci_controller_id: 1,
        pf_id: 0,
        mac_address: registered_mac.to_string(),
        physical_port_name: "c1pf0".to_string(),
        ..Default::default()
    }];
    harness
        .api()
        .discover_machine(Request::new(rpc::forge::MachineDiscoveryInfo {
            machine_interface_id: Some(caller),
            discovery_data: Some(rpc::DiscoveryData::Info(discovery)),
            discovery_reporter: rpc::forge::MachineDiscoveryReporter::DpuAgent as i32,
            ..Default::default()
        }))
        .await?;
    explorer.run_single_iteration().await?;

    // Read persisted boot intent and readiness after public registration and background repair, with DHCP still absent.
    let view = harness
        .api()
        .get_machine_boot_interfaces(Request::new(rpc::forge::GetMachineBootInterfacesRequest {
            machine_id: Some(host.host.id.into()),
        }))
        .await?
        .into_inner();
    let repaired = harness
        .find_machine(host.host.id.into())
        .await
        .remove(0)
        .status
        .expect("host after BF4 registration");
    let links = repaired
        .interfaces
        .iter()
        .filter(|interface| interface.attached_dpu_machine_id == Some(dpu_id))
        .collect::<Vec<_>>();
    assert_eq!(links.len(), 1);
    assert!(links[0].primary_interface);
    assert_eq!(links[0].mac_address, registered_mac.to_string());
    // Verify the original DHCP allocation moved to the registered PF and the provisional row is detached without a lease.
    assert_eq!(links[0].address, provisional.address);
    let retired = repaired
        .interfaces
        .iter()
        .find(|interface| interface.id == provisional.id)
        .expect("retained provisional row");
    assert!(!retired.primary_interface);
    assert_eq!(retired.attached_dpu_machine_id, None);
    assert!(retired.address.is_empty());
    assert_eq!(
        view.machine_interfaces
            .iter()
            .find(|interface| interface.interface_id == links[0].id)
            .expect("registered DPU boot row")
            .boot_interface_id
            .as_deref(),
        Some(registered_boot_id)
    );
    let selection = view.reconciliation.expect("declared boot selection");
    assert_eq!(
        selection.selection_source,
        rpc::forge::BootInterfaceSelectionSource::ExpectedMachine as i32
    );
    let desired = selection
        .desired_boot_interface
        .expect("declared boot pair");
    assert_eq!(desired.mac_address, declared_mac.to_string());
    assert_eq!(desired.interface_id, prediction.boot_interface_id);
    let target = model::machine_boot_interface::MachineBootInterfaceTarget::Pair(
        model::machine_boot_interface::MachineBootInterface {
            mac_address: declared_mac,
            interface_id: desired.interface_id.expect("current Redfish ID"),
        },
    );
    assert!(
        db::host_boot_interface::not_ready_reason(&pool, &host.host.id, 1)
            .await?
            .is_none()
    );
    assert!(
        db::host_boot_interface::boot_order_not_ready_reason(&pool, &host.host.id, 1, &target)
            .await?
            .is_none()
    );
    let mut txn = pool.begin().await?;
    assert!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), declared_mac)
            .await?
            .is_empty()
    );
    let preserved = db::predicted_machine_interface::find_by_mac_address(&mut txn, declared_mac)
        .await?
        .expect("first boot must not fabricate a leased interface");
    assert_eq!(preserved.id, prediction.id);
    assert_eq!(preserved.machine_id, prediction.machine_id);
    assert!(preserved.primary_interface);
    txn.rollback().await?;

    // Materialize the declared NIC through its first HostInband DHCP request, then reconcile the now-dormant registered PF.
    let lease = harness
        .api()
        .discover_dhcp(
            rpc::forge::DhcpDiscovery::builder(declared_mac, host_inband_segment.relay_address)
                .vendor_string("PXEClient")
                .tonic_request(),
        )
        .await?
        .into_inner();
    explorer.run_single_iteration().await?;

    // Reload committed networking to prove the integrated NIC is primary and neither Admin row retains DHCP.
    let promoted = harness
        .find_machine(host.host.id.into())
        .await
        .remove(0)
        .status
        .expect("host after integrated-NIC DHCP");
    let primary = promoted
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("promoted integrated primary");
    assert_eq!(primary.mac_address, declared_mac.to_string());
    assert_eq!(primary.segment_id, Some(host_inband_segment.id));
    assert_eq!(primary.address, vec![lease.address]);
    for mac in [config.dpus[0].host_mac_address, registered_mac] {
        let admin_interface = promoted
            .interfaces
            .iter()
            .find(|interface| interface.mac_address == mac.to_string())
            .expect("retained Admin interface");
        assert!(!admin_interface.primary_interface);
        assert!(admin_interface.address.is_empty());
    }
    let mut txn = pool.begin().await?;
    assert!(
        db::predicted_machine_interface::find_by_mac_address(&mut txn, declared_mac)
            .await?
            .is_none()
    );
    txn.rollback().await?;
    Ok(())
}
