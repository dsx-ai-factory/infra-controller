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

//! Tests for DPF state transitions during reprovisioning.
//!
//! Verifies that DPF states (`Reprovisioning` -> `Provisioning` -> `WaitingForReady`)
//! transition correctly when the outer state is `DPUReprovision`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_dpf::types::{DpuDeviceSummary, DpuNodeSummary, HostDpfSnapshot};
use carbide_dpf::{DpfError, DpuDeploymentType, DpuPhase};
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_uuid::machine::{DpuMachineId, HostMachineId};
use carbide_uuid::rack::RackId;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    DpfState, DpuReprovisionStates, FailureCause, InstanceState, ManagedHostState, ReprovisionState,
};
use rpc::forge::dpu_reprovisioning_request::Mode;
use rpc::forge::forge_server::Forge;
use tokio::time::timeout;

use super::{dpf_config, get_host_state};
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder;
use crate::tests::common::api_fixtures::{
    TEST_RMS_RACK_PROFILE_ID, TestEnv, TestEnvOverrides, TestManagedHost,
    create_managed_host_with_dpf, create_managed_host_with_dpf_multi,
    create_test_env_with_overrides, get_config, get_config_with_rack_profiles,
};
use crate::tests::common::postgres::wait_for_blocked_query;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

fn snapshot_with_crs_present(dpu_count: usize) -> HostDpfSnapshot {
    HostDpfSnapshot {
        dpu_node: Some(DpuNodeSummary {
            name: "node-mock".to_string(),
            labels: Default::default(),
            annotations: Default::default(),
            dpu_device_refs: (0..dpu_count).map(|i| format!("device-{i}")).collect(),
        }),
        dpu_devices: (0..dpu_count)
            .map(|i| DpuDeviceSummary {
                name: format!("device-{i}"),
                labels: Default::default(),
                bmc_ip: None,
                bmc_port: None,
                serial_number: String::new(),
            })
            .collect(),
        dpus: vec![],
    }
}

/// Build a `MockDpfOperations` with only the expectations needed for the
/// initial provisioning flow triggered by `create_managed_host_with_dpf`.
/// `dpu_ready` controls whether `get_dpu_phase` returns `Ready` or `Provisioning`.
fn provisioning_mock(dpu_ready: Arc<AtomicBool>) -> MockDpfOperations {
    provisioning_mock_with_dpu_count(dpu_ready, 1)
}

