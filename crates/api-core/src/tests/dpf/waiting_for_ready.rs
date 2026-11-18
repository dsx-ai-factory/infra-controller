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

//! Tests for the WaitingForReady DPF state handler.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use carbide_dpf::{
    DpfError, DpuDeploymentType, DpuFailureObservation, DpuPhase, DpuPhaseObservation,
    dpu_device_cr_name,
};
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_redfish::libredfish::RedfishClientPool;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::{DpuMachineId, HostMachineId};
use db::TransactionVending;
use libredfish::SystemPowerControl;
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::machine::{
    DpfState, DpuInitState, FailureCause, FailureDetails, ManagedHostState, PerformPowerOperation,
};
use tokio::time::timeout;

use super::{
    assert_provisioning_suppressed, dpf_config, expect_dpf_service_inventory, get_host_state,
    phase_observation,
};
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_managed_host_multi_dpu,
    create_managed_host_with_dpf, create_test_env_with_overrides, get_config, reboot_completed,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// True after the DPF `DeviceReady` sync: no DPU remains in `DpfStates`, or host init has started.
fn dpf_left_operator_provisioning_substates(host: &ManagedHostState) -> bool {
    match host {
        ManagedHostState::DPUInit { dpu_states } => dpu_states
            .states
            .values()
            .all(|s| !matches!(s, DpuInitState::DpfStates { .. })),
        ManagedHostState::HostInit { .. } => true,
        _ => false,
    }
}

/// Set up the initial provisioning expectations shared by all WaitingForReady tests.
/// Does NOT set up `get_dpu_phase_observation` -- each test configures it to control the
/// DPU CR phase (the authoritative readiness signal).
fn expect_provisioning(mock: &mut MockDpfOperations) {
    mock.expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_deployment_type_for_dpu()
        .returning(move |__, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    expect_dpf_service_inventory(mock);
}

/// Persists the DPUs' DPF substate directly so tests can isolate a handler
/// transition without replaying the preceding operator workflow.
async fn reset_host_to_dpf_state(
    pool: &sqlx::PgPool,
    host_id: &HostMachineId,
    dpu_ids: &[DpuMachineId],
    dpf_state: DpfState,
) {
    let state = ManagedHostState::DPUInit {
        dpu_states: model::machine::DpuInitStates {
            states: dpu_ids
                .iter()
                .map(|id| {
                    (
                        *id,
                        DpuInitState::DpfStates {
                            state: dpf_state.clone(),
                        },
                    )
                })
                .collect(),
        },
    };
    let state_json = serde_json::to_value(&state).unwrap();
    let version = format!("V999-T{}", chrono::Utc::now().timestamp_micros());

    let mut txn = pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE machines SET \
            controller_state = $1, \
            controller_state_version = $2, \
            controller_state_outcome = NULL, \
            health_reports = '{\"merges\": {}, \"replace\": null}'::jsonb, \
            last_reboot_requested = NULL, \
            last_reboot_time = NULL \
         WHERE id = $3",
    )
    .bind(sqlx::types::Json(&state_json))
    .bind(&version)
    .bind(host_id)
    .execute(txn.as_mut())
    .await
    .unwrap();
    if state.suppresses_exploration_for_dpf() {
        let host = db::machine::find_one(txn.as_mut(), host_id, Default::default())
            .await
            .unwrap()
            .unwrap();
        let mut macs = vec![host.status.bmc_info.mac.unwrap()];
        for dpu_id in dpu_ids {
            let dpu = db::machine::find_one(txn.as_mut(), dpu_id, Default::default())
                .await
                .unwrap()
                .unwrap();
            macs.push(dpu.status.bmc_info.mac.unwrap());
        }
        for mac in macs {
            db::bmc_suppression::upsert(
                txn.as_mut(),
                &NewBmcSuppression {
                    bmc_mac_address: mac,
                    subsystem: BmcSuppressionSubsystem::SiteExplorer,
                    source: BmcSuppressionSource::DpfProvisioning,
                    reason: "provisioning fixture".to_string(),
                },
            )
            .await
            .unwrap();
        }
    }
    txn.commit().await.unwrap();
}

