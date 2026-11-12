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

use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_machine_controller::metrics::MachineMetrics;
use carbide_uuid::machine::{HostMachineId, MachineInterfaceId};
use futures::FutureExt;
use mac_address::MacAddress;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::hardware_info::HardwareInfo;
use model::machine::{DpuInitState, ManagedHostState};
use model::machine_boot_interface::{BootInterfaceSelectionSource, MachineBootInterfaceTarget};
use rpc::forge::forge_server::Forge;
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::io::StateControllerIO;
use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerOutcome};
use tonic::{Code, Request};

use super::common::api_fixtures::dpu::dpu_discover_dhcp;
use super::common::api_fixtures::test_managed_host::TestPredictedManagedHost;
use super::common::api_fixtures::{TestEnv, create_test_env, network_configured_with_health};

/// Registers a BF4 report through the public DpuAgent registration path so bootstrap and PF persistence remain part of the wiring proof.
async fn register_facts(
    env: &TestEnv,
    interface_id: MachineInterfaceId,
    hardware_info: HardwareInfo,
) -> Result<(), tonic::Status> {
    env.api
        .discover_machine(Request::new(rpc::MachineDiscoveryInfo {
            machine_interface_id: Some(interface_id),
            discovery_data: Some(rpc::DiscoveryData::Info(
                rpc::DiscoveryInfo::try_from(hardware_info).expect("wire hardware"),
            )),
            create_machine: true,
            discovery_reporter: rpc::MachineDiscoveryReporter::DpuAgent as i32,
            ..Default::default()
        }))
        .await
        .map(drop)
}

/// Associates checkpoint reports in either discovery order before controller finalization, keeping DMI identity stable and factual PF MACs distinct from bootstrap networking.
async fn explore_checkpoint_host<'a>(
    env: &'a TestEnv,
    agent_first: bool,
    declared_primary: Option<MacAddress>,
) -> (
    super::common::api_fixtures::site_explorer::MockExploredHost<'a>,
    HardwareInfo,
) {
    let mut config = env.managed_host_config();
    if let Some(mac_address) = declared_primary {
        // Keep this NIC unleased so boot selection must use the declared primary prediction.
        config.non_dpu_macs.push(mac_address);
        config
            .expected_machine_data
            .get_or_insert_with(Default::default)
            .interfaces
            .push(model::expected_machine::ExpectedInterface {
                mac_address: Some(mac_address),
                primary: Some(true),
                ..Default::default()
            });
    }
    let dpu = config.get_and_assert_single_dpu();
    let mut hardware_info = HardwareInfo::from(dpu);
    let dpu_info = hardware_info.dpu_info.as_mut().expect("DPU hardware");
    dpu_info.part_number = "900-9D4B4-CWAA-TSA".to_string(); // VPD supplies late BF4 classification without changing the DMI-derived ID.
    dpu_info.host_pfs = vec![model::hardware_info::DpuHostPf {
        pci_controller_id: env.config.dpu_host_pf_pci_controller_id,
        pf_id: 0,
        mac_address: "02:00:00:fa:00:01".parse().expect("corrected PF MAC"),
    }];
    if agent_first {
        // Let the API create provisional host networking before Explorer supplies host and DPU reports.
        let interface_id = dpu_discover_dhcp(env, &dpu.oob_mac_address.to_string()).await;
        register_facts(env, interface_id, hardware_info.clone())
            .await
            .expect("register BF4 facts");
    }
    // Register the declaration before Explorer receives host inventory and DPU reports.
    super::common::api_fixtures::site_explorer::register_expected_machine(env, &config, None).await;
    let mut explored =
        super::common::api_fixtures::site_explorer::MockExploredHost::new(env, config.clone())
            .discover_dhcp_dpu_bmc(0, |_, _| Ok(()))
            .await
            .expect("DPU BMC lease")
            .discover_dhcp_dpu_primary_iface(0)
            .await
            .discover_dhcp_host_bmc(|_, _| Ok(()))
            .boxed()
            .await
            .expect("host BMC lease")
            .insert_site_exploration_results()
            .expect("exploration reports");
    if declared_primary.is_some() {
        let mut report = model::site_explorer::EndpointExplorationReport::from(dpu.clone());
        report
            .chassis
            .iter_mut()
            .find(|chassis| chassis.id == "Card1")
            .expect("DPU product chassis")
            .part_number = None; // The DPU BMC omits BF4 part evidence; only the host inventory may establish it.
        let serial = report
            .dpu_pairing_serial_number()
            .expect("DPU pairing serial");
        let mut host_report = model::site_explorer::EndpointExplorationReport::from(config.clone());
        host_report
            .systems
            .iter_mut()
            .flat_map(|system| &mut system.pcie_devices)
            .find(|device| device.serial_number.as_deref().map(str::trim) == Some(serial))
            .expect("serial-matched host inventory")
            .part_number = Some("900-9D4B4-CWAA-TSA".to_string());
        assert!(
            !report
                .systems
                .first()
                .and_then(|system| system.model.as_deref())
                .is_some_and(|model| carbide_utils::is_bf4_dpu("", model))
        );
        env.endpoint_explorer.insert_endpoints(vec![
            (explored.dpu_bmc_ips[&0], report),
            (explored.host_bmc_ip.expect("host BMC"), host_report),
        ]);
    }
    // Run ordinary exploration and preingestion after publishing the report, before controller setup.
    explored = explored
        .run_site_explorer_iteration()
        .boxed()
        .await
        .mark_preingestion_complete()
        .boxed()
        .await
        .expect("preingestion")
        .run_site_explorer_iteration()
        .boxed()
        .await;
    (explored, hardware_info)
}