fn provisioning_mock_with_dpu_count(
    dpu_ready: Arc<AtomicBool>,
    dpu_count: usize,
) -> MockDpfOperations {
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device().returning(|_, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_snapshot_host()
        .returning(move |_| Ok(snapshot_with_crs_present(dpu_count)));
    mock.expect_get_dpu_phase().returning(move |_, _| {
        if dpu_ready.load(Ordering::SeqCst) {
            Ok(DpuPhase::Ready)
        } else {
            Ok(DpuPhase::Provisioning("OsInstalling".into()))
        }
    });
    mock
}

/// Builds a DPF mock whose existing DPUNode still belongs to the generic BF3
/// deployment while inventory now selects the GB200 deployment.
fn source_deployment_mock(dpu_count: usize) -> MockDpfOperations {
    source_deployment_mock_with_verification_observer(dpu_count, |_| {})
}

/// Builds a source deployment mock that reports each DPUNode label check to
/// `observer`.
fn source_deployment_mock_with_verification_observer(
    dpu_count: usize,
    observer: impl Fn(DpuDeploymentType) + Send + Sync + 'static,
) -> MockDpfOperations {
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device().returning(|_, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels()
        .returning(move |_, deployment| {
            observer(deployment);
            Ok(deployment == DpuDeploymentType::Bf3)
        });
    mock.expect_snapshot_host()
        .returning(move |_| Ok(snapshot_with_crs_present(dpu_count)));
    mock.expect_get_dpu_phase()
        .returning(|_, _| Ok(DpuPhase::Ready));
    mock
}

/// A request rechecks migration eligibility after Site Explorer changes the
/// host while holding its attachment locks.
#[crate::sqlx_test]
async fn test_gb200_deployment_migration_rechecks_after_attachment_updates(pool: sqlx::PgPool) {
    let mock = source_deployment_mock(2);
    let mut config = get_config_with_rack_profiles();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");
    mh.mark_machine_for_updates().await;

    // Stand in for Site Explorer changing the machine from a generic BF3 host
    // to a GB200 host while the request still sees the earlier data.
    let admin_lock_admission = db::machine_interface::admin_lock_admission().await;
    let mut attachment_txn = pool.begin().await.unwrap();
    db::machine_interface::lock_all_admin_segments(attachment_txn.as_mut())
        .await
        .unwrap();
    configure_gb200_b3240_host_in_txn(attachment_txn.as_mut(), &mh).await;

    let api = env.api.clone();
    let requested_dpu_id = mh.dpu_ids[0];
    let mut request_task = tokio::spawn(async move {
        api.trigger_dpu_reprovisioning(tonic::Request::new(
            ::rpc::forge::DpuReprovisioningRequest {
                dpu_id: Some(requested_dpu_id.into()),
                machine_id: None,
                mode: Mode::Set as i32,
                initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                update_firmware: true,
            },
        ))
        .await
    });

    assert!(
        timeout(Duration::from_millis(250), &mut request_task)
            .await
            .is_err(),
        "the request must wait for attachment updates before checking migration"
    );

    attachment_txn.commit().await.unwrap();
    drop(admin_lock_admission);
    let error = timeout(TEST_TIMEOUT, request_task)
        .await
        .expect("timed out after releasing the attachment locks")
        .expect("the reprovisioning request task panicked")
        .expect_err("an individual DPU request must be rejected after GB200 becomes visible");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    let mut txn = pool.begin().await.unwrap();
    let dpu_machines = mh.dpu_db_machines(&mut txn).await;
    assert_eq!(dpu_machines.len(), mh.dpu_ids.len());
    assert!(
        dpu_machines
            .iter()
            .all(|dpu| dpu.reprovision_requested.is_none()),
        "a rejected partial request must not update any attached DPU"
    );
    txn.commit().await.unwrap();
}

/// Verifies that a `Set` request preserves controller progress made after its
/// initial validation.
async fn assert_dpu_reprovision_set_rechecks_request_updates(
    pool: &sqlx::PgPool,
    env: &TestEnv,
    mh: &TestManagedHost,
) {
    let requested_dpu_id = mh.dpu_ids[0];
    let mut request_txn = pool.begin().await.unwrap();
    db::machine::trigger_dpu_reprovisioning_request(
        &requested_dpu_id,
        request_txn.as_mut(),
        "test",
        true,
    )
    .await
    .unwrap();
    request_txn.commit().await.unwrap();

    // Stand in for the controller owning the DPU row while the API validates
    // the current request.
    let mut controller_txn = pool.begin().await.unwrap();
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(controller_txn.as_mut())
        .await
        .unwrap();
    let locked_dpu = db::machine::find_one(
        controller_txn.as_mut(),
        &requested_dpu_id,
        MachineSearchConfig {
            for_update: true,
            ..MachineSearchConfig::default()
        },
    )
    .await
    .unwrap();
    assert!(
        locked_dpu.is_some(),
        "the controller must lock the test DPU"
    );

    let api = env.api.clone();
    let request_task = tokio::spawn(async move {
        api.trigger_dpu_reprovisioning(tonic::Request::new(
            ::rpc::forge::DpuReprovisioningRequest {
                dpu_id: Some(requested_dpu_id.into()),
                machine_id: None,
                mode: Mode::Set as i32,
                initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                update_firmware: true,
            },
        ))
        .await
    });

    // Both the protected row lock and the stale write it replaces must reach
    // the DPU row after initial validation before the controller moves it.
    tokio::select! {
        _ = wait_for_blocked_query(pool, blocker_pid, "SELECT row_to_json") => {}
        _ = wait_for_blocked_query(
            pool,
            blocker_pid,
            "UPDATE machines SET reprovisioning_requested",
        ) => {}
    }
    db::machine::update_dpu_reprovision_start_time(&requested_dpu_id, controller_txn.as_mut())
        .await
        .unwrap();
    controller_txn.commit().await.unwrap();

    let error = timeout(TEST_TIMEOUT, request_task)
        .await
        .expect("timed out after the controller released the DPU row")
        .expect("the reprovisioning request task panicked")
        .expect_err("the request must reject controller state advanced after validation");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(
        error.message(),
        "internal error: reprovisioning is already started"
    );

    let mut txn = pool.begin().await.unwrap();
    let dpu = db::machine::find_one(txn.as_mut(), &requested_dpu_id, Default::default())
        .await
        .unwrap()
        .unwrap();
    assert!(
        dpu.reprovision_requested
            .is_some_and(|request| request.started_at.is_some()),
        "the rejected API request must preserve the controller's start time"
    );
    txn.commit().await.unwrap();
}

/// An ordinary DPF reprovision request rechecks controller progress before
/// replacing the request JSON.
#[crate::sqlx_test]
async fn test_dpu_reprovision_set_rechecks_after_request_updates(pool: sqlx::PgPool) {
    let mock = provisioning_mock(Arc::new(AtomicBool::new(true)));
    let mut config = get_config();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");
    mh.mark_machine_for_updates().await;

    assert_dpu_reprovision_set_rechecks_request_updates(&pool, &env, &mh).await;
}

/// A GB200 request rechecks controller progress after reading deployment
/// labels, even when the target deployment is already active.
#[crate::sqlx_test]
async fn test_gb200_deployment_migration_rechecks_after_dpu_request_updates(pool: sqlx::PgPool) {
    let mock = provisioning_mock(Arc::new(AtomicBool::new(true)));
    let mut config = get_config_with_rack_profiles();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");
    configure_gb200_b3240_host(&pool, &mh).await;
    mh.mark_machine_for_updates().await;

    assert_dpu_reprovision_set_rechecks_request_updates(&pool, &env, &mh).await;
}

/// Build the DPU reprovision states map for the given DPF sub-state.
fn build_dpf_reprovision_states(
    dpu_ids: &[DpuMachineId],
    dpf_state: DpfState,
) -> DpuReprovisionStates {
    let states: HashMap<DpuMachineId, ReprovisionState> = dpu_ids
        .iter()
        .map(|id| {
            (
                *id,
                ReprovisionState::DpfStates {
                    substate: dpf_state.clone(),
                },
            )
        })
        .collect();
    DpuReprovisionStates { states }
}

/// Write a managed-host state directly to the database.
async fn write_host_state(pool: &sqlx::PgPool, host_id: &HostMachineId, state: &ManagedHostState) {
    let state_json = serde_json::to_value(state).unwrap();
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

/// Set the host to `DPUReprovision` with the given DPF sub-state for each DPU.
async fn set_reprovision_dpf_state(
    pool: &sqlx::PgPool,
    host_id: &HostMachineId,
    dpu_ids: &[DpuMachineId],
    dpf_state: DpfState,
) {
    let state = ManagedHostState::DPUReprovision {
        dpu_states: build_dpf_reprovision_states(dpu_ids, dpf_state),
    };
    write_host_state(pool, host_id, &state).await;
}

/// Set the host to `Assigned { InstanceState::DPUReprovision }` with the given DPF sub-state.
/// The host must already have a real instance allocated via `instance_builer().build_and_return()`.
async fn set_assigned_reprovision_dpf_state(
    pool: &sqlx::PgPool,
    host_id: &HostMachineId,
    dpu_ids: &[DpuMachineId],
    dpf_state: DpfState,
) {
    let state = ManagedHostState::Assigned {
        instance_state: InstanceState::DPUReprovision {
            dpu_states: build_dpf_reprovision_states(dpu_ids, dpf_state),
        },
    };
    write_host_state(pool, host_id, &state).await;
}

async fn dpu_device_names(pool: &sqlx::PgPool, mh: &TestManagedHost) -> HashSet<String> {
    let mut txn = pool.begin().await.unwrap();
    let mut names = HashSet::new();
    for dpu_id in &mh.dpu_ids {
        let dpu = db::machine::find_one(txn.as_mut(), dpu_id, Default::default())
            .await
            .unwrap()
            .unwrap();
        names.insert(dpu.dpf_id().unwrap());
    }
    names
}

/// Gives a DPF test host the rack and DPU inventory that select the GB200
/// deployment and returns the created rack ID.
async fn configure_gb200_b3240_host(pool: &sqlx::PgPool, mh: &TestManagedHost) -> RackId {
    let mut txn = pool.begin().await.unwrap();
    let rack_id = configure_gb200_b3240_host_in_txn(txn.as_mut(), mh).await;
    txn.commit().await.unwrap();

    rack_id
}

/// Gives a DPF test host the GB200 rack and DPU inventory in an existing
/// transaction and returns the created rack ID.
async fn configure_gb200_b3240_host_in_txn(
    conn: &mut sqlx::PgConnection,
    mh: &TestManagedHost,
) -> RackId {
    let rack_id = TestRackDbBuilder::new()
        .with_rack_profile_id(TEST_RMS_RACK_PROFILE_ID)
        .persist(&mut *conn)
        .await
        .unwrap();
    let rack_assignment = sqlx::query("UPDATE machines SET rack_id = $1 WHERE id = $2")
        .bind(rack_id.as_str())
        .bind(mh.id)
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        rack_assignment.rows_affected(),
        1,
        "the test host must be attached to the GB200 rack profile"
    );
    for dpu_id in &mh.dpu_ids {
        let dpu = db::machine::find_one(&mut *conn, dpu_id, Default::default())
            .await
            .unwrap()
            .unwrap();
        let mut hardware_info = dpu
            .status
            .hardware_info
            .expect("fixture DPU should have hardware information");
        hardware_info
            .dpu_info
            .as_mut()
            .expect("fixture DPU should have DPU information")
            .part_number = "900-9D3B6-00CN-PA0".to_string();
        db::machine_topology::set_topology_update_needed(&mut *conn, dpu_id, true)
            .await
            .unwrap();
        db::machine_topology::create_or_update(&mut *conn, dpu_id, &hardware_info)
            .await
            .unwrap();
    }

    rack_id
}

/// Recreates the partial DPF reprovision state that a controller without
/// deployment migration admission could persist during a rolling update.
async fn set_started_partial_dpf_reprovision(
    pool: &sqlx::PgPool,
    mh: &TestManagedHost,
    requested_dpu_index: usize,
) {
    let requested_dpu_id = mh.dpu_ids[requested_dpu_index];
    let mut txn = pool.begin().await.unwrap();
    db::machine::trigger_dpu_reprovisioning_request(&requested_dpu_id, txn.as_mut(), "test", true)
        .await
        .unwrap();
    db::machine::update_dpu_reprovision_start_time(&requested_dpu_id, txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let states = mh
        .dpu_ids
        .iter()
        .map(|dpu_id| {
            let state = if *dpu_id == requested_dpu_id {
                ReprovisionState::DpfStates {
                    substate: DpfState::Reprovisioning,
                }
            } else {
                ReprovisionState::NotUnderReprovision
            };
            (*dpu_id, state)
        })
        .collect();
    write_host_state(
        pool,
        &mh.id,
        &ManagedHostState::DPUReprovision {
            dpu_states: DpuReprovisionStates { states },
        },
    )
    .await;
}

/// Recreates a complete request set that an older controller has already
/// advanced unevenly during a rolling update.
async fn set_started_complete_dpf_reprovision_with_progress(
    pool: &sqlx::PgPool,
    mh: &TestManagedHost,
) {
    let mut txn = pool.begin().await.unwrap();
    for dpu_id in &mh.dpu_ids {
        db::machine::trigger_dpu_reprovisioning_request(dpu_id, txn.as_mut(), "test", true)
            .await
            .unwrap();
        db::machine::update_dpu_reprovision_start_time(dpu_id, txn.as_mut())
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();

    let states = mh
        .dpu_ids
        .iter()
        .enumerate()
        .map(|(index, dpu_id)| {
            let substate = if index == 0 {
                DpfState::WaitingForReady { phase_detail: None }
            } else {
                DpfState::Reprovisioning
            };
            (*dpu_id, ReprovisionState::DpfStates { substate })
        })
        .collect();
    write_host_state(
        pool,
        &mh.id,
        &ManagedHostState::DPUReprovision {
            dpu_states: DpuReprovisionStates { states },
        },
    )
    .await;
}

/// Reprovisioning handler: `DpfState::Reprovisioning` transitions the DPU
/// to `DpfState::WaitingForReady` under `DPUReprovision`.
#[crate::sqlx_test]
async fn test_dpf_reprovisioning_transitions_to_provisioning(pool: sqlx::PgPool) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let mut mock = provisioning_mock(device_ready);
    mock.expect_reprovision_dpu().returning(|_, _| Ok(()));
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

    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Reprovisioning).await;

    // One iteration: Reprovisioning -> WaitingForReady
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let host_state = get_host_state(&env, &mh).await;

    match &host_state {
        ManagedHostState::DPUReprovision { dpu_states } => {
            for (dpu_id, state) in &dpu_states.states {
                assert!(
                    matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::WaitingForReady { .. }
                        }
                    ),
                    "DPU {dpu_id} should be in DpfStates::WaitingForReady after Reprovisioning, got: {state:?}"
                );
            }
        }
        other => {
            panic!("Expected DPUReprovision state, got: {other:?}");
        }
    }
}