/// Restores WaitingForReady so tests can replay operator readiness and reboot
/// decisions after their initial DPF ingestion has completed.
async fn reset_host_to_waiting_for_ready(
    pool: &sqlx::PgPool,
    host_id: &HostMachineId,
    dpu_id: &DpuMachineId,
) {
    reset_host_to_dpf_state(
        pool,
        host_id,
        std::slice::from_ref(dpu_id),
        DpfState::WaitingForReady { phase_detail: None },
    )
    .await;
}

fn failure_observation() -> DpuFailureObservation {
    DpuFailureObservation {
        name: Some("failed-dpu".to_string()),
        namespace: Some("dpf-system".to_string()),
        uid: Some("failed-uid".to_string()),
        resource_version: Some("123".to_string()),
        generation: Some(7),
        deployment: Some("dpf-system_astra".to_string()),
        dpu_node_name: "failed-node".to_string(),
        dpu_device_name: "failed-device".to_string(),
        dpu_flavor: "astra-flavor".to_string(),
        bfb: None,
        blue_field_software: Some("bf-software-123".to_string()),
        status: serde_json::from_value(serde_json::json!({
            "phase": "Error",
            "previousPhase": "Update Firmware",
            "redfishTaskId": "42",
            "conditions": [{
                "type": "FirmwareReady",
                "status": "False",
                "reason": "TaskNotFound",
                "message": "Redfish task disappeared",
                "lastTransitionTime": "2026-09-30T01:02:03Z",
            }],
        }))
        .expect("valid failure status fixture"),
    }
}

/// Persist the observed failure and history without hold or reboot calls.
#[crate::sqlx_test]
async fn test_waiting_for_ready_preserves_failure_diagnostics(pool: sqlx::PgPool) {
    let failure = failure_observation();
    let expected_diagnostics =
        serde_json::to_value(&failure).expect("serialize expected failure diagnostics");
    let mut mock = MockDpfOperations::new();
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_get_dpu_phase_observation()
        .times(1)
        .return_once(move |_, _| {
            Ok(DpuPhaseObservation {
                phase: DpuPhase::Error,
                failure: Some(failure),
            })
        });
    mock.expect_release_maintenance_hold().never();
    mock.expect_is_reboot_required().never();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::default().with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = create_managed_host(&env).await;
    reset_host_to_waiting_for_ready(&pool, &mh.id, &mh.dpu_ids[0]).await;
    assert_provisioning_suppressed(&env, &mh, true).await;

    env.run_machine_state_controller_iteration().await;
    let host = get_host_state(&env, &mh).await;
    let ManagedHostState::Failed {
        details:
            FailureDetails {
                cause: FailureCause::DpfProvisioning { ref err },
                ..
            },
        machine_id,
        ..
    } = host
    else {
        panic!("expected persisted DPF failure, got {host:?}")
    };
    assert_eq!(machine_id, mh.dpu_ids[0].into());
    let prefix = format!(
        "DPU {} entered error phase during DPF provisioning: ",
        mh.dpu_ids[0]
    );
    let diagnostics: serde_json::Value = serde_json::from_str(
        err.strip_prefix(&prefix)
            .expect("failure should retain the provisioning error prefix"),
    )
    .expect("failure suffix should contain diagnostics JSON");
    assert_eq!(diagnostics, expected_diagnostics);
    assert_provisioning_suppressed(&env, &mh, false).await;

    let mut txn = env.db_txn().await;
    let history = db::state_history::for_object(
        txn.as_mut(),
        db::state_history::StateHistoryTableId::Machine,
        &mh.id,
    )
    .await
    .unwrap();
    assert!(history.iter().any(|record| {
        serde_json::from_str::<ManagedHostState>(&record.state).unwrap() == host
    }));
}