/// Advances an explored checkpoint fixture to its initial network acknowledgement so repair tests can distinguish a fresh required ACK from the provisional one.
async fn checkpoint_host(
    env: &TestEnv,
    agent_first: bool,
    declared_primary: Option<MacAddress>,
) -> (TestPredictedManagedHost, HardwareInfo, MachineInterfaceId) {
    let (explored, hardware_info) =
        explore_checkpoint_host(env, agent_first, declared_primary).await;
    let dpu = explored.managed_host.get_and_assert_single_dpu().clone();
    let explored = explored
        .dpu_state_controller_iterations_to_network_install()
        .boxed()
        .await;
    let dpu_id = explored.dpu_machine_ids[&0];
    let mut read = env.db_txn().await;
    let host_id = db::machine::find_host_by_dpu_machine_id(&mut read, &dpu_id)
        .await
        .expect("host lookup")
        .expect("paired host")
        .id;
    read.rollback().await.expect("close fixture read");
    let host = TestPredictedManagedHost {
        id: host_id.try_into().expect("predicted host"),
        dpu_ids: vec![dpu_id],
        api: env.api.clone(),
    };
    if !agent_first {
        // Model VPD arriving before the required authenticated PF report while preserving unrelated topology fields.
        let mut setup = env.db_txn().await;
        let mut topology =
            db::machine_topology::find_latest_by_machine_ids(&mut setup, &[host.dpu_ids[0]])
                .await
                .expect("topology")
                .remove(&host.dpu_ids[0])
                .expect("DPU topology");
        let mut partial = hardware_info.clone();
        partial
            .dpu_info
            .as_mut()
            .expect("DPU hardware")
            .host_pfs
            .clear();
        topology.topology.discovery_data.info = partial;
        sqlx::query("UPDATE machine_topologies SET topology = $2 WHERE machine_id = $1")
            .bind(host.dpu_ids[0])
            .bind(sqlx::types::Json(&topology.topology))
            .execute(setup.as_mut())
            .await
            .expect("late classification");
        setup.commit().await.expect("commit late classification");
    }
    let interface_id = dpu_discover_dhcp(env, &dpu.oob_mac_address.to_string()).await;
    host.dpu().forge_agent_control().await;
    network_configured_with_health(env, &host.dpu_ids[0], None).await;
    (host, hardware_info, interface_id)
}

/// Repairs Explorer-first bootstrap only after factual PF registration, rejects the old ACK, and resumes on ordinary controller sweeps without another discovery event.
///
/// Controller-7 selection rejects controller-1-only discovery and retains c7pf0 through finalization, exposing a hardcoded default in either registration or controller configuration.
#[crate::sqlx_test]
async fn explorer_first_checkpoint_waits_repairs_and_requires_new_ack(pool: sqlx::PgPool) {
    // Select controller 7 while the common fixture provides ordinary pairing, provisional networking and an initial ACK.
    let mut config = super::common::api_fixtures::get_config();
    config.dpu_host_pf_pci_controller_id = 7;
    let env = super::common::api_fixtures::create_test_env_with_overrides(
        pool,
        super::common::api_fixtures::TestEnvOverrides::with_config(config),
    )
    .await;
    let (host, mut hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU").host_pfs[0].mac_address;

    // Observe the old networking version and persisted missing-facts wait after the first checkpoint pass.
    let mut read = env.db_txn().await;
    let before = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let waiting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(
        matches!(&waiting.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("c7pf0") && reason.contains(&host.dpu_ids[0].to_string()))
    );
    assert_eq!(
        waiting.network_config.version,
        before.network_config.version
    );

    // Reject a report that supplies only the default controller before accepting competing c1pf0 and c7pf0 facts.
    let default_pf = model::hardware_info::DpuHostPf {
        pci_controller_id: 1, // A registration hardcoded to the default would incorrectly accept this report.
        pf_id: 0,
        mac_address: "02:00:00:fa:00:02"
            .parse()
            .expect("default-controller PF MAC"),
    };
    let mut default_only = hardware_info.clone();
    default_only.dpu_info.as_mut().expect("DPU").host_pfs = vec![default_pf.clone()];
    let rejected = register_facts(&env, interface_id, default_only)
        .await
        .expect_err("configured controller 7 must be present");
    // Require registration to identify the configured PF rather than fail for an unrelated discovery condition.
    assert_eq!(rejected.code(), Code::FailedPrecondition);
    assert!(rejected.message().contains("c7pf0"));

    // Include both observations so finalization must select c7pf0 over the default controller.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU")
        .host_pfs
        .push(default_pf);

    // Submit fresh authenticated facts and run one checkpoint pass, retaining the repair's immediate Wait outcome.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(
        matches!(&repaired.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("finalized") && reason.contains("reload"))
    );
    assert_ne!(
        repaired.network_config.version,
        before.network_config.version
    );
    let found = host.host().rpc_machine().await;
    let status = found.status.expect("persisted host status");
    let primary = status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("primary");
    assert_eq!(primary.mac_address, corrected_mac.to_string());
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
    assert_eq!(
        db::machine_desired_boot_interface::get(&env.pool, &host.id.into())
            .await
            .expect("desired")
            .expect("initialized target")
            .value,
        MachineBootInterfaceTarget::MacOnly(corrected_mac)
    );

    // A later sweep must inspect persisted facts and the new version instead of admitting the pre-repair ACK.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let unacknowledged = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(
        matches!(&unacknowledged.state.value, ManagedHostState::DPUInit { dpu_states }
        if dpu_states.states.values().all(|state| matches!(state, DpuInitState::WaitingForNetworkConfig)))
    );
    assert_eq!(
        unacknowledged.network_config.version,
        repaired.network_config.version
    );
    assert!(
        matches!(&unacknowledged.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("current network config version"))
    );

    // Report the repaired version through the public network-status API, then admit HostInit on the next sweep.
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit {
            machine_state: model::machine::MachineState::EnableIpmiOverLan
        }
    ));
    assert_eq!(
        admitted.network_config.version,
        repaired.network_config.version
    );
}

