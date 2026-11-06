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

use super::*;

/// Registers BF4 evidence for the fixture's existing boot NIC so lifecycle tests isolate legacy or operator intent without changing networking.
async fn register_existing_bf4_boot_target(
    env: &TestEnv,
    mh: &TestManagedHost,
) -> MachineBootInterfaceTarget {
    // Retain the real fixture target and topology identity while introducing the new BF4 registration requirement.
    let dpu_id = mh.dpu_ids[0];
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    let target = host
        .config
        .desired_boot_interface
        .as_ref()
        .expect("fixture boot intent")
        .value
        .clone();
    let interface_id = target.interface_id().expect("fixture boot ID").to_string();
    let mut topology = db::machine_topology::find_latest_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .expect("DPU topology")
        .remove(&dpu_id)
        .expect("registered DPU");
    let hardware = &mut topology.topology.discovery_data.info;
    let dpu = hardware.dpu_info.as_mut().expect("DPU inventory");
    dpu.part_number = "900-9D4B4-CWAA-TSA".to_string(); // Select BF4 policy without changing the real DPU identity.
    dpu.host_pfs = vec![model::hardware_info::DpuHostPf {
        pci_controller_id: 1,
        pf_id: 0,
        mac_address: target.mac_address(), // Keep the registered target equal to the retained physical boot NIC.
        physical_port_name: "c1pf0".to_string(),
        netdev_name: None,
        pci_address: None,
    }];
    let registered = dpu.clone();
    // Refresh platform classification before the agent merge because generic topology updates cannot initialize host_pfs.
    db::machine_topology::set_topology_update_needed(&mut txn, &dpu_id.into(), true)
        .await
        .expect("refresh fixture platform");
    db::machine_topology::create_or_update(&mut txn, &dpu_id.into(), hardware)
        .await
        .expect("store BF4 classification");
    db::machine_topology::record_dpu_host_pfs(&mut txn, &dpu_id, &registered)
        .await
        .expect("register host-PF");
    let report = EndpointExplorationReport {
        systems: vec![model::site_explorer::ComputerSystem {
            ethernet_interfaces: vec![model::site_explorer::EthernetInterface {
                mac_address: Some(target.mac_address()),
                id: Some(interface_id),
                uefi_device_path: Some("PciRoot(0x0)/Pci(0x2,0x0)".parse().expect("PCI path")),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let bmc_ip = host.status.bmc_info.ip.expect("host BMC IP");
    db::explored_endpoints::insert(bmc_ip, &report, false, &mut txn)
        .await
        .expect("ensure host endpoint");
    // Replace cached evidence directly because these tests control controller iterations instead of running Site Explorer.
    sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
        .bind(bmc_ip)
        .bind(sqlx::types::Json(&report))
        .execute(txn.as_mut())
        .await
        .expect("publish BF4 boot report");
    txn.commit().await.expect("commit BF4 evidence");
    target
}

/// Resumes legacy HostInit/SetBootOrder when its retained target matches BF4 registration, without rewinding the lifecycle or inventing selection history.
#[crate::sqlx_test]
async fn test_legacy_bf4_host_init_resumes_same_boot_target(pool: sqlx::PgPool) {
    // Create a Ready host with registered BF4 evidence for its existing boot target.
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let target = register_existing_bf4_boot_target(&env, &mh).await;

    // Model a pre-source-tracking target already persisted in HostInit/SetBootOrder.
    sqlx::query("UPDATE machine_boot_interfaces SET selection_source = 'legacy_unknown', selection_updated_at = NULL WHERE machine_id = $1")
        .bind(mh.host().id).execute(&env.pool).await.expect("restore truthful legacy metadata");
    set_host_stuck_in_set_boot_order(&env, mh.host().id).await;
    let mut txn = env.db_txn().await;
    let before = mh.host().db_machine(&mut txn).await;
    txn.rollback().await.expect("close legacy snapshot");
    // Model a configured BIOS with boot-order drift so the boot-order stage performs meaningful work.
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);
    let checkpoint = env.redfish_sim.timepoint();

    // Advance the existing state directly; no pre-HostInit network repair or manual state reset is involved.
    env.run_machine_state_controller_iteration().await;
    // Reload the committed stage and networking to distinguish successful continuation from an unchanged wait.
    let mut txn = env.db_txn().await;
    let advanced = mh.host().db_machine(&mut txn).await;
    assert!(
        matches!(advanced.current_state(), ManagedHostState::HostInit {
        machine_state: MachineState::SetBootOrder { set_boot_order_info: Some(info) },
    } if info.set_boot_order_state == SetBootOrderState::WaitForSetBootOrderJobScheduled)
    );
    assert!(matches!(
        advanced.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    assert_eq!(
        serde_json::to_value(&advanced.status.interfaces).expect("serialize preserved interfaces"),
        serde_json::to_value(&before.status.interfaces).expect("serialize original interfaces")
    );
    assert_eq!(
        advanced.network_config.version,
        before.network_config.version
    );
    assert_eq!(advanced.network_config.value, before.network_config.value);
    assert_eq!(
        advanced
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| (&desired.value, desired.version)),
        before
            .config
            .desired_boot_interface
            .as_ref()
            .map(|desired| (&desired.value, desired.version))
    );
    assert_eq!(
        advanced.config.boot_interface_selection,
        before.config.boot_interface_selection
    );
    txn.rollback().await.expect("close resumed snapshot");

    // The externally published request must use the preserved registered target.
    assert!(env.redfish_sim.actions_since(&checkpoint).all_hosts().iter().any(|action| matches!(
        action,
        RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac } if boot_interface_mac == &target.mac_address().to_string()
    )));
}

/// Advances Ready boot convergence with a fresh DPU report before each iteration so target checks remain reachable without bypassing DPU reachability.
async fn advance_ready_boot_with_agent_reports(
    env: &TestEnv,
    mh: &TestManagedHost,
    max_iterations: u32,
    state_check: impl Fn(&model::machine::StableHostMachine) -> bool,
) {
    for _ in 0..max_iterations {
        // Report after the previous state change so Prepare and CheckHostConfig can verify current DPU reachability.
        crate::tests::common::api_fixtures::network_configured(env, &mh.dpu_ids).await;
        env.run_machine_state_controller_iteration().await;

        // Reload the committed controller outcome rather than accepting an in-memory transition.
        let mut txn = env.db_txn().await;
        let host = mh.host().db_machine(&mut txn).await;
        txn.rollback().await.expect("close boot convergence read");
        if state_check(&host) {
            return;
        }
    }
    panic!(
        "Ready boot convergence did not reach its expected outcome after {max_iterations} iterations"
    );
}

/// Recovers blocked Ready boot convergence through an explicit alternate MAC while preserving the networking primary and administrative addresses.
#[crate::sqlx_test]
async fn test_operator_alternate_bf4_boot_target_recovers_ready_convergence(pool: sqlx::PgPool) {
    // Create a Ready host with registered BF4 evidence and a MAC different from its existing boot target.
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let registered = register_existing_bf4_boot_target(&env, &mh).await;
    let alternate = MacAddress::new([2, 0, 0, 0, 0xfe, 7]);
    assert_ne!(alternate, registered.mac_address());

    // Model preserved automatic intent for a different NIC; only explicit selection may authorize it without network migration.
    let mut txn = env.db_txn().await;
    db::machine_desired_boot_interface::set(
        &mut txn,
        &mh.host().id,
        &MachineBootInterfaceTarget::MacOnly(alternate),
        BootInterfaceSelectionSource::RedfishChassisId,
    )
    .await
    .expect("persist provisional automatic intent");
    txn.commit().await.expect("commit automatic intent");
    let mut txn = env.db_txn().await;
    let before = mh.host().db_machine(&mut txn).await;
    txn.rollback().await.expect("close networking baseline");
    // Model a configured BIOS with boot-order drift so the boot-order stage performs meaningful work.
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);
    env.redfish_sim
        .set_lockdown(libredfish::EnabledDisabled::Disabled);
    let checkpoint = env.redfish_sim.timepoint();

    // Let Ready enter its normal persisted boot flow and prove the different automatic MAC is blocked with a recovery action.
    advance_ready_boot_with_agent_reports(&env, &mh, 10, |host| {
        matches!(host.current_state(), ManagedHostState::BootConfiguring {
            boot_config_state: ReadyBootConfigState::SetBootOrder { .. }, ..
        }) && matches!(&host.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason.contains("preserved automatic boot target") && reason.contains("SetDpuFirstBootOrder"))
    }).await;
    assert!(
        env.redfish_sim
            .actions_since(&checkpoint)
            .all_hosts()
            .iter()
            .all(|action| !matches!(action, RedfishSimAction::SetBootOrderDpuFirst { .. }))
    );

    // Publish explicit boot-only intent through the supported API without changing the persisted lifecycle state.
    env.api
        .set_dpu_first_boot_order(Request::new(rpc::forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(mh.host().id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(alternate.to_string()),
        }))
        .await
        .expect("select alternate operator boot target");
    advance_ready_boot_with_agent_reports(&env, &mh, 8, |host| {
        matches!(host.current_state(), ManagedHostState::BootConfiguring {
            boot_config_state: ReadyBootConfigState::SetBootOrder { set_boot_order_info }, ..
        } if set_boot_order_info.set_boot_order_state == SetBootOrderState::WaitForSetBootOrderJobScheduled)
    }).await;

    // Reload committed state to prove the requested MAC progressed while networking and address ownership stayed intact.
    let mut txn = env.db_txn().await;
    let advanced = mh.host().db_machine(&mut txn).await;
    assert_eq!(
        serde_json::to_value(&advanced.status.interfaces).expect("serialize preserved interfaces"),
        serde_json::to_value(&before.status.interfaces).expect("serialize original interfaces")
    );
    assert_eq!(
        advanced.network_config.version,
        before.network_config.version
    );
    assert_eq!(advanced.network_config.value, before.network_config.value);
    assert_eq!(
        advanced
            .config
            .desired_boot_interface
            .as_ref()
            .expect("operator target")
            .value,
        MachineBootInterfaceTarget::MacOnly(alternate)
    );
    assert_eq!(
        advanced
            .config
            .boot_interface_selection
            .expect("operator authority")
            .source,
        BootInterfaceSelectionSource::Operator
    );
    assert!(matches!(
        advanced.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    txn.rollback().await.expect("close operator recovery read");
    assert!(env.redfish_sim.actions_since(&checkpoint).all_hosts().iter().any(|action| matches!(
        action,
        RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac } if boot_interface_mac == &alternate.to_string()
    )));
}

/// Applies an alternate Operator MAC during HostInit through ordinary target resolution so the existing networking primary cannot redirect explicit boot intent.
#[crate::sqlx_test]
async fn test_operator_bf4_host_init_target_overrides_network_primary(pool: sqlx::PgPool) {
    // Create a Ready host with registered BF4 evidence and a MAC different from its existing boot target.
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let registered = register_existing_bf4_boot_target(&env, &mh).await;
    let alternate = MacAddress::new([2, 0, 0, 0, 0xfe, 9]);
    assert_ne!(alternate, registered.mac_address());
    // Start with an existing HostInit boot-order stage and retain its actual interfaces for the preservation check.
    set_host_stuck_in_set_boot_order(&env, mh.host().id).await;
    let mut txn = env.db_txn().await;
    let before = mh.host().db_machine(&mut txn).await;
    txn.rollback().await.expect("close HostInit baseline");
    // Model a configured BIOS with boot-order drift so the boot-order stage performs meaningful work.
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);
    let checkpoint = env.redfish_sim.timepoint();

    // Unlike Ready convergence, HostInit supplies no captured target, so the shared resolver must read the persisted explicit intent.
    env.api
        .machine_setup(Request::new(rpc::forge::MachineSetupRequest {
            machine_id: Some(mh.host().id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(alternate.to_string()),
        }))
        .await
        .expect("record explicit HostInit boot target");
    env.run_machine_state_controller_iteration().await;

    // Reload the persisted stage and published request to prove the operator target progressed without changing network ownership.
    let mut txn = env.db_txn().await;
    let advanced = mh.host().db_machine(&mut txn).await;
    assert!(
        matches!(advanced.current_state(), ManagedHostState::HostInit {
        machine_state: MachineState::SetBootOrder { set_boot_order_info: Some(info) },
    } if info.set_boot_order_state == SetBootOrderState::WaitForSetBootOrderJobScheduled)
    );
    assert!(matches!(
        advanced.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    assert_eq!(
        serde_json::to_value(&advanced.status.interfaces).expect("serialize preserved interfaces"),
        serde_json::to_value(&before.status.interfaces).expect("serialize original interfaces")
    );
    assert_eq!(
        advanced.network_config.version,
        before.network_config.version
    );
    assert_eq!(advanced.network_config.value, before.network_config.value);
    assert_eq!(
        advanced
            .config
            .desired_boot_interface
            .as_ref()
            .expect("operator target")
            .value,
        MachineBootInterfaceTarget::MacOnly(alternate)
    );
    txn.rollback().await.expect("close HostInit recovery read");
    assert!(env.redfish_sim.actions_since(&checkpoint).all_hosts().iter().any(|action| matches!(
        action,
        RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac } if boot_interface_mac == &alternate.to_string()
    )));
}

/// Advances persisted BF4 boot-order scheduling and completion polls despite missing host Redfish evidence so the initial publication check cannot strand accepted jobs.
#[crate::sqlx_test]
async fn test_bf4_existing_boot_order_jobs_progress_without_host_report(pool: sqlx::PgPool) {
    // Register complete BF4 evidence for the existing fixture target before removing only the host report.
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let target = register_existing_bf4_boot_target(&env, &mh).await;

    // Remove current host evidence while retaining the registered BF4 host-PF and persisted boot target.
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
        .bind(host.status.bmc_info.ip.expect("host BMC IP"))
        .bind(sqlx::types::Json(EndpointExplorationReport::default()))
        .execute(txn.as_mut())
        .await
        .expect("remove current host Redfish evidence");
    txn.commit().await.expect("commit missing host report");
    let checkpoint = env.redfish_sim.timepoint();

    for (polling_stage, job_state, expected_stage) in [
        // A scheduled job must reach its reboot stage even when the new publication gate cannot resolve the host report.
        (
            SetBootOrderState::WaitForSetBootOrderJobScheduled,
            libredfish::JobState::Scheduled,
            SetBootOrderState::RebootHost,
        ),
        // A completed job in the post-reboot polling stage must reach verification without another boot-order publication.
        (
            SetBootOrderState::WaitForSetBootOrderJobCompletion,
            libredfish::JobState::Completed,
            SetBootOrderState::CheckBootOrder,
        ),
    ] {
        // Model a previously accepted vendor job and nonzero retry budget at each supported upgrade polling stage.
        let pending_job = SetBootOrderInfo {
            set_boot_order_jid: Some(TEST_BOOT_ORDER_JOB_ID.to_string()), // Retain the accepted job across the upgrade.
            set_boot_order_state: polling_stage,
            retry_count: 2, // Polling must preserve the existing convergence budget.
        };
        set_host_controller_state_stuck_in(
            &env,
            mh.host().id,
            &ManagedHostState::HostInit {
                machine_state: MachineState::SetBootOrder {
                    set_boot_order_info: Some(pending_job),
                },
            },
            0,
        )
        .await;
        // Supply the vendor status that selects the scheduling or completion transition.
        env.redfish_sim.set_job_state_sequence(vec![job_state]);

        // Confirm the exact guard for new requests would still reject this BF4 target before polling the retained job.
        assert!(matches!(
            db::host_boot_interface::boot_order_not_ready_reason(
                &env.pool,
                &mh.host().id,
                1,
                &target,
            )
            .await
            .expect("check unresolved BF4 report"),
            Some(
                db::host_boot_interface::HostBootInterfaceNotReady::Selection(
                    model::site_explorer::HostPrimaryInterfaceError::MissingHostSystem,
                )
            ),
        ));

        // Run the real controller once so its polling result is committed before reloading the host.
        env.run_machine_state_controller_iteration().await;

        // Prove polling advanced the persisted stage while preserving the accepted job and retry budget.
        let mut txn = env.db_txn().await;
        let advanced = mh.host().db_machine(&mut txn).await;
        assert_eq!(
            advanced.current_state(),
            &ManagedHostState::HostInit {
                machine_state: MachineState::SetBootOrder {
                    set_boot_order_info: Some(SetBootOrderInfo {
                        set_boot_order_jid: Some(TEST_BOOT_ORDER_JOB_ID.to_string()),
                        set_boot_order_state: expected_stage,
                        retry_count: 2,
                    }),
                },
            },
        );
        assert!(matches!(
            advanced.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Transition { .. })
        ));
        txn.rollback().await.expect("close continued job snapshot");
    }

    // Neither polling stage may publish a replacement boot-order job while completing the retained work.
    assert!(
        env.redfish_sim
            .actions_since(&checkpoint)
            .all_hosts()
            .iter()
            .all(|action| !matches!(action, RedfishSimAction::SetBootOrderDpuFirst { .. }))
    );
}

/// Keeps HostInit and new boot-order requests waiting after rejected BF4 host-PF drift, then resumes both after matching DPU-agent registration clears only its dedicated alert.
#[crate::sqlx_test]
async fn test_bf4_identity_drift_blocks_new_boot_work_until_matching_report(pool: sqlx::PgPool) {
    // Retain the fixture's accepted boot NIC and healthy network while adding complete BF4 registration.
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let target = register_existing_bf4_boot_target(&env, &mh).await;
    let dpu_id = mh.dpu_ids[0];
    let network_wait = ManagedHostState::DPUInit {
        dpu_states: model::machine::DpuInitStates {
            states: HashMap::from([(dpu_id, DpuInitState::WaitingForNetworkConfig)]),
        },
    };
    set_host_controller_state_stuck_in(&env, mh.host().id, &network_wait, 0).await;
    let mut txn = env.db_txn().await;
    db::host_boot_interface::reconcile_pre_host_init(&mut txn, &mh.host().id, 1, false, None)
        .await
        .expect("publish complete automatic boot facts");
    txn.commit().await.expect("commit boot readiness");
    let _ = mh.dpu().forge_agent_control().await;
    network_configured_with_health(&env, &dpu_id, None).await;

    // Keep an independent alert so accepting the original registration must preserve other health sources.
    let mut independent_health = HealthReport::empty("independent-health".to_string());
    independent_health.alerts.push(HealthProbeAlert {
        id: "IndependentAlert".parse().expect("probe ID"),
        target: Some(dpu_id.to_string()),
        in_alert_since: None,
        message: "preserve this alert".to_string(),
        tenant_message: None,
        classifications: vec![], // Keep this unrelated alert from introducing another lifecycle blocker.
    });
    env.api
        .insert_machine_health_report(Request::new(InsertMachineHealthReportRequest {
            machine_id: Some(dpu_id.into()),
            health_report_entry: Some(HealthReportEntry {
                report: Some(independent_health.into()),
                ..Default::default()
            }),
        }))
        .await
        .expect("publish independent alert");

    // Derive phone-home data from the accepted topology so only the registered boot MAC changes in the rejected report.
    let mut txn = env.db_txn().await;
    let accepted_dpu = mh.dpu().db_machine(&mut txn).await;
    let health_before_drift = accepted_dpu.health_reports.merges.clone();
    let caller_interface = accepted_dpu
        .status
        .interfaces
        .iter()
        .find(|interface| interface.interface_type == model::machine_interface::InterfaceType::Data)
        .expect("DPU discovery interface")
        .id;
    let topology = db::machine_topology::find_latest_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .expect("accepted DPU topology")
        .remove(&dpu_id)
        .expect("registered DPU");
    let accepted_inventory = topology.topology.discovery_data.info;
    let accepted_report =
        DiscoveryInfo::try_from(accepted_inventory.clone()).expect("accepted discovery report");
    let mut rejected_inventory = accepted_inventory.clone();
    rejected_inventory
        .dpu_info
        .as_mut()
        .expect("BF4 inventory")
        .host_pfs[0]
        .mac_address = MacAddress::new([2, 0, 0, 0, 0xfe, 20]); // Drift the immutable MAC without changing the stable DPU ID.
    txn.rollback()
        .await
        .expect("close accepted registration read");
    // Confirm retained accepted facts authorize both operations before the rejected report introduces the only blocker.
    assert!(
        db::host_boot_interface::not_ready_reason(&env.pool, &mh.host().id, 1)
            .await
            .expect("check complete HostInit facts")
            .is_none()
    );
    assert!(
        db::host_boot_interface::boot_order_not_ready_reason(&env.pool, &mh.host().id, 1, &target)
            .await
            .expect("check complete boot-order facts")
            .is_none()
    );

    // Reject the public DPU-agent report before running either controller path so its independently committed alert is exercised.
    let rejected = env
        .api
        .discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(caller_interface),
            discovery_data: Some(DiscoveryData::Info(
                DiscoveryInfo::try_from(rejected_inventory).expect("drift report"),
            )),
            discovery_reporter: rpc::forge::MachineDiscoveryReporter::DpuAgent as i32,
            ..Default::default()
        }))
        .await
        .expect_err("immutable registered MAC must be rejected");
    assert_eq!(rejected.code(), Code::FailedPrecondition);

    // Reload the dedicated alert and accepted inventory to prove discovery failed without removing the previously valid boot facts.
    let mut txn = env.db_txn().await;
    let rejected_dpu = mh.dpu().db_machine(&mut txn).await;
    assert!(
        rejected_dpu.health_reports.merges["dpu-host-pf"]
            .has_classification(&HealthAlertClassification::prevent_host_state_changes())
    );
    assert_eq!(
        rejected_dpu.health_reports.merges[HealthReport::DPU_AGENT_SOURCE],
        health_before_drift[HealthReport::DPU_AGENT_SOURCE]
    );
    let retained = db::machine_topology::find_latest_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .expect("reload rejected registration")
        .remove(&dpu_id)
        .expect("retained DPU");
    assert_eq!(
        retained
            .topology
            .discovery_data
            .info
            .dpu_info
            .expect("retained BF4")
            .host_pfs,
        accepted_inventory.dpu_info.expect("accepted BF4").host_pfs
    );
    txn.rollback().await.expect("close rejected discovery read");

    // Retain a new-request stage and nonzero retry budget to exercise a host already past the HostInit entry guard.
    let boot_order = ManagedHostState::HostInit {
        machine_state: MachineState::SetBootOrder {
            set_boot_order_info: Some(SetBootOrderInfo {
                set_boot_order_jid: None, // Exercise admission of new work rather than polling an accepted job.
                set_boot_order_state: SetBootOrderState::SetBootOrder,
                retry_count: 2, // Waiting must preserve the existing retry budget.
            }),
        },
    };
    // Keep BIOS configured and boot order incorrect so clearing drift must publish a new vendor request.
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);
    let checkpoint = env.redfish_sim.timepoint();
    for state in [
        // Healthy synchronized networking must not let rejected identity drift enter HostInit.
        &network_wait,
        // A host already past initialization must not publish a new boot-order job from retained accepted facts.
        &boot_order,
    ] {
        // Exercise each reachable state through a controller iteration rather than calling its readiness helper directly.
        set_host_controller_state_stuck_in(&env, mh.host().id, state, 0).await;
        env.run_machine_state_controller_iteration().await;

        // Reload the committed Wait and exact state so a skipped controller branch cannot pass as blocked boot work.
        let mut txn = env.db_txn().await;
        let waiting = mh.host().db_machine(&mut txn).await;
        assert_eq!(waiting.current_state(), state);
        assert!(matches!(&waiting.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason.contains("registered host-PF identity drift") && reason.contains(&dpu_id.to_string())));
        txn.rollback().await.expect("close blocked controller read");
    }
    // Neither blocked controller path may perform a recorded Redfish operation.
    assert!(
        env.redfish_sim
            .actions_since(&checkpoint)
            .all_hosts()
            .is_empty()
    );

    // Accept matching immutable identities through the same public API before expecting either controller path to resume.
    env.api
        .discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(caller_interface),
            discovery_data: Some(DiscoveryData::Info(accepted_report)),
            discovery_reporter: rpc::forge::MachineDiscoveryReporter::DpuAgent as i32,
            ..Default::default()
        }))
        .await
        .expect("accept original registered identities");

    // A fresh find must show that only the dedicated report cleared while healthy network and independent alerts survived.
    let mut txn = env.db_txn().await;
    let recovered_dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(recovered_dpu.health_reports.merges, health_before_drift);
    txn.rollback()
        .await
        .expect("close recovered registration read");

    for state in [
        // Clearing the dedicated alert releases the otherwise-ready DPUInit transition.
        &network_wait,
        // The same accepted inventory authorizes the new vendor job after the rejection is resolved.
        &boot_order,
    ] {
        // Refresh network convergence after changing the lifecycle state so the recovery checks isolate the cleared identity alert.
        set_host_controller_state_stuck_in(&env, mh.host().id, state, 0).await;
        network_configured_with_health(&env, &dpu_id, None).await;
        env.run_machine_state_controller_iteration().await;

        // Reload committed state to prove HostInit entry or publication of the new boot-order job actually advanced.
        let mut txn = env.db_txn().await;
        let resumed = mh.host().db_machine(&mut txn).await;
        if state == &network_wait {
            assert!(matches!(
                resumed.current_state(),
                ManagedHostState::HostInit {
                    machine_state: MachineState::EnableIpmiOverLan,
                }
            ));
        } else {
            assert!(
                matches!(resumed.current_state(), ManagedHostState::HostInit {
                machine_state: MachineState::SetBootOrder { set_boot_order_info: Some(info) },
            } if info.set_boot_order_state == SetBootOrderState::WaitForSetBootOrderJobScheduled
                && info.retry_count == 2)
            );
        }
        assert!(matches!(
            resumed.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Transition { .. })
        ));
        txn.rollback().await.expect("close resumed controller read");
    }
    // Confirm the recovered boot-order stage published the retained physical target to Redfish.
    assert!(env.redfish_sim.actions_since(&checkpoint).all_hosts().iter().any(|action| matches!(
        action,
        RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac } if boot_interface_mac == &target.mac_address().to_string()
    )));
}