async fn assert_peer_failure_before_shared_actions(
    pool: sqlx::PgPool,
    deployment_type: DpuDeploymentType,
) {
    let scenario = format!("{deployment_type:?}");
    let mut failure = failure_observation();
    let mut expected_diagnostics =
        serde_json::to_value(&failure).expect("serialize expected failure diagnostics");
    let mut mock = MockDpfOperations::new();
    mock.expect_deployment_type_for_dpu()
        .returning(move |_, _| Ok(deployment_type));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_release_maintenance_hold().never();
    mock.expect_is_reboot_required().never();

    if deployment_type == DpuDeploymentType::Bf3 {
        mock.expect_get_dpu_phase_observation()
            .times(1)
            .return_once(|_, _| Ok(phase_observation(DpuPhase::Ready)));
        mock.expect_get_dpu_phase_observation()
            .times(1)
            .return_once(move |device_name, _| {
                failure.dpu_device_name = dpu_device_cr_name(device_name);
                Ok(DpuPhaseObservation {
                    phase: DpuPhase::Error,
                    failure: Some(failure),
                })
            });
        mock.expect_get_dpu_phase_observations_for_deployment_type()
            .never();
    } else {
        mock.expect_get_dpu_phase_observation().never();
        mock.expect_get_dpu_phase_observations_for_deployment_type()
            .times(1)
            .return_once(move |device_names, _, _| {
                let mut observations = device_names
                    .iter()
                    .map(|name| (name.clone(), phase_observation(DpuPhase::Ready)))
                    .collect::<BTreeMap<_, _>>();
                let failed_device = device_names.get(1).expect("two DPU fixture");
                failure.dpu_device_name = dpu_device_cr_name(failed_device);
                observations.insert(
                    failed_device.clone(),
                    DpuPhaseObservation {
                        phase: DpuPhase::Error,
                        failure: Some(failure),
                    },
                );
                Ok(Some(observations))
            });
    }

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::default().with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    let failed_dpu = &snapshot.dpu_snapshots[1];
    let failed_dpu_id = failed_dpu.id;
    expected_diagnostics["dpuDeviceName"] = serde_json::Value::String(dpu_device_cr_name(
        &failed_dpu.dpf_id().expect("fixture DPU has a DPF ID"),
    ));
    txn.commit().await.expect("finish fixture snapshot read");
    reset_host_to_dpf_state(
        &pool,
        &mh.id,
        &mh.dpu_ids,
        DpfState::WaitingForReady { phase_detail: None },
    )
    .await;

    // The first DPU is healthy; its host actions must wait for the peer read.
    env.run_machine_state_controller_iteration().await;

    let host = get_host_state(&env, &mh).await;
    let ManagedHostState::Failed {
        details:
            FailureDetails {
                cause: FailureCause::DpfProvisioning { ref err },
                ..
            },
        machine_id,
        ..
    } = host
    else {
        panic!("{scenario}: expected persisted peer failure, got {host:?}");
    };
    assert_eq!(machine_id, failed_dpu_id.into(), "{scenario}");
    let prefix = format!("DPU {failed_dpu_id} entered error phase during DPF provisioning: ");
    let diagnostics: serde_json::Value = serde_json::from_str(
        err.strip_prefix(&prefix)
            .expect("failure should identify the failed peer"),
    )
    .expect("failure suffix should contain diagnostics JSON");
    assert_eq!(diagnostics, expected_diagnostics, "{scenario}");
}

#[crate::sqlx_test]
async fn test_direct_waiting_for_ready_records_peer_failure_before_shared_actions(
    pool: sqlx::PgPool,
) {
    assert_peer_failure_before_shared_actions(pool, DpuDeploymentType::Bf3).await;
}

#[crate::sqlx_test]
async fn test_scoped_waiting_for_ready_records_peer_failure_before_shared_actions(
    pool: sqlx::PgPool,
) {
    assert_peer_failure_before_shared_actions(pool, DpuDeploymentType::Bf3Gb200).await;
}