/// Provisioning handler under reprovisioning: `DpfState::Provisioning`
/// transitions all DPUs to `DpfState::WaitingForReady` under `DPUReprovision`.
#[crate::sqlx_test]
async fn test_dpf_provisioning_transitions_to_waiting_for_ready_during_reprovision(
    pool: sqlx::PgPool,
) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(provisioning_mock(device_ready.clone()));
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

    // Prevent the WaitingForReady handler from advancing past this state.
    device_ready.store(false, Ordering::SeqCst);

    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Provisioning).await;

    // Run several iterations: Provisioning -> WaitingForReady, then stays
    // in WaitingForReady because the device is not ready.
    for _ in 0..5 {
        timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
            .await
            .expect("timed out during state controller iteration");
    }

    let host_state = get_host_state(&env, &mh).await;

    match &host_state {
        ManagedHostState::DPUReprovision { dpu_states } => {
            for (dpu_id, state) in &dpu_states.states {
                assert!(
                    matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::WaitingForReady { .. }
                        }
                    ),
                    "DPU {dpu_id} should be in DpfStates::WaitingForReady after Provisioning, got: {state:?}"
                );
            }
        }
        other => {
            panic!("Expected DPUReprovision state, got: {other:?}");
        }
    }
}

/// When WaitingForReady completes during reprovisioning, the host must
/// transition to `PoweringOffHost` (the reprovisioning power-cycle path),
/// **not** to `HostInit` which is the initial-provisioning exit.
#[crate::sqlx_test]
async fn test_dpf_waiting_for_ready_exits_to_powering_off_host_during_reprovision(
    pool: sqlx::PgPool,
) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(provisioning_mock(device_ready));
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

    // Start with WaitingForReady under DPUReprovision, device is ready.
    set_reprovision_dpf_state(
        &pool,
        &mh.id,
        &mh.dpu_ids,
        DpfState::WaitingForReady { phase_detail: None },
    )
    .await;

    // Run iterations: enter maintenance -> release hold + check ready -> exit
    for _ in 0..5 {
        timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
            .await
            .expect("timed out during state controller iteration");
    }

    let host_state = get_host_state(&env, &mh).await;

    match &host_state {
        ManagedHostState::DPUReprovision { dpu_states } => {
            for (dpu_id, state) in &dpu_states.states {
                assert!(
                    matches!(state, ReprovisionState::WaitingForNetworkConfig),
                    "DPU {dpu_id} should be in WaitingForNetworkConfig after WaitingForReady during reprovision, got: {state:?}"
                );
            }
        }
        // It is acceptable if the state controller advanced past PoweringOffHost
        // within the 5 iterations, as long as it did NOT go to HostInit.
        ManagedHostState::HostInit { .. } => {
            panic!(
                "WaitingForReady during reprovisioning must NOT exit to HostInit. \
                 Expected DPUReprovision/PoweringOffHost."
            );
        }
        _other => {
            // May have advanced further in the reprovisioning flow; that's OK.
        }
    }
}