/// Holds a concurrent network writer behind the production handler's returned transaction until HostInit publication commits, preventing ACK validation from racing a later network version.
#[crate::sqlx_test]
async fn checkpoint_handler_retains_network_fence_until_publication(pool: sqlx::PgPool) {
    // Settle corrected PF networking and acknowledge it before calling the handler directly.
    let env = create_test_env(pool).await;
    let (host, hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    let host_id: HostMachineId = host.id.into();
    let controller_io = MachineStateControllerIO {
        host_health: env.config.host_health,
        sla_config: model::machine::slas::MachineSlaConfig::new(
            env.config.machine_state_controller.failure_retry_time,
        ),
    };
    let mut read = env.db_txn().await;
    let mut snapshot = controller_io
        .load_object_state(read.as_mut(), &host_id)
        .await
        .expect("load acknowledged host")
        .expect("host snapshot");
    read.rollback().await.expect("close snapshot read");
    let controller_state = snapshot.host_snapshot.state.clone();
    let acknowledged_network = snapshot.host_snapshot.network_config.clone();
    let acknowledged_network_version = acknowledged_network.version;

    // Retain the exact transaction supplied by the production handler instead of constructing a fence in the test.
    let mut handler_services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut handler_services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };
    let mut outcome = env
        .machine_state_handler
        .handle_object_state(&host_id, &mut snapshot, &controller_state.value, &mut ctx)
        .await
        .expect("admit acknowledged host");
    let next = ManagedHostState::HostInit {
        machine_state: model::machine::MachineState::EnableIpmiOverLan,
    };
    assert!(
        matches!(&outcome, StateHandlerOutcome::Transition { next_state, .. } if next_state == &next)
    );
    let mut publication = outcome
        .take_transaction()
        .expect("handler must return its publication fence");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(publication.as_mut())
        .await
        .expect("publication backend");

    // Attempt a real network-version update while the returned transaction still holds the validated host row.
    let writer_pool = env.pool.clone();
    let writer = tokio::spawn(async move {
        let mut txn = writer_pool.begin().await.expect("network writer");
        let result = db::machine::try_update_network_config(
            &mut txn,
            host_id.as_machine_id(),
            acknowledged_network.version,
            &acknowledged_network.value,
        )
        .await
        .expect("network update");
        txn.commit().await.expect("commit network update");
        result
    });
    db::test_support::postgres::wait_for_blocked_query(
        &env.pool,
        blocker_pid,
        "network_config_version",
    )
    .await;

    // Apply queued writes and publish through the returned transaction, matching the controller processor's commit order.
    let publication_version = controller_state.version.increment();
    pending_db_writes
        .apply_all(&mut publication)
        .await
        .expect("apply handler writes");
    assert_eq!(
        controller_io
            .persist_controller_state(
                publication.as_mut(),
                &host_id,
                controller_state.version,
                publication_version,
                &next,
            )
            .await
            .expect("publish HostInit"),
        db::ConditionalWrite::Applied(())
    );
    assert_eq!(
        db::test_support::postgres::count_blocked_queries(
            &env.pool,
            blocker_pid,
            "network_config_version",
        )
        .await,
        1
    );
    assert!(!writer.is_finished());
    publication
        .commit()
        .await
        .expect("commit HostInit publication");
    assert_eq!(
        writer.await.expect("writer task"),
        db::ConditionalWrite::Applied(())
    );

    // Reload both versions to prove state publication succeeded before the blocked writer advanced networking.
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    assert_eq!(admitted.state.value, next);
    assert_eq!(admitted.state.version, publication_version);
    assert_eq!(
        admitted.network_config.version.version_nr(),
        acknowledged_network_version.version_nr() + 1
    );
    read.rollback().await.expect("close publication read");
}

/// Keeps BF4 initialization in DPUInit when a blocking DPU health report commits after the repair snapshot, then admits HostInit after recovery so publication cannot trust stale health.
#[crate::sqlx_test]
async fn checkpoint_reloads_dpu_health_before_publication(pool: sqlx::PgPool) {
    // Settle corrected PF networking and acknowledge it with a healthy report before the publication attempt.
    let env = create_test_env(pool).await;
    let (host, hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let dpu_id = host.dpu_ids[0];
    network_configured_with_health(&env, &dpu_id, None).await;

    // Find the settled state and clean report so a stale ACK or earlier alert cannot explain the later wait.
    let mut read = env.db_txn().await;
    let acknowledged = host.snapshot(&mut read).await;
    read.rollback().await.expect("close acknowledged read");
    assert!(
        matches!(&acknowledged.managed_state, ManagedHostState::DPUInit { dpu_states }
        if dpu_states.states.get(&dpu_id) == Some(&DpuInitState::WaitingForNetworkConfig))
    );
    assert!(acknowledged.managed_host_network_config_version_synced());
    assert!(
        acknowledged.dpu_snapshots[0]
            .dpu_agent_health_report()
            .expect("initial DPU health report")
            .alerts
            .is_empty()
    );
    let controller_state = acknowledged.host_snapshot.state;
    let network_version = acknowledged.host_snapshot.network_config.version;

    // Hold the machine_state_history advisory lock so repair can commit before publication waits without locking machine rows.
    let mut publication_gate = env.db_txn().await;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 'machine_state_history'::regclass::bigint))")
        .bind(host.id.to_string())
        .execute(publication_gate.as_mut())
        .await
        .expect("hold HostInit publication");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(publication_gate.as_mut())
        .await
        .expect("publication gate backend");

    // Commit the changed report after the repair snapshot and before the publication reload, bounding unexpected lock waits.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(
            env.run_machine_state_controller_iteration_without_requeue(),
            async {
                db::test_support::postgres::wait_for_blocked_query(
                    &env.pool,
                    blocker_pid,
                    "machine_state_history",
                )
                .await;

                // Preserve the matching network ACK while adding the lifecycle-blocking alert through the public API.
                let blocking_health = rpc::health::HealthReport {
                    source: "forge-dpu-agent".to_string(),
                    alerts: vec![rpc::health::HealthProbeAlert {
                        id: "ContainerExists".to_string(),
                        message: "HBN container not found".to_string(),
                        classifications: vec![
                            health_report::HealthAlertClassification::prevent_host_state_changes()
                                .to_string(), // This classification must prevent HostInit even with a current ACK.
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                network_configured_with_health(&env, &dpu_id, Some(blocking_health)).await;

                // Find the committed alert while publication is blocked and prove neither host version changed.
                let mut read = env.db_txn().await;
                let reported = host.snapshot(&mut read).await;
                read.rollback().await.expect("close changed health read");
                assert_eq!(
                    reported.host_snapshot.state.version,
                    controller_state.version
                );
                assert_eq!(
                    reported.host_snapshot.network_config.version,
                    network_version
                );
                assert!(reported.managed_host_network_config_version_synced());
                assert!(
                    reported.dpu_snapshots[0]
                        .dpu_agent_health_report()
                        .expect("committed blocking report")
                        .has_classification(
                            &health_report::HealthAlertClassification::prevent_host_state_changes()
                        )
                );
                // Release publication only after finding the committed alert so its reload must observe the changed health.
                publication_gate
                    .rollback()
                    .await
                    .expect("release HostInit publication");
            }
        );
    })
    .await
    .expect("publication reload and health reporting must complete");

    // Find the controller's persisted health wait so a generic busy wait cannot stand in for the publication reload.
    let mut read = env.db_txn().await;
    let waiting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close health wait read");
    assert_eq!(waiting.state.value, controller_state.value);
    assert_eq!(waiting.state.version, controller_state.version);
    assert_eq!(waiting.network_config.version, network_version);
    assert!(
        matches!(&waiting.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, source_ref: Some(source_ref) })
        if reason.contains(&dpu_id.to_string())
            && reason.contains("check the DPU health alerts")
            && source_ref.file.ends_with("/handler/host_pfs.rs"))
    );

    // Clear the reported alert and retry through the ordinary sweep to prove healthy recovery needs no networking change.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close recovered host read");
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit {
            machine_state: model::machine::MachineState::EnableIpmiOverLan
        }
    ));
    assert_eq!(admitted.network_config.version, network_version);
}