/// Starts HostInit and publishes the declared BF4 boot target before that integrated NIC leases, preventing boot readiness from waiting on the boot it must enable.
#[crate::sqlx_test]
async fn test_bf4_declared_primary_prediction_starts_initial_boot_without_dhcp(pool: sqlx::PgPool) {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};

    // Only the integrated NIC is unleased; the ordinary fixture and registration helper supply the remaining BF4 boot evidence.
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let registered = register_existing_bf4_boot_target(&env, &mh).await;
    let dpu_id = mh.dpu_ids[0];
    let declared_mac = MacAddress::new([2, 0, 0, 0, 0xfe, 10]);
    assert_ne!(declared_mac, registered.mac_address());
    let declared =
        MachineBootInterfaceTarget::Pair(model::machine_boot_interface::MachineBootInterface {
            mac_address: declared_mac,
            interface_id: "Integrated.Pending".to_string(),
        });
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    let bmc_mac = host.status.bmc_info.mac.expect("host BMC MAC");
    let bmc_ip = host.status.bmc_info.ip.expect("host BMC IP");
    let mut expected = db::expected_machine::find_by_bmc_mac_address(txn.as_mut(), bmc_mac)
        .await
        .expect("find ExpectedMachine")
        .expect("expected host");
    expected
        .data
        .interfaces
        .push(model::expected_machine::ExpectedInterface {
            mac_address: Some(declared_mac),
            role: model::expected_machine::ExpectedInterfaceRole::Host,
            primary: Some(true), // Declare boot intent while preserving the leased DPU primary.
            ..Default::default()
        });
    db::expected_machine::update(&mut txn, &expected)
        .await
        .expect("declare integrated primary");
    // Publish the current Redfish pair and the pending prediction in the same state used by ingestion before DHCP.
    let mut report = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![bmc_ip])
        .await
        .expect("host endpoint")
        .remove(0)
        .report;
    report.systems[0]
        .ethernet_interfaces
        .push(model::site_explorer::EthernetInterface {
            mac_address: Some(declared_mac),
            id: Some("Integrated.Pending".to_string()),
            ..Default::default()
        });
    sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
        .bind(bmc_ip)
        .bind(sqlx::types::Json(&report))
        .execute(txn.as_mut())
        .await
        .expect("publish declared boot evidence");
    let host_machine_id = MachineId::from(mh.host().id);
    let prediction = db::predicted_machine_interface::create(
        model::predicted_machine_interface::NewPredictedMachineInterface {
            machine_id: &host_machine_id, // Bind pending boot intent to this host until DHCP.
            mac_address: declared_mac,
            expected_network_segment_type: model::network_segment::NetworkSegmentType::HostInband, // Expect a HostInband relay during DHCP promotion.
            boot_interface_id: Some("Integrated.Pending"), // Require the same current Redfish ID at readiness.
            primary_interface: true, // Carry the declared primary into the first real interface.
        },
        &mut txn,
    )
    .await
    .expect("record pending declared primary");
    db::machine::update_state(
        &mut txn,
        &mh.host().id,
        &ManagedHostState::DPUInit {
            dpu_states: model::machine::DpuInitStates {
                states: HashMap::from([(dpu_id, DpuInitState::WaitingForNetworkConfig)]),
            },
        },
    )
    .await
    .expect("return host to initial DPU readiness");
    txn.commit().await.expect("commit pending declared primary");

    // Commit registered-link repair before testing readiness so desired intent and every DPU association are current.
    let mut repair = env.db_txn().await;
    db::host_boot_interface::reconcile_pre_host_init(&mut repair, &mh.host().id, 1, false, None)
        .await
        .expect("reconcile declared prediction before DHCP");
    repair
        .commit()
        .await
        .expect("publish pending declared boot intent");

    check_cases_async(
        [
            // Another machine's prediction cannot authorize this host's declared boot NIC even when its MAC and ID agree.
            Case {
                scenario: "foreign prediction",
                input: (MachineId::from(dpu_id), "Integrated.Pending"),
                expect: Yields((true, true)),
            },
            // A prediction for this host still needs the current Redfish ID before it can replace a real-primary proof.
            Case {
                scenario: "stale predicted ID",
                input: (host_machine_id, "Integrated.Stale"),
                expect: Yields((true, true)),
            },
        ],
        |(prediction_machine_id, boot_id)| {
            let env = &env;
            let mh = &mh;
            let prediction_id = prediction.id;
            async move {
                // Change only the named prediction fact while leaving the declaration, desired pair, and registered DPU links intact.
                sqlx::query("UPDATE predicted_machine_interfaces SET machine_id = $2, boot_interface_id = $3 WHERE id = $1")
                    .bind(prediction_id).bind(prediction_machine_id).bind(boot_id)
                    .execute(&env.pool).await.map_err(|error| error.to_string())?;
                // Attempt repair against the altered prediction without retaining any writes.
                let mut repair = env.db_txn().await;
                let repair_rejected = matches!(
                    db::host_boot_interface::reconcile_pre_host_init(
                        &mut repair, &mh.host().id, 1, false, None,
                    ).await,
                    Err(db::DatabaseError::FailedPrecondition(message))
                    if message.contains("has no unique owned interface")
                );
                repair.rollback().await.map_err(|error| error.to_string())?;
                // Readiness must independently reject the same invalid proof after repair rolls back.
                let readiness_rejected = matches!(
                    db::host_boot_interface::not_ready_reason(&env.pool, &mh.host().id, 1)
                        .await.map_err(|error| error.to_string())?,
                    Some(db::host_boot_interface::HostBootInterfaceNotReady::Primary(mac))
                    if mac == declared_mac
                );
                Ok::<_, String>((repair_rejected, readiness_rejected))
            }
        },
    ).await;
    // Restore exact persisted prediction evidence before exercising controller admission and boot publication.
    sqlx::query("UPDATE predicted_machine_interfaces SET machine_id = $2, boot_interface_id = $3 WHERE id = $1")
        .bind(prediction.id).bind(host_machine_id).bind("Integrated.Pending")
        .execute(&env.pool).await.expect("restore declared prediction");
    // Refresh the DPU's network observation after repair before the controller evaluates initial readiness.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration().await;

    // Reload the controller outcome to prove initial admission progressed while the declared NIC still has no DHCP row.
    let mut txn = env.db_txn().await;
    let initialized = mh.host().db_machine(&mut txn).await;
    assert!(matches!(
        initialized.current_state(),
        ManagedHostState::HostInit {
            machine_state: MachineState::EnableIpmiOverLan,
        }
    ));
    assert!(matches!(
        initialized.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    assert_eq!(
        initialized
            .config
            .desired_boot_interface
            .as_ref()
            .expect("declared target")
            .value,
        declared
    );
    assert!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), declared_mac)
            .await
            .expect("find declared DHCP row")
            .is_empty()
    );
    txn.rollback().await.expect("close initial readiness read");

    // Enter boot-order work without a captured target so ordinary production resolution must use the persisted declared pair.
    set_host_stuck_in_set_boot_order(&env, mh.host().id).await;
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);
    let checkpoint = env.redfish_sim.timepoint();
    env.run_machine_state_controller_iteration().await;

    // Verify both persisted scheduling progress and the actual Redfish target without materializing the pending NIC.
    let mut txn = env.db_txn().await;
    let published = mh.host().db_machine(&mut txn).await;
    assert!(
        matches!(published.current_state(), ManagedHostState::HostInit {
        machine_state: MachineState::SetBootOrder { set_boot_order_info: Some(info) },
    } if info.set_boot_order_state == SetBootOrderState::WaitForSetBootOrderJobScheduled)
    );
    assert!(matches!(
        published.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    assert!(
        db::machine_interface::find_by_mac_address(txn.as_mut(), declared_mac)
            .await
            .expect("find pending DHCP row")
            .is_empty()
    );
    let preserved = db::predicted_machine_interface::find_by_mac_address(&mut txn, declared_mac)
        .await
        .expect("find pending prediction")
        .expect("prediction must await DHCP");
    assert_eq!(preserved.id, prediction.id);
    assert_eq!(preserved.machine_id, host_machine_id);
    assert!(preserved.primary_interface);
    txn.rollback().await.expect("close published boot read");
    // Confirm Redfish received the declared MAC rather than the interim networking primary.
    assert!(env.redfish_sim.actions_since(&checkpoint).all_hosts().iter().any(|action| matches!(
        action,
        RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac } if boot_interface_mac == &declared_mac.to_string()
    )));
}