/// Build a capturing mock that records device names from `register_dpu_device`
/// and `reprovision_dpu`.
fn capturing_mock(
    dpu_ready: Arc<AtomicBool>,
    registered_devices: Arc<Mutex<Vec<String>>>,
    reprovisioned_devices: Arc<Mutex<Vec<String>>>,
    dpu_count: usize,
) -> MockDpfOperations {
    let mut mock = MockDpfOperations::new();

    mock.expect_register_dpu_device().returning(move |info, _| {
        registered_devices.lock().unwrap().push(info.device_id);
        Ok(())
    });

    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_deployment_type_for_dpu()
        .returning(|__, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_snapshot_host()
        .returning(move |_| Ok(snapshot_with_crs_present(dpu_count)));

    let reprovisioned_for_ready = reprovisioned_devices.clone();
    mock.expect_get_dpu_phase()
        .returning(move |device_name, _| {
            let ready_global = dpu_ready.load(Ordering::SeqCst);
            let repro = reprovisioned_for_ready.lock().unwrap();
            let ready_if_reprovisioned = repro.iter().any(|d| d == device_name);
            if ready_global || ready_if_reprovisioned {
                Ok(DpuPhase::Ready)
            } else {
                Ok(DpuPhase::Provisioning("OsInstalling".into()))
            }
        });

    mock.expect_reprovision_dpu()
        .returning(move |device_name, _| {
            reprovisioned_devices
                .lock()
                .unwrap()
                .push(device_name.to_string());
            Ok(())
        });

    mock
}

// ---------------------------------------------------------------------------
// Multi-DPU tests
// ---------------------------------------------------------------------------

/// Provisioning with multiple DPUs: `register_dpu_device` must be called for
/// every DPU, not just the first.
#[crate::sqlx_test]
async fn test_multi_dpu_provisioning_registers_all_devices(pool: sqlx::PgPool) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let registered_devices = Arc::new(Mutex::new(Vec::new()));
    let reprovisioned_devices = Arc::new(Mutex::new(Vec::new()));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(capturing_mock(
        device_ready.clone(),
        registered_devices.clone(),
        reprovisioned_devices.clone(),
        2,
    ));
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");
    assert_eq!(mh.dpu_ids.len(), 2, "Expected 2 DPUs");

    // Clear registrations captured during initial provisioning.
    registered_devices.lock().unwrap().clear();
    // Block WaitingForReady so we can observe the Provisioning -> WaitingForReady transition.
    device_ready.store(false, Ordering::SeqCst);

    // Put host into DPUReprovision / Provisioning with 2 DPUs.
    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Provisioning).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let registered: HashSet<String> = registered_devices
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .collect();
    let expected = dpu_device_names(&pool, &mh).await;
    assert_eq!(
        registered, expected,
        "register_dpu_device must be called for every DPU.\n\
         Registered: {registered:?}\n\
         Expected:   {expected:?}"
    );
}

/// GB200 host identity and every attached B3240 DPU must select one shared deployment.
#[crate::sqlx_test]
async fn test_gb200_b3240_pair_uses_specialized_deployment_from_report_or_rack(pool: sqlx::PgPool) {
    let classified_dpus = Arc::new(Mutex::new(Vec::new()));
    let verified_deployments = Arc::new(Mutex::new(Vec::new()));
    let registered_deployments = Arc::new(Mutex::new(Vec::new()));

    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device().returning(|_, _| Ok(()));
    let registered_deployments_for_mock = registered_deployments.clone();
    mock.expect_register_dpu_node().returning(move |info| {
        registered_deployments_for_mock
            .lock()
            .unwrap()
            .push(info.deployment_type);
        Ok(())
    });
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    let classified_dpus_for_mock = classified_dpus.clone();
    mock.expect_deployment_type_for_dpu()
        .returning(move |dpu, _| {
            classified_dpus_for_mock.lock().unwrap().push(dpu.id);
            Ok(DpuDeploymentType::Bf3)
        });
    let verified_deployments_for_mock = verified_deployments.clone();
    mock.expect_verify_node_labels()
        .returning(move |_, deployment_type| {
            verified_deployments_for_mock
                .lock()
                .unwrap()
                .push(deployment_type);
            Ok(true)
        });
    mock.expect_snapshot_host()
        .returning(|_| Ok(snapshot_with_crs_present(2)));
    mock.expect_get_dpu_phase()
        .returning(|_, _| Ok(DpuPhase::Ready));

    let mut config = get_config_with_rack_profiles();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");

    // Give the host a GB200 rack and both DPUs the supported B3240 identity.
    let rack_id = configure_gb200_b3240_host(&pool, &mh).await;

    // Ignore initial ingestion and observe one complete host deployment selection pass.
    classified_dpus.lock().unwrap().clear();
    verified_deployments.lock().unwrap().clear();
    registered_deployments.lock().unwrap().clear();
    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Provisioning).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let classified = classified_dpus.lock().unwrap().clone();
    assert_eq!(classified.len(), mh.dpu_ids.len());
    assert_eq!(
        classified.into_iter().collect::<HashSet<_>>(),
        mh.dpu_ids.iter().copied().map(Into::into).collect()
    );
    assert_eq!(
        *verified_deployments.lock().unwrap(),
        vec![DpuDeploymentType::Bf3Gb200]
    );
    assert_eq!(
        *registered_deployments.lock().unwrap(),
        vec![DpuDeploymentType::Bf3Gb200]
    );

    // Site Explorer can identify a GB200 before discovery assigns its rack.
    // Repeat the same selection with only the persisted Redfish model present.
    let mut txn = pool.begin().await.unwrap();
    sqlx::query("UPDATE machines SET rack_id = NULL WHERE id = $1")
        .bind(mh.id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    mh.host().set_exploration_model(&mut txn, "GB200 NVL").await;
    assert!(mh.host().db_machine(&mut txn).await.rack_id.is_none());
    txn.commit().await.unwrap();

    verified_deployments.lock().unwrap().clear();
    registered_deployments.lock().unwrap().clear();
    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Provisioning).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    assert_eq!(
        *verified_deployments.lock().unwrap(),
        vec![DpuDeploymentType::Bf3Gb200]
    );
    assert_eq!(
        *registered_deployments.lock().unwrap(),
        vec![DpuDeploymentType::Bf3Gb200]
    );

    // A recognized report takes precedence over the rack fallback. Restore
    // the GB200 rack and report GB300 so this pass selects generic BF3.
    let mut txn = pool.begin().await.unwrap();
    sqlx::query("UPDATE machines SET rack_id = $1 WHERE id = $2")
        .bind(rack_id.as_str())
        .bind(mh.id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    mh.host()
        .set_exploration_model(&mut txn, "DGX GB300 Compute Tray")
        .await;
    txn.commit().await.unwrap();

    verified_deployments.lock().unwrap().clear();
    registered_deployments.lock().unwrap().clear();
    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Provisioning).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    assert_eq!(
        *verified_deployments.lock().unwrap(),
        vec![DpuDeploymentType::Bf3]
    );
    assert_eq!(
        *registered_deployments.lock().unwrap(),
        vec![DpuDeploymentType::Bf3]
    );
}