/// WaitingForReady with reboot required:
///   1. Releases maintenance hold, sees reboot required, power-cycles host (ForceOff + On)
///   2. After reboot_completed, device ready -> leaves DPF `DpfStates` (then may advance further)
#[crate::sqlx_test]
async fn test_waiting_for_ready_reboot_flow(pool: sqlx::PgPool) {
    let mut mock = MockDpfOperations::new();
    expect_provisioning(&mut mock);

    mock.expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(DpuPhase::Ready)));
    mock.expect_release_maintenance_hold()
        .times(1..)
        .returning(|_| Ok(()));

    // Starts false so initial provisioning completes, flipped to true for the test phase.
    let reboot_required = Arc::new(AtomicBool::new(false));
    let rr = reboot_required.clone();
    mock.expect_is_reboot_required()
        .returning(move |_| Ok(rr.load(Ordering::SeqCst)));
    let rr2 = reboot_required.clone();
    mock.expect_reboot_complete()
        .times(1..)
        .returning(move |_| {
            rr2.store(false, Ordering::SeqCst);
            Ok(())
        });

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    reboot_required.store(true, Ordering::SeqCst);

    reset_host_to_waiting_for_ready(&pool, &mh.id, &mh.dpu_ids[0]).await;

    let redfish_timepoint = env.redfish_sim.timepoint();

    timeout(TEST_TIMEOUT, async {
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
    })
    .await
    .expect("timed out during state controller iterations");

    assert_provisioning_suppressed(&env, &mh, true).await;

    let actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .all_hosts();
    assert!(
        actions.contains(&RedfishSimAction::Power(SystemPowerControl::ForceOff)),
        "Expected ForceOff to be sent, actions: {:?}",
        actions
    );
    assert!(
        actions.contains(&RedfishSimAction::Power(SystemPowerControl::On)),
        "Expected On to be sent after ForceOff, actions: {:?}",
        actions
    );

    reboot_completed(&env, mh.id).await;

    // Complete On, observe DPU readiness, then cross the DeviceReady barrier.
    timeout(TEST_TIMEOUT, async {
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
    })
    .await
    .expect("timed out during post-reboot iterations");

    let host = get_host_state(&env, &mh).await;
    assert!(
        dpf_left_operator_provisioning_substates(&host),
        "Host should have left DPF operator substates after DeviceReady, got: {:?}",
        host
    );
    assert_provisioning_suppressed(&env, &mh, false).await;
}

/// WaitingForReady without reboot: enters maintenance, releases hold,
/// waits for DPU CR to reach Ready phase, then transitions.
#[crate::sqlx_test]
async fn test_waiting_for_ready_no_reboot(pool: sqlx::PgPool) {
    let mut mock = MockDpfOperations::new();
    expect_provisioning(&mut mock);

    let dpu_ready = Arc::new(AtomicBool::new(true));
    let dr = dpu_ready.clone();
    mock.expect_get_dpu_phase_observation()
        .returning(move |_, _| {
            if dr.load(Ordering::SeqCst) {
                Ok(phase_observation(DpuPhase::Ready))
            } else {
                Ok(phase_observation(DpuPhase::Provisioning(
                    "OsInstalling".into(),
                )))
            }
        });
    mock.expect_release_maintenance_hold()
        .times(1..)
        .returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    dpu_ready.store(false, Ordering::SeqCst);

    reset_host_to_waiting_for_ready(&pool, &mh.id, &mh.dpu_ids[0]).await;

    timeout(TEST_TIMEOUT, async {
        for _ in 0..5 {
            env.run_machine_state_controller_iteration().await;
        }
    })
    .await
    .expect("timed out during state controller iterations");

    let host = get_host_state(&env, &mh).await;
    assert!(
        matches!(host, ManagedHostState::DPUInit { .. }),
        "Host should still be in DPUInit waiting for DPU Ready phase, got: {:?}",
        host
    );

    assert_provisioning_suppressed(&env, &mh, true).await;

    // A wait must not recreate suppression removed by another writer.
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    let version_before_wait = host.state.version;
    db::bmc_suppression::delete_many_for_source(
        txn.as_mut(),
        &[
            host.status.bmc_info.mac.unwrap(),
            dpu.status.bmc_info.mac.unwrap(),
        ],
        BmcSuppressionSubsystem::SiteExplorer,
        BmcSuppressionSource::DpfProvisioning,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    assert_eq!(host.state.version, version_before_wait);
    assert!(
        matches!(
            &host.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { reason, .. })
                if reason == "Waiting for DPU to reach Ready phase"
        ),
        "unexpected outcome: {:?}",
        host.controller_state_outcome
    );
    txn.commit().await.unwrap();
    assert_provisioning_suppressed(&env, &mh, false).await;

    dpu_ready.store(true, Ordering::SeqCst);

    timeout(TEST_TIMEOUT, async {
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
    })
    .await
    .expect("timed out during post-ready iterations");

    let host = get_host_state(&env, &mh).await;
    assert!(
        dpf_left_operator_provisioning_substates(&host),
        "Host should have left DPF operator substates after DeviceReady, got: {:?}",
        host
    );
    assert_provisioning_suppressed(&env, &mh, false).await;
}

