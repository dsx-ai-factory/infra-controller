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

use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bmc_explorer::test_support::generate_managed_host_reports;
use bmc_mock::HardwareType;
use carbide_site_explorer::config::{SiteExplorerConfig, SiteExplorerExploreMode};
use carbide_test_harness::network::segment::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    DpuConfigExt as _, FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::machine::MachineId;
use db::ObjectFilter;
use db::sku::CURRENT_SKU_VERSION;
use itertools::Itertools;
use mac_address::MacAddress;
use model::bmc_suppression::{BmcSuppressionSubsystem, NewBmcSuppression};
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{AnyMachine, LoadSnapshotOptions};
use model::machine_boot_interface::BootInterfaceSelectionSource;
use model::metadata::Metadata;
use model::site_explorer::{
    BlueFieldOperatingMode, Chassis, ComputerSystem, EndpointExplorationError,
    EndpointExplorationReport, EndpointType, ExploredDpu, ExploredManagedHost, NetworkAdapter,
    PreingestionState, UefiDevicePath,
};
use model::test_support::{DpuConfig, ManagedHostConfig};
use rpc::forge::GetSiteExplorationRequest;
use rpc::site_explorer::ExploredDpu as RpcExploredDpu;
use tonic::Request;

use crate::env::{self, Env};

const LAST_RUN_MISSING_CREDENTIAL_KEY: &str = "machines/bmc/site/root";

trait EnvExt {
    fn new_machine(&self, mac: &str, vendor: &str) -> FakeMachine;
}

impl EnvExt for Env {
    fn new_machine(&self, mac: &str, vendor: &str) -> FakeMachine {
        FakeMachine::new(self.underlay_segment, mac, vendor)
    }
}

trait DiscoverDhcp {
    async fn discover_dhcp(&mut self, api: &Api) -> Result<(), Box<dyn std::error::Error>>;
}

impl DiscoverDhcp for FakeMachine {
    async fn discover_dhcp(&mut self, api: &Api) -> Result<(), Box<dyn std::error::Error>> {
        let response = api
            .discover_dhcp(
                rpc::forge::DhcpDiscovery::builder(self.mac, self.segment.relay_address)
                    .vendor_string(&self.dhcp_vendor)
                    .tonic_request(),
            )
            .await?
            .into_inner();
        tracing::info!(
            mac_address = %self.mac,
            ip_address = %response.address,
            "DHCP assigned ip"
        );
        self.ip = response.address;
        Ok(())
    }
}

impl DiscoverDhcp for Vec<FakeMachine> {
    async fn discover_dhcp(&mut self, api: &Api) -> Result<(), Box<dyn std::error::Error>> {
        for machine in self.iter_mut() {
            machine.discover_dhcp(api).await?
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct FakeMachine {
    pub mac: MacAddress,
    pub dhcp_vendor: String,
    pub segment: TestNetworkSegment,
    pub ip: String,
}

impl FakeMachine {
    fn new(segment: TestNetworkSegment, mac: &str, vendor: &str) -> Self {
        Self {
            mac: mac.parse().unwrap(),
            dhcp_vendor: vendor.to_string(),
            segment,
            ip: String::new(),
        }
    }

    fn as_mock_dpu(&self) -> DpuConfig {
        DpuConfig {
            bmc_mac_address: self.mac,
            ..DpuConfig::default()
        }
    }

    fn as_mock_host(&self, dpus: Vec<DpuConfig>) -> ManagedHostConfig {
        ManagedHostConfig {
            bmc_mac_address: self.mac,
            dpus,
            ..ManagedHostConfig::default()
        }
    }
}

fn suppression_test_config(explorations_per_run: u64) -> SiteExplorerConfig {
    SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run,
        concurrent_explorations: 1,
        run_interval: Duration::from_secs(1),
        create_machines: Arc::new(false.into()),
        create_power_shelves: Arc::new(false.into()),
        create_switches: Arc::new(false.into()),
        ..Default::default()
    }
}

fn suppression_input(
    bmc_mac_address: MacAddress,
    subsystem: BmcSuppressionSubsystem,
) -> NewBmcSuppression {
    NewBmcSuppression {
        bmc_mac_address,
        reason: "site explorer suppression test".to_string(),
        subsystem,
    }
}

fn cached_suppression_report(details: &str) -> EndpointExplorationReport {
    EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        last_exploration_error: Some(EndpointExplorationError::Unauthorized {
            details: details.to_string(),
            response_body: None,
            response_code: None,
        }),
        ..Default::default()
    }
}

#[sqlx_test]
async fn test_periodic_suppression_skips_every_candidate_class(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let mut machines = vec![
        env.new_machine("02:00:00:00:10:01", "Vendor1"),
        env.new_machine("02:00:00:00:10:02", "Vendor2"),
        env.new_machine("02:00:00:00:10:03", "Vendor3"),
        env.new_machine("02:00:00:00:10:04", "Vendor4"),
    ];
    machines.discover_dhcp(env.api()).await?;

    let unexplored_ip: IpAddr = machines[0].ip.parse()?;
    let priority_ip: IpAddr = machines[1].ip.parse()?;
    let routine_ip: IpAddr = machines[2].ip.parse()?;
    let dhcp_only_ip: IpAddr = machines[3].ip.parse()?;
    let priority_report = cached_suppression_report("priority refresh");
    let routine_report = cached_suppression_report("routine refresh");

    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::insert(priority_ip, &priority_report, false, txn.as_mut()).await?;
    db::explored_endpoints::insert(routine_ip, &routine_report, false, txn.as_mut()).await?;
    db::explored_endpoints::request_exploration_for_addresses(&[priority_ip], txn.as_mut()).await?;
    for machine in &machines[..3] {
        db::bmc_suppression::upsert(
            txn.as_mut(),
            &suppression_input(machine.mac, BmcSuppressionSubsystem::SiteExplorer),
        )
        .await?;
    }
    db::bmc_suppression::upsert(
        txn.as_mut(),
        &suppression_input(machines[3].mac, BmcSuppressionSubsystem::Dhcp),
    )
    .await?;
    let priority_before = db::explored_endpoints::find_all_by_ip(priority_ip, txn.as_mut()).await?;
    let routine_before = db::explored_endpoints::find_all_by_ip(routine_ip, txn.as_mut()).await?;
    txn.commit().await?;
    assert_eq!(priority_before.len(), 1);
    assert!(priority_before[0].exploration_requested);
    assert_eq!(routine_before.len(), 1);
    assert!(!routine_before[0].exploration_requested);

    let explorer = env.test_site_explorer(suppression_test_config(10));
    explorer.insert_endpoints(vec![(
        dhcp_only_ip,
        EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            ..Default::default()
        },
    )]);
    explorer.run_single_iteration().await?;