/// A DPF-ingested GB200 host remains reprovisionable when runtime DPF support
/// is disabled and no SDK is installed.
#[crate::sqlx_test]
async fn test_runtime_dpf_disable_skips_deployment_migration_probe(pool: sqlx::PgPool) {
    let mut config = get_config_with_rack_profiles();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(source_deployment_mock(1))),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");
    configure_gb200_b3240_host(&pool, &mh).await;
    mh.mark_machine_for_updates().await;

    let mut disabled_config = get_config_with_rack_profiles();
    disabled_config.dpf.enabled = false;
    let disabled_api = TestApiBuilder::new(
        env.pool.clone(),
        env.api.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(Arc::new(disabled_config))
    .build();
    assert!(disabled_api.dpf_sdk.is_none());

    disabled_api
        .trigger_dpu_reprovisioning(tonic::Request::new(
            ::rpc::forge::DpuReprovisioningRequest {
                dpu_id: None,
                machine_id: Some(mh.id.into()),
                mode: Mode::Set as i32,
                initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                update_firmware: true,
            },
        ))
        .await
        .expect("runtime DPF disable must skip the deployment migration probe");

    let mut txn = pool.begin().await.unwrap();
    assert!(
        mh.dpu_n(0)
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_some(),
        "the request must persist without a DPF SDK"
    );
    txn.commit().await.unwrap();
}

