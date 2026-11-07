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
use model::machine::ManagedHostStateSnapshot;
use state_controller::io::StateControllerIO;
use state_controller::state_handler::{FromStateHandlerResult, StateHandlerOutcome};

use super::*;
use crate::tests::common::api_fixtures::test_managed_host::TestPredictedManagedHost;

/// Reconciles a different registered BF4 MAC so network-version tests exercise a real replacement of provisional host networking.
///
/// Callers must supply an unreserved MAC different from the fixture's provisional host MAC.
async fn repair_registered_boot_target(
    env: &TestEnv,
    mh: &TestPredictedManagedHost,
    registered_mac: MacAddress,
) {
    // Change only the registered host-PF MAC; the existing fixture supplies DPU identity, pairing, and admin networking.
    let dpu_id = mh.dpu_ids[0];
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    let mut topology = db::machine_topology::find_latest_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .expect("DPU topology")
        .remove(&dpu_id)
        .expect("registered DPU");
    let hardware = &mut topology.topology.discovery_data.info;
    let dpu = hardware.dpu_info.as_mut().expect("DPU inventory");
    dpu.part_number = "900-9D4B4-CWAA-TSA".to_string(); // Apply BF4 registration policy to the fixture's DPU.
    dpu.host_pfs = vec![model::hardware_info::DpuHostPf {
        pci_controller_id: 1,
        pf_id: 0,
        mac_address: registered_mac, // Replace the provisional factory-MAC host link during repair.
        physical_port_name: "c1pf0".to_string(),
        netdev_name: None,
        pci_address: None,
    }];
    let registered = dpu.clone();
    // Refresh BF4 classification before the trusted merge because generic topology refreshes cannot initialize host_pfs.
    db::machine_topology::set_topology_update_needed(&mut txn, &dpu_id.into(), true)
        .await
        .expect("refresh fixture platform");
    db::machine_topology::create_or_update(&mut txn, &dpu_id.into(), hardware)
        .await
        .expect("store BF4 classification");
    db::machine_topology::record_dpu_host_pfs(&mut txn, &dpu_id, &registered)
        .await
        .expect("merge registered inventory");

    // Publish matching host evidence before reconciliation so the new MAC can select a complete boot target.
    let report = EndpointExplorationReport {
        systems: vec![model::site_explorer::ComputerSystem {
            ethernet_interfaces: vec![model::site_explorer::EthernetInterface {
                mac_address: Some(registered_mac), // Match the trusted registration instead of the provisional interface.
                id: Some("Registered.ReplacementPF0".to_string()),
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
    sqlx::query("UPDATE explored_endpoints SET exploration_report = $2 WHERE address = $1")
        .bind(bmc_ip)
        .bind(sqlx::types::Json(&report))
        .execute(txn.as_mut())
        .await
        .expect("publish registered boot evidence");
    txn.commit()
        .await
        .expect("commit registration and host report");

    // Commit the real repair separately, matching discovery's post-commit reconciliation order.
    let mut repair = env.db_txn().await;
    db::host_boot_interface::reconcile_pre_host_init(&mut repair, &mh.host().id, 1, false, None)
        .await
        .expect("reconcile registered host-PF");
    repair.commit().await.expect("publish repaired networking");
}

/// Runs the production handler on a captured snapshot while retaining its writes and transaction so tests can control the persistence interleaving.
async fn handle_captured_snapshot(
    env: &TestEnv,
    snapshot: &mut ManagedHostStateSnapshot,
) -> Result<(StateHandlerOutcome<ManagedHostState>, DbWriteBatch), StateHandlerError> {
    // Supply ordinary controller services and collect the writes that the processor would apply before its state CAS.
    let host_id = snapshot.host_snapshot.id;
    let state = snapshot.managed_state.clone();
    let mut services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };
    let outcome = env
        .machine_state_handler
        .handle_object_state(&host_id, snapshot, &state, &mut ctx)
        .await?;
    Ok((outcome, pending_db_writes))
}

/// Rejects a controller snapshot acknowledged before BF4 repair and waits for the repaired version before entering HostInit, preventing boot work on provisional networking.
#[crate::sqlx_test]
async fn test_bf4_repair_invalidates_captured_network_ack(pool: sqlx::PgPool) {
    // Capture healthy provisional networking before registration can replace its host-PF MAC.
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_id = mh.dpu_ids[0];
    // Complete the fixture's requested DPU reboot so restart verification cannot preempt the network-readiness branch.
    let _ = mh.dpu().forge_agent_control().await;
    network_configured_with_health(&env, &dpu_id, None).await;
    let mut txn = env.db_txn().await;
    let mut captured = mh.snapshot(&mut txn).await;
    txn.rollback().await.expect("close captured snapshot");
    // Prove the captured acknowledgment is usable before introducing the concurrent repair.
    assert!(captured.managed_host_network_config_version_synced());
    let captured_network_version = captured.host_snapshot.network_config.version;
    let captured_state_version = captured.host_snapshot.state.version;

    // Replace the provisional interface through the real repair while the captured controller snapshot remains unchanged.
    let registered_mac = MacAddress::new([2, 0, 0, 0xfe, 0x20, 1]);
    assert_ne!(registered_mac, host_config.dpus[0].host_mac_address);
    repair_registered_boot_target(&env, &mh, registered_mac).await;
    let mut txn = env.db_txn().await;
    let repaired = mh.snapshot(&mut txn).await;
    txn.rollback().await.expect("close repaired snapshot");
    // The repair changes networking without changing the controller-state token or receiving a new acknowledgment.
    assert_ne!(
        repaired.host_snapshot.network_config.version,
        captured_network_version
    );
    assert_eq!(repaired.host_snapshot.state.version, captured_state_version);
    assert!(!repaired.managed_host_network_config_version_synced());

    // Exercise the stale handler after boot facts have become ready; the network-version guard must reject its transition.
    let result = handle_captured_snapshot(&env, &mut captured).await;
    // The handler must reject the stale prerequisite rather than returning a HostInit transition.
    assert!(matches!(
        result,
        Err(StateHandlerError::IterationInvalidated { .. })
    ));
    let mut txn = env.db_txn().await;
    let retained = mh.snapshot(&mut txn).await;
    txn.rollback()
        .await
        .expect("close rejected transition read");
    // Rejecting the iteration must preserve the committed repair and the pre-HostInit controller state.
    assert_eq!(retained.managed_state, repaired.managed_state);
    assert_eq!(retained.host_snapshot.state.version, captured_state_version);
    assert_eq!(
        retained.host_snapshot.network_config.version,
        repaired.host_snapshot.network_config.version
    );
    assert!(!retained.managed_host_network_config_version_synced());

    // Let the real controller read the repaired host while the DPU still acknowledges the provisional version.
    env.run_machine_state_controller_iteration_no_requeue()
        .await;
    let mut txn = env.db_txn().await;
    let waiting = mh.host().db_machine(&mut txn).await;
    txn.rollback()
        .await
        .expect("close current-version wait read");
    // The persisted wait must identify the missing acknowledgment after boot readiness has already converged.
    assert_eq!(waiting.current_state(), &repaired.managed_state);
    assert!(matches!(&waiting.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
        if reason.contains("current network config version")));

    // A later acknowledgment must recover through the same controller and commit HostInit for the repaired networking.
    network_configured_with_health(&env, &dpu_id, None).await;
    env.run_machine_state_controller_iteration_no_requeue()
        .await;
    let mut txn = env.db_txn().await;
    let initialized = mh.snapshot(&mut txn).await;
    txn.rollback().await.expect("close recovered HostInit read");
    // Reloaded state and outcome must prove that acknowledging the repaired version restores HostInit progress.
    assert!(initialized.managed_host_network_config_version_synced());
    assert!(matches!(
        initialized.managed_state,
        ManagedHostState::HostInit {
            machine_state: MachineState::EnableIpmiOverLan,
        }
    ));
    assert!(matches!(
        initialized.host_snapshot.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
}

/// Rechecks a newer DPU report even when the host network version is unchanged, preventing a formerly healthy snapshot from authorizing HostInit after the agent reports an older applied config.
#[crate::sqlx_test]
async fn test_bf4_host_init_rechecks_latest_network_ack(pool: sqlx::PgPool) {
    // Prepare repaired BF4 networking and capture a healthy acknowledgment of its current version.
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_id = mh.dpu_ids[0];
    // Complete the fixture's requested DPU reboot so restart verification cannot preempt the network-readiness branch.
    let _ = mh.dpu().forge_agent_control().await;
    let mut txn = env.db_txn().await;
    let provisional_version = mh.host().db_machine(&mut txn).await.network_config.version;
    txn.rollback()
        .await
        .expect("close provisional version read");
    repair_registered_boot_target(&env, &mh, MacAddress::new([2, 0, 0, 0xfe, 0x20, 2])).await;
    network_configured_with_health(&env, &dpu_id, None).await;
    let mut txn = env.db_txn().await;
    let mut captured = mh.snapshot(&mut txn).await;
    txn.rollback()
        .await
        .expect("close healthy captured snapshot");
    // Prove the original snapshot is ready before the newer status report changes its acknowledgment.
    assert!(captured.managed_host_network_config_version_synced());

    // A newer public status report can acknowledge an older version without changing the host's network version.
    env.api
        .record_dpu_network_status(Request::new(rpc::forge::DpuNetworkStatus {
            dpu_machine_id: Some(dpu_id),
            dpu_agent_version: Some(TEST_DPU_AGENT_VERSION.to_string()),
            network_config_version: Some(provisional_version.to_string()), // Revoke the captured acknowledgment by reporting the older applied config.
            dpu_health: Some(rpc::health::HealthReport {
                source: "forge-dpu-agent".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await
        .expect("report older applied network config");

    // Re-evaluate the captured healthy snapshot and observe the newer report's network wait without retaining readiness locks.
    let (mut outcome, _) = handle_captured_snapshot(&env, &mut captured)
        .await
        .expect("evaluate latest acknowledgment");
    // The wait must use the newer report and release the transaction before returning.
    assert!(matches!(&outcome, StateHandlerOutcome::Wait { reason, .. }
        if reason.contains("current network config version")));
    assert!(outcome.take_transaction().is_none());
    let mut txn = env.db_txn().await;
    let waiting = mh.snapshot(&mut txn).await;
    txn.rollback().await.expect("close latest report read");
    // The reported acknowledgment changes while both the host network version and controller state stay fixed.
    assert_eq!(waiting.managed_state, captured.managed_state);
    assert_eq!(
        waiting.host_snapshot.network_config.version,
        captured.host_snapshot.network_config.version
    );
    assert!(!waiting.managed_host_network_config_version_synced());
}

/// Keeps a network writer behind HostInit publication after readiness succeeds, preventing an unacknowledged config version from committing between validation and the controller-state CAS.
#[crate::sqlx_test]
async fn test_bf4_host_init_retains_network_lock_through_publication(pool: sqlx::PgPool) {
    // Capture repaired, acknowledged networking so this case isolates the interval after the readiness checks.
    let env = create_test_env(pool.clone()).await;
    let host_config = env.managed_host_config();
    let mh = create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_id = mh.dpu_ids[0];
    // Complete the fixture's requested DPU reboot so restart verification cannot preempt the network-readiness branch.
    let _ = mh.dpu().forge_agent_control().await;
    repair_registered_boot_target(&env, &mh, MacAddress::new([2, 0, 0, 0xfe, 0x20, 3])).await;
    network_configured_with_health(&env, &dpu_id, None).await;
    let mut txn = env.db_txn().await;
    let mut captured = mh.snapshot(&mut txn).await;
    txn.rollback().await.expect("close publication snapshot");
    let host_id: HostMachineId = mh.host().id.into();
    let expected_version = captured.host_snapshot.state.version;
    let acknowledged_network = captured.host_snapshot.network_config.clone();

    // Pause the successful production outcome before the processor would persist its state transition.
    let (mut outcome, pending_db_writes) = handle_captured_snapshot(&env, &mut captured)
        .await
        .expect("evaluate ready HostInit transition");
    let next_state = match &outcome {
        StateHandlerOutcome::Transition { next_state, .. } => next_state.clone(),
        _ => panic!("expected a HostInit transition"),
    };
    // The handler must select HostInit and retain the transaction that protects its readiness checks.
    assert_eq!(
        next_state,
        ManagedHostState::HostInit {
            machine_state: MachineState::EnableIpmiOverLan,
        }
    );
    let persisted_outcome = PersistentStateHandlerOutcome::from_result(Ok(&outcome));
    let mut publication = outcome
        .take_transaction()
        .expect("HostInit must retain its readiness transaction");
    let publication_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(publication.as_mut())
        .await
        .expect("publication backend");
    let mut writer = pool.begin().await.expect("network writer transaction");
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(writer.as_mut())
        .await
        .expect("network writer backend");

    // Start a real network-version update and wait for PostgreSQL to prove that publication blocks it.
    let network_write = async {
        assert_eq!(
            db::machine::try_update_network_config(
                &mut writer,
                &host_id.into(),
                acknowledged_network.version,
                &acknowledged_network.value,
            )
            .await
            .expect("update network version after readiness"),
            db::ConditionalWrite::Applied(())
        );
        // Read the committed controller state after the blocked write resumes to prove publication happened first.
        let after_publication = mh.host().db_machine(&mut writer).await;
        assert_eq!(after_publication.current_state(), &next_state);
        writer
            .commit()
            .await
            .expect("commit serialized network write");
    };
    let publish_state = async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                .bind(publication_pid)
                .bind(writer_pid)
                .fetch_one(&pool)
                .await
                .expect("observe blocked network writer");
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }

        // The network version and controller state remain at the validated values until the guarded publication commits.
        let mut txn = env.db_txn().await;
        let before_publication = mh.host().db_machine(&mut txn).await;
        txn.rollback().await.expect("close blocked-writer read");
        assert_eq!(before_publication.current_state(), &captured.managed_state);
        assert_eq!(
            before_publication.network_config.version,
            acknowledged_network.version
        );

        // Use the processor's actual IO methods and transaction order, including history-lock reacquisition during state persistence.
        pending_db_writes
            .apply_all(&mut publication)
            .await
            .expect("apply queued controller writes");
        let io = MachineStateControllerIO::default();
        assert_eq!(
            io.persist_controller_state(
                &mut publication,
                &host_id,
                expected_version,
                expected_version.increment(),
                &next_state,
            )
            .await
            .expect("publish guarded HostInit"),
            db::ConditionalWrite::Applied(())
        );
        io.persist_outcome(&mut publication, &host_id, persisted_outcome)
            .await
            .expect("persist HostInit outcome");
        publication.commit().await.expect("commit guarded HostInit");
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(network_write, publish_state);
    })
    .await
    .expect("network writer must serialize after HostInit without deadlock");

    // Reload both committed effects to prove the network change occurred only after HostInit publication.
    let mut txn = env.db_txn().await;
    let published = mh.host().db_machine(&mut txn).await;
    txn.rollback()
        .await
        .expect("close serialized publication read");
    assert_eq!(published.current_state(), &next_state);
    assert_ne!(
        published.network_config.version,
        acknowledged_network.version
    );
    assert!(matches!(
        published.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
}