    assert_eq!(
        explorer
            .endpoint_explorer()
            .explore_endpoint_calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.ip_address)
            .collect::<Vec<_>>(),
        vec![dhcp_only_ip]
    );

    let mut txn = env.pool.begin().await?;
    assert!(
        db::explored_endpoints::find_all_by_ip(unexplored_ip, txn.as_mut())
            .await?
            .is_empty()
    );
    assert_eq!(
        db::explored_endpoints::find_all_by_ip(priority_ip, txn.as_mut()).await?,
        priority_before
    );
    assert_eq!(
        db::explored_endpoints::find_all_by_ip(routine_ip, txn.as_mut()).await?,
        routine_before
    );
    assert_eq!(
        db::explored_endpoints::find_all_by_ip(dhcp_only_ip, txn.as_mut())
            .await?
            .len(),
        1
    );
    for machine in &machines[..3] {
        assert!(
            db::bmc_suppression::find(
                txn.as_mut(),
                machine.mac,
                BmcSuppressionSubsystem::SiteExplorer,
            )
            .await?
            .unwrap()
            .acknowledged_at
            .is_some()
        );
    }
    let dhcp_only_suppression =
        db::bmc_suppression::find(txn.as_mut(), machines[3].mac, BmcSuppressionSubsystem::Dhcp)
            .await?
            .unwrap();
    assert!(dhcp_only_suppression.acknowledged_at.is_none());
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_suppression_skips_cached_switch_and_power_shelf_ingestion(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let mut devices = vec![
        env.new_machine("02:00:00:00:10:11", "SwitchVendor"),
        env.new_machine("02:00:00:00:10:12", "PowerShelfVendor"),
    ];
    devices.discover_dhcp(env.api()).await?;
    let switch_ip: IpAddr = devices[0].ip.parse()?;
    let power_shelf_ip: IpAddr = devices[1].ip.parse()?;

    let switch_report = EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        chassis: vec![Chassis {
            id: "mgx_nvswitch_0".to_string(),
            manufacturer: Some("NVIDIA".to_string()),
            model: Some("Switch".to_string()),
            serial_number: Some("SUPPRESSED-SWITCH".to_string()),
            part_number: Some("SUPPRESSED-SWITCH".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let power_shelf_report = EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        chassis: vec![Chassis {
            id: "powershelf".to_string(),
            manufacturer: Some("NVIDIA".to_string()),
            model: Some("PowerShelf".to_string()),
            serial_number: Some("SUPPRESSED-POWER-SHELF".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };

    let mut txn = env.pool.begin().await?;
    db::expected_switch::create(
        &mut txn,
        model::expected_switch::ExpectedSwitch {
            expected_switch_id: None,
            bmc_mac_address: devices[0].mac,
            nvos_mac_addresses: Vec::new(),
            serial_number: "SUPPRESSED-SWITCH".to_string(),
            bmc_username: "admin".to_string(),
            bmc_password: "password".to_string(),
            nvos_username: None,
            nvos_password: None,
            bmc_ip_address: None,
            nvos_ip_address: None,
            metadata: Metadata {
                name: "Suppressed switch".to_string(),
                ..Default::default()
            },
            rack_id: None,
            bmc_retain_credentials: None,
        },
    )
    .await?;
    db::expected_power_shelf::create(
        &mut txn,
        model::expected_power_shelf::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: devices[1].mac,
            bmc_username: "admin".to_string(),
            bmc_password: "password".to_string(),
            serial_number: "SUPPRESSED-POWER-SHELF".to_string(),
            bmc_ip_address: None,
            metadata: Metadata {
                name: "Suppressed power shelf".to_string(),
                ..Default::default()
            },
            rack_id: None,
            bmc_retain_credentials: None,
        },
    )
    .await?;
    for (device, ip, report) in [
        (&devices[0], switch_ip, &switch_report),
        (&devices[1], power_shelf_ip, &power_shelf_report),
    ] {
        db::explored_endpoints::insert(ip, report, false, txn.as_mut()).await?;
        db::explored_endpoints::set_preingestion_complete(ip, &mut txn).await?;
        db::bmc_suppression::upsert(
            txn.as_mut(),
            &suppression_input(device.mac, BmcSuppressionSubsystem::SiteExplorer),
        )
        .await?;
    }
    txn.commit().await?;

    let explorer = env.test_site_explorer(SiteExplorerConfig {
        create_machines: Arc::new(false.into()),
        create_switches: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        switches_created_per_run: 1,
        power_shelves_created_per_run: 1,
        ..suppression_test_config(2)
    });
    explorer.run_single_iteration().await?;

    let mut txn = env.pool.begin().await?;
    assert!(
        db::switch::find_by_bmc_mac_address(txn.as_mut(), devices[0].mac)
            .await?
            .is_none()
    );
    assert!(
        db::power_shelf::find_by_bmc_mac_address(txn.as_mut(), devices[1].mac)
            .await?
            .is_none()
    );
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_suppressed_unexplored_endpoint_does_not_consume_budget_and_resumes(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let mut machines = vec![
        env.new_machine("02:00:00:00:11:01", "Vendor1"),
        env.new_machine("02:00:00:00:11:02", "Vendor2"),
    ];
    machines.discover_dhcp(env.api()).await?;

    let suppressed_ip: IpAddr = machines[0].ip.parse()?;
    let routine_ip: IpAddr = machines[1].ip.parse()?;
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::insert(
        routine_ip,
        &cached_suppression_report("eligible routine refresh"),
        false,
        txn.as_mut(),
    )
    .await?;
    db::bmc_suppression::upsert(
        txn.as_mut(),
        &suppression_input(machines[0].mac, BmcSuppressionSubsystem::SiteExplorer),
    )
    .await?;
    let routine_before = db::explored_endpoints::find_all_by_ip(routine_ip, txn.as_mut()).await?;
    txn.commit().await?;
    assert_eq!(routine_before.len(), 1);

    let explorer = env.test_site_explorer(suppression_test_config(1));
    explorer.insert_endpoints(vec![
        (
            suppressed_ip,
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                ..Default::default()
            },
        ),
        (
            routine_ip,
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                ..Default::default()
            },
        ),
    ]);
    explorer.run_single_iteration().await?;

    assert_eq!(
        explorer
            .endpoint_explorer()
            .explore_endpoint_calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.ip_address)
            .collect::<Vec<_>>(),
        vec![routine_ip]
    );
    let mut txn = env.pool.begin().await?;
    assert!(
        db::explored_endpoints::find_all_by_ip(suppressed_ip, txn.as_mut())
            .await?
            .is_empty()
    );
    let routine_after = db::explored_endpoints::find_all_by_ip(routine_ip, txn.as_mut()).await?;
    assert_eq!(routine_after.len(), 1);
    assert_eq!(
        routine_after[0].report_version.version_nr(),
        routine_before[0].report_version.version_nr() + 1
    );
    db::bmc_suppression::delete(
        txn.as_mut(),
        machines[0].mac,
        BmcSuppressionSubsystem::SiteExplorer,
    )
    .await?;
    txn.commit().await?;

    explorer
        .endpoint_explorer()
        .explore_endpoint_calls
        .lock()
        .unwrap()
        .clear();
    explorer.run_single_iteration().await?;

    assert_eq!(
        explorer
            .endpoint_explorer()
            .explore_endpoint_calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.ip_address)
            .collect::<Vec<_>>(),
        vec![suppressed_ip]
    );
    let mut txn = env.pool.begin().await?;
    assert_eq!(
        db::explored_endpoints::find_all_by_ip(suppressed_ip, txn.as_mut())
            .await?
            .len(),
        1
    );
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_suppression_is_acknowledged_before_precondition_failure(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let suppressed_mac: MacAddress = "02:00:00:00:12:01".parse()?;
    let mut txn = env.pool.begin().await?;
    db::bmc_suppression::upsert(
        txn.as_mut(),
        &suppression_input(suppressed_mac, BmcSuppressionSubsystem::SiteExplorer),
    )
    .await?;
    txn.commit().await?;

    let explorer = env.test_site_explorer(suppression_test_config(1));
    explorer.endpoint_explorer().set_precondition_result(Err(
        EndpointExplorationError::MissingCredentials {
            key: LAST_RUN_MISSING_CREDENTIAL_KEY.to_string(),
            cause: "missing site-wide credential".to_string(),
        },
    ));
    explorer
        .run_single_iteration()
        .await
        .expect_err("credential precondition should fail the iteration");

    assert_eq!(
        explorer.endpoint_explorer().explore_endpoint_call_count(),
        0
    );
    let mut txn = env.pool.begin().await?;
    assert!(
        db::bmc_suppression::find(
            txn.as_mut(),
            suppressed_mac,
            BmcSuppressionSubsystem::SiteExplorer,
        )
        .await?
        .unwrap()
        .acknowledged_at
        .is_some()
    );
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_suppression_acknowledgement_waits_for_in_flight_exploration(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let mut machine = env.new_machine("02:00:00:00:12:11", "Vendor");
    machine.discover_dhcp(env.api()).await?;
    let bmc_ip: IpAddr = machine.ip.parse()?;
    let report = EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        ..Default::default()
    };

    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::insert(bmc_ip, &report, false, txn.as_mut()).await?;
    txn.commit().await?;

    let explorer = env.test_site_explorer(suppression_test_config(1));
    explorer.insert_endpoints(vec![(bmc_ip, report)]);
    let blocker = explorer.endpoint_explorer().block_next_exploration();
    let service = explorer.endpoint_exploration_service();
    let refresh = tokio::spawn(async move { service.refresh_endpoint_report(bmc_ip).await });
    blocker.wait_until_started().await;

    let mut txn = env.pool.begin().await?;
    db::bmc_suppression::upsert(
        txn.as_mut(),
        &suppression_input(machine.mac, BmcSuppressionSubsystem::SiteExplorer),
    )
    .await?;
    txn.commit().await?;

    explorer.run_single_iteration().await?;
    let suppression = db::bmc_suppression::find(
        &env.pool,
        machine.mac,
        BmcSuppressionSubsystem::SiteExplorer,
    )
    .await?
    .unwrap();
    assert!(
        suppression.acknowledged_at.is_none(),
        "suppression must remain pending while an endpoint probe holds the lock"
    );

    blocker.release();
    refresh.await??;
    explorer.run_single_iteration().await?;
    let suppression = db::bmc_suppression::find(
        &env.pool,
        machine.mac,
        BmcSuppressionSubsystem::SiteExplorer,
    )
    .await?
    .unwrap();
    assert!(
        suppression.acknowledged_at.is_some(),
        "suppression should be acknowledged after the in-flight probe releases its lock"
    );

    Ok(())
}

#[sqlx_test]
async fn test_handle_redfish_error_powers_on_machine(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let mut machine = env.new_machine("6a:6b:6c:6d:6e:70", "Vendor1");
    machine.discover_dhcp(env.api()).await?;
    let bmc_ip: IpAddr = machine.ip.parse()?;

    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: machine.mac,
            data: ExpectedMachineData {
                serial_number: "host-needs-power-on".to_string(),
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_result(
        bmc_ip,
        Err(EndpointExplorationError::RedfishError {
            details: "transient redfish failure".to_string(),
            response_body: None,
            response_code: Some(500),
        }),
    );
    explorer
        .endpoint_explorer()
        .power_states
        .lock()
        .unwrap()
        .insert(bmc_ip, libredfish::PowerState::Off);

    explorer.run_single_iteration().await?;

    {
        let calls = explorer
            .endpoint_explorer()
            .redfish_power_control_calls
            .lock()
            .unwrap();
        assert_eq!(
            calls.as_slice(),
            &[(
                std::net::SocketAddr::new(bmc_ip, 443),
                libredfish::SystemPowerControl::On
            )]
        );
    }

    let mut txn = env.pool.begin().await?;
    let endpoints = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;
    txn.commit().await?;
    assert_eq!(endpoints.len(), 1, "expected one explored endpoint");
    Ok(())
}

/// Strict ingestion gate: a host whose BMC reports no DPU PCIe devices and
/// whose effective `HostDpuPolicy` resolves to `Manage` is skipped (with a
/// warning + a `NoDpuReportedByHost` pairing-blocker metric) rather than
/// ingested. Operators opt in to zero-DPU through `Nic` or `Ignore`.
#[sqlx_test]
async fn test_site_explorer_skips_unexpected_zero_dpu_host(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let mut machine = env.new_machine("AA:AB:AC:AD:AA:11", "Vendor1");
    machine.discover_dhcp(env.api()).await?;

    // ExpectedMachine with the default `Manage` policy -- the host is expected
    // to have DPUs.
    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: machine.mac,
            data: ExpectedMachineData {
                serial_number: "host-expected-dpus-but-has-none".to_string(),
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    // BMC report with no PCIe devices / no chassis -- the gate sees
    // zero DPUs.
    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![(
        machine.ip.parse().unwrap(),
        Ok(EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            vendor: Some(bmc_vendor::BMCVendor::Lenovo),
            systems: vec![ComputerSystem {
                serial_number: Some("0123456789".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }),
    )]);
    let test_meter = &env.test_harness.test_meter;

    // First iteration populates `explored_endpoints`; second runs
    // `identify_managed_hosts` after preingestion is complete.
    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::set_preingestion_complete(machine.ip.parse().unwrap(), &mut txn)
        .await?;
    txn.commit().await?;
    explorer.run_single_iteration().await.unwrap();

    // No managed host should have been identified.
    let explored_managed_hosts = db::explored_managed_host::find_all(&env.pool).await?;
    assert!(
        explored_managed_hosts.is_empty(),
        "strict gate should refuse to ingest a zero-DPU host whose policy resolves to `Manage`, got {:?}",
        explored_managed_hosts,
    );

    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_exploration_identified_managed_hosts_count")
            .unwrap(),
        "0"
    );

    // The pairing-blocker metric should have ticked for `NoDpuReportedByHost`.
    let blocker_metric = test_meter
        .formatted_metric("carbide_host_dpu_pairing_blockers_count")
        .expect("expected `carbide_host_dpu_pairing_blockers_count` to be emitted");
    assert!(
        blocker_metric.contains("no_dpu_reported_by_host"),
        "expected pairing-blocker metric to mention `no_dpu_reported_by_host`, got {blocker_metric}",
    );

    Ok(())
}

/// Companion to `test_site_explorer_skips_unexpected_zero_dpu_host`: when
/// the operator selects `HostDpuPolicy::Nic`, a host whose BMC reports
/// zero usable DPU PCIe devices (because anything that is a
/// BlueField has been stripped as "DPU in NIC mode") should be ingested as
/// a zero-DPU managed host -- the operator has already opted into "treat
/// as zero-DPU" semantics through the resolved policy.
#[sqlx_test]
async fn test_site_explorer_ingests_nic_mode_host_with_no_observed_dpus(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let mut machine = env.new_machine("AA:AB:AC:AD:AA:22", "Vendor1");
    machine.discover_dhcp(env.api()).await?;

    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: machine.mac,
            data: ExpectedMachineData {
                serial_number: "host-nic-mode-no-observed-dpus".to_string(),
                dpu_policy: model::expected_machine::HostDpuPolicy::Nic,
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![(
        machine.ip.parse().unwrap(),
        Ok(EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            vendor: Some(bmc_vendor::BMCVendor::Lenovo),
            systems: vec![ComputerSystem {
                serial_number: Some("0123456789".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }),
    )]);

    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::set_preingestion_complete(machine.ip.parse().unwrap(), &mut txn)
        .await?;
    txn.commit().await?;
    explorer.run_single_iteration().await.unwrap();

    let explored_managed_hosts = db::explored_managed_host::find_all(&env.pool).await?;
    assert_eq!(
        explored_managed_hosts.len(),
        1,
        "Nic policy should let the host through the strict gate even with zero observed DPUs",
    );
    assert!(
        explored_managed_hosts[0].dpus.is_empty(),
        "Nic-policy hosts ingest with an empty `dpus` vector",
    );

    Ok(())
}

/// Third member of the zero-DPU triad (alongside the `Manage`-policy skip
/// test and the `Nic`-policy ingest test): a host with
/// `HostDpuPolicy::Ignore` ingests as a zero-DPU managed host. The `Ignore`
/// fast-path in `identify_managed_hosts` short-circuits before any
/// DPU PCIe enumeration, so this holds regardless of what the BMC reports.
#[sqlx_test]
async fn test_site_explorer_ingests_no_dpu_host(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let mut machine = env.new_machine("AA:AB:AC:AD:AA:33", "Vendor1");
    machine.discover_dhcp(env.api()).await?;

    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: machine.mac,
            data: ExpectedMachineData {
                serial_number: "host-no-dpu-declared".to_string(),
                dpu_policy: model::expected_machine::HostDpuPolicy::Ignore,
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![(
        machine.ip.parse().unwrap(),
        Ok(EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            vendor: Some(bmc_vendor::BMCVendor::Lenovo),
            systems: vec![ComputerSystem {
                serial_number: Some("0123456789".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }),
    )]);

    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::set_preingestion_complete(machine.ip.parse().unwrap(), &mut txn)
        .await?;
    txn.commit().await?;
    explorer.run_single_iteration().await.unwrap();

    let explored_managed_hosts = db::explored_managed_host::find_all(&env.pool).await?;
    assert_eq!(
        explored_managed_hosts.len(),
        1,
        "Ignore policy should ingest the host as zero-DPU",
    );
    assert!(
        explored_managed_hosts[0].dpus.is_empty(),
        "Ignore hosts ingest with an empty `dpus` vector",
    );

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_unknown_vendor(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let underlay_segment = env.underlay_segment;

    let mut machine = env.new_machine("B8:3F:D2:90:97:A7", "Vendor1");
    machine.discover_dhcp(env.api()).await?;

    let mut txn = env.pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, &underlay_segment.id)
            .await
            .unwrap(),
        1
    );
    txn.commit().await.unwrap();

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 2,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_result(
        machine.ip.parse().unwrap(),
        Err(EndpointExplorationError::UnsupportedVendor {
            vendor: "Unknown".to_string(),
        }),
    );

    explorer.run_single_iteration().await.unwrap();
    // Since we configured a limit of 2 entries, we should have those 2 results now
    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 1);
    let report = &explored[0];
    assert_eq!(report.report_version.version_nr(), 1);
    assert_eq!(
        report.report.last_exploration_error,
        Some(EndpointExplorationError::UnsupportedVendor {
            vendor: "Unknown".to_string(),
        })
    );

    let guard = explorer.endpoint_explorer().reports.lock().unwrap();
    let res = guard.get(&report.address).unwrap().as_ref();
    assert!(res.is_err());
    assert_eq!(
        res.unwrap_err(),
        report.report.last_exploration_error.as_ref().unwrap()
    );

    Ok(())
}

#[sqlx_test]
async fn test_expected_machine_device_type_metrics(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let test_sku_gpu_id = format!("test-sku-gpu-{}", uuid::Uuid::new_v4());
    let test_sku_no_type_id = format!("test-sku-no-type-{}", uuid::Uuid::new_v4());
    const EXPECTED_MACHINE_1_MAC: &str = "AA:BB:CC:DD:EE:01";
    const EXPECTED_MACHINE_2_MAC: &str = "AA:BB:CC:DD:EE:02";
    const EXPECTED_MACHINE_3_MAC: &str = "AA:BB:CC:DD:EE:03";

    // Create fake machines with network interfaces so they can be discovered
    let mut machines = vec![
        env.new_machine(EXPECTED_MACHINE_1_MAC, "Vendor1"),
        env.new_machine(EXPECTED_MACHINE_2_MAC, "Vendor2"),
        env.new_machine(EXPECTED_MACHINE_3_MAC, "Vendor3"),
    ];
    machines.discover_dhcp(env.api()).await?;

    // Create test SKUs in database
    let mut txn = env.pool.begin().await?;

    let test_sku_with_device_type = model::sku::Sku {
        schema_version: db::sku::CURRENT_SKU_VERSION,
        id: test_sku_gpu_id.clone(),
        description: "Test GPU SKU".to_string(),
        created: chrono::Utc::now(),
        components: model::sku::SkuComponents {
            chassis: model::sku::SkuComponentChassis {
                vendor: format!("test_vendor_gpu_{}", uuid::Uuid::new_v4()),
                model: format!("test_model_gpu_{}", uuid::Uuid::new_v4()),
                architecture: "x86_64".to_string(),
            },
            cpus: vec![],
            gpus: vec![],
            memory: vec![],
            infiniband_devices: vec![],
            storage: vec![],
            tpm: None,
        },
        device_type: Some("gpu".to_string()),
    };

    let test_sku_without_device_type = model::sku::Sku {
        schema_version: db::sku::CURRENT_SKU_VERSION,
        id: test_sku_no_type_id.clone(),
        description: "Test SKU without device type".to_string(),
        created: chrono::Utc::now(),
        components: model::sku::SkuComponents {
            chassis: model::sku::SkuComponentChassis {
                vendor: format!("test_vendor_no_type_{}", uuid::Uuid::new_v4()),
                model: format!("test_model_no_type_{}", uuid::Uuid::new_v4()),
                architecture: "x86_64".to_string(),
            },
            cpus: vec![],
            gpus: vec![],
            memory: vec![],
            infiniband_devices: vec![],
            storage: vec![],
            tpm: None,
        },
        device_type: None,
    };

    db::sku::create(&mut txn, &test_sku_with_device_type).await?;
    db::sku::create(&mut txn, &test_sku_without_device_type).await?;

    // Create expected machines with different SKU configurations
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: EXPECTED_MACHINE_1_MAC.parse().unwrap(),
            data: ExpectedMachineData {
                bmc_username: "user1".to_string(),
                bmc_password: "pass1".to_string(),
                serial_number: "serial1".to_string(),
                fallback_dpu_serial_numbers: vec![],
                metadata: Metadata::new_with_default_name(),
                sku_id: Some(test_sku_gpu_id.clone()),
                default_pause_ingestion_and_poweron: None,
                interfaces: vec![],
                rack_id: None,
                dpf_enabled: Some(true),
                bmc_ip_address: None,
                bmc_retain_credentials: None,
                dpu_policy: Default::default(),
                bmc_ip_allocation: Default::default(),
                host_lifecycle_profile: Default::default(),
            },
        },
    )
    .await?;

    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: EXPECTED_MACHINE_2_MAC.parse().unwrap(),
            data: ExpectedMachineData {
                bmc_username: "user2".to_string(),
                bmc_password: "pass2".to_string(),
                serial_number: "serial2".to_string(),
                fallback_dpu_serial_numbers: vec![],
                metadata: Metadata::new_with_default_name(),
                sku_id: Some(test_sku_no_type_id.clone()),
                default_pause_ingestion_and_poweron: None,
                interfaces: vec![],
                rack_id: None,
                dpf_enabled: Some(true),
                bmc_ip_address: None,
                bmc_retain_credentials: None,
                dpu_policy: Default::default(),
                bmc_ip_allocation: Default::default(),
                host_lifecycle_profile: Default::default(),
            },
        },
    )
    .await?;

    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: EXPECTED_MACHINE_3_MAC.parse().unwrap(),
            data: ExpectedMachineData {
                bmc_username: "user3".to_string(),
                bmc_password: "pass3".to_string(),
                serial_number: "serial3".to_string(),
                fallback_dpu_serial_numbers: vec![],
                metadata: Metadata::new_with_default_name(),
                sku_id: None, // No SKU
                default_pause_ingestion_and_poweron: None,
                interfaces: vec![],
                rack_id: None,
                dpf_enabled: Some(true),
                bmc_ip_address: None,
                bmc_retain_credentials: None,
                dpu_policy: Default::default(),
                bmc_ip_allocation: Default::default(),
                host_lifecycle_profile: Default::default(),
            },
        },
    )
    .await?;

    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 3, // Explore our 3 machines
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(false.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    };

    let explorer = env.test_site_explorer(explorer_config);
    // Mock exploration results for each machine
    explorer.insert_endpoint_results(vec![
        (
            machines[0].ip.parse().unwrap(),
            Ok(EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                last_exploration_error: None,
                last_exploration_latency: Some(std::time::Duration::from_millis(100)),
                vendor: Some(bmc_vendor::BMCVendor::Dell),
                managers: vec![],
                systems: vec![],
                chassis: vec![],
                service: vec![],
                machine_id: None,
                versions: std::collections::HashMap::new(),
                model: Some("test-model".to_string()),
                machine_setup_status: None,
                secure_boot_status: None,
                lockdown_status: None,
                power_shelf_id: None,
                switch_id: None,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
                remediation_error: None,
            }),
        ),
        (
            machines[1].ip.parse().unwrap(),
            Ok(EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                last_exploration_error: None,
                last_exploration_latency: Some(std::time::Duration::from_millis(100)),
                vendor: Some(bmc_vendor::BMCVendor::Nvidia),
                managers: vec![],
                systems: vec![],
                chassis: vec![],
                service: vec![],
                machine_id: None,
                versions: std::collections::HashMap::new(),
                model: Some("test-model".to_string()),
                machine_setup_status: None,
                secure_boot_status: None,
                lockdown_status: None,
                power_shelf_id: None,
                switch_id: None,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
                remediation_error: None,
            }),
        ),
        (
            machines[2].ip.parse().unwrap(),
            Ok(EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                last_exploration_error: None,
                last_exploration_latency: Some(std::time::Duration::from_millis(100)),
                vendor: Some(bmc_vendor::BMCVendor::Supermicro),
                managers: vec![],
                systems: vec![],
                chassis: vec![],
                service: vec![],
                machine_id: None,
                versions: std::collections::HashMap::new(),
                model: Some("test-model".to_string()),
                machine_setup_status: None,
                secure_boot_status: None,
                lockdown_status: None,
                power_shelf_id: None,
                switch_id: None,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
                remediation_error: None,
            }),
        ),
    ]);
    let test_meter = &env.test_harness.test_meter;

    // Run site explorer to collect metrics
    explorer.run_single_iteration().await.unwrap();

    // Verify expected machines SKU count metrics
    let device_type_metrics: HashMap<String, String> = test_meter
        .parsed_metrics("carbide_site_exploration_expected_machines_sku_count")
        .into_iter()
        .collect();

    assert!(!device_type_metrics.is_empty());

    // Expected machines metrics are now recorded based on both SKU ID and device type
    // Now that we properly set device_type using update_metadata:
    // - 1 machine with GPU SKU -> sku_id=test_sku_gpu_id, device_type="gpu"
    // - 1 machine with no device_type SKU -> sku_id=test_sku_no_type_id, device_type="unknown"
    // - 1 machine with no SKU -> sku_id="unknown", device_type="unknown"

    // Check machine with GPU SKU
    let gpu_sku_key = format!("{{device_type=\"gpu\",sku_id=\"{test_sku_gpu_id}\"}}");
    assert_eq!(device_type_metrics.get(&gpu_sku_key).unwrap(), "1");

    // Check machine with SKU but no device type
    let no_type_sku_key = format!("{{device_type=\"unknown\",sku_id=\"{test_sku_no_type_id}\"}}");
    assert_eq!(device_type_metrics.get(&no_type_sku_key).unwrap(), "1");

    // Check machine with no SKU
    assert_eq!(
        device_type_metrics
            .get("{device_type=\"unknown\",sku_id=\"unknown\"}")
            .unwrap(),
        "1"
    );

    // Verify total count by summing all device types
    let total_count: u32 = device_type_metrics
        .values()
        .map(|v| v.parse::<u32>().unwrap())
        .sum();
    assert_eq!(total_count, 3);

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_default_pause_ingestion_and_poweron(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool.clone()).await;
    let underlay_segment = env.underlay_segment.id;

    let bmc_mac_address = "6a:6b:6c:6d:6e:6f".parse().unwrap();
    let mut txn = pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address,
            data: ExpectedMachineData {
                bmc_username: "ADMIN".into(),
                bmc_password: "Pwd2023x0x0x0x0x7".into(),
                serial_number: "VVG121GL".into(),
                dpu_policy: model::expected_machine::HostDpuPolicy::Ignore,
                default_pause_ingestion_and_poweron: Some(true),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    txn.commit().await?;

    let mut machines = vec![env.new_machine(&bmc_mac_address.to_string(), "Vendor1")];
    machines.discover_dhcp(env.api()).await?;

    let mut txn = env.pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, &underlay_segment)
            .await
            .unwrap(),
        1
    );
    txn.commit().await?;

    let mock_host = machines[0].as_mock_host(vec![]);

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 2,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![(
        machines[0].ip.parse().unwrap(),
        Ok(mock_host.clone().into()),
    )]);

    // check the ingestion state of the machine
    let response = env
        .api()
        .determine_machine_ingestion_state(tonic::Request::new(rpc::forge::BmcEndpointRequest {
            mac_address: Some("6a:6b:6c:6d:6e:6f".to_string()),
            ip_address: "".to_string(),
        }))
        .await?;
    assert_eq!(
        rpc::forge::MachineIngestionState::NotDiscovered,
        response.into_inner().machine_ingestion_state()
    );

    // run the exploration cycle
    explorer.run_single_iteration().await.unwrap();

    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 1);
    assert!(explored[0].pause_ingestion_and_poweron);

    // make sure the machine has not been ingested
    let response = env
        .api()
        .determine_machine_ingestion_state(tonic::Request::new(rpc::forge::BmcEndpointRequest {
            mac_address: Some("6a:6b:6c:6d:6e:6f".to_string()),
            ip_address: "".to_string(),
        }))
        .await?;
    assert_eq!(
        rpc::forge::MachineIngestionState::WaitingForIngestion,
        response.into_inner().machine_ingestion_state()
    );

    // now that the explored endpoint has been added to the DB, mark it as preingestion complete
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::set_preingestion_complete(explored[0].address, &mut txn)
        .await
        .unwrap();
    txn.commit().await?;

    // and run another exploration cycle
    explorer.run_single_iteration().await.unwrap();

    // make sure the machie still has not been ingested
    let response = env
        .api()
        .determine_machine_ingestion_state(tonic::Request::new(rpc::forge::BmcEndpointRequest {
            mac_address: Some("6a:6b:6c:6d:6e:6f".to_string()),
            ip_address: "".to_string(),
        }))
        .await?;
    assert_eq!(
        rpc::forge::MachineIngestionState::WaitingForIngestion,
        response.into_inner().machine_ingestion_state()
    );

    let machine_snapshots =
        db::managed_host::load_all(&env.pool, LoadSnapshotOptions::default()).await?;
    assert_eq!(machine_snapshots.len(), 0);
    let explored_managed_hosts = db::explored_managed_host::find_all(&env.pool).await?;
    assert_eq!(explored_managed_hosts.len(), 0);

    // now flip the flag and run another interation
    let _ = env
        .api()
        .allow_ingestion_and_power_on(tonic::Request::new(rpc::forge::BmcEndpointRequest {
            mac_address: Some("6a:6b:6c:6d:6e:6f".to_string()),
            ip_address: "".to_string(),
        }))
        .await?;

    // run the exploration cycle
    explorer.run_single_iteration().await.unwrap();

    // the machine should be ingested now
    // unfortunately, there is no way to test a hypothetical situation when
    // an explored managed host has been created, but the machine has not
    // been created yet as those are performed in the same site explorer
    // iteration
    let response = env
        .api()
        .determine_machine_ingestion_state(tonic::Request::new(rpc::forge::BmcEndpointRequest {
            mac_address: Some("6a:6b:6c:6d:6e:6f".to_string()),
            ip_address: "".to_string(),
        }))
        .await?;
    assert_eq!(
        rpc::forge::MachineIngestionState::IngestionMachineCreated,
        response.into_inner().machine_ingestion_state()
    );

    let explored_managed_hosts = db::explored_managed_host::find_all(&env.pool).await?;
    assert_eq!(explored_managed_hosts.len(), 1);
    let machine_snapshots =
        db::managed_host::load_all(&env.pool, LoadSnapshotOptions::default()).await?;
    assert_eq!(machine_snapshots.len(), 1);

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_main(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = TestHarness::builder(pool.clone()).build().await;
    let domain = test_harness.test_domain().await;
    let network_controller = test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let underlay_segment_id = underlay_segment.id;
    let admin_segment_id = admin_segment.id;
    let api = test_harness.api();

    // Let's create 3 machines on the underlay, and 1 on the admin network
    // The 1 on the admin network is not supposed to be searched. This is verified
    // by providing no mocked exploration data for this machine, which would lead
    // to a panic if the machine is queried
    let mut machines = vec![
        // machines[0] is a DPU belonging to machines[1]
        FakeMachine::new(underlay_segment, "B8:3F:D2:90:97:A6", "Vendor1"),
        // machines[1] has 1 dpu (machines[0])
        FakeMachine::new(underlay_segment, "AA:AB:AC:AD:AA:02", "Vendor2"),
        // machines[2] has no DPUs
        FakeMachine::new(underlay_segment, "AA:AB:AC:AD:AA:03", "Vendor3"),
        // machines[3] is not on the underlay network and should not be searched.
        FakeMachine::new(admin_segment, "AA:AB:AC:AD:BB:01", "VendorInvalidSegment"),
    ];
    machines.discover_dhcp(test_harness.api()).await?;

    let mut txn = pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, &underlay_segment_id)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, &admin_segment_id)
            .await
            .unwrap(),
        1
    );
    txn.commit().await.unwrap();

    // Register `expected_machines` so site-explorer accepts these hosts: the
    // host with a DPU pair resolves to `HostDpuPolicy::Manage`, and the zero-DPU
    // host resolves to `HostDpuPolicy::Ignore` to pass the strict ingestion gate.
    let mut txn = pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: machines[1].mac,
            data: ExpectedMachineData {
                serial_number: "host-with-dpu".to_string(),
                ..Default::default()
            },
        },
    )
    .await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: machines[2].mac,
            data: ExpectedMachineData {
                serial_number: "host-with-no-dpu".to_string(),
                dpu_policy: model::expected_machine::HostDpuPolicy::Ignore,
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let mock_dpu = machines[0].as_mock_dpu();

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 2,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env::test_site_explorer(&test_harness, explorer_config);
    explorer.insert_endpoint_results(vec![
        (machines[0].ip.parse().unwrap(), Ok(mock_dpu.clone().into())),
        (
            machines[1].ip.parse().unwrap(),
            Err(EndpointExplorationError::Unauthorized {
                details: "Not authorized".to_string(),
                response_body: None,
                response_code: None,
            }),
        ),
        (
            machines[2].ip.parse().unwrap(),
            Ok(EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                last_exploration_error: None,
                last_exploration_latency: None,
                vendor: Some(bmc_vendor::BMCVendor::Lenovo),
                machine_id: None,
                managers: Vec::new(),
                systems: vec![ComputerSystem {
                    serial_number: Some("0123456789".to_string()),
                    ..Default::default()
                }],
                chassis: Vec::new(),
                service: Vec::new(),
                versions: HashMap::default(),
                model: None,
                machine_setup_status: None,
                secure_boot_status: None,
                lockdown_status: None,
                power_shelf_id: None,
                switch_id: None,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
                remediation_error: None,
            }),
        ),
    ]);
    let test_meter = &test_harness.test_meter;

    explorer.run_single_iteration().await.unwrap();
    // Since we configured a limit of 2 entries, we should have those 2 results now
    let mut txn = pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 2);

    for report in &explored {
        assert_eq!(report.report_version.version_nr(), 1);
        let guard = explorer.endpoint_explorer().reports.lock().unwrap();
        let res = guard.get(&report.address).unwrap().as_ref();
        if res.is_err() {
            assert_eq!(
                res.unwrap_err(),
                report.report.last_exploration_error.as_ref().unwrap()
            );
        } else {
            assert_eq!(res.unwrap().endpoint_type, report.report.endpoint_type);
            assert_eq!(res.unwrap().vendor, report.report.vendor);
            assert_eq!(res.unwrap().managers, report.report.managers);
            assert_eq!(res.unwrap().systems, report.report.systems);
            assert_eq!(res.unwrap().chassis, report.report.chassis);
            assert_eq!(res.unwrap().service, report.report.service);
        }
    }

    // Retrieve the report via gRPC
    let report = fetch_exploration_report(api).await;
    assert!(report.managed_hosts.is_empty());

    // We should also have metric entries
    assert_eq!(
        test_meter
            .formatted_metric("carbide_endpoint_explorations_count")
            .unwrap(),
        "2"
    );
    assert!(
        test_meter
            .formatted_metric("carbide_endpoint_exploration_success_count")
            .is_some()
    );
    // The failure metric is not emitted if no failure happened
    assert_eq!(
        test_meter
            .formatted_metric("carbide_endpoint_exploration_duration_milliseconds_count")
            .unwrap_or("2".to_string()),
        "2"
    );
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_exploration_identified_managed_hosts_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_explorer_created_machines_count")
            .unwrap(),
        "0"
    );

    // Running again should yield all 3 entries
    explorer.run_single_iteration().await.unwrap();
    // Since we configured a limit of 2 entries, we should have those 2 results now
    let mut txn = pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 3);
    let mut versions = Vec::new();
    for report in &explored {
        versions.push(report.report_version.version_nr());
        let guard = explorer.endpoint_explorer().reports.lock().unwrap();
        let res = guard.get(&report.address).unwrap().as_ref();
        if res.is_err() {
            assert_eq!(
                res.unwrap_err(),
                report.report.last_exploration_error.as_ref().unwrap()
            );
        } else {
            assert_eq!(res.unwrap().endpoint_type, report.report.endpoint_type);
            assert_eq!(res.unwrap().vendor, report.report.vendor);
            assert_eq!(res.unwrap().managers, report.report.managers);
            assert_eq!(res.unwrap().systems, report.report.systems);
            assert_eq!(res.unwrap().chassis, report.report.chassis);
            assert_eq!(res.unwrap().service, report.report.service);
        }
    }
    versions.sort();
    assert_eq!(&versions, &[1, 1, 2]);

    // Retrieve the report via gRPC
    let report = fetch_exploration_report(api).await;
    assert!(report.managed_hosts.is_empty());

    assert_eq!(
        test_meter
            .formatted_metric("carbide_endpoint_explorations_count")
            .unwrap(),
        "2"
    );
    assert!(
        test_meter
            .formatted_metric("carbide_endpoint_exploration_success_count")
            .is_some()
    );
    assert_eq!(
        test_meter
            .formatted_metric("carbide_endpoint_exploration_duration_milliseconds_count")
            .unwrap_or("4".to_string()),
        "4"
    );
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_exploration_identified_managed_hosts_count")
            .unwrap(),
        "0"
    );
    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_explorer_created_machines_count")
            .unwrap(),
        "0"
    );

    // Now make 1 previously existing endpoint unreachable and 1 previously unreachable
    // endpoint reachable and show the managed host.
    // Both changes should show up after 2 updates
    explorer.insert_endpoint_results(vec![
        (
            machines[0].ip.parse().unwrap(),
            Err(EndpointExplorationError::Unreachable {
                details: Some("test_unreachable_detail".to_string()),
            }),
        ),
        (
            machines[1].ip.parse().unwrap(),
            Ok(machines[1].as_mock_host(vec![mock_dpu.clone()]).into()),
        ),
    ]);

    // We don't want to test the preingestion stuff here, so fake that it all completed successfully.
    let mut txn = pool.begin().await?;
    for addr in ["192.0.1.3", "192.0.1.4", "192.0.1.5"] {
        db::explored_endpoints::set_preingestion_complete(
            std::net::IpAddr::from_str(addr).unwrap(),
            &mut txn,
        )
        .await
        .unwrap();
    }
    txn.commit().await?;

    explorer.run_single_iteration().await.unwrap();
    explorer.run_single_iteration().await.unwrap();
    let mut txn = pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 3);
    let mut versions = Vec::new();
    for report in &explored {
        versions.push(report.report_version.version_nr());
        assert_eq!(report.report.endpoint_type, EndpointType::Bmc);
        match report.address.to_string() {
            a if a == machines[0].ip => {
                // The original successful report is retained, while only the latest
                // exploration failure details are updated.
                assert_eq!(report.report.vendor, Some(bmc_vendor::BMCVendor::Nvidia));
                assert_eq!(
                    report.report.last_exploration_error.clone().unwrap(),
                    EndpointExplorationError::Unreachable {
                        details: Some("test_unreachable_detail".to_string())
                    }
                );
                assert!(report.report.last_exploration_latency.is_some());
            }
            a if a == machines[1].ip => {
                assert_eq!(report.report.vendor, Some(bmc_vendor::BMCVendor::Dell));
                assert!(report.report.last_exploration_error.is_none());
            }
            a if a == machines[2].ip => {
                assert_eq!(report.report.vendor, Some(bmc_vendor::BMCVendor::Lenovo));
                assert!(report.report.last_exploration_error.is_none());
            }
            _ => panic!("No other endpoints should be discovered"),
        }
    }
    // Four iterations perform eight scans. The oldest-first scheduler does not
    // guarantee an even distribution, but every endpoint should be refreshed.
    assert_eq!(versions.iter().sum::<u64>(), 8);
    assert!(versions.iter().all(|version| *version >= 2));

    let report = fetch_exploration_report(api).await;
    assert_eq!(report.endpoints.len(), 3);
    let mut addresses: Vec<String> = report
        .endpoints
        .iter()
        .map(|ep| ep.address.clone())
        .collect();
    addresses.sort();
    let mut expected_addresses: Vec<String> = machines
        .iter()
        .filter(|m| m.segment.id == underlay_segment_id)
        .map(|m| m.ip.to_string())
        .collect();
    expected_addresses.sort();
    assert_eq!(addresses, expected_addresses);

    // We should now have two managed hosts: One with a single DPU, and one with no DPUs.
    assert_eq!(report.managed_hosts.len(), 2);
    let managed_host_1 = report
        .managed_hosts
        .iter()
        .find(|h| h.dpus.len() == 1)
        .expect("Should have found one managed host with a single DPU")
        .clone();
    let managed_host_2 = report
        .managed_hosts
        .iter()
        .find(|h| h.dpus.is_empty())
        .expect("Should have found one managed host with zero DPUs")
        .clone();

    assert_eq!(managed_host_1.host_bmc_ip, machines[1].ip);
    assert_eq!(
        managed_host_1.dpus,
        vec![RpcExploredDpu {
            bmc_ip: machines[0].ip.clone(),
            host_pf_mac_address: Some(mock_dpu.host_mac_address.to_string()),
        }]
    );

    assert_eq!(managed_host_2.host_bmc_ip, machines[2].ip);
    assert!(managed_host_2.dpus.is_empty());

    assert_eq!(
        test_meter
            .formatted_metric("carbide_site_exploration_identified_managed_hosts_count")
            .unwrap(),
        "2"
    );

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_audit_exploration_results(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool.clone()).await;
    let underlay_segment = env.underlay_segment.id;

    let mut txn = pool.begin().await?;
    for (bmc_mac_address, serial_number, fallback_dpu_serial_numbers) in [
        ("0a:0b:0c:0d:0e:0f", "VVG121GG", vec![]),
        ("1a:1b:1c:1d:1e:1f", "VVG121GH", vec![]),
        ("2a:2b:2c:2d:2e:2f", "VVG121GI", vec![]),
        ("3a:3b:3c:3d:3e:3f", "VVG121GJ", vec!["dpu_serial1"]),
        (
            "4a:4b:4c:4d:4e:4f",
            "VVG121GK",
            vec!["dpu_serial2", "dpu_serial3"],
        ),
        ("5a:5b:5c:5d:5e:5f", "VVG121GL", vec![]),
    ] {
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: bmc_mac_address.parse().unwrap(),
                data: ExpectedMachineData {
                    bmc_username: "ADMIN".into(),
                    bmc_password: "Pwd2023x0x0x0x0x7".into(),
                    serial_number: serial_number.into(),
                    fallback_dpu_serial_numbers: fallback_dpu_serial_numbers
                        .into_iter()
                        .map(ToString::to_string)
                        .collect(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    }
    txn.commit().await?;

    let mut machines = vec![
        // This will be our expected DPU, and it will have the
        // expected serial number, but we assume no DPUs are expected,
        // should it still shouldn't be counted as `expected`        .
        env.new_machine("5a:5b:5c:5d:5e:5f", "Vendor1"),
        // This will be expected but unauthorized, and the serial is mismatched
        env.new_machine("0a:0b:0c:0d:0e:0f", "Vendor3"),
        // This host will be expected but missing credentials, and the serial is mismatched
        env.new_machine("1a:1b:1c:1d:1e:1f", "Vendor3"),
        // This host will be expected, but the serial number will be mismatched.
        env.new_machine("2a:2b:2c:2d:2e:2f", "Vendor3"),
        // This will be expected, with a good serial number.
        // It will also have associated DPUs and should get a managed host.
        env.new_machine("3a:3b:3c:3d:3e:3f", "Vendor3"),
        // This host is not expected.
        env.new_machine("ab:cd:ef:ab:cd:ef", "Vendor3"),
        // This DPU is really not expected. (i.e. no DB entry)
        env.new_machine("ef:cd:ab:ef:cd:ab", "Vendor3"),
    ];

    machines.discover_dhcp(env.api()).await?;

    let mut txn = env.pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, &underlay_segment)
            .await
            .unwrap(),
        7
    );
    txn.commit().await.unwrap();

    // Make a mock host for machines[4] to generate the report
    // This serial is from the create_expected_machine.sql seed.
    let machine_4_host = ManagedHostConfig::default().with_serial("VVG121GJ".to_string());

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 7,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        machines_created_per_run: 1,
        override_target_ip: None,
        override_target_port: None,
        allow_changing_bmc_proxy: None,
        bmc_proxy: Arc::default(),
        reset_rate_limit: chrono::Duration::hours(1),
        admin_segment_type_non_dpu: Arc::new(false.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        rotate_switch_nvos_credentials: Arc::new(false.into()),
        dpu_policy: None,
        deprecated_force_dpu_nic_mode: None,
        // Tests use MockEndpointExplorer. So this doesn't affect anything.
        explore_mode: SiteExplorerExploreMode::NvRedfish,
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoints(vec![
        (
            machines[0].ip.parse().unwrap(),
            DpuConfig::with_serial("VVG121GL".to_string()).into(),
        ),
        (
            machines[1].ip.parse().unwrap(),
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                // Pretend there was previously a successful exploration
                // but now something has gone wrong.
                last_exploration_error: Some(EndpointExplorationError::Unauthorized {
                    details: "Not authorized".to_string(),
                    response_body: None,
                    response_code: None,
                }),
                last_exploration_latency: None,
                vendor: Some(bmc_vendor::BMCVendor::Lenovo),
                machine_id: None,
                model: None,
                managers: Vec::new(),
                systems: Vec::new(),
                chassis: Vec::new(),
                service: Vec::new(),
                versions: HashMap::default(),
                machine_setup_status: None,
                secure_boot_status: None,
                lockdown_status: None,
                power_shelf_id: None,
                switch_id: None,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
                remediation_error: None,
            },
        ),
        (
            machines[2].ip.parse().unwrap(),
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                // Pretend there was previously a successful exploration
                // but now something has gone wrong.
                last_exploration_error: Some(EndpointExplorationError::MissingCredentials {
                    key: "some_cred".to_string(),
                    cause: "it's not there!".to_string(),
                }),
                last_exploration_latency: None,
                vendor: Some(bmc_vendor::BMCVendor::Lenovo),
                machine_id: None,
                model: None,
                managers: Vec::new(),
                systems: Vec::new(),
                chassis: Vec::new(),
                service: Vec::new(),
                versions: HashMap::default(),
                machine_setup_status: None,
                secure_boot_status: None,
                lockdown_status: None,
                power_shelf_id: None,
                switch_id: None,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
                remediation_error: None,
            },
        ),
        (
            machines[3].ip.parse().unwrap(),
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                last_exploration_error: None,
                last_exploration_latency: None,
                vendor: Some(bmc_vendor::BMCVendor::Lenovo),
                machine_id: None,
                model: None,
                managers: Vec::new(),
                systems: Vec::new(),
                chassis: Vec::new(),
                service: Vec::new(),
                versions: HashMap::default(),
                machine_setup_status: None,
                secure_boot_status: None,
                lockdown_status: None,
                power_shelf_id: None,
                switch_id: None,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
                remediation_error: None,
            },
        ),
        (
            machines[4].ip.parse().unwrap(),
            machine_4_host.clone().into(),
        ),
        (
            machines[5].ip.parse().unwrap(),
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                last_exploration_error: None,
                last_exploration_latency: None,
                vendor: Some(bmc_vendor::BMCVendor::Lenovo),
                machine_id: None,
                model: None,
                managers: Vec::new(),
                systems: Vec::new(),
                chassis: Vec::new(),
                service: Vec::new(),
                versions: HashMap::default(),
                machine_setup_status: None,
                secure_boot_status: None,
                lockdown_status: None,
                power_shelf_id: None,
                switch_id: None,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
                remediation_error: None,
            },
        ),
        (
            // This is the DPU from machines[4]
            machines[6].ip.parse().unwrap(),
            machine_4_host.dpus[0].clone().into(),
        ),
    ]);
    let test_meter = &env.test_harness.test_meter;

    explorer.run_single_iteration().await.unwrap();
    // carbide_endpoint_exploration_preingestions_incomplete_overall_count
    let m: HashMap<String, String> = test_meter
        .parsed_metrics("carbide_endpoint_exploration_preingestions_incomplete_overall_count")
        .into_iter()
        .collect();

    assert!(!m.is_empty());
    assert_eq!(
        m.get("{expectation=\"na\",machine_type=\"dpu\"}").unwrap(),
        "2"
    );
    assert_eq!(
        m.get("{expectation=\"expected\",machine_type=\"host\"}")
            .unwrap(),
        "4" // 2 normal + 2 previously explored but in an error state
    );
    assert_eq!(
        m.get("{expectation=\"unexpected\",machine_type=\"host\"}")
            .unwrap(),
        "1"
    );

    let mut txn = pool.begin().await?;
    for final_octet in 2..10 {
        db::explored_endpoints::set_preingestion_complete(
            std::net::IpAddr::from(std::net::Ipv4Addr::new(192, 0, 1, final_octet)),
            &mut txn,
        )
        .await
        .unwrap();
    }
    txn.commit().await?;
    explorer.run_single_iteration().await.unwrap();

    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 7);

    for report in &explored {
        assert_eq!(report.report_version.version_nr(), 2);
        let guard = explorer.endpoint_explorer().reports.lock().unwrap();
        let res = guard.get(&report.address).unwrap().as_ref();
        if res.is_err() {
            assert_eq!(
                res.unwrap_err(),
                report.report.last_exploration_error.as_ref().unwrap()
            );
        } else {
            assert_eq!(res.unwrap().endpoint_type, report.report.endpoint_type);
            assert_eq!(res.unwrap().vendor, report.report.vendor);
            assert_eq!(res.unwrap().managers, report.report.managers);
            assert_eq!(res.unwrap().systems, report.report.systems);
            assert_eq!(res.unwrap().chassis, report.report.chassis);
            assert_eq!(res.unwrap().service, report.report.service);
        }
    }

    // Retrieve the report via gRPC
    let report = fetch_exploration_report(env.api()).await;

    // We should have at least one managed host built by this point.
    assert!(!report.managed_hosts.is_empty());

    // Check for the expected metrics

    // carbide_endpoint_exploration_failures_overall_count
    let m: HashMap<String, String> = test_meter
        .parsed_metrics("carbide_endpoint_exploration_failures_overall_count")
        .into_iter()
        .collect();

    assert!(!m.is_empty());
    assert!(m.get("{failure=\"unauthorized\"}").unwrap() == "1");
    assert!(m.get("{failure=\"missing_credentials\"}").unwrap() == "1");

    // carbide_endpoint_exploration_preingestions_incomplete_overall_count
    let m: HashMap<String, String> = test_meter
        .parsed_metrics("carbide_endpoint_exploration_preingestions_incomplete_overall_count")
        .into_iter()
        .collect();
    // Everything should be done with preingestion now.
    assert!(m.is_empty());

    // carbide_endpoint_exploration_expected_serial_number_mismatches_overall_count
    let m: HashMap<String, String> = test_meter
        .parsed_metrics(
            "carbide_endpoint_exploration_expected_serial_number_mismatches_overall_count",
        )
        .into_iter()
        .collect();

    assert!(!m.is_empty());
    assert_eq!(m.get("{machine_type=\"host\"}").unwrap(), "3");

    // carbide_endpoint_exploration_machines_explored_overall_count
    let m: HashMap<String, String> = test_meter
        .parsed_metrics("carbide_endpoint_exploration_machines_explored_overall_count")
        .into_iter()
        .collect();

    assert!(!m.is_empty());
    assert_eq!(
        m.get("{expectation=\"na\",machine_type=\"dpu\"}").unwrap(),
        "2"
    );
    assert_eq!(
        m.get("{expectation=\"expected\",machine_type=\"host\"}")
            .unwrap(),
        "4"
    );
    assert_eq!(
        m.get("{expectation=\"unexpected\",machine_type=\"host\"}")
            .unwrap(),
        "1"
    );

    // carbide_endpoint_exploration_expected_machines_missing_overall_count
    assert_eq!(
        test_meter
            .formatted_metric(
                "carbide_endpoint_exploration_expected_machines_missing_overall_count"
            )
            .unwrap(),
        "1"
    );

    // carbide_endpoint_exploration_identified_managed_hosts_overall_count
    let m: HashMap<String, String> = test_meter
        .parsed_metrics("carbide_endpoint_exploration_identified_managed_hosts_overall_count")
        .into_iter()
        .collect();

    assert!(!m.is_empty());
    assert_eq!(m.get("{expectation=\"expected\"}").unwrap(), "1");

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_reexplore(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool.clone()).await;
    let underlay_segment = env.underlay_segment.id;

    let mut machines = vec![
        env.new_machine("B8:3F:D2:90:97:A6", "Vendor1"),
        env.new_machine("AA:AB:AC:AD:AA:02", "Vendor2"),
    ];

    machines.discover_dhcp(env.api()).await?;

    let mut txn = env.pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, &underlay_segment)
            .await
            .unwrap(),
        2
    );
    txn.commit().await.unwrap();

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 1,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(false.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    };

    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![
        (
            machines[0].ip.parse().unwrap(),
            Ok(DpuConfig::default().into()),
        ),
        (
            machines[1].ip.parse().unwrap(),
            Err(EndpointExplorationError::Unauthorized {
                details: "Not authorized".to_string(),
                response_body: None,
                response_code: None,
            }),
        ),
    ]);

    explorer.run_single_iteration().await.unwrap();
    // Since we configured a limit of 1 entries, we should have 1 results now
    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 1);
    let explored_ip = explored[0].address;

    for report in &explored {
        assert_eq!(report.report_version.version_nr(), 1);
        assert!(!report.exploration_requested);
    }

    // Re-exploring the first endpoint should prioritize it while preserving
    // routine capacity for another endpoint.
    env.api()
        .re_explore_endpoint(tonic::Request::new(rpc::forge::ReExploreEndpointRequest {
            ip_address: explored_ip.to_string(),
            if_version_match: None,
        }))
        .await
        .unwrap();

    // Calling the API should set the `exploration_requested` flag on the endpoint
    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    for report in &explored {
        assert!(report.exploration_requested);
    }

    // The 2nd iteration updates the priority endpoint and still uses the
    // routine budget to discover another endpoint.
    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 2);

    let reexplored = explored
        .iter()
        .find(|report| report.address == explored_ip)
        .unwrap();
    assert_eq!(reexplored.report_version.version_nr(), 2);
    assert!(!reexplored.exploration_requested);
    let current_version = reexplored.report_version;

    // Using if_version_match with an incorrect version does nothing
    let unexpected_version = current_version.increment();
    let e = env
        .api()
        .re_explore_endpoint(tonic::Request::new(rpc::forge::ReExploreEndpointRequest {
            ip_address: explored_ip.to_string(),
            if_version_match: Some(unexpected_version.version_string()),
        }))
        .await
        .expect_err("Should fail due to invalid version");
    assert_eq!(e.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        e.message(),
        format!(
            "an object of type explored_endpoint was intended to be modified did not have the expected version {}",
            unexpected_version.version_string()
        )
    );

    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    for report in &explored {
        assert!(!report.exploration_requested);
    }

    // Using if_version_match with correct version string does flag the endpoint again
    env.api()
        .re_explore_endpoint(tonic::Request::new(rpc::forge::ReExploreEndpointRequest {
            ip_address: explored_ip.to_string(),
            if_version_match: Some(current_version.version_string()),
        }))
        .await
        .unwrap()
        .into_inner();

    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    let reexplored = explored
        .iter()
        .find(|report| report.address == explored_ip)
        .unwrap();
    assert!(reexplored.exploration_requested);

    // 3rd iteration still yields the same two known endpoints.
    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    let explored = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await?;
    assert_eq!(explored.len(), 2);

    Ok(())
}