/// Excludes an expired retained ID from PF repair so HostInit cannot resurrect an obsolete Pair before Explorer's next sweep.
#[crate::sqlx_test]
async fn checkpoint_respects_retained_boot_interface_expiry(pool: sqlx::PgPool) {
    // Configure a finite window while the usual checkpoint fixture supplies a corrected MAC with no current report ID.
    let mut config = super::common::api_fixtures::get_config();
    config.retained_boot_interface_window = Some(chrono::Duration::hours(1));
    let env = super::common::api_fixtures::create_test_env_with_overrides(
        pool,
        super::common::api_fixtures::TestEnvOverrides::with_config(config),
    )
    .await;
    let (host, hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU").host_pfs[0].mac_address;
    let mut setup = env.db_txn().await;
    db::retained_boot_interface::upsert(&mut setup, corrected_mac, "NIC.Obsolete.1")
        .await
        .expect("retain old ID");
    sqlx::query("UPDATE retained_boot_interfaces SET recorded_at = NOW() - INTERVAL '2 hours' WHERE mac_address = $1")
        .bind(corrected_mac)
        .execute(setup.as_mut())
        .await.expect("backdate retained ID beyond policy");
    assert_eq!(
        db::retained_boot_interface::find_by_mac(&mut setup, corrected_mac, None)
            .await
            .expect("retained record before repair")
            .as_deref(),
        Some("NIC.Obsolete.1"),
    );
    setup.commit().await.expect("commit expired observation");

    // Repair and acknowledge through the real controller without running an Explorer expiry sweep.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload the desired target and corrected row, then prove boot resolution remains MacOnly.
    let mut read = env.db_txn().await;
    let snapshot = host.snapshot(&mut read).await;
    assert!(matches!(
        snapshot.managed_state,
        ManagedHostState::HostInit { .. }
    ));
    assert_eq!(
        snapshot
            .host_snapshot
            .config
            .desired_boot_interface
            .as_ref()
            .expect("desired boot target")
            .value,
        MachineBootInterfaceTarget::MacOnly(corrected_mac)
    );
    let corrected = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|row| row.mac_address == corrected_mac)
        .expect("corrected PF row");
    assert!(corrected.boot_interface_id.is_none());
    assert_eq!(
        carbide_machine_controller::boot_interface::boot_interface_target(&snapshot, &[]),
        Some(carbide_redfish::boot_interface::BootInterfaceTarget::MacOnly(corrected_mac)),
    );
    assert!(
        db::retained_boot_interface::find_by_mac(&mut read, corrected_mac, None)
            .await
            .expect("consumed retained record")
            .is_none(),
    );
    read.rollback().await.expect("close read");
}

/// Applies the same checkpoint after API-first bootstrap and later Explorer association so discovery order cannot restore the provisional factory-MAC target.
#[crate::sqlx_test]
async fn api_first_checkpoint_uses_registered_pf_after_exploration(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host, hardware_info, _) = checkpoint_host(&env, true, None).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU").host_pfs[0].mac_address;

    // Process only the checkpoint so the provisional owned link cannot authorize immediate HostInit.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");

    // Find the host again through the public API and prove the registered PF replaced the factory bootstrap link.
    assert!(matches!(
        repaired.state.value,
        ManagedHostState::DPUInit { .. }
    ));
    assert!(matches!(
        repaired.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { .. })
    ));
    let found = host.host().rpc_machine().await;
    let primary = found
        .status
        .expect("host status")
        .interfaces
        .into_iter()
        .find(|interface| interface.primary_interface)
        .expect("primary");
    assert_eq!(primary.mac_address, corrected_mac.to_string());
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
}