/// Regression for the reprovision race: after `reprovision_dpu` deletes the DPU
/// CR, the old CR lingers (finalizer) with a `deletionTimestamp` while its
/// `status.phase` is still `Ready`. `get_dpu_phase_observation` surfaces that as
/// `DpuPhase::Deleting`; WaitingForReady must keep waiting instead of reading the
/// stale `Ready` and short-circuiting to `DeviceReady`. Once the fresh CR reports
/// `Ready`, it advances.
#[crate::sqlx_test]
async fn test_waiting_for_ready_waits_while_dpu_deleting(pool: sqlx::PgPool) {
    let mut mock = MockDpfOperations::new();
    expect_provisioning(&mut mock);

    // True during initial provisioning; flipped to false to simulate the
    // terminating old CR observed right after a reprovision delete.
    let dpu_ready = Arc::new(AtomicBool::new(true));
    let dr = dpu_ready.clone();
    mock.expect_get_dpu_phase_observation()
        .returning(move |_, _| {
            if dr.load(Ordering::SeqCst) {
                Ok(phase_observation(DpuPhase::Ready))
            } else {
                Ok(phase_observation(DpuPhase::Deleting))
            }
        });
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    // Simulate the terminating old CR and re-enter WaitingForReady.
    dpu_ready.store(false, Ordering::SeqCst);
    reset_host_to_waiting_for_ready(&pool, &mh.id, &mh.dpu_ids[0]).await;

    timeout(TEST_TIMEOUT, async {
        for _ in 0..5 {
            env.run_machine_state_controller_iteration().await;
        }
    })
    .await
    .expect("timed out during state controller iterations");

    let host = get_host_state(&env, &mh).await;
    assert!(
        matches!(host, ManagedHostState::DPUInit { .. })
            && !dpf_left_operator_provisioning_substates(&host),
        "Host must keep waiting in DPF substates while the DPU CR is Deleting, got: {:?}",
        host
    );

    // The operator recreates the DPU; it now reports Ready and provisioning advances.
    dpu_ready.store(true, Ordering::SeqCst);

    timeout(TEST_TIMEOUT, async {
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
    })
    .await
    .expect("timed out during post-ready iterations");

    let host = get_host_state(&env, &mh).await;
    assert!(
        dpf_left_operator_provisioning_substates(&host),
        "Host should have left DPF operator substates once the fresh DPU CR is Ready, got: {:?}",
        host
    );
}