/// A GB200 deployment migration rejects a request for one DPU, then proceeds
/// when a host request includes every attached DPU.
#[crate::sqlx_test]
async fn test_gb200_deployment_migration_requires_every_dpu(pool: sqlx::PgPool) {
    let node_uses_target_labels = Arc::new(AtomicBool::new(false));
    let target_dpu_phase = Arc::new(AtomicUsize::new(0));
    let released_holds = Arc::new(AtomicUsize::new(0));
    let transferred_deployments = Arc::new(Mutex::new(Vec::new()));
    let deleted_source_devices = Arc::new(Mutex::new(Vec::new()));

    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device().returning(|_, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    let released_holds_for_mock = released_holds.clone();
    mock.expect_release_maintenance_hold().returning(move |_| {
        released_holds_for_mock.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    let node_uses_target_labels_for_verify = node_uses_target_labels.clone();
    mock.expect_verify_node_labels()
        .returning(move |_, deployment| {
            let expected = if node_uses_target_labels_for_verify.load(Ordering::SeqCst) {
                DpuDeploymentType::Bf3Gb200
            } else {
                DpuDeploymentType::Bf3
            };
            Ok(deployment == expected)
        });
    mock.expect_snapshot_host()
        .returning(|_| Ok(snapshot_with_crs_present(2)));
    mock.expect_get_dpu_phase()
        .returning(|_, _| Ok(DpuPhase::Ready));
    let target_dpu_phase_for_mock = target_dpu_phase.clone();
    mock.expect_get_dpu_phases_for_deployment_type().returning(
        move |device_names, _, deployment_type| {
            assert_eq!(deployment_type, DpuDeploymentType::Bf3Gb200);
            match target_dpu_phase_for_mock.load(Ordering::SeqCst) {
                0 => Ok(None),
                1 => Ok(Some(
                    device_names
                        .iter()
                        .map(|name| {
                            (
                                name.clone(),
                                DpuPhase::Provisioning("OsInstalling".to_string()),
                            )
                        })
                        .collect::<BTreeMap<_, _>>(),
                )),
                2 => Ok(Some(
                    device_names
                        .iter()
                        .map(|name| (name.clone(), DpuPhase::Ready))
                        .collect::<BTreeMap<_, _>>(),
                )),
                _ => Err(DpfError::InvalidState(
                    "target DPU has the wrong flavor".to_string(),
                )),
            }
        },
    );
    let transferred_deployments_for_mock = transferred_deployments.clone();
    mock.expect_transfer_dpu_node_deployment_labels()
        .returning(move |_, source, target| {
            let mut transfers = transferred_deployments_for_mock.lock().unwrap();
            transfers.push((source, target));
            node_uses_target_labels.store(true, Ordering::SeqCst);
            Ok(())
        });
    let deleted_source_devices_for_mock = deleted_source_devices.clone();
    mock.expect_delete_source_dpus_for_deployment_migration()
        .returning(move |device_names, _, source, target| {
            assert_eq!(source, DpuDeploymentType::Bf3);
            assert_eq!(target, DpuDeploymentType::Bf3Gb200);
            *deleted_source_devices_for_mock.lock().unwrap() = device_names.to_vec();
            Ok(())
        });

    let mut config = get_config_with_rack_profiles();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");
    configure_gb200_b3240_host(&pool, &mh).await;
    let released_holds_before_migration = released_holds.load(Ordering::SeqCst);

    mh.mark_machine_for_updates().await;
    let partial_request_error = env
        .api
        .trigger_dpu_reprovisioning(tonic::Request::new(
            ::rpc::forge::DpuReprovisioningRequest {
                dpu_id: None,
                machine_id: Some(mh.dpu_ids[0].into()),
                mode: Mode::Set as i32,
                initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                update_firmware: true,
            },
        ))
        .await;
    let partial_request_error =
        partial_request_error.expect_err("a request for one DPU must be rejected");
    assert_eq!(
        partial_request_error.code(),
        tonic::Code::FailedPrecondition
    );
    assert!(
        partial_request_error.message().contains(&mh.id.to_string()),
        "the rejection must identify the request using the host ID that can migrate the full DPU set"
    );

    assert!(
        matches!(get_host_state(&env, &mh).await, ManagedHostState::Ready),
        "a rejected partial request must not change the host state"
    );
    assert!(transferred_deployments.lock().unwrap().is_empty());
    assert!(deleted_source_devices.lock().unwrap().is_empty());

    let mut txn = pool.begin().await.unwrap();
    let first_request = mh.dpu_n(0).db_machine(&mut txn).await.reprovision_requested;
    assert!(first_request.is_none());
    assert!(
        mh.dpu_n(1)
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_none()
    );
    txn.commit().await.unwrap();

    mh.host().trigger_dpu_reprovisioning(Mode::Set, true).await;

    let partial_clear_error = env
        .api
        .trigger_dpu_reprovisioning(tonic::Request::new(
            ::rpc::forge::DpuReprovisioningRequest {
                dpu_id: None,
                machine_id: Some(mh.dpu_ids[0].into()),
                mode: Mode::Clear as i32,
                initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                update_firmware: true,
            },
        ))
        .await
        .expect_err("clearing one DPU from a complete migration request must be rejected");
    assert_eq!(partial_clear_error.code(), tonic::Code::FailedPrecondition);

    let mut txn = pool.begin().await.unwrap();
    for dpu_index in 0..mh.dpu_ids.len() {
        assert!(
            mh.dpu_n(dpu_index)
                .db_machine(&mut txn)
                .await
                .reprovision_requested
                .is_some(),
            "a rejected partial clear must preserve every migration request"
        );
    }
    txn.commit().await.unwrap();

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while starting the complete DPU set");

    let started_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            started_state,
            ManagedHostState::DPUReprovision { ref dpu_states }
                if dpu_states.states.values().all(|state| {
                    matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::Reprovisioning
                        }
                    )
                })
        ),
        "the host request must start every DPU in the source deployment: {started_state:?}"
    );

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while parking deployment migration");

    let parked_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            &parked_state,
            ManagedHostState::DPUReprovision { dpu_states }
                if dpu_states.states.values().all(|state| {
                    matches!(state, ReprovisionState::NotUnderReprovision)
                })
        ),
        "the complete DPU set must be parked before changing selectors: {parked_state:?}"
    );
    assert!(transferred_deployments.lock().unwrap().is_empty());

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during deployment migration");

    let migrated_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            migrated_state,
            ManagedHostState::DPUReprovision { ref dpu_states }
                if dpu_states.states.values().all(|state| {
                    matches!(state, ReprovisionState::NotUnderReprovision)
                })
        ),
        "the migration must remain parked until every target DPU is observed: {migrated_state:?}"
    );
    assert_eq!(
        *transferred_deployments.lock().unwrap(),
        vec![(DpuDeploymentType::Bf3, DpuDeploymentType::Bf3Gb200)]
    );
    let expected_devices = dpu_device_names(&pool, &mh).await;
    assert_eq!(
        deleted_source_devices
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<HashSet<_>>(),
        expected_devices
    );

    // A source DPU can briefly retain the deterministic name after the
    // selector changes. Keep the complete set parked until target ownership
    // is observed for every DPU in one snapshot.
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while checking a stale source DPU");

    let stale_source_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            stale_source_state,
            ManagedHostState::DPUReprovision { ref dpu_states }
                if dpu_states.states.values().all(|state| {
                    matches!(state, ReprovisionState::NotUnderReprovision)
                })
        ),
        "source ownership must keep every DPU parked: {stale_source_state:?}"
    );
    assert_eq!(
        released_holds.load(Ordering::SeqCst),
        released_holds_before_migration,
        "a DPU owned by the source must not release the target deployment's hold"
    );

    target_dpu_phase.store(3, Ordering::SeqCst);
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while checking target deployment configuration drift");

    let mismatched_target_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            &mismatched_target_state,
            ManagedHostState::Failed { details, .. }
                if matches!(
                    &details.cause,
                    FailureCause::DpfProvisioning { err }
                        if err.contains("target DPU has the wrong flavor")
                )
        ),
        "a Ready target DPU with configuration drift must fail visibly: {mismatched_target_state:?}"
    );
    assert_eq!(
        released_holds.load(Ordering::SeqCst),
        released_holds_before_migration,
        "a mismatched target DPU must not release the maintenance hold"
    );

    // Restore the parked checkpoint to continue exercising the successful path.
    write_host_state(&pool, &mh.id, &parked_state).await;

    target_dpu_phase.store(1, Ordering::SeqCst);
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while checking target deployment provisioning");

    let target_provisioning_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            target_provisioning_state,
            ManagedHostState::DPUReprovision { ref dpu_states }
                if dpu_states.states.values().all(|state| {
                    matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::WaitingForReady { .. }
                        }
                    )
                })
        ),
        "observing the complete target set must move every DPU to WaitingForReady: {target_provisioning_state:?}"
    );
    assert_eq!(
        released_holds.load(Ordering::SeqCst),
        released_holds_before_migration,
        "changing the durable migration marker must not release the maintenance hold"
    );

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while processing target deployment provisioning");

    let target_provisioning_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            target_provisioning_state,
            ManagedHostState::DPUReprovision { ref dpu_states }
                if dpu_states.states.values().all(|state| {
                    matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::WaitingForReady { .. }
                        }
                    )
                })
        ),
        "target provisioning must remain in WaitingForReady: {target_provisioning_state:?}"
    );
    assert_eq!(
        released_holds.load(Ordering::SeqCst),
        released_holds_before_migration + 1,
        "a target DPU may release the shared maintenance hold while provisioning"
    );

    target_dpu_phase.store(2, Ordering::SeqCst);
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while checking the target deployment");

    let progressing_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            progressing_state,
            ManagedHostState::DPUReprovision { ref dpu_states }
                if dpu_states.states.values().any(|state| {
                    matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::DeviceReady
                        }
                    )
                })
        ),
        "the target deployment labels must let reprovisioning continue: {progressing_state:?}"
    );
}