/// Rejects both implicit admin boot paths with absent or automatic intent during pending BF4 initialization while explicit MAC selection and no-MAC reapply preserve independent Operator intent.
#[crate::sqlx_test]
async fn pending_admin_boot_requires_explicit_intent(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (host, mut hardware_info, interface_id) = checkpoint_host(&env, false, None).await;
    // Persist authenticated BF4 classification even though selected PF acquisition still needs a retry.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("DPU hardware")
        .host_pfs
        .clear();
    let pending = register_facts(&env, interface_id, hardware_info)
        .await
        .expect_err("missing selected PF");
    assert_eq!(pending.code(), Code::FailedPrecondition);
    // Preserve the preclassification automatic target for the first pair of requests.
    let mut read = env.db_txn().await;
    let automatic = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close automatic read");
    assert!(automatic.config.desired_boot_interface.is_some());
    assert!(!matches!(
        automatic
            .config
            .boot_interface_selection
            .expect("automatic source")
            .source,
        BootInterfaceSelectionSource::Operator | BootInterfaceSelectionSource::ExpectedMachine
    ));

    for (scenario, keep_automatic) in [
        // Existing provisional intent must not authorize either implicit admin operation.
        ("existing automatic provisional intent", true),
        // Removing intent retains the original fallback-rejection and Explorer deferral proof.
        ("absent intent after Explorer refresh", false),
    ] {
        if !keep_automatic {
            // Refresh older Explorer hardware after deletion so it cannot republish a provisional target.
            sqlx::query("DELETE FROM machine_boot_interfaces WHERE machine_id = $1")
                .bind(host.id)
                .execute(&env.pool)
                .await
                .expect("unset provisional intent");
            env.run_site_explorer_iteration().await;
        }
        let mut read = env.db_txn().await;
        let before = host.host().db_machine(&mut read).await;
        read.rollback().await.expect("close before read");
        assert_eq!(
            before.config.desired_boot_interface.is_some(),
            keep_automatic,
            "{scenario}"
        );
        let timepoint = env.redfish_sim.timepoint();

        for (command, setup) in [
            // MachineSetup must not fall through to raw managed BIOS work.
            ("MachineSetup", true),
            // Boot-order-only requests must not promote automatic intent to Operator authority.
            ("SetDpuFirstBootOrder", false),
        ] {
            // Exercise the public no-MAC command while the selected fixture's desired state remains persisted.
            let result = if setup {
                env.api
                    .machine_setup(Request::new(rpc::forge::MachineSetupRequest {
                        machine_id: Some(host.id.to_string()),
                        bmc_endpoint_request: None,
                        boot_interface_mac: None,
                    }))
                    .await
                    .map(drop)
            } else {
                env.api
                    .set_dpu_first_boot_order(Request::new(
                        rpc::forge::SetDpuFirstBootOrderRequest {
                            machine_id: Some(host.id.to_string()),
                            bmc_endpoint_request: None,
                            boot_interface_mac: None,
                        },
                    ))
                    .await
                    .map(drop)
            };
            let error = result.expect_err("initialization is pending");
            assert_eq!(
                error.code(),
                Code::FailedPrecondition,
                "{scenario}: {command}"
            );
            assert!(error.message().contains("initialization is pending"));

            // Reload the desired target/version, source and decision time after each rejection to detect premature publication.
            let mut read = env.db_txn().await;
            let after = host.host().db_machine(&mut read).await;
            read.rollback().await.expect("close after read");
            assert_eq!(
                after
                    .config
                    .desired_boot_interface
                    .as_ref()
                    .map(|desired| (&desired.value, desired.version)),
                before
                    .config
                    .desired_boot_interface
                    .as_ref()
                    .map(|desired| (&desired.value, desired.version)),
                "{scenario}: {command}"
            );
            assert_eq!(
                after.config.boot_interface_selection, before.config.boot_interface_selection,
                "{scenario}: {command}"
            );
        }
        assert!(
            env.redfish_sim
                .actions_since(&timepoint)
                .all_hosts()
                .is_empty(),
            "{scenario}"
        );
    }

    // Explicit intent can be recorded before PF acquisition, and omission later reapplies that same selection.
    let explicit_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 9]);
    env.api
        .set_dpu_first_boot_order(Request::new(rpc::forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host.id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(explicit_mac.to_string()),
        }))
        .await
        .expect("explicit boot target");
    let mut read = env.db_txn().await;
    let selected = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    env.api
        .machine_setup(Request::new(rpc::forge::MachineSetupRequest {
            machine_id: Some(host.id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await
        .expect("reapply explicit intent");
    let mut read = env.db_txn().await;
    let reapplied = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");

    // Find persisted selection metadata so reapply cannot silently replace Operator provenance or its timestamp.
    assert_eq!(
        selected
            .config
            .boot_interface_selection
            .expect("selected source")
            .source,
        BootInterfaceSelectionSource::Operator
    );
    assert_eq!(
        reapplied.config.boot_interface_selection,
        selected.config.boot_interface_selection
    );
    assert_eq!(
        reapplied
            .config
            .desired_boot_interface
            .expect("desired")
            .value,
        MachineBootInterfaceTarget::MacOnly(explicit_mac)
    );
}

/// Boots a declared NIC before its first lease and keeps DPU networking selected when DHCP later adopts it, preventing boot predictions from becoming networking authority.
#[crate::sqlx_test]
async fn declared_integrated_nic_boots_before_its_first_lease(pool: sqlx::PgPool) {
    let env = super::common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let declared_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 8]);
    let (host, hardware_info, interface_id) =
        checkpoint_host(&env, false, Some(declared_mac)).await;

    // A declaration cannot bypass managed-DPU readiness while authenticated PF facts are missing.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let waiting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(
        matches!(waiting.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains("c1pf0"))
    );

    // Finalize registered PF networking and acknowledge it before the separate HostInit admission pass.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload persisted prediction and controller state; no DHCP-created row may be needed to target the declared NIC.
    let mut read = env.db_txn().await;
    let host_id: carbide_uuid::machine::HostMachineId = host.id.into();
    let snapshot = db::managed_host::load_snapshot(
        &mut *read,
        host_id.as_machine_id(),
        model::machine::LoadSnapshotOptions::default(),
    )
    .await
    .expect("managed snapshot")
    .expect("host");
    let predictions =
        db::predicted_machine_interface::find_by_machine_id(&mut read, host_id.as_machine_id())
            .await
            .expect("predictions");
    assert!(matches!(
        snapshot.managed_state,
        ManagedHostState::HostInit { .. }
    ));
    assert!(
        db::machine_interface::find_by_mac_address(&mut *read, declared_mac)
            .await
            .expect("declared interfaces")
            .is_empty()
    );
    let target =
        carbide_machine_controller::boot_interface::boot_interface_target(&snapshot, &predictions)
            .expect("initial boot target");
    assert!(
        matches!(target, carbide_redfish::boot_interface::BootInterfaceTarget::Pair(pair)
        if pair.mac_address == declared_mac && pair.interface_id.starts_with("NIC.Embedded."))
    );
    let network_primary = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("HBN primary")
        .clone();
    assert!(network_primary.attached_dpu_machine_id.is_some());
    assert!(!network_primary.addresses.is_empty());
    assert!(
        !predictions
            .iter()
            .find(|row| row.mac_address == declared_mac)
            .expect("boot prediction")
            .primary_interface
    );
    read.rollback().await.expect("close read");

    // Lease after HostInit through ordinary prediction promotion, without changing any flags in the fixture.
    env.api
        .discover_dhcp(
            rpc::forge::DhcpDiscovery::builder(declared_mac, "192.0.3.1").tonic_request(),
        )
        .await
        .expect("late boot-NIC lease");
    let mut read = env.db_txn().await;
    let leased = host.host().db_machine(&mut read).await;
    let nic = db::machine_interface::find_by_mac_address(&mut *read, declared_mac)
        .await
        .expect("leased NIC")
        .remove(0);

    // Reload both decisions so a late lease cannot silently move networking onto the separate boot NIC.
    assert!(!nic.primary_interface);
    assert!(!nic.addresses.is_empty());
    let after_primary = leased
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("retained primary");
    assert_eq!(after_primary.id, network_primary.id);
    assert_eq!(after_primary.addresses, network_primary.addresses);
    assert_eq!(
        leased.config.primary_interface_selection,
        snapshot.host_snapshot.config.primary_interface_selection
    );
    let before_boot = snapshot
        .host_snapshot
        .config
        .desired_boot_interface
        .as_ref()
        .expect("boot intent before lease");
    let after_boot = leased
        .config
        .desired_boot_interface
        .as_ref()
        .expect("boot intent after lease");
    assert_eq!(after_boot.value, before_boot.value);
    assert_eq!(after_boot.version, before_boot.version);
    assert_eq!(
        leased.config.boot_interface_selection,
        snapshot.host_snapshot.config.boot_interface_selection
    );
    read.rollback().await.expect("close late lease read");
    // Fetch and acknowledge real agent networking again to detect late loss of its required primary Admin interface.
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
}