/// After the operator removes the old DPU CR and before it recreates the new one,
/// `get_dpu_phase_observation` returns `NotFound`. WaitingForReady must treat that window as a
/// wait, not an error, and advance once the fresh CR reports `Ready`.
#[crate::sqlx_test]
async fn test_waiting_for_ready_waits_when_dpu_not_found(pool: sqlx::PgPool) {
    let mut mock = MockDpfOperations::new();
    expect_provisioning(&mut mock);

    // True during initial provisioning; flipped to false to simulate the gap
    // between the old CR being removed and the new CR being created.
    let dpu_ready = Arc::new(AtomicBool::new(true));
    let dr = dpu_ready.clone();
    mock.expect_get_dpu_phase_observation()
        .returning(move |_, _| {
            if dr.load(Ordering::SeqCst) {
                Ok(phase_observation(DpuPhase::Ready))
            } else {
                Err(DpfError::not_found("DPU", "node-dpu-001-device-dpu-001"))
            }
        });
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    dpu_ready.store(false, Ordering::SeqCst);
    reset_host_to_waiting_for_ready(&pool, &mh.id, &mh.dpu_ids[0]).await;

    timeout(TEST_TIMEOUT, async {
        for _ in 0..5 {
            env.run_machine_state_controller_iteration().await;
        }
    })
    .await
    .expect("timed out during state controller iterations");

    let host = get_host_state(&env, &mh).await;
    assert!(
        matches!(host, ManagedHostState::DPUInit { .. })
            && !dpf_left_operator_provisioning_substates(&host),
        "Host must keep waiting in DPF substates while the DPU CR is absent, got: {:?}",
        host
    );

    dpu_ready.store(true, Ordering::SeqCst);

    timeout(TEST_TIMEOUT, async {
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
    })
    .await
    .expect("timed out during post-ready iterations");

    let host = get_host_state(&env, &mh).await;
    assert!(
        dpf_left_operator_provisioning_substates(&host),
        "Host should have left DPF operator substates once the fresh DPU CR is Ready, got: {:?}",
        host
    );
}

/// WaitingForReady idempotent reboot: ForceOff is only sent once,
/// not on every iteration while waiting for the host to come back.
#[crate::sqlx_test]
async fn test_waiting_for_ready_idempotent_reboot(pool: sqlx::PgPool) {
    let mut mock = MockDpfOperations::new();
    expect_provisioning(&mut mock);

    // Starts true so initial provisioning completes, flipped to false for the test phase.
    let dpu_ready = Arc::new(AtomicBool::new(true));
    let dr = dpu_ready.clone();
    mock.expect_get_dpu_phase_observation()
        .returning(move |_, _| {
            if dr.load(Ordering::SeqCst) {
                Ok(phase_observation(DpuPhase::Ready))
            } else {
                Ok(phase_observation(DpuPhase::Provisioning(
                    "OsInstalling".into(),
                )))
            }
        });
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));

    // Starts false so initial provisioning completes, flipped to true for the test phase.
    let reboot_required = Arc::new(AtomicBool::new(false));
    let rr = reboot_required.clone();
    mock.expect_is_reboot_required()
        .returning(move |_| Ok(rr.load(Ordering::SeqCst)));
    let rr2 = reboot_required.clone();
    mock.expect_reboot_complete()
        .times(1..)
        .returning(move |_| {
            rr2.store(false, Ordering::SeqCst);
            Ok(())
        });

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    reboot_required.store(true, Ordering::SeqCst);
    dpu_ready.store(false, Ordering::SeqCst);

    reset_host_to_waiting_for_ready(&pool, &mh.id, &mh.dpu_ids[0]).await;

    let redfish_timepoint = env.redfish_sim.timepoint();

    timeout(TEST_TIMEOUT, async {
        for _ in 0..5 {
            env.run_machine_state_controller_iteration().await;
        }
    })
    .await
    .expect("timed out during state controller iterations");

    let actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .all_hosts();
    let force_off_count = actions
        .iter()
        .filter(|x| matches!(x, RedfishSimAction::Power(SystemPowerControl::ForceOff)))
        .count();

    assert_eq!(
        force_off_count, 1,
        "ForceOff should be sent exactly once (idempotent guard), got {}",
        force_off_count
    );

    let redfish_timepoint2 = env.redfish_sim.timepoint();
    timeout(TEST_TIMEOUT, async {
        for _ in 0..5 {
            env.run_machine_state_controller_iteration().await;
        }
    })
    .await
    .expect("timed out during second iteration batch");

    let actions2 = env
        .redfish_sim
        .actions_since(&redfish_timepoint2)
        .all_hosts();
    let force_off_count2 = actions2
        .iter()
        .filter(|x| matches!(x, RedfishSimAction::Power(SystemPowerControl::ForceOff)))
        .count();

    assert_eq!(
        force_off_count2, 0,
        "No additional ForceOff should be sent while waiting for reboot, got {}",
        force_off_count2
    );
}