// This regression intentionally keeps the exploration transaction open while
// awaiting the competing clear, so it can verify both row-lock orderings.
#[sqlx_test]
async fn test_site_explorer_clear_last_known_error(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let ip_address = "192.168.1.1";
    let bmc_ip: IpAddr = IpAddr::from_str(ip_address)?;
    let last_error = Some(EndpointExplorationError::Unauthorized {
        details: "Not authorized".to_string(),
        response_body: None,
        response_code: Some(401),
    });

    let mut dpu_report1: EndpointExplorationReport = DpuConfig {
        last_exploration_error: last_error.clone(),
        ..DpuConfig::default()
    }
    .into();
    dpu_report1.generate_machine_id(false)?;

    let mut txn = db::Transaction::begin(&env.pool).await?;
    db::explored_endpoints::insert(bmc_ip, &dpu_report1, false, &mut txn).await?;
    txn.commit().await?;

    txn = db::Transaction::begin(&env.pool).await?;
    let nodes = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;
    txn.commit().await?;
    assert_eq!(nodes.len(), 1);
    let node = nodes.first().unwrap();
    assert_eq!(node.report.last_exploration_error, last_error);
    let old_version = node.report_version;

    env.api()
        .clear_site_exploration_error(Request::new(rpc::forge::ClearSiteExplorationErrorRequest {
            ip_address: ip_address.to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    let mut txn = env.pool.begin().await?;
    let nodes = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;
    txn.commit().await?;
    assert_eq!(nodes.len(), 1);
    let node = nodes.first().unwrap();
    assert_eq!(node.report.last_exploration_error, None);
    assert_eq!(
        node.report_version.version_nr(),
        old_version.version_nr() + 1
    );

    let mut txn = db::Transaction::begin(&env.pool).await?;
    let stale_write_applied = db::explored_endpoints::try_update_last_exploration_error(
        bmc_ip,
        old_version,
        &EndpointExplorationError::AvoidLockout,
        Duration::from_secs(1),
        &mut txn,
    )
    .await?;
    txn.commit().await?;
    assert!(
        !stale_write_applied,
        "a stale exploration result must not overwrite a cleared error"
    );

    let mut txn = env.pool.begin().await?;
    let nodes = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;
    txn.commit().await?;
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes.first().unwrap().report.last_exploration_error, None);

    // Exercise the inverse race: an exploration write acquires the row lock
    // first. The clear must wait for that write to commit, then clear the error
    // from the freshly persisted report.
    let version_before_race = nodes.first().unwrap().report_version;
    let mut exploration_txn = db::Transaction::begin(&env.pool).await?;
    let exploration_write_applied = db::explored_endpoints::try_update_last_exploration_error(
        bmc_ip,
        version_before_race,
        &EndpointExplorationError::AvoidLockout,
        Duration::from_secs(2),
        &mut exploration_txn,
    )
    .await?;
    assert!(exploration_write_applied);

    let mut clear_txn = db::Transaction::begin(&env.pool).await?;
    {
        let clear = db::explored_endpoints::clear_last_known_error(bmc_ip, &mut clear_txn);
        tokio::pin!(clear);
        tokio::select! {
            result = &mut clear => {
                panic!("clear unexpectedly completed while the exploration write held the row lock: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }

        exploration_txn.commit().await?;
        clear.await?;
    }
    clear_txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    let nodes = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;
    txn.commit().await?;
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        nodes.first().unwrap().report.last_exploration_error,
        None,
        "the clear must retry after the exploration write wins the race"
    );

    Ok(())
}

/// Clearing the site exploration error should also lift a terminal preingestion
/// `Failed` state back to `Initial`, so an operator can retry preingestion
/// without force-deleting and rediscovering the endpoint.
#[sqlx_test]
async fn test_clear_error_resets_failed_preingestion(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let mut txn = db::Transaction::begin(&env.pool).await?;
    let ip_address = "192.168.1.2";
    let bmc_ip: IpAddr = IpAddr::from_str(ip_address)?;

    let mut report: EndpointExplorationReport = DpuConfig::default().into();
    report.generate_machine_id(false)?;
    db::explored_endpoints::insert(bmc_ip, &report, false, &mut txn).await?;

    // Put the endpoint in the terminal Failed preingestion state, as a failed
    // BMC time sync would.
    db::explored_endpoints::set_preingestion_failed(
        bmc_ip,
        "BMC time synchronization failed after 3 reset attempts. Time difference exceeds 5 minutes threshold.".to_string(),
        &mut txn,
    )
    .await?;
    txn.commit().await?;

    env.api()
        .clear_site_exploration_error(Request::new(rpc::forge::ClearSiteExplorationErrorRequest {
            ip_address: ip_address.to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    let mut txn = db::Transaction::begin(&env.pool).await?;
    let nodes = db::explored_endpoints::find_all_by_ip(bmc_ip, &mut txn).await?;
    txn.commit().await?;
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        nodes.first().unwrap().preingestion_state,
        PreingestionState::Initial,
        "clearing the error should reset a Failed preingestion to Initial"
    );

    Ok(())
}

#[sqlx_test]
async fn test_fallback_dpu_serial(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = TestHarness::builder(pool.clone())
        .with_resource_pools(ResourcePoolBuilder::default().build())
        .build()
        .await;
    let domain = test_harness.test_domain().await;
    let network_controller = test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    // Need to create admin segment because this test creates managed
    // host and check that it is created.
    network_controller.create_admin_segment(&domain).await;
    let api = test_harness.api();

    const HOST1_DPU_BMC_MAC: &str = "B8:3F:D2:90:97:A6";
    const HOST1_BMC_MAC: &str = "AA:AB:AC:AD:AA:02";
    const HOST1_DPU_SERIAL_NUMBER: &str = "host1_dpu_serial_number";

    let mut host1_dpu_bmc = FakeMachine::new(underlay_segment, HOST1_DPU_BMC_MAC, "NVIDIA/BF/BMC");

    let mut host1_bmc = FakeMachine::new(underlay_segment, HOST1_BMC_MAC, "Vendor2");

    // Create dhcp entries and machine_interface entries for the machines
    for machine in [&mut host1_dpu_bmc, &mut host1_bmc] {
        machine.discover_dhcp(api).await?;
    }
    // Create a host and dpu reports && host has no dpu_serial
    let host1_dpu_report = DpuConfig {
        serial: HOST1_DPU_SERIAL_NUMBER.to_string(),
        bmc_mac_address: HOST1_DPU_BMC_MAC.parse()?,
        ..DpuConfig::default()
    };
    let host1_report = ManagedHostConfig {
        bmc_mac_address: HOST1_BMC_MAC.parse()?,
        ..ManagedHostConfig::default()
    };
    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        create_power_shelves: Arc::new(true.into()),
        power_shelves_created_per_run: 1,
        create_switches: Arc::new(true.into()),
        switches_created_per_run: 1,
        ..Default::default()
    };
    let explorer = env::test_site_explorer(&test_harness, explorer_config);
    explorer.insert_endpoint_results(vec![
        (
            host1_dpu_bmc.ip.parse().unwrap(),
            Ok(host1_dpu_report.into()),
        ),
        (host1_bmc.ip.parse().unwrap(), Ok(host1_report.into())),
    ]);

    // Create expected_machine entry for host1 w.o fallback_dpu_serial_number
    let mut txn = pool.begin().await?;

    // Create the SKU record first
    let test_sku = model::sku::Sku {
        schema_version: CURRENT_SKU_VERSION,
        id: "Sku1".to_string(),
        description: "Test SKU for site explorer test".to_string(),
        created: chrono::Utc::now(),
        components: model::sku::SkuComponents {
            chassis: model::sku::SkuComponentChassis {
                vendor: "Vendor1".to_string(),
                model: "Chassis1".to_string(),
                architecture: "x86_64".to_string(),
            },
            cpus: vec![],
            gpus: vec![],
            memory: vec![],
            infiniband_devices: vec![],
            storage: vec![],
            tpm: None,
        },
        device_type: None, // This will result in "unknown" device type
    };
    db::sku::create(&mut txn, &test_sku).await?;

    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: HOST1_BMC_MAC.to_string().parse().unwrap(),
            data: ExpectedMachineData {
                bmc_username: "user1".to_string(),
                bmc_password: "pw".to_string(),
                serial_number: "host1".to_string(),
                fallback_dpu_serial_numbers: vec![],
                metadata: Metadata::new_with_default_name(),
                sku_id: Some("Sku1".to_string()),
                default_pause_ingestion_and_poweron: None,
                interfaces: vec![],
                rack_id: None,
                dpf_enabled: Some(true),
                bmc_ip_address: None,
                bmc_retain_credentials: None,
                dpu_policy: Default::default(),
                bmc_ip_allocation: Default::default(),
                host_lifecycle_profile: Default::default(),
            },
        },
    )
    .await?;
    txn.commit().await?;

    // Run site explorer
    explorer.run_single_iteration().await.unwrap();
    let mut txn = pool.begin().await?;
    let explored_endpoints = db::explored_endpoints::find_all(txn.as_mut())
        .await
        .unwrap();

    // Mark explored endpoints as pre-ingestion_complete
    for ee in &explored_endpoints {
        db::explored_endpoints::set_preingestion_complete(ee.address, &mut txn).await?;
    }
    txn.commit().await?;

    assert_eq!(explored_endpoints.len(), 2);

    let mut explored_managed_hosts = db::explored_managed_host::find_all(&pool).await?;
    let mut machines = db::machine::find(
        &pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap();

    // There should be no managed host
    assert_eq!(explored_managed_hosts.len(), 0);
    assert_eq!(machines.len(), 0);

    // Now update expected_machine entry with fallback_dpu_serial
    let mut txn = pool.begin().await?;
    let mut host1_expected_machine =
        db::expected_machine::find_by_bmc_mac_address(txn.as_mut(), HOST1_BMC_MAC.parse().unwrap())
            .await?
            .expect("Expected machine not found");
    host1_expected_machine.data = ExpectedMachineData {
        bmc_username: "user1".to_string(),
        bmc_password: "pw".to_string(),
        serial_number: "host1".to_string(),
        fallback_dpu_serial_numbers: vec![HOST1_DPU_SERIAL_NUMBER.to_string()],
        metadata: Metadata::new_with_default_name(),
        sku_id: None,
        default_pause_ingestion_and_poweron: None,
        interfaces: vec![],
        rack_id: None,
        dpf_enabled: Some(true),
        bmc_ip_address: None,
        bmc_retain_credentials: None,
        dpu_policy: Default::default(),
        bmc_ip_allocation: Default::default(),
        host_lifecycle_profile: Default::default(),
    };
    db::expected_machine::update(&mut txn, &host1_expected_machine).await?;
    txn.commit().await?;

    explorer.run_single_iteration().await.unwrap();
    explored_managed_hosts = db::explored_managed_host::find_all(&pool).await?;
    machines = db::machine::find(
        &pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap();

    // We should see one explored_managed host && 2 machines
    assert_eq!(
        <Vec<ExploredManagedHost> as AsRef<Vec<ExploredManagedHost>>>::as_ref(
            &explored_managed_hosts
        )
        .len(),
        1
    );
    assert_eq!(
        <Vec<AnyMachine> as AsRef<Vec<AnyMachine>>>::as_ref(&machines).len(),
        2
    );

    // Make sure they are the machines we just created
    let mut bmc_ip_addresses = vec![explored_managed_hosts[0].host_bmc_ip.to_string()];
    for dpu in explored_managed_hosts[0].clone().dpus {
        bmc_ip_addresses.push(dpu.bmc_ip.to_string())
    }
    assert_eq!(bmc_ip_addresses.len(), 2);
    for bmc_ip in bmc_ip_addresses {
        assert!(
            <Vec<AnyMachine> as AsRef<Vec<AnyMachine>>>::as_ref(&machines)
                .iter()
                .any(|x| {
                    x.status
                        .bmc_info
                        .ip
                        .is_some_and(|ip| ip.to_string() == bmc_ip)
                })
        );
    }
    Ok(())
}

async fn fetch_exploration_report(api: &Api) -> rpc::site_explorer::SiteExplorationReport {
    api.get_site_exploration_report(tonic::Request::new(GetSiteExplorationRequest::default()))
        .await
        .unwrap()
        .into_inner()
}

#[sqlx_test]
async fn test_select_host_primary_interface(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut mock_dpus = (0..NUM_DPUS).map(|_| DpuConfig::default()).collect_vec();

    // Make the second DPU have the lower-numbered UEFI device path... we will assert later that
    // it's the primary DPU.
    mock_dpus[0].override_hosts_uefi_device_path = Some(
        UefiDevicePath::from_str("PciRoot(0x8)/Pci(0x2,0xa)/Pci(0x1,0x1)/MAC(A088C208545C,0x1)")
            .unwrap(),
    );
    mock_dpus[1].override_hosts_uefi_device_path = Some(
        UefiDevicePath::from_str("PciRoot(0x8)/Pci(0x2,0xa)/Pci(0x0,0x2)/MAC(A088C208545C,0x1)")
            .unwrap(),
    );

    let host_report: EndpointExplorationReport = ManagedHostConfig::default()
        .with_dpus(mock_dpus.clone())
        .into();

    const NUM_DPUS: usize = 2;

    let env = Env::new(pool).await;
    let mut oob_interfaces = Vec::new();
    let mut explored_dpus = Vec::new();

    for (i, mock_dpu) in mock_dpus.iter().enumerate() {
        let oob_mac = mock_dpu.bmc_mac_address;
        let mut dpu_bmc = FakeMachine {
            mac: oob_mac,
            dhcp_vendor: "NVIDIA/BF/BMC".to_string(),
            segment: env.underlay_segment,
            ip: String::new(),
        };
        dpu_bmc.discover_dhcp(env.api()).await?;

        assert!(!dpu_bmc.ip.is_empty());
        let mut txn = env.pool.begin().await?;
        let oob_interface =
            db::machine_interface::find_by_mac_address(txn.as_mut(), oob_mac).await?;
        txn.commit().await?;
        assert!(oob_interface[0].primary_interface);
        oob_interfaces.push(oob_interface[0].clone());

        let mut dpu_report: EndpointExplorationReport = mock_dpu.clone().into();
        dpu_report.generate_machine_id(false)?;
        let dpu_report = Arc::new(dpu_report);
        explored_dpus.push(ExploredDpu {
            bmc_ip: IpAddr::from_str(format!("192.168.1.{i}").as_str())?,
            host_pf_mac_address: Some(mock_dpu.host_mac_address),
            host_chassis_id: None,
            report: dpu_report,
        });
    }

    // No declaration: the automatic pick stands -- the DPU host PF with the
    // lowest UEFI PCI path (the second mock DPU, given the paths set above).
    let expected_mac: MacAddress = mock_dpus[1].host_mac_address;
    let selection = host_report
        .select_host_primary_interface(&explored_dpus, None)
        .unwrap();
    assert_eq!(selection.mac_address, expected_mac);
    assert_eq!(
        selection.source,
        BootInterfaceSelectionSource::RedfishUefiPci
    );

    // Preserve the all-or-nothing PCI rule: one matching DPU interface without
    // a UEFI path disables PCI selection for the whole host, allowing the
    // provisional chassis or serial ordering to run.
    let mut incomplete_pci_report = host_report.clone();
    let missing_path_mac = mock_dpus[0].host_mac_address;
    incomplete_pci_report.systems[0]
        .ethernet_interfaces
        .iter_mut()
        .find(|interface| interface.mac_address == Some(missing_path_mac))
        .expect("fixture should include the first DPU host-PF")
        .uefi_device_path = None;
    assert_eq!(
        incomplete_pci_report.select_host_primary_interface(&explored_dpus, None),
        None,
    );

    // A declared primary on a DPU host-PF wins over the automatic pick -- here
    // the first DPU, which the PCI ordering would NOT have chosen.
    let declared_dpu_pf = mock_dpus[0].host_mac_address;
    assert_eq!(
        host_report.select_host_primary_interface(&explored_dpus, Some(declared_dpu_pf)),
        Some(model::site_explorer::HostPrimaryInterfaceSelection {
            mac_address: declared_dpu_pf,
            source: BootInterfaceSelectionSource::ExpectedMachine,
        }),
    );

    // The headline case: a declared *integrated* NIC -- which the DPU-only
    // automatic pick can never name -- becomes the explored default.
    let integrated_nic = host_report
        .systems
        .first()
        .unwrap()
        .ethernet_interfaces
        .iter()
        .filter_map(|e| e.mac_address)
        .find(|mac| {
            !explored_dpus
                .iter()
                .any(|d| d.host_pf_mac_address == Some(*mac))
        })
        .expect("the fixture host should have a non-DPU integrated NIC");
    assert_eq!(
        host_report.select_host_primary_interface(&explored_dpus, Some(integrated_nic)),
        Some(model::site_explorer::HostPrimaryInterfaceSelection {
            mac_address: integrated_nic,
            source: BootInterfaceSelectionSource::ExpectedMachine,
        }),
    );

    // A declared MAC absent from this report is ignored -- the automatic pick
    // stands.
    let absent_mac: MacAddress = "de:ad:be:ef:00:01".parse().unwrap();
    assert_eq!(
        host_report.select_host_primary_interface(&explored_dpus, Some(absent_mac)),
        Some(model::site_explorer::HostPrimaryInterfaceSelection {
            mac_address: expected_mac,
            source: BootInterfaceSelectionSource::RedfishUefiPci,
        }),
    );
    Ok(())
}

/// Integration regression guard for the auto-correct path: when an
/// `ExpectedMachine` resolves to `HostDpuPolicy::Nic` but the discovered
/// DPU hardware is reporting `BlueFieldOperatingMode::Dpu`, site-explorer should call
/// `set_nic_mode(Nic)` on the DPU during its per-host matching loop.
///
/// This exercises the full wire (site-explorer iteration → per-host policy
/// resolution → `check_and_configure_dpu_mode` → mock Redfish
/// `set_nic_mode`) that the unit tests only cover in pieces.
#[sqlx_test]
async fn test_site_explorer_auto_corrects_nic_mode_per_expected_machine(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::expected_machine::{ExpectedMachine, ExpectedMachineData, HostDpuPolicy};
    use model::site_explorer::BlueFieldOperatingMode;

    let env = Env::new(pool).await;

    // DPU hardware reports DPU mode (so it looks like a "properly
    // configured" DPU to the BF3-DPU heuristic) -- the selected
    // `HostDpuPolicy::Nic` is what forces the correction to NIC mode.
    let dpu_config = DpuConfig {
        nic_mode: Some(BlueFieldOperatingMode::Dpu),
        ..DpuConfig::default()
    };
    let mock_host =
        model::test_support::ManagedHostConfig::default().with_dpus(vec![dpu_config.clone()]);
    let host_bmc_mac = mock_host.bmc_mac_address;

    // Seed an ExpectedMachine whose policy resolves to
    // `HostDpuPolicy::Nic` and matches the mock host's BMC MAC.
    // Site-explorer's per-host resolution will look this up by IP via the
    // expected-endpoint index after DHCP assigns the host its BMC IP.
    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: host_bmc_mac,
            data: ExpectedMachineData {
                bmc_username: "ADMIN".to_string(),
                bmc_password: "PASS".to_string(),
                serial_number: "EM-866-NIC-OVERRIDE".to_string(),
                metadata: model::metadata::Metadata::new_with_default_name(),
                dpu_policy: HostDpuPolicy::Nic,
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let mut host_bmc = env.new_machine(&host_bmc_mac.to_string(), "SomeVendor");
    let mut dpu_bmc = env.new_machine(&dpu_config.bmc_mac_address.to_string(), "NVIDIA/BF/BMC");
    host_bmc.discover_dhcp(env.api()).await?;
    dpu_bmc.discover_dhcp(env.api()).await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![
        (dpu_bmc.ip.parse().unwrap(), Ok(dpu_config.clone().into())),
        (host_bmc.ip.parse().unwrap(), Ok(mock_host.into())),
    ]);

    // First iteration: initial endpoint exploration.
    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    for ip in [host_bmc.ip.parse()?, dpu_bmc.ip.parse()?] {
        db::explored_endpoints::set_preingestion_complete(ip, &mut txn).await?;
    }
    txn.commit().await?;
    // Second iteration: per-host DPU matching + check_and_configure_dpu_mode.
    explorer.run_single_iteration().await.unwrap();

    let calls = explorer
        .endpoint_explorer()
        .set_nic_mode_calls
        .lock()
        .unwrap();
    assert!(
        calls
            .iter()
            .any(|(_, mode)| *mode == BlueFieldOperatingMode::Nic),
        "expected at least one set_nic_mode(Nic) call triggered by the operator's Nic policy; calls so far: {calls:?}"
    );

    Ok(())
}

/// A queued `set_nic_mode` only takes effect after a host power cycle, and
/// site-explorer drives that power cycle itself for every vendor -- the
/// Redfish `ComputerSystem.Reset` action is standard across BMCs. This is
/// the non-Dell guard for that behavior: a Lenovo host whose DPU needs the
/// mode correction gets an automatic `PowerCycle` on its host BMC in the
/// same pass that issued `set_nic_mode`, rather than parking on a manual
/// power cycle.
#[sqlx_test]
async fn test_site_explorer_power_cycles_non_dell_host_to_apply_nic_mode(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    // DPU hardware reports DPU mode; the operator's `Nic` policy is what
    // forces the correction (and therefore the power cycle).
    let dpu_config = DpuConfig {
        nic_mode: Some(BlueFieldOperatingMode::Dpu),
        ..DpuConfig::default()
    };
    let mock_host = ManagedHostConfig {
        dpus: vec![dpu_config.clone()],
        vendor: Some(bmc_vendor::BMCVendor::Lenovo),
        ..ManagedHostConfig::default()
    };
    let host_bmc_mac = mock_host.bmc_mac_address;

    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: host_bmc_mac,
            data: ExpectedMachineData {
                bmc_username: "ADMIN".to_string(),
                bmc_password: "PASS".to_string(),
                serial_number: "EM-866-NIC-POWERCYCLE".to_string(),
                metadata: Metadata::new_with_default_name(),
                dpu_policy: model::expected_machine::HostDpuPolicy::Nic,
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let mut host_bmc = env.new_machine(&host_bmc_mac.to_string(), "SomeVendor");
    let mut dpu_bmc = env.new_machine(&dpu_config.bmc_mac_address.to_string(), "NVIDIA/BF/BMC");
    host_bmc.discover_dhcp(env.api()).await?;
    dpu_bmc.discover_dhcp(env.api()).await?;

    let host_bmc_ip: IpAddr = host_bmc.ip.parse()?;
    let dpu_bmc_ip: IpAddr = dpu_bmc.ip.parse()?;
    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoints(
        mock_host
            .exploration_results(Some(host_bmc_ip), &[(0, dpu_bmc_ip)])?
            .into_endpoints(),
    );

    // First iteration: initial endpoint exploration.
    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::set_preingestion_complete(host_bmc_ip, &mut txn).await?;
    db::explored_endpoints::set_preingestion_complete(dpu_bmc_ip, &mut txn).await?;
    txn.commit().await?;
    // Second iteration: the matching loop issues `set_nic_mode` and,
    // with the DPU now needing reconfiguration, power-cycles the host
    // so the queued mode change applies.
    explorer.run_single_iteration().await.unwrap();

    let nic_mode_calls = explorer
        .endpoint_explorer()
        .set_nic_mode_calls
        .lock()
        .unwrap();
    assert!(
        nic_mode_calls
            .iter()
            .any(|(_, mode)| *mode == BlueFieldOperatingMode::Nic),
        "expected set_nic_mode(Nic) before the power cycle; calls so far: {nic_mode_calls:?}"
    );

    let power_calls = explorer
        .endpoint_explorer()
        .redfish_power_control_calls
        .lock()
        .unwrap();
    assert!(
        power_calls
            .iter()
            .any(|(_, action)| matches!(action, libredfish::SystemPowerControl::PowerCycle)),
        "expected an automatic host PowerCycle on the non-Dell (Lenovo) host to apply the queued NIC mode change; power calls so far: {power_calls:?}"
    );

    Ok(())
}

/// `PowerCycle` is implemented only by Dell and the DPU BMCs; other vendors --
/// and Vikings -- refuse it. When that happens, site-explorer falls back to a
/// cold `ACPowercycle` so the queued NIC-mode change still applies without an
/// operator, rather than parking immediately on `ManualPowerCycleRequired`.
#[sqlx_test]
async fn test_site_explorer_falls_back_to_ac_powercycle_when_powercycle_refused(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    // DPU reports DPU mode; the operator's `Nic` policy forces the
    // correction (and therefore the reset).
    let dpu_config = DpuConfig {
        nic_mode: Some(BlueFieldOperatingMode::Dpu),
        ..DpuConfig::default()
    };
    let mock_host = ManagedHostConfig {
        dpus: vec![dpu_config.clone()],
        vendor: Some(bmc_vendor::BMCVendor::Lenovo),
        ..ManagedHostConfig::default()
    };
    let host_bmc_mac = mock_host.bmc_mac_address;

    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: host_bmc_mac,
            data: ExpectedMachineData {
                bmc_username: "ADMIN".to_string(),
                bmc_password: "PASS".to_string(),
                serial_number: "EM-2635-AC-FALLBACK".to_string(),
                metadata: Metadata::new_with_default_name(),
                dpu_policy: model::expected_machine::HostDpuPolicy::Nic,
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let mut host_bmc = env.new_machine(&host_bmc_mac.to_string(), "SomeVendor");
    let mut dpu_bmc = env.new_machine(&dpu_config.bmc_mac_address.to_string(), "NVIDIA/BF/BMC");
    host_bmc.discover_dhcp(env.api()).await?;
    dpu_bmc.discover_dhcp(env.api()).await?;

    let host_bmc_ip: IpAddr = host_bmc.ip.parse()?;
    let dpu_bmc_ip: IpAddr = dpu_bmc.ip.parse()?;
    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    // This vendor refuses `PowerCycle` (like a Viking); the reset must fall
    // back to the cold `ACPowercycle`.
    explorer
        .endpoint_explorer()
        .fail_power_control(libredfish::SystemPowerControl::PowerCycle);
    explorer.insert_endpoints(
        mock_host
            .exploration_results(Some(host_bmc_ip), &[(0, dpu_bmc_ip)])?
            .into_endpoints(),
    );

    // First iteration: initial endpoint exploration.
    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    db::explored_endpoints::set_preingestion_complete(host_bmc_ip, &mut txn).await?;
    db::explored_endpoints::set_preingestion_complete(dpu_bmc_ip, &mut txn).await?;
    txn.commit().await?;
    // Second iteration: matching issues `set_nic_mode`, then the reset path
    // tries `PowerCycle`, gets refused, and falls back to `ACPowercycle`.
    explorer.run_single_iteration().await.unwrap();

    let power_calls = explorer
        .endpoint_explorer()
        .redfish_power_control_calls
        .lock()
        .unwrap();
    // Scope the fallback-order check to the host under test so another
    // endpoint's power actions can't skew the `PowerCycle`-before-`ACPowercycle`
    // positions.
    let host_power_calls = power_calls
        .iter()
        .filter(|(addr, _)| addr.ip() == host_bmc_ip)
        .collect::<Vec<_>>();
    let powercycle_pos = host_power_calls
        .iter()
        .position(|(_, action)| matches!(action, libredfish::SystemPowerControl::PowerCycle));
    let acpowercycle_pos = host_power_calls
        .iter()
        .position(|(_, action)| matches!(action, libredfish::SystemPowerControl::ACPowercycle));
    assert!(
        powercycle_pos.is_some(),
        "expected `PowerCycle` to be attempted; power calls so far: {power_calls:?}"
    );
    assert!(
        acpowercycle_pos.is_some(),
        "expected the `ACPowercycle` fallback after `PowerCycle` was refused; power calls so far: {power_calls:?}"
    );
    // A fallback is only correct if `PowerCycle` is the one tried first.
    assert!(
        powercycle_pos < acpowercycle_pos,
        "expected `PowerCycle` (at {powercycle_pos:?}) before the `ACPowercycle` fallback (at {acpowercycle_pos:?}); power calls: {power_calls:?}"
    );

    Ok(())
}

/// Regression guard for the fallback-serial path (#2631): a DPU paired only
/// through `fallback_dpu_serial_numbers` must get the same NIC-mode enforcement
/// as a host-reported one. The host BMC here enumerates no DPU over PCIe -- the
/// usual reason the fallback exists (e.g. a GB200 that drops a DPU from its
/// inventory) -- so the only link is the operator-listed serial, and the DPU is
/// still reporting DPU mode against a `HostDpuPolicy::Nic` host.
///
/// Before the fix the fallback path trusted the match as already-configured: it
/// attached the DPU without a mode check, then dropped it to zero-DPU, so the
/// host registered under `Nic` while the BlueField stayed in DPU mode and
/// `set_nic_mode` was never issued. Now the flip is issued, the host is
/// power-cycled to apply it, and the host waits instead of settling this pass.
#[sqlx_test]
async fn test_site_explorer_enforces_nic_mode_on_fallback_serial_match(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::expected_machine::{ExpectedMachine, ExpectedMachineData, HostDpuPolicy};
    use model::site_explorer::BlueFieldOperatingMode;

    let env = Env::new(pool).await;

    const FALLBACK_DPU_SERIAL: &str = "fallback-only-dpu-serial";
    // DPU reports DPU mode; the host report carries no DPU device, so the
    // serial is the only thing that can pair them.
    let dpu_config = DpuConfig {
        nic_mode: Some(BlueFieldOperatingMode::Dpu),
        serial: FALLBACK_DPU_SERIAL.to_string(),
        ..DpuConfig::default()
    };
    let mock_host = ManagedHostConfig::default();
    let host_bmc_mac = mock_host.bmc_mac_address;

    // Operator selects `HostDpuPolicy::Nic` and lists the DPU's serial as
    // a pairing fallback.
    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: host_bmc_mac,
            data: ExpectedMachineData {
                bmc_username: "ADMIN".to_string(),
                bmc_password: "PASS".to_string(),
                serial_number: "EM-2631-FALLBACK-NIC".to_string(),
                metadata: model::metadata::Metadata::new_with_default_name(),
                dpu_policy: HostDpuPolicy::Nic,
                fallback_dpu_serial_numbers: vec![FALLBACK_DPU_SERIAL.to_string()],
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let mut host_bmc = env.new_machine(&host_bmc_mac.to_string(), "SomeVendor");
    let mut dpu_bmc = env.new_machine(&dpu_config.bmc_mac_address.to_string(), "NVIDIA/BF/BMC");
    host_bmc.discover_dhcp(env.api()).await?;
    dpu_bmc.discover_dhcp(env.api()).await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![
        (dpu_bmc.ip.parse().unwrap(), Ok(dpu_config.clone().into())),
        (host_bmc.ip.parse().unwrap(), Ok(mock_host.into())),
    ]);

    // First iteration: initial endpoint exploration.
    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    for ip in [host_bmc.ip.parse()?, dpu_bmc.ip.parse()?] {
        db::explored_endpoints::set_preingestion_complete(ip, &mut txn).await?;
    }
    txn.commit().await?;
    // Second iteration: per-host matching falls through to the fallback-serial
    // path, which must enforce the resolved `Nic` policy.
    explorer.run_single_iteration().await.unwrap();

    {
        let calls = explorer
            .endpoint_explorer()
            .set_nic_mode_calls
            .lock()
            .unwrap();
        assert!(
            calls
                .iter()
                .any(|(_, mode)| *mode == BlueFieldOperatingMode::Nic),
            "fallback-matched DPU on a Nic-policy host should get set_nic_mode(Nic); calls so far: {calls:?}"
        );
    }

    // The host must not settle as a zero-DPU managed host until the flip has
    // applied -- otherwise the database records a `Nic` host while the
    // BlueField is still physically in DPU mode.
    let explored_managed_hosts = db::explored_managed_host::find_all(&env.pool).await?;
    assert!(
        explored_managed_hosts.is_empty(),
        "host should wait for the queued NIC-mode flip to apply, not register as zero-DPU this pass"
    );

    // The reset path fires even though the host BMC never enumerated the DPU
    // over PCIe (`expected_managed_dpus_total == 0`), so the queued flip can
    // actually apply.
    {
        let power_calls = explorer
            .endpoint_explorer()
            .redfish_power_control_calls
            .lock()
            .unwrap();
        assert!(
            power_calls
                .iter()
                .any(|(_, action)| matches!(action, libredfish::SystemPowerControl::PowerCycle)),
            "host should be power-cycled to apply the queued NIC-mode flip; power calls so far: {power_calls:?}"
        );
    }

    Ok(())
}

/// Some host BMCs (e.g. the AMI/Lenovo GB300 `HG635N_V2`) report the BlueField
/// as the chassis object itself -- model, part_number and serial_number live on
/// the chassis, while its nested network adapter carries an empty serial -- and
/// don't enumerate the DPU over PCIe at all. The host<->DPU serial match must
/// therefore consider the chassis identity, not just `chassis.network_adapters[]`.
///
/// Here the operator declares NO `fallback_dpu_serial_numbers` and the host
/// resolves to `HostDpuPolicy::Manage`, so the only thing that can pair the
/// host with its DPU is the chassis-reported serial. The host must pair (and
/// not fall through to the zero-DPU path).
#[sqlx_test]
async fn test_site_explorer_pairs_dpu_from_chassis_serial(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
    use model::site_explorer::BlueFieldOperatingMode;

    let env = Env::new(pool).await;

    const CHASSIS_DPU_SERIAL: &str = "chassis-reported-dpu-serial";
    // The DPU's BMC reports DPU mode; the host BMC carries no DPU PCIe device,
    // only the BlueField chassis below, so the chassis serial is the only link.
    let dpu_config = DpuConfig {
        nic_mode: Some(BlueFieldOperatingMode::Dpu),
        serial: CHASSIS_DPU_SERIAL.to_string(),
        ..DpuConfig::default()
    };
    // A host with no DPU configs: its report carries no BlueField PCIe device or
    // network adapter, so the only BlueField is the chassis we push below. (A
    // configured DPU would inject a phantom BlueField into the host's PCIe scan,
    // making `expected_managed_total() != 0` and skipping the chassis fallback.)
    let mock_host = ManagedHostConfig::zero_dpu();
    let host_bmc_mac = mock_host.bmc_mac_address;

    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: host_bmc_mac,
            data: ExpectedMachineData {
                bmc_username: "ADMIN".to_string(),
                bmc_password: "PASS".to_string(),
                serial_number: "EM-GB300-CHASSIS-SERIAL".to_string(),
                metadata: model::metadata::Metadata::new_with_default_name(),
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let mut host_report: EndpointExplorationReport = mock_host.into();
    host_report.chassis.push(Chassis {
        id: "Riser_Slot1_BlueField_3_SmartNIC_Main_Card".to_string(),
        manufacturer: Some("Nvidia".to_string()),
        model: Some("BlueField-3 SmartNIC Main Card".to_string()),
        part_number: Some("900-9D3B6-00CN-PA0".to_string()),
        serial_number: Some(CHASSIS_DPU_SERIAL.to_string()),
        network_adapters: vec![NetworkAdapter {
            id: "Riser_Slot1_BlueField_3_SmartNIC_Main_Card".to_string(),
            serial_number: Some(String::new()),
            ..Default::default()
        }],
        ..Default::default()
    });

    let mut host_bmc = env.new_machine(&host_bmc_mac.to_string(), "SomeVendor");
    let mut dpu_bmc = env.new_machine(&dpu_config.bmc_mac_address.to_string(), "NVIDIA/BF/BMC");
    host_bmc.discover_dhcp(env.api()).await?;
    dpu_bmc.discover_dhcp(env.api()).await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![
        (dpu_bmc.ip.parse().unwrap(), Ok(dpu_config.clone().into())),
        (host_bmc.ip.parse().unwrap(), Ok(host_report)),
    ]);

    // First iteration: initial endpoint exploration.
    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    for ip in [host_bmc.ip.parse()?, dpu_bmc.ip.parse()?] {
        db::explored_endpoints::set_preingestion_complete(ip, &mut txn).await?;
    }
    txn.commit().await?;
    // Second iteration: per-host matching pairs the DPU off the chassis serial.
    explorer.run_single_iteration().await.unwrap();

    let explored_managed_hosts = db::explored_managed_host::find_all(&env.pool).await?;
    assert_eq!(
        explored_managed_hosts.len(),
        1,
        "GB300 host should pair via its chassis-reported BlueField serial"
    );
    assert_eq!(
        explored_managed_hosts[0].dpus.len(),
        1,
        "the chassis-matched DPU should be attached to the host"
    );

    Ok(())
}

/// Vera Rubin host BMCs report the attached BF4 as its own `BlueField_0`
/// chassis, with the usable pairing serial on that chassis object and no DPU
/// under `Systems[].PCIeDevices`. This is the shape from the real VR Redfish
/// dump, and it must pair without falling through to the zero-DPU gate.
#[sqlx_test]
async fn test_site_explorer_pairs_vr_bf4_from_bluefield_chassis(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;

    let reports = generate_managed_host_reports(HardwareType::NvidiaDgxVr).await?;
    let dpu = reports
        .dpus
        .first()
        .expect("NvidiaDgxVr should generate one DPU");
    let host_bmc_mac = reports.host.machine_info.bmc_mac_address;
    let dpu_bmc_mac = dpu.machine_info.bmc_mac_address;
    let host_serial = reports
        .host
        .report
        .systems
        .first()
        .and_then(|system| system.serial_number.clone())
        .expect("VR host report should include a system serial");
    let host_pf_mac = dpu
        .report
        .systems
        .first()
        .and_then(|system| system.base_mac)
        .expect("DPU report should include host PF base MAC")
        .to_mac();

    let mut host_bmc = env.new_machine(&host_bmc_mac.to_string(), "NVIDIA");
    let mut dpu_bmc = env.new_machine(&dpu_bmc_mac.to_string(), "NVIDIA/BF/BMC");
    host_bmc.discover_dhcp(env.api()).await?;
    dpu_bmc.discover_dhcp(env.api()).await?;
    let host_bmc_ip: IpAddr = host_bmc.ip.parse()?;
    let dpu_bmc_ip: IpAddr = dpu_bmc.ip.parse()?;

    let mut txn = env.pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: host_bmc_mac,
            data: ExpectedMachineData {
                bmc_username: "ADMIN".to_string(),
                bmc_password: "PASS".to_string(),
                serial_number: host_serial,
                metadata: Metadata::new_with_default_name(),
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env.test_site_explorer(explorer_config);
    explorer.insert_endpoint_results(vec![
        (dpu_bmc_ip, Ok(dpu.report.clone())),
        (host_bmc_ip, Ok(reports.host.report.clone())),
    ]);

    explorer.run_single_iteration().await.unwrap();
    let mut txn = env.pool.begin().await?;
    for ip in [host_bmc_ip, dpu_bmc_ip] {
        db::explored_endpoints::set_preingestion_complete(ip, &mut txn).await?;
    }
    txn.commit().await?;
    explorer.run_single_iteration().await.unwrap();

    let explored_managed_hosts = db::explored_managed_host::find_all(&env.pool).await?;
    assert_eq!(
        explored_managed_hosts.len(),
        1,
        "VR host should pair via BlueField_0 chassis serial"
    );
    let managed_host = &explored_managed_hosts[0];
    assert_eq!(managed_host.dpus.len(), 1);
    assert_eq!(managed_host.dpus[0].bmc_ip, dpu_bmc_ip);
    assert_eq!(
        managed_host.dpus[0].host_pf_mac_address,
        Some(host_pf_mac),
        "BF4 host-PF MAC should come from the DPU report base_mac"
    );
    if let Some(blocker_metric) = env
        .test_harness
        .test_meter
        .formatted_metric("carbide_host_dpu_pairing_blockers_count")
    {
        assert!(
            !blocker_metric.contains("no_dpu_reported_by_host"),
            "VR BF4 chassis pairing should not hit the zero-DPU blocker: {blocker_metric}"
        );
    }

    Ok(())
}

/// A managed host's DPU-facing `machine_interface` is created (via DHCP) with
/// just a MAC and no `boot_interface_id`. The exploration that ingests the host
/// then backfills the vendor-specific Redfish interface id onto that row, matched
/// by MAC, at which the primary interface ends up with a full `MachineBootInterface`.
/// This is the same backfill path any DHCP-derived interface takes (the capture is
/// keyed on MAC, not on how the row was created).
#[sqlx_test]
async fn test_site_explorer_backfills_boot_interface_id_onto_machine_interface(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let test_harness = TestHarness::builder(pool).build().await;
    let domain = test_harness.test_domain().await;
    let network_controller = test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let explorer_config = SiteExplorerConfig {
        enabled: Arc::new(true.into()),
        retained_boot_interface_window: None,
        explorations_per_run: 10,
        concurrent_explorations: 1,
        run_interval: std::time::Duration::from_secs(1),
        create_machines: Arc::new(true.into()),
        ..Default::default()
    };
    let explorer = env::test_site_explorer(&test_harness, explorer_config);

    let dpu = DpuConfig::default();
    let host_pf_mac = dpu.host_mac_address;
    let managed_host = ManagedHostConfig::default().with_dpus(vec![dpu]);
    let (created_host, _) = test_harness
        .managed_host_builder(&explorer, underlay_segment)
        .with_config(managed_host)
        .build()
        .await;

    // TestManagedHostBuilder runs initial endpoint exploration and marks
    // preingestion complete. Its second iteration creates the predicted host-PF
    // interface with the Redfish boot interface id from the endpoint report.
    created_host
        .host
        .dhcp_discover_primary_iface(admin_segment)
        .await;

    // Third iteration: the DHCP-created row is matched by MAC and receives
    // the boot interface id from the predicted interface.
    explorer.run_single_iteration().await.unwrap();

    let mut txn = test_harness.db_txn().await;
    let interfaces =
        db::machine_interface::find_by_machine_ids(&mut txn, &[created_host.host.id]).await?;
    txn.commit().await?;
    let primary = interfaces
        .get(&created_host.host.id)
        .into_iter()
        .flatten()
        .find(|i| i.primary_interface)
        .expect("ingested host should have a primary machine_interface");

    // The primary row is the DPU host-PF interface (same factory MAC), now
    // holding both halves of the pair: its MAC plus the Redfish interface id the
    // host report named for it. The `ManagedHostConfig` fixture ids its DPU
    // interfaces "NIC.Slot.{index + 5}-1", so the first DPU is "NIC.Slot.5-1".
    assert_eq!(primary.mac_address, host_pf_mac);
    assert_eq!(
        primary.boot_interface_id.as_deref(),
        Some("NIC.Slot.5-1"),
        "exploration should backfill the Redfish interface id onto the machine_interface row",
    );

    Ok(())
}