/// Preserves the previously Ready primary DPU after the controller reloads a history-free polling snapshot, even when corrected PF facts make another DPU the report-selected candidate.
#[crate::sqlx_test]
async fn reinitialization_loads_ready_history_before_preserving_primary_dpu(pool: sqlx::PgPool) {
    use std::collections::HashMap;

    // Start from a working legacy host so actual persisted Ready history, rather than stable identity, qualifies preservation.
    let env = create_test_env(pool).await;
    let host = super::common::api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    let mut read = env.db_txn().await;
    let current_host = host.host().db_machine(&mut read).await;
    let primary_dpu = current_host
        .primary_attached_dpu_machine_id()
        .expect("working primary DPU");
    let primary_index = host
        .dpu_ids
        .iter()
        .position(|id| *id == primary_dpu)
        .expect("primary DPU snapshot");
    let dpu = host.dpu_n(primary_index).db_machine(&mut read).await;
    let interface_id = dpu
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("DPU OOB")
        .id;
    let mut hardware_info = dpu.status.hardware_info.expect("DPU hardware");
    read.rollback().await.expect("close read");
    assert_ne!(
        current_host
            .config
            .boot_interface_selection
            .expect("automatic source")
            .source,
        BootInterfaceSelectionSource::Operator
    );
    let corrected_mac = "02:00:00:fa:00:02".parse().expect("corrected PF MAC");
    let dpu_info = hardware_info.dpu_info.as_mut().expect("DPU data");
    dpu_info.part_number = "900-9D4B4-CWAA-TSA".to_string();
    dpu_info.host_pfs = vec![model::hardware_info::DpuHostPf {
        pci_controller_id: 1,
        pf_id: 0,
        mac_address: corrected_mac,
    }];

    // Register corrected facts while Ready, then deliberately reenter initialization without the validation-time shortcut.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    let mut setup = env.db_txn().await;
    sqlx::query("UPDATE machines SET last_machine_validation_time = NULL WHERE id = $1")
        .bind(host.id)
        .execute(setup.as_mut())
        .await
        .expect("isolate Ready history");
    db::machine::update_state(
        &mut setup,
        &host.id.into(),
        &ManagedHostState::DPUInit {
            dpu_states: model::machine::DpuInitStates {
                states: host
                    .dpu_ids
                    .iter()
                    .copied()
                    .map(|id| (id, DpuInitState::WaitingForNetworkConfig))
                    .collect::<HashMap<_, _>>(),
            },
        },
    )
    .await
    .expect("reenter initialization");
    setup.commit().await.expect("commit initialization");

    // A normal controller iteration starts without history; the repair must reload it under its locks.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload persisted state and confirm the old DPU identity survived while its provisional MAC changed.
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    let primary = repaired
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("repaired primary");
    assert_eq!(primary.attached_dpu_machine_id, Some(primary_dpu));
    assert_eq!(primary.mac_address, corrected_mac);
    assert_ne!(
        repaired.network_config.version,
        current_host.network_config.version
    );
    assert!(
        matches!(repaired.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("finalized") && reason.contains("reload"))
    );
}