/// Verifies the On intent is persisted in a separate reconciliation before
/// issuing power-on, preventing a crash from leaving durable state at Off.
#[crate::sqlx_test]
async fn test_reboot_persists_on_intent_before_power_command(pool: sqlx::PgPool) {
    let mut mock = MockDpfOperations::new();
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::default().with_dpf_sdk(dpf_sdk),
    )
    .await;
    let managed_host = create_managed_host(&env).await;

    // Start with hardware Off and durable state at the completed Off phase.
    let bmc_access_info = {
        let mut txn = env.pool.txn_begin().await.unwrap();
        let bmc_access_info = managed_host.host().bmc_access(&mut txn).await;
        txn.commit().await.unwrap();
        bmc_access_info
    };
    env.redfish_sim
        .client_by_info(&bmc_access_info)
        .await
        .unwrap()
        .power(SystemPowerControl::ForceOff)
        .await
        .unwrap();
    reset_host_to_dpf_state(
        &pool,
        &managed_host.id,
        &managed_host.dpu_ids,
        DpfState::HandleReboot {
            op: PerformPowerOperation::Off,
            retry_count: 0,
        },
    )
    .await;
    let redfish_timepoint = env.redfish_sim.timepoint();

    // The first reconciliation only persists On intent and performs no power-on.
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while persisting On intent");
    let first_actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .all_hosts();
    assert!(
        !first_actions.contains(&RedfishSimAction::Power(SystemPowerControl::On)),
        "On must not be issued before its intent is persisted: {first_actions:?}"
    );
    let host_state = get_host_state(&env, &managed_host).await;
    let ManagedHostState::DPUInit { dpu_states } = host_state else {
        panic!("expected DPUInit after persisting On intent");
    };
    assert!(matches!(
        dpu_states.states.get(&managed_host.dpu_ids[0]),
        Some(DpuInitState::DpfStates {
            state: DpfState::HandleReboot {
                op: PerformPowerOperation::On,
                retry_count: 0,
            },
        })
    ));

    // The next reconciliation may now issue On from the durable On phase.
    let second_timepoint = env.redfish_sim.timepoint();
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while issuing On command");
    let second_actions = env.redfish_sim.actions_since(&second_timepoint).all_hosts();
    assert!(
        second_actions.contains(&RedfishSimAction::Power(SystemPowerControl::On)),
        "On must be issued from the durable On phase: {second_actions:?}"
    );
}