/// A partial request started before the migration gate was rolled
/// out continues under its current deployment instead of entering a terminal
/// migration failure.
#[crate::sqlx_test]
async fn test_started_partial_migration_continues_under_source_deployment(pool: sqlx::PgPool) {
    let reprovisioned_devices = Arc::new(Mutex::new(Vec::new()));
    let mut mock = source_deployment_mock(2);
    let reprovisioned_devices_for_mock = reprovisioned_devices.clone();
    mock.expect_reprovision_dpu()
        .times(1)
        .returning(move |device_name, _| {
            reprovisioned_devices_for_mock
                .lock()
                .unwrap()
                .push(device_name.to_string());
            Ok(())
        });

    let mut config = get_config_with_rack_profiles();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");
    configure_gb200_b3240_host(&pool, &mh).await;
    set_started_partial_dpf_reprovision(&pool, &mh, 0).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while continuing the source deployment");

    let waiting_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            waiting_state,
            ManagedHostState::DPUReprovision { ref dpu_states }
                if matches!(
                    dpu_states.states.get(&mh.dpu_ids[0]),
                    Some(ReprovisionState::DpfStates {
                        substate: DpfState::WaitingForReady { .. }
                    })
                ) && matches!(
                    dpu_states.states.get(&mh.dpu_ids[1]),
                    Some(ReprovisionState::NotUnderReprovision)
                )
        ),
        "the DPU already being reprovisioned must continue without moving the shared selector: {waiting_state:?}"
    );
    assert_eq!(reprovisioned_devices.lock().unwrap().len(), 1);

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while accepting source deployment readiness");

    let ready_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            ready_state,
            ManagedHostState::DPUReprovision { ref dpu_states }
                if matches!(
                    dpu_states.states.get(&mh.dpu_ids[0]),
                    Some(ReprovisionState::DpfStates {
                        substate: DpfState::DeviceReady
                    })
                ) && matches!(
                    dpu_states.states.get(&mh.dpu_ids[1]),
                    Some(ReprovisionState::NotUnderReprovision)
                )
        ),
        "source labels must remain valid until the existing request finishes: {ready_state:?}"
    );
}

/// A complete request already advanced by an older controller finishes under
/// BF3 instead of failing when its DPU states are no longer synchronized at the
/// migration handoff.
#[crate::sqlx_test]
async fn test_started_complete_migration_with_progress_continues_under_source_deployment(
    pool: sqlx::PgPool,
) {
    let mut mock = source_deployment_mock(2);
    mock.expect_reprovision_dpu().returning(|_, _| Ok(()));

    let mut config = get_config_with_rack_profiles();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");
    configure_gb200_b3240_host(&pool, &mh).await;
    set_started_complete_dpf_reprovision_with_progress(&pool, &mh).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out while continuing the progressed request under BF3");

    let state = get_host_state(&env, &mh).await;
    let ManagedHostState::DPUReprovision { ref dpu_states } = state else {
        panic!("an existing request must remain in DPUReprovision under BF3: {state:?}");
    };
    let stayed_in_dpf = dpu_states
        .states
        .values()
        .all(|dpu_state| matches!(dpu_state, ReprovisionState::DpfStates { .. }));
    let reached_device_ready = dpu_states.states.values().any(|dpu_state| {
        matches!(
            dpu_state,
            ReprovisionState::DpfStates {
                substate: DpfState::DeviceReady
            }
        )
    });
    let still_reprovisioning = dpu_states.states.values().any(|dpu_state| {
        matches!(
            dpu_state,
            ReprovisionState::DpfStates {
                substate: DpfState::Reprovisioning
            }
        )
    });

    assert!(
        stayed_in_dpf && (reached_device_ready || !still_reprovisioning),
        "an existing request must make progress under BF3 without entering the migration handoff: {state:?}"
    );
}

/// A mixed DPU pair must enter a terminal failure without starting a DPF registration attempt.
#[crate::sqlx_test]
async fn test_mixed_dpu_deployment_types_fail_without_registration(pool: sqlx::PgPool) {
    let mixed_pair = Arc::new(AtomicBool::new(false));
    let deployment_type_calls = Arc::new(AtomicUsize::new(0));
    let registered_devices = Arc::new(AtomicUsize::new(0));
    let registered_nodes = Arc::new(AtomicUsize::new(0));

    let mut mock = MockDpfOperations::new();
    let registered_devices_for_mock = registered_devices.clone();
    mock.expect_register_dpu_device().returning(move |_, _| {
        registered_devices_for_mock.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let registered_nodes_for_mock = registered_nodes.clone();
    mock.expect_register_dpu_node().returning(move |_| {
        registered_nodes_for_mock.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    let mixed_pair_for_mock = mixed_pair.clone();
    let deployment_type_calls_for_mock = deployment_type_calls.clone();
    mock.expect_deployment_type_for_dpu()
        .returning(move |_, _| {
            if !mixed_pair_for_mock.load(Ordering::SeqCst) {
                return Ok(DpuDeploymentType::Bf3);
            }
            let index = deployment_type_calls_for_mock.fetch_add(1, Ordering::SeqCst);
            Ok(if index.is_multiple_of(2) {
                DpuDeploymentType::Bf3
            } else {
                DpuDeploymentType::Bf4Generic
            })
        });
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_snapshot_host()
        .returning(|_| Ok(snapshot_with_crs_present(2)));
    mock.expect_get_dpu_phase()
        .returning(|_, _| Ok(DpuPhase::Ready));

    let mut config = get_config();
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");

    mixed_pair.store(true, Ordering::SeqCst);

    // A mixed selection must become a visible terminal failure both before and after DPF resource
    // registration. In either phase, it must not make a partial registration attempt.
    for (scenario, dpf_state) in [
        ("during provisioning", DpfState::Provisioning),
        (
            "while waiting for DPF",
            DpfState::WaitingForReady { phase_detail: None },
        ),
    ] {
        registered_devices.store(0, Ordering::SeqCst);
        registered_nodes.store(0, Ordering::SeqCst);
        deployment_type_calls.store(0, Ordering::SeqCst);
        set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, dpf_state).await;

        timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
            .await
            .expect("timed out during mixed-pair state controller iteration");

        assert_eq!(registered_devices.load(Ordering::SeqCst), 0, "{scenario}");
        assert_eq!(registered_nodes.load(Ordering::SeqCst), 0, "{scenario}");
        assert!(
            matches!(
                get_host_state(&env, &mh).await,
                ManagedHostState::Failed { details, .. }
                    if matches!(
                        &details.cause,
                        FailureCause::DpfProvisioning { err }
                            if err.starts_with("DPF deployment selection failed:")
                                && err.contains("mixed DPF deployment types")
                    )
            ),
            "{scenario}"
        );
    }
}

/// Reprovisioning with multiple DPUs: each DPU in Reprovisioning is
/// reprovisioned when its DpfState is reconciled. Run iterations until
/// all DPUs have been reprovisioned.
#[crate::sqlx_test]
async fn test_multi_dpu_reprovisioning_calls_all_dpus(pool: sqlx::PgPool) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let registered_devices = Arc::new(Mutex::new(Vec::new()));
    let reprovisioned_devices = Arc::new(Mutex::new(Vec::new()));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(capturing_mock(
        device_ready.clone(),
        registered_devices,
        reprovisioned_devices.clone(),
        2,
    ));
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");
    assert_eq!(mh.dpu_ids.len(), 2, "Expected 2 DPUs");

    device_ready.store(false, Ordering::SeqCst);
    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Reprovisioning).await;

    let expected = dpu_device_names(&pool, &mh).await;
    for _ in 0..10 {
        timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
            .await
            .expect("timed out during state controller iteration");
        let reprovisioned: HashSet<String> = reprovisioned_devices
            .lock()
            .unwrap()
            .clone()
            .into_iter()
            .collect();
        if reprovisioned == expected {
            break;
        }
    }

    let reprovisioned: HashSet<String> = reprovisioned_devices
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .collect();
    assert_eq!(
        reprovisioned, expected,
        "Every DPU marked for reprovisioning must be reprovisioned when its DpfState is reconciled.\n\
         Reprovisioned: {reprovisioned:?}\n\
         Expected:      {expected:?}"
    );
}

// ---------------------------------------------------------------------------
// Assigned / InstanceState::DPUReprovision tests
// ---------------------------------------------------------------------------

/// DPF reprovisioning under `Assigned { DPUReprovision }` transitions to
/// `WaitingForReady` without returning `InvalidState`.
#[crate::sqlx_test]
async fn test_assigned_dpf_reprovisioning_transitions_to_provisioning(pool: sqlx::PgPool) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let mut mock = provisioning_mock(device_ready);
    mock.expect_reprovision_dpu().returning(|_, _| Ok(()));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    // Allocate a real instance so the InstanceStateHandler has valid instance data.
    let (_tinstance, _rpc_instance) = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build_and_return()
        .await;

    set_assigned_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Reprovisioning).await;

    // One iteration: Reprovisioning -> WaitingForReady
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let host_state = get_host_state(&env, &mh).await;
    match &host_state {
        ManagedHostState::Assigned {
            instance_state:
                InstanceState::DPUReprovision {
                    dpu_states: DpuReprovisionStates { states },
                },
        } => {
            for (dpu_id, state) in states {
                assert!(
                    matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::WaitingForReady { .. },
                        }
                    ),
                    "DPU {dpu_id} expected WaitingForReady, got: {state:?}"
                );
            }
        }
        other => {
            panic!("Expected Assigned/DPUReprovision with WaitingForReady, got: {other:?}");
        }
    }
}