/// Honors Operator boot intent over an owned declared NIC during HostInit while preserving the selected DPU's corrected networking and provenance.
#[crate::sqlx_test]
async fn operator_boot_selection_overrides_owned_declared_nic(pool: sqlx::PgPool) {
    use carbide_redfish::libredfish::test_support::RedfishSimAction;
    use model::machine::{MachineState, SetBootOrderInfo, SetBootOrderState};

    // Supply an in-band segment so the declared NIC can obtain its own row through ordinary DHCP promotion.
    let env = super::common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let declared_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 8]);
    let (host, hardware_info, interface_id) =
        checkpoint_host(&env, false, Some(declared_mac)).await;
    let corrected_mac = hardware_info.dpu_info.as_ref().expect("DPU").host_pfs[0].mac_address;
    // Observe the production-created boot-only prediction before ordinary DHCP adoption.
    let mut setup = env.db_txn().await;
    let declared_prediction =
        db::predicted_machine_interface::find_by_mac_address(&mut setup, declared_mac)
            .await
            .expect("declared prediction lookup")
            .expect("declared prediction");
    assert!(!declared_prediction.primary_interface);
    setup.rollback().await.expect("close prediction read");
    let leased = env
        .api
        .discover_dhcp(
            rpc::forge::DhcpDiscovery::builder(declared_mac, "192.0.3.1").tonic_request(),
        )
        .await
        .expect("lease declared NIC")
        .into_inner();
    let declared_interface = leased.machine_interface_id.expect("owned declared NIC");

    // Seed the legacy Operator primary baseline because this predicted-ID fixture cannot use the stable-only primary API.
    let mut setup = env.db_txn().await;
    db::machine_interface::set_primary_interface_selection(
        &mut setup,
        &host.id,
        BootInterfaceSelectionSource::Operator,
        true,
    )
    .await
    .expect("record legacy primary choice");
    setup.commit().await.expect("commit primary provenance");

    // Record independent Operator boot intent through the public API without changing ExpectedMachine's declaration.
    let host_id: carbide_uuid::machine::HostMachineId = host.id.into();
    env.api
        .set_dpu_first_boot_order(Request::new(rpc::forge::SetDpuFirstBootOrderRequest {
            machine_id: Some(host.id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: Some(corrected_mac.to_string()),
        }))
        .await
        .expect("record Operator boot intent");
    // Model an older owned row without an ID so finalization must capture the declared NIC's own report-resolved ID.
    sqlx::query("UPDATE machine_interfaces SET boot_interface_id = NULL WHERE id = $1")
        .bind(declared_interface)
        .execute(&env.pool)
        .await
        .expect("clear uncaptured ID");
    let mut read = env.db_txn().await;
    let operator_selected = host.host().db_machine(&mut read).await;
    let primary_selection = operator_selected
        .config
        .primary_interface_selection
        .expect("Operator primary source");
    let operator_target = operator_selected
        .config
        .desired_boot_interface
        .expect("Operator target");
    let operator_selection = operator_selected
        .config
        .boot_interface_selection
        .expect("Operator source");
    assert_eq!(
        operator_selection.source,
        BootInterfaceSelectionSource::Operator
    );
    read.rollback().await.expect("close read");

    // Publish corrected PF networking before acknowledging it; finalization must preserve DPU identity and Operator intent.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register BF4 facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let found = host.host().rpc_machine().await;
    let primary = found
        .status
        .expect("host status")
        .interfaces
        .into_iter()
        .find(|row| row.primary_interface)
        .expect("primary interface");
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
    assert_eq!(primary.mac_address, corrected_mac.to_string());
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    let declared = db::machine_interface::find_one(&mut *read, declared_interface)
        .await
        .expect("declared NIC");
    assert_eq!(declared.machine_id, Some(host_id.into()));
    assert!(!declared.primary_interface);
    assert!(
        declared
            .boot_interface_id
            .as_deref()
            .expect("captured declared ID")
            .starts_with("NIC.Embedded.")
    );
    assert!(
        db::predicted_machine_interface::find_by_mac_address(&mut read, declared_mac)
            .await
            .expect("consumed prediction")
            .is_none()
    );
    let repaired_target = repaired
        .config
        .desired_boot_interface
        .expect("retained Operator target");
    assert_eq!(repaired_target.value, operator_target.value);
    assert_eq!(repaired_target.version, operator_target.version);
    assert_eq!(
        repaired.config.boot_interface_selection,
        Some(operator_selection)
    );
    // Correcting this DPU's PF must retain networking provenance even when ExpectedMachine declares another NIC.
    assert_eq!(
        repaired.config.primary_interface_selection,
        Some(primary_selection)
    );
    assert!(
        matches!(repaired.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("finalized") && reason.contains("reload"))
    );
    read.rollback().await.expect("close read");

    // Admit HostInit only after acknowledging the repaired version, then isolate its boot-order actuator from unrelated initial setup.
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut setup = env.db_txn().await;
    let admitted = host.host().db_machine(&mut setup).await;
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit { .. }
    ));
    db::machine::update_state(
        &mut setup,
        &host_id,
        &ManagedHostState::HostInit {
            machine_state: MachineState::SetBootOrder {
                set_boot_order_info: Some(SetBootOrderInfo {
                    set_boot_order_jid: None,
                    set_boot_order_state: SetBootOrderState::SetBootOrder,
                    retry_count: 0,
                }),
            },
        },
    )
    .await
    .expect("isolate initial boot-order step");
    setup
        .commit()
        .await
        .expect("commit initial boot-order state");
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);
    env.redfish_sim
        .set_lockdown(libredfish::EnabledDisabled::Disabled);
    let timepoint = env.redfish_sim.timepoint();

    // Exercise the runtime resolver through a real initial boot-order controller step with no remaining prediction.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Observe the stored Operator target and persisted progress even though the declared NIC remains owned with an ID.
    let expected_mac = corrected_mac.to_string();
    assert!(env.redfish_sim.actions_since(&timepoint).all_hosts().iter().any(|action|
        matches!(action, RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac } if boot_interface_mac == &expected_mac)
    ));
    let mut read = env.db_txn().await;
    let booting = host.host().db_machine(&mut read).await;
    read.rollback().await.expect("close read");
    assert!(matches!(
        booting.state.value,
        ManagedHostState::HostInit {
            machine_state: MachineState::SetBootOrder {
                set_boot_order_info: Some(SetBootOrderInfo {
                    set_boot_order_state: SetBootOrderState::WaitForSetBootOrderJobScheduled,
                    ..
                })
            }
        }
    ));
    assert_eq!(
        booting.primary_attached_dpu_machine_id(),
        Some(host.dpu_ids[0])
    );
    let booting_target = booting
        .config
        .desired_boot_interface
        .expect("retained Operator target");
    assert_eq!(booting_target.value, operator_target.value);
    assert_eq!(booting_target.version, operator_target.version);
    assert_eq!(
        booting.config.boot_interface_selection,
        Some(operator_selection)
    );
}

/// Withholds API-first automatic boot publication until PF finalization, so a later repair cannot mask premature Site Explorer authority over the factory bootstrap MAC.
#[crate::sqlx_test]
async fn api_first_exploration_withholds_automatic_boot_intent_before_checkpoint(
    pool: sqlx::PgPool,
) {
    // Register corrected facts before exploration and stop before any controller finalization or network ACK.
    let env = create_test_env(pool).await;
    let (explored, hardware_info) = explore_checkpoint_host(&env, true, None).await;
    let dpu_id = explored.dpu_machine_ids[&0];
    let mut read = env.db_txn().await;
    let host_id = db::machine::find_host_by_dpu_machine_id(&mut read, &dpu_id)
        .await
        .expect("host lookup")
        .expect("paired host")
        .id;
    let host = db::machine::find_one(&mut *read, &host_id, Default::default())
        .await
        .expect("host read")
        .expect("host");
    let topology = db::machine_topology::find_latest_by_machine_ids(&mut read, &[dpu_id])
        .await
        .expect("DPU topology")
        .remove(&dpu_id)
        .expect("registered topology");

    // Prove complete facts coexist with provisional networking; no finalization has had a chance to rewrite boot intent.
    assert_eq!(
        topology
            .dpu_host_pf_inventory
            .expect("authenticated inventory")
            .host_pfs,
        hardware_info.dpu_info.as_ref().expect("DPU facts").host_pfs
    );
    assert!(
        db::machine::bf4_host_pf_initialization_pending(&mut read, &host_id)
            .await
            .expect("pending classification")
    );
    assert_eq!(
        host.status
            .interfaces
            .iter()
            .find(|row| row.primary_interface)
            .expect("bootstrap primary")
            .mac_address,
        hardware_info.factory_mac_address().expect("factory MAC")
    );
    assert!(host.config.desired_boot_interface.is_none());
    assert!(host.config.boot_interface_selection.is_none());
    read.rollback().await.expect("close observation");
}