/// When the host is already Off and last_reboot_requested is None,
/// the reboot handler should skip ForceOff and go straight to PowerOn.
#[crate::sqlx_test]
async fn test_waiting_for_ready_host_already_off(pool: sqlx::PgPool) {
    let mut mock = MockDpfOperations::new();
    expect_provisioning(&mut mock);

    mock.expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(DpuPhase::Ready)));
    mock.expect_release_maintenance_hold()
        .times(1..)
        .returning(|_| Ok(()));

    let reboot_required = Arc::new(AtomicBool::new(false));
    let rr = reboot_required.clone();
    mock.expect_is_reboot_required()
        .returning(move |_| Ok(rr.load(Ordering::SeqCst)));
    let rr2 = reboot_required.clone();
    mock.expect_reboot_complete()
        .times(1..)
        .returning(move |_| {
            rr2.store(false, Ordering::SeqCst);
            Ok(())
        });

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    reboot_required.store(true, Ordering::SeqCst);
    reset_host_to_waiting_for_ready(&pool, &mh.id, &mh.dpu_ids[0]).await;

    // Set the host BMC power state to Off before entering the reboot path.
    let bmc_access_info = {
        let mut txn = env.pool.txn_begin().await.unwrap();
        let bmc_access_info = mh.host().bmc_access(&mut txn).await;
        txn.commit().await.unwrap();
        bmc_access_info
    };
    env.redfish_sim
        .client_by_info(&bmc_access_info)
        .await
        .unwrap()
        .power(SystemPowerControl::ForceOff)
        .await
        .unwrap();

    let redfish_timepoint = env.redfish_sim.timepoint();

    timeout(TEST_TIMEOUT, async {
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
    })
    .await
    .expect("timed out during state controller iterations");

    let actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .all_hosts();

    let force_off_count = actions
        .iter()
        .filter(|x| matches!(x, RedfishSimAction::Power(SystemPowerControl::ForceOff)))
        .count();
    assert_eq!(
        force_off_count, 0,
        "ForceOff should NOT be sent when host is already Off, got {} in {:?}",
        force_off_count, actions
    );

    assert!(
        actions.contains(&RedfishSimAction::Power(SystemPowerControl::On)),
        "Expected PowerOn to be sent for already-off host, actions: {:?}",
        actions
    );

    reboot_completed(&env, mh.id).await;

    // Complete On, observe DPU readiness, then cross the DeviceReady barrier.
    timeout(TEST_TIMEOUT, async {
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
    })
    .await
    .expect("timed out during post-reboot iterations");

    let host = get_host_state(&env, &mh).await;
    assert!(
        dpf_left_operator_provisioning_substates(&host),
        "Host should have left DPF operator substates after DeviceReady, got: {:?}",
        host
    );
}

/// Write a raw DPUInit state with a bogus `dpfstate` tag to simulate
/// a stale/invalid state stored by a previous implementation.
async fn write_unknown_dpf_init_state(
    pool: &sqlx::PgPool,
    host_id: &HostMachineId,
    dpu_id: &DpuMachineId,
) {
    let state_json: serde_json::Value = serde_json::json!({
        "state": "dpuinit",
        "dpu_states": {
            "states": {
                dpu_id.to_string(): {
                    "dpustate": "dpfstates",
                    "state": {
                        "dpfstate": "oldimplstate"
                    }
                }
            }
        }
    });
    let version = format!("V999-T{}", chrono::Utc::now().timestamp_micros());
    sqlx::query(
        "UPDATE machines SET \
            controller_state = $1, \
            controller_state_version = $2, \
            controller_state_outcome = NULL \
         WHERE id = $3",
    )
    .bind(sqlx::types::Json(&state_json))
    .bind(&version)
    .bind(host_id)
    .execute(pool)
    .await
    .unwrap();
}

/// Unknown DPF state in DPUInit transitions to Provisioning.
#[crate::sqlx_test]
async fn test_unknown_dpf_state_transitions_to_provisioning(pool: sqlx::PgPool) {
    let mut mock = MockDpfOperations::new();
    expect_provisioning(&mut mock);
    mock.expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(DpuPhase::Ready)));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));

    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    write_unknown_dpf_init_state(&pool, &mh.id, &mh.dpu_ids[0]).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let host_state = get_host_state(&env, &mh).await;
    match &host_state {
        ManagedHostState::DPUInit { dpu_states } => {
            let dpu_state = &dpu_states.states[&mh.dpu_ids[0]];
            assert!(
                matches!(
                    dpu_state,
                    DpuInitState::DpfStates {
                        state: DpfState::Provisioning
                    }
                ),
                "Unknown DPF state should transition to Provisioning, got: {dpu_state:?}"
            );
        }
        other => panic!("Expected DPUInit, got: {other:?}"),
    }
}