/// `WaitingForReady` under `Assigned { DPUReprovision }` exits to
/// `PoweringOffHost`, not `HostInit`.
#[crate::sqlx_test]
async fn test_assigned_waiting_for_ready_exits_to_powering_off_host(pool: sqlx::PgPool) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(provisioning_mock(device_ready));
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    // Allocate a real instance so the InstanceStateHandler has valid instance data.
    let (_tinstance, _rpc_instance) = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build_and_return()
        .await;

    set_assigned_reprovision_dpf_state(
        &pool,
        &mh.id,
        &mh.dpu_ids,
        DpfState::WaitingForReady { phase_detail: None },
    )
    .await;

    for _ in 0..5 {
        timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
            .await
            .expect("timed out during state controller iteration");
    }

    let host_state = get_host_state(&env, &mh).await;
    match &host_state {
        ManagedHostState::Assigned {
            instance_state:
                InstanceState::DPUReprovision {
                    dpu_states: DpuReprovisionStates { states },
                },
        } => {
            for (dpu_id, state) in states {
                assert!(
                    matches!(state, ReprovisionState::WaitingForNetworkConfig),
                    "DPU {dpu_id} should be PoweringOffHost after WaitingForReady \
                     during assigned reprovision, got: {state:?}"
                );
            }
        }
        ManagedHostState::HostInit { .. } => {
            panic!(
                "WaitingForReady during assigned reprovisioning must NOT exit to HostInit. \
                 Expected Assigned/DPUReprovision/PoweringOffHost."
            );
        }
        _other => {
            // May have advanced further in the reprovisioning flow; that's OK.
        }
    }
}

/// Each DPU is reprovisioned independently: the per-DPU handler advances
/// one DPU per iteration. After enough iterations both DPUs complete the
/// DPF cycle and reach PoweringOffHost.
#[crate::sqlx_test]
async fn test_multi_dpu_reprovisioning_per_dpu(pool: sqlx::PgPool) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let registered_devices = Arc::new(Mutex::new(Vec::new()));
    let reprovisioned_devices = Arc::new(Mutex::new(Vec::new()));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(capturing_mock(
        device_ready.clone(),
        registered_devices,
        reprovisioned_devices.clone(),
        2,
    ));
    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
        .await
        .expect("timed out during initial provisioning");
    assert_eq!(mh.dpu_ids.len(), 2, "Expected 2 DPUs");

    device_ready.store(false, Ordering::SeqCst);
    reprovisioned_devices.lock().unwrap().clear();
    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Reprovisioning).await;

    for _ in 0..10 {
        timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
            .await
            .expect("timed out during state controller iteration");
    }

    let reprovisioned: HashSet<String> = reprovisioned_devices
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .collect();
    let expected = dpu_device_names(&pool, &mh).await;
    assert_eq!(
        reprovisioned, expected,
        "Both DPUs must be reprovisioned after multiple iterations.\n\
         Reprovisioned: {reprovisioned:?}\n\
         Expected:      {expected:?}"
    );

    let host_state = get_host_state(&env, &mh).await;
    match &host_state {
        ManagedHostState::DPUReprovision { dpu_states } => {
            for (dpu_id, state) in &dpu_states.states {
                assert!(
                    !matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::Reprovisioning
                        }
                    ),
                    "DPU {dpu_id} should have completed DPF reprovisioning, got: {state:?}"
                );
            }
        }
        other => {
            panic!("Expected DPUReprovision state, got: {other:?}");
        }
    }
}

/// Unknown DPF state during reprovisioning transitions to Provisioning.
#[crate::sqlx_test]
async fn test_unknown_dpf_state_transitions_to_provisioning_during_reprovision(pool: sqlx::PgPool) {
    let device_ready = Arc::new(AtomicBool::new(true));
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(provisioning_mock(device_ready));
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

    set_reprovision_dpf_state(&pool, &mh.id, &mh.dpu_ids, DpfState::Unknown).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let host_state = get_host_state(&env, &mh).await;
    match &host_state {
        ManagedHostState::DPUReprovision { dpu_states } => {
            for (dpu_id, state) in &dpu_states.states {
                assert!(
                    matches!(
                        state,
                        ReprovisionState::DpfStates {
                            substate: DpfState::Provisioning
                        }
                    ),
                    "DPU {dpu_id} should transition from Unknown to Provisioning, got: {state:?}"
                );
            }
        }
        other => panic!("Expected DPUReprovision, got: {other:?}"),
    }
}