/// Adopts a declared NIC leased before BF4 ingestion without selecting it for networking, then renders corrected HBN configuration and reaches HostInit after a fresh ACK.
#[crate::sqlx_test]
async fn leased_declared_boot_nic_keeps_automatic_bf4_hbn_networking(pool: sqlx::PgPool) {
    // Lease the NIC before ExpectedMachine ingestion so Explorer must adopt an anonymous owned-address row.
    let env = super::common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let declared_mac = MacAddress::new([2, 0, 0, 0xfa, 0, 8]);
    let lease = env
        .api
        .discover_dhcp(
            rpc::forge::DhcpDiscovery::builder(declared_mac, "192.0.3.1").tonic_request(),
        )
        .await
        .expect("anonymous boot-NIC lease")
        .into_inner();
    let nic_id = lease.machine_interface_id.expect("leased interface");
    let mut read = env.db_txn().await;
    let anonymous = db::machine_interface::find_one(&mut *read, nic_id)
        .await
        .expect("anonymous NIC");
    assert!(anonymous.machine_id.is_none());
    assert!(!anonymous.addresses.is_empty());
    read.rollback().await.expect("close anonymous read");
    let (host, hardware_info, interface_id) =
        checkpoint_host(&env, false, Some(declared_mac)).await;
    let host_id: carbide_uuid::machine::HostMachineId = host.id.into();
    let corrected_mac = hardware_info
        .dpu_info
        .as_ref()
        .expect("registered PF")
        .host_pfs[0]
        .mac_address;

    // Reload actual history and provenance to exclude the Operator and previously-working preservation branches.
    let mut read = env.db_txn().await;
    let before = db::machine::find_one(
        &mut *read,
        &host_id,
        model::machine::machine_search_config::MachineSearchConfig {
            include_history: true,
            ..Default::default()
        },
    )
    .await
    .expect("initial host read")
    .expect("initial host");
    assert!(before.last_machine_validation_time.is_none());
    assert!(!before.history.is_empty());
    assert!(before.history.iter().all(|record| !matches!(
        serde_json::from_str::<ManagedHostState>(&record.state).expect("history state"),
        ManagedHostState::Ready
            | ManagedHostState::Assigned { .. }
            | ManagedHostState::BootConfiguring { .. }
    )));
    assert!(
        before
            .config
            .primary_interface_selection
            .is_none_or(|selection| selection.source != BootInterfaceSelectionSource::Operator)
    );
    let adopted = db::machine_interface::find_one(&mut *read, nic_id)
        .await
        .expect("adopted boot NIC");
    assert_eq!(adopted.machine_id, Some(host_id.into()));
    assert!(!adopted.primary_interface);
    assert_eq!(adopted.addresses, anonymous.addresses);
    read.rollback().await.expect("close initial read");

    // Host-only BF4 evidence must persist before registration without fabricating authenticated PF inventory or publishing provisional boot intent.
    let mut read = env.db_txn().await;
    let topology = db::machine_topology::find_latest_by_machine_ids(&mut read, &host.dpu_ids)
        .await
        .expect("provisional topology")
        .remove(&host.dpu_ids[0])
        .expect("DPU topology");
    assert!(topology.topology.discovery_data.info.is_bf4_dpu());
    assert!(topology.dpu_host_pf_inventory.is_none());
    assert!(
        db::machine::bf4_host_pf_initialization_pending(&mut read, &host_id)
            .await
            .expect("pending PF facts")
    );
    assert!(before.config.desired_boot_interface.is_none());
    read.rollback().await.expect("close provisional read");

    // Finalize the authenticated PF while the separate declared NIC is already leased and owned.
    register_facts(&env, interface_id, hardware_info)
        .await
        .expect("register PF facts");
    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let mut read = env.db_txn().await;
    let repaired = host.host().db_machine(&mut read).await;
    let primary = repaired
        .status
        .interfaces
        .iter()
        .find(|row| row.primary_interface)
        .expect("HBN primary");
    assert_eq!(primary.mac_address, corrected_mac);
    assert_eq!(primary.attached_dpu_machine_id, Some(host.dpu_ids[0]));
    assert_eq!(
        primary.network_segment_type,
        Some(model::network_segment::NetworkSegmentType::Admin)
    );
    let admin_ip = primary
        .addresses
        .iter()
        .find(|ip| ip.is_ipv4())
        .expect("HBN admin address")
        .to_string();
    assert_eq!(
        repaired
            .config
            .primary_interface_selection
            .expect("automatic networking source")
            .source,
        BootInterfaceSelectionSource::RedfishSerialNumber
    );
    let desired = repaired
        .config
        .desired_boot_interface
        .as_ref()
        .expect("declared boot intent");
    assert_eq!(desired.value.mac_address(), declared_mac);
    assert!(
        desired
            .value
            .interface_id()
            .expect("declared Redfish ID")
            .starts_with("NIC.Embedded.")
    );
    assert_eq!(
        repaired
            .config
            .boot_interface_selection
            .expect("declared source")
            .source,
        BootInterfaceSelectionSource::ExpectedMachine
    );
    assert_ne!(
        repaired.network_config.version,
        before.network_config.version
    );
    assert!(matches!(
        repaired.state.value,
        ManagedHostState::DPUInit { .. }
    ));
    read.rollback().await.expect("close repaired read");

    // Render the real agent response before acknowledging the corrected version and advancing the controller.
    let networking = env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(host.dpu_ids[0]),
            },
        ))
        .await
        .expect("render HBN admin networking")
        .into_inner();
    assert!(networking.use_admin_network);
    assert_eq!(
        networking
            .admin_interface
            .expect("rendered admin interface")
            .addresses
            .iter()
            .find(|address| address.address_family == rpc::forge::AddressFamily::V4 as i32)
            .expect("rendered IPv4 admin address")
            .ip,
        admin_ip
    );
    assert_eq!(
        networking.managed_host_config_version,
        repaired.network_config.version.to_string()
    );
    network_configured_with_health(&env, &host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    // Reload admission and the leased secondary NIC to prove the automatic path completed with independent boot intent.
    let mut read = env.db_txn().await;
    let admitted = host.host().db_machine(&mut read).await;
    let boot_nic = db::machine_interface::find_one(&mut *read, nic_id)
        .await
        .expect("secondary boot NIC");
    assert!(matches!(
        admitted.state.value,
        ManagedHostState::HostInit { .. }
    ));
    assert_eq!(
        admitted.primary_attached_dpu_machine_id(),
        Some(host.dpu_ids[0])
    );
    assert_eq!(
        admitted.network_config.version,
        repaired.network_config.version
    );
    assert_eq!(
        admitted
            .config
            .desired_boot_interface
            .expect("admitted boot target")
            .value,
        desired.value
    );
    assert!(!boot_nic.primary_interface);
    assert_eq!(boot_nic.addresses, anonymous.addresses);
    read.rollback().await.expect("close admitted read");
}
