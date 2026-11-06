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

use carbide_dpf::{DpuDeploymentType, DpuPhase, DpuPhaseObservation};
use carbide_machine_controller::dpf::MockDpfOperations;
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem};
use model::machine::DpfState;
use model::site_explorer::EthernetInterface;

use super::*;
use crate::tests::common::api_fixtures::dpu::create_machine_inventory;
use crate::tests::common::api_fixtures::site_explorer::{
    MockExploredHost, register_expected_machine, seed_bmc_root_credentials,
};

/// Resumes BF4 host-PF repair after DPF suppression and late agent registration so the same controller reaches
/// HostInit only after fresh host evidence commits.
#[crate::sqlx_test]
async fn test_bf4_suppressed_exploration_resumes_after_late_registration(pool: sqlx::PgPool) {
    // Let the real controller enter and leave DPF provisioning; only the operator's external responses are mocked.
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_get_dpu_phase_observation().returning(|_, _| {
        Ok(DpuPhaseObservation {
            phase: DpuPhase::Ready,
            failure: None,
        })
    });
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf4Generic));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_get_service_versions_for_dpu()
        .returning(|_| Ok(Vec::new()));
    let mut config = get_config();
    config.dpf.enabled = true; // Exercise the controller-owned DPF provisioning transitions.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;

    // Withhold the registered MAC from the initial host report; the remaining fixture fields provide ordinary
    // DPU pairing and admin DHCP.
    let host_config = env.managed_host_config();
    let registered_mac: MacAddress = "02:00:00:fe:10:01".parse().expect("registered host-PF MAC");
    assert_ne!(registered_mac, host_config.dpus[0].host_mac_address);
    register_expected_machine(&env, &host_config, Some(true)).await;
    seed_bmc_root_credentials(&env, &host_config)
        .await
        .expect("BMC credentials");
    let explored = MockExploredHost::new(&env, host_config.clone())
        .discover_dhcp_dpu_bmc(0, |response, _| {
            response?;
            Ok(())
        })
        .await
        .expect("DPU BMC DHCP")
        .discover_dhcp_dpu_primary_iface(0)
        .await
        .discover_dhcp_host_bmc(|response, _| {
            response?;
            Ok(())
        })
        .await
        .expect("host BMC DHCP");
    let host_bmc_ip = explored.host_bmc_ip.expect("host BMC address");
    let dpu_bmc_ip = explored.dpu_bmc_ips[&0];
    let mut reports = host_config
        .exploration_results(Some(host_bmc_ip), &[(0, dpu_bmc_ip)])
        .expect("paired reports");
    let dpu_id = reports.dpu_machine_ids()[&0];
    // Classify the DPU as BF4 from Redfish before any agent has registered its host PFs.
    let dpu_report = &mut reports.dpu_reports[0].report;
    dpu_report.chassis[0].part_number = Some("900-9D4B4-CWAA-TSA".to_string());
    // Supply the BF4 DMI product as well because DPF platform handling classifies it independently of the part number.
    dpu_report.systems[0].model = Some("NVIDIA BlueField-4 DPU".to_string());
    dpu_report.systems[0].pcie_devices[0].part_number = Some("900-9D4B4-CWAA-TSA".to_string());
    let mut resumed_report = reports.host_report.as_ref().expect("host report").1.clone();
    assert!(
        resumed_report
            .find_interface_id_for_mac(registered_mac)
            .is_none()
    );
    env.endpoint_explorer
        .insert_endpoints(reports.into_endpoints());
    explored
        .run_site_explorer_iteration()
        .await
        .mark_preingestion_complete()
        .await
        .expect("preingestion complete")
        .run_site_explorer_iteration()
        .await
        .discover_dhcp_host_primary_iface(|response, _| {
            response?;
            Ok(())
        })
        .await
        .expect("provisional admin DHCP");
    let mut txn = env.db_txn().await;
    let host_id = db::machine::DpuHostAssociations::load(txn.as_mut(), &[dpu_id])
        .await
        .expect("DPU associations")
        .host_machine_id(&dpu_id)
        .expect("paired host");
    txn.rollback().await.expect("close host lookup");
    let host_machine = common::api_fixtures::TestMachine::new(host_id, env.api.clone());
    let waiting_for_dpf_ready = ManagedHostState::DPUInit {
        dpu_states: model::machine::DpuInitStates {
            states: HashMap::from([(
                dpu_id,
                DpuInitState::DpfStates {
                    state: DpfState::WaitingForReady { phase_detail: None },
                },
            )]),
        },
    };

    // Stop at DPF readiness before requeued transitions can clear exploration suppression.
    env.run_machine_state_controller_iteration_until_state_condition_with_requeue(
        &host_id,
        30,
        false,
        |machine| machine.current_state() == &waiting_for_dpf_ready,
    )
    .await;

    // Read the suppression created by the controller and retain its actual provisional link for the later repair proof.
    let mut txn = env.db_txn().await;
    for mac in [
        host_config.bmc_mac_address,
        host_config.dpus[0].bmc_mac_address,
    ] {
        assert!(
            db::bmc_suppression::find(
                txn.as_mut(),
                mac,
                BmcSuppressionSubsystem::SiteExplorer,
                BmcSuppressionSource::DpfProvisioning
            )
            .await
            .expect("DPF suppression lookup")
            .is_some()
        );
    }
    let before = host_machine.db_machine(&mut txn).await;
    assert!(before.config.dpf.used_for_ingestion);
    let provisional = before
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("provisional primary");
    assert_eq!(
        provisional.mac_address,
        host_config.dpus[0].host_mac_address
    );
    assert_eq!(
        provisional.addresses.len(),
        1,
        "the fixture leases one IPv4 admin address"
    );
    let cached = db::explored_endpoints::find_all_by_ip(host_bmc_ip, txn.as_mut())
        .await
        .expect("cached report")
        .remove(0);
    txn.rollback().await.expect("close suppressed snapshot");

    // Offer new Redfish evidence while suppressed, then queue a refresh through the public API before running
    // Site Explorer.
    resumed_report.systems[0]
        .ethernet_interfaces
        .push(EthernetInterface {
            mac_address: Some(registered_mac), // Only the resumed report exposes the agent's exact registered target.
            id: Some("Registered.HostPF0".to_string()), // Persist the current Redfish ID with the exact MAC.
            // Automatic boot selection requires a usable PCI path for this registered candidate.
            uefi_device_path: Some(
                "PciRoot(0x0)/Pci(0x2,0x0)"
                    .parse()
                    .expect("registered PCI path"),
            ),
            ..Default::default()
        });
    env.endpoint_explorer
        .insert_endpoints(vec![(host_bmc_ip, resumed_report)]);
    let calls_before = env.endpoint_explorer.explore_endpoint_call_count();
    env.api
        .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
            ip_address: host_bmc_ip.to_string(),
        }))
        .await
        .expect("queue suppressed host refresh");
    env.run_site_explorer_iteration().await;
    assert_eq!(
        env.endpoint_explorer.explore_endpoint_call_count(),
        calls_before
    );
    let mut txn = env.db_txn().await;
    let deferred = db::explored_endpoints::find_all_by_ip(host_bmc_ip, txn.as_mut())
        .await
        .expect("deferred report")
        .remove(0);
    assert_eq!(deferred.report, cached.report);
    assert_eq!(deferred.report_version, cached.report_version);
    assert!(deferred.exploration_requested);
    txn.rollback().await.expect("close deferred read");

    // Register through DpuAgent while exploration remains suppressed, using the existing public DPU discovery identity.
    let registered_dpu = env.find_machine(&dpu_id.into()).await.remove(0);
    let caller = registered_dpu
        .bmc
        .as_ref()
        .and_then(|bmc| bmc.machine_interface_id)
        .expect("owned DPU BMC interface");
    let mut discovery = registered_dpu
        .status
        .expect("DPU status")
        .discovery_info
        .expect("Redfish DPU identity");
    assert!(
        discovery
            .dpu_info
            .as_ref()
            .expect("BF4 inventory")
            .host_pfs
            .is_empty()
    );
    discovery.dpu_info.as_mut().expect("BF4 inventory").host_pfs =
        vec![rpc::machine_discovery::DpuHostPf {
            pci_controller_id: 1, // The default BF4 policy selects controller 1 PF 0.
            pf_id: 0,             // BF4 requires PF 0 on the selected controller.
            mac_address: registered_mac.to_string(),
            physical_port_name: "c1pf0".to_string(),
            netdev_name: None,
            pci_address: None,
        }];
    env.api
        .discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(caller),
            discovery_data: Some(DiscoveryData::Info(discovery)),
            discovery_reporter: rpc::forge::MachineDiscoveryReporter::DpuAgent as i32,
            ..Default::default()
        }))
        .await
        .expect("late DPU-agent registration");
    let registered = env
        .find_machine(&dpu_id.into())
        .await
        .remove(0)
        .status
        .expect("persisted DPU status")
        .discovery_info
        .expect("persisted discovery")
        .dpu_info
        .expect("persisted BF4 inventory")
        .host_pfs;
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].mac_address, registered_mac.to_string());
    env.run_site_explorer_iteration().await;

    // Late inventory cannot substitute for fresh host evidence or change links while the report is still suppressed.
    assert_eq!(
        env.endpoint_explorer.explore_endpoint_call_count(),
        calls_before
    );
    let mut txn = env.db_txn().await;
    let still_suppressed = host_machine.db_machine(&mut txn).await;
    let suppressed_primary = still_suppressed
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("suppressed primary");
    assert_eq!(suppressed_primary.id, provisional.id);
    assert_eq!(suppressed_primary.attached_dpu_machine_id, Some(dpu_id));
    assert_eq!(suppressed_primary.addresses, provisional.addresses);
    assert!(
        !still_suppressed
            .status
            .interfaces
            .iter()
            .any(|interface| interface.mac_address == registered_mac)
    );
    assert!(
        matches!(still_suppressed.current_state(), ManagedHostState::DPUInit { dpu_states } if matches!(dpu_states.states.get(&dpu_id), Some(DpuInitState::DpfStates { state: DpfState::WaitingForReady { .. } })))
    );
    txn.rollback().await.expect("close late registration read");

    // Finish the ordinary DPU discovery prerequisites before allowing the same controller to observe DPF Ready.
    create_machine_inventory(&env, dpu_id).await;
    discovery_completed(&env, &dpu_id.into()).await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &host_id,
        12,
        ManagedHostState::DPUInit {
            dpu_states: model::machine::DpuInitStates {
                states: HashMap::from([(dpu_id, DpuInitState::WaitingForNetworkConfig)]),
            },
        },
    )
    .await;
    // Report healthy networking before testing the independent BF4 evidence wait.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration().await;

    // Read committed suppression removal and the BF4 wait to prove DPF completion alone cannot enter HostInit.
    let mut txn = env.db_txn().await;
    for mac in [
        host_config.bmc_mac_address,
        host_config.dpus[0].bmc_mac_address,
    ] {
        assert!(
            db::bmc_suppression::find(
                txn.as_mut(),
                mac,
                BmcSuppressionSubsystem::SiteExplorer,
                BmcSuppressionSource::DpfProvisioning
            )
            .await
            .expect("released DPF suppression lookup")
            .is_none()
        );
    }
    let waiting = host_machine.db_machine(&mut txn).await;
    assert!(
        matches!(waiting.current_state(), ManagedHostState::DPUInit { dpu_states } if dpu_states.states.get(&dpu_id) == Some(&DpuInitState::WaitingForNetworkConfig))
    );
    assert!(
        matches!(&waiting.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains("Waiting for BF4 boot interface") && reason.contains(&registered_mac.to_string()))
    );
    txn.rollback().await.expect("close readiness wait");

    // Resume the queued host exploration through the existing Site Explorer after DPF releases suppression.
    env.run_site_explorer_iteration().await;
    // Reload the committed report, exact DPU link, boot interface ID and admin addresses to prove repair completed.
    let mut txn = env.db_txn().await;
    let resumed = db::explored_endpoints::find_all_by_ip(host_bmc_ip, txn.as_mut())
        .await
        .expect("resumed report")
        .remove(0);
    assert!(!resumed.exploration_requested);
    assert_ne!(resumed.report_version, cached.report_version);
    assert_eq!(
        resumed.report.find_interface_id_for_mac(registered_mac),
        Some("Registered.HostPF0")
    );
    let repaired = host_machine.db_machine(&mut txn).await;
    let primary = repaired
        .status
        .interfaces
        .iter()
        .filter(|interface| interface.primary_interface)
        .collect::<Vec<_>>();
    assert_eq!(primary.len(), 1);
    assert_eq!(primary[0].mac_address, registered_mac);
    assert_eq!(primary[0].attached_dpu_machine_id, Some(dpu_id));
    assert_eq!(
        primary[0].boot_interface_id.as_deref(),
        Some("Registered.HostPF0")
    );
    assert_eq!(primary[0].addresses, provisional.addresses);
    let retired = repaired
        .status
        .interfaces
        .iter()
        .find(|interface| interface.id == provisional.id)
        .expect("retained provisional interface");
    assert!(!retired.primary_interface);
    assert_eq!(retired.attached_dpu_machine_id, None);
    assert!(retired.addresses.is_empty());
    assert_ne!(
        repaired.network_config.version,
        waiting.network_config.version
    );
    let desired = &repaired
        .config
        .desired_boot_interface
        .as_ref()
        .expect("registered boot target")
        .value;
    assert_eq!(desired.mac_address(), registered_mac);
    assert_eq!(desired.interface_id(), Some("Registered.HostPF0"));
    txn.rollback().await.expect("close repaired read");
    assert!(
        env.endpoint_explorer
            .explore_endpoint_calls
            .lock()
            .expect("exploration calls")
            .iter()
            .skip(calls_before)
            .any(|call| call.ip_address == host_bmc_ip)
    );

    // Acknowledge the repaired network version so the same controller can reevaluate boot readiness.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    // Reload the persisted HostInit transition to prove progress without a controller or lifecycle reset.
    let initialized = host_machine.db_machine(&mut txn).await;
    assert!(matches!(
        initialized.current_state(),
        ManagedHostState::HostInit {
            machine_state: MachineState::EnableIpmiOverLan
        }
    ));
    assert!(matches!(
        initialized.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    txn.rollback().await.expect("close HostInit read");
}
