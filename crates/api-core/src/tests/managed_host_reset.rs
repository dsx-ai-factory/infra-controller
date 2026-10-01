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

//! Operator-requested managed host reset, as reached through
//! `managed-host reset set|clear|list`.

use std::sync::Arc;
use std::time::Duration;

use carbide_dpf::types::{DpuDeviceSummary, DpuNodeSummary, HostDpfSnapshot};
use carbide_dpf::{DpuDeploymentType, DpuPhase};
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_uuid::machine::MachineId;
use model::machine::{
    CleanupContext, CleanupState, DpuDiscoveringState, FailureDetails, ManagedHostState, ResetState,
};
use rpc::forge::forge_server::Forge;
use rpc::forge::managed_host_reset_request::Mode;
use rpc::forge::{ManagedHostResetListRequest, ManagedHostResetRequest, UpdateInitiator};
use tokio::time::timeout;
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::test_managed_host::TestManagedHost;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_managed_host_with_dpf,
    create_managed_host_with_dpf_multi, create_test_env, create_test_env_with_overrides,
    get_config, network_configured_with_health,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Expectations for the initial provisioning flow, shared by every env below.
fn provisioning_mock() -> MockDpfOperations {
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device().returning(|_, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_get_dpu_phase()
        .returning(|_, _| Ok(DpuPhase::Ready));
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    // BMC MACs are recorded before discovery, so its DPU service lookup is reached, not skipped.
    crate::tests::dpf::expect_dpf_service_inventory(&mut mock);
    mock
}

/// A reset is only offered to DPF-ingested hosts, and `create_managed_host_with_dpf`
/// drives the real provisioning flow, so these tests need DPF enabled in config plus an
/// SDK for it to register against. Same starting point as the `dpf` suites.
async fn dpf_test_env(pool: sqlx::PgPool) -> TestEnv {
    env_with_dpf_mock(pool, provisioning_mock()).await
}

/// What `snapshot_host` reports for the host's DPF CRs, which is the only thing
/// `DeletingCrs` polls on.
#[derive(Clone, Copy)]
enum DpfCrs {
    Present { dpu_count: usize },
    Gone,
}

async fn reset_controller_env(pool: sqlx::PgPool, crs: DpfCrs) -> TestEnv {
    let mut mock = provisioning_mock();
    mock.expect_force_delete_host().returning(|_, _| Ok(()));
    mock.expect_snapshot_host()
        .returning(move |_| Ok(host_dpf_snapshot(crs)));

    env_with_dpf_mock(pool, mock).await
}

/// A present CR set must match the fixture's DPU count. An incomplete set
/// reports a teardown error instead of waiting for all CRs to disappear.
fn host_dpf_snapshot(crs: DpfCrs) -> HostDpfSnapshot {
    match crs {
        DpfCrs::Gone => HostDpfSnapshot {
            dpu_node: None,
            dpu_devices: Vec::new(),
            dpus: Vec::new(),
        },
        DpfCrs::Present { dpu_count } => {
            let device_names = (0..dpu_count)
                .map(|index| format!("device-{index}"))
                .collect::<Vec<_>>();
            HostDpfSnapshot {
                dpu_node: Some(DpuNodeSummary {
                    name: "node-mock".to_string(),
                    labels: Default::default(),
                    annotations: Default::default(),
                    dpu_device_refs: device_names.clone(),
                }),
                dpu_devices: device_names
                    .into_iter()
                    .map(|name| DpuDeviceSummary {
                        name,
                        labels: Default::default(),
                        bmc_ip: None,
                        bmc_port: None,
                        serial_number: String::new(),
                    })
                    .collect(),
                dpus: Vec::new(),
            }
        }
    }
}

async fn env_with_dpf_mock(pool: sqlx::PgPool, mock: MockDpfOperations) -> TestEnv {
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock);

    let mut config = get_config();
    config.dpf = crate::cfg::file::DpfConfig {
        enabled: true,
        deployments: crate::cfg::file::DpfDeploymentsConfig {
            bf3: crate::cfg::file::DpfDeploymentConfig {
                bfb_url: Some("http://example.com/test.bfb".to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };

    create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await
}

async fn dpf_ingested_host(env: &TestEnv) -> TestManagedHost {
    timeout(TEST_TIMEOUT, create_managed_host_with_dpf(env))
        .await
        .expect("timed out during initial provisioning")
}

fn reset_request(machine_id: MachineId, mode: Mode) -> Request<ManagedHostResetRequest> {
    Request::new(ManagedHostResetRequest {
        machine_id: Some(machine_id),
        mode: mode.into(),
        initiator: UpdateInitiator::AdminCli.into(),
        allow_reset_with_instance: false,
        ignore_cleanup: false,
    })
}

/// The operator's acknowledgement that the reset may destroy a live instance.
fn reset_request_allowing_instance(machine_id: MachineId) -> Request<ManagedHostResetRequest> {
    let mut request = reset_request(machine_id, Mode::Set);
    request.get_mut().allow_reset_with_instance = true;
    request
}

/// `Set` persists a pending request in `machines.reset_requested` that both
/// the controller and `reset list` can read. With no Instance to retain, Reset
/// proceeds to DPF teardown even when no DPU network observation is available.
#[crate::sqlx_test]
async fn reset_without_instance_records_request_and_skips_network_wait(pool: sqlx::PgPool) {
    let env = dpf_test_env(pool).await;
    let managed_host = dpf_ingested_host(&env).await;
    managed_host.mark_machine_for_updates().await;
    let host_id: MachineId = managed_host.id.into();

    // Nothing is pending beforehand, so the list is selecting on the column rather than
    // returning every host.
    let listed = env
        .api
        .list_managed_hosts_waiting_for_reset(Request::new(ManagedHostResetListRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert!(listed.hosts.is_empty());

    env.api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap();

    let mut txn = env.db_txn().await;
    let request = managed_host
        .host()
        .db_machine(&mut txn)
        .await
        .reset_requested
        .expect("the reset request should persist on the host row");
    assert_eq!(request.initiator, UpdateInitiator::AdminCli.as_str_name());
    assert!(
        request.started_at.is_none(),
        "a fresh request is unstarted, which is the condition the controller hinge fires on"
    );

    let listed = env
        .api
        .list_managed_hosts_waiting_for_reset(Request::new(ManagedHostResetListRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.hosts.len(), 1);
    assert_eq!(listed.hosts[0].id, Some(host_id));
    assert_eq!(
        listed.hosts[0].initiator,
        UpdateInitiator::AdminCli.as_str_name()
    );
    assert!(listed.hosts[0].requested_at.is_some());
    assert!(
        listed.hosts[0].started_at.is_none(),
        "the operator reads an absent Started At as 'the controller has not picked this up'"
    );
    drop(txn);

    // An unassigned host can need Reset precisely because its agent never
    // reported a usable configuration. There is no tenant allocation to retain.
    sqlx::query("UPDATE machines SET network_status_observation = NULL WHERE id = $1")
        .bind(managed_host.dpu_ids[0])
        .execute(&env.pool)
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        3,
        ManagedHostState::Reset {
            reset_state: ResetState::DeletingCrs,
        },
    )
    .await;
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert!(snapshot.instance.is_none());
    assert!(
        snapshot.dpu_snapshots[0]
            .network_status_observation
            .is_none()
    );
    assert!(
        snapshot
            .host_snapshot
            .reset_requested
            .as_ref()
            .unwrap()
            .started_at
            .is_some()
    );
}

/// A reset tears down every DPU attached to a host, so it is only expressible against the
/// host; and the host-update alert is what takes the host out of allocation before its
/// data is destroyed. Neither refusal may leave a half-recorded reset behind.
#[crate::sqlx_test]
async fn reset_set_rejects_a_dpu_target_and_an_unacknowledged_host(pool: sqlx::PgPool) {
    let env = dpf_test_env(pool).await;
    let managed_host = dpf_ingested_host(&env).await;

    let error = env
        .api
        .trigger_managed_host_reset(reset_request(managed_host.dpu().id.into(), Mode::Set))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(
        error.message().contains("not a host machine"),
        "unexpected message: {}",
        error.message()
    );

    // DPF-ingested, but no operator has taken it out of service yet.
    let error = env
        .api
        .trigger_managed_host_reset(reset_request(managed_host.id.into(), Mode::Set))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(
        error.message().contains("HostUpdateInProgress"),
        "unexpected message: {}",
        error.message()
    );

    let mut txn = env.db_txn().await;
    assert!(
        managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .reset_requested
            .is_none()
    );
}

/// Re-ingestion re-registers the host's DPF CRs, so a host that was never ingested through
/// DPF has nothing to come back as and is refused before anything is recorded.
#[crate::sqlx_test]
async fn reset_set_rejects_a_host_not_ingested_through_dpf(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    // Carries the alert, so the DPF gate is the only precondition it can fail.
    managed_host.mark_machine_for_updates().await;

    let error = env
        .api
        .trigger_managed_host_reset(reset_request(managed_host.id.into(), Mode::Set))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("not ingested via DPF"),
        "unexpected message: {}",
        error.message()
    );

    let mut txn = env.db_txn().await;
    assert!(
        managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .reset_requested
            .is_none()
    );
}

/// A reset destroys a live instance and its data, so the flag is the only thing allowing it.
#[crate::sqlx_test]
async fn reset_set_destroys_a_live_instance_only_when_the_operator_allows_it(pool: sqlx::PgPool) {
    let env = dpf_test_env(pool).await;
    let managed_host = dpf_ingested_host(&env).await;
    let host_id: MachineId = managed_host.id.into();

    // The required alert prevents allocation, so allocate before marking for updates.
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let _instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    managed_host.mark_machine_for_updates().await;

    let error = env
        .api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("--allow-reset-with-instance"),
        "the refusal has to name the flag that overrides it: {}",
        error.message()
    );

    let mut txn = env.db_txn().await;
    assert!(
        managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .reset_requested
            .is_none()
    );
    drop(txn);

    // Same host, same live instance: the acknowledgement is the only thing that changes.
    env.api
        .trigger_managed_host_reset(reset_request_allowing_instance(host_id))
        .await
        .unwrap();

    let mut txn = env.db_txn().await;
    assert!(
        managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .reset_requested
            .is_some(),
        "an acknowledged reset has to be recorded for the controller to act on"
    );
}

/// The controller stops managing a force-deleting host, so a reset recorded there never runs.
#[crate::sqlx_test]
async fn reset_set_rejects_a_force_deleting_host(pool: sqlx::PgPool) {
    let env = dpf_test_env(pool).await;
    let managed_host = dpf_ingested_host(&env).await;
    // Every other precondition is satisfied, so force-deletion is the only one left to fail.
    managed_host.mark_machine_for_updates().await;

    let mut txn = env.db_txn().await;
    let host = managed_host.host().db_machine(&mut txn).await;
    assert!(
        db::machine::advance(&host, &mut txn, &ManagedHostState::ForceDeletion, None)
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();

    let error = env
        .api
        .trigger_managed_host_reset(reset_request(managed_host.id.into(), Mode::Set))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("force-deleted"),
        "unexpected message: {}",
        error.message()
    );

    let mut txn = env.db_txn().await;
    assert!(
        managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .reset_requested
            .is_none()
    );
}

/// `Clear` is the withdrawal path, and it closes once the controller stamps `started_at`
/// and begins tearing the host down. Both halves matter: an operator has to be able to take
/// back a request that has not started, and a late clear must not cancel a teardown that is
/// already deleting the tenant instance and the host's DPF CRs. A second `set` is refused for
/// the same reason: it would restart the teardown from scratch.
#[crate::sqlx_test]
async fn reset_set_and_clear_are_refused_once_a_reset_has_started(pool: sqlx::PgPool) {
    let env = dpf_test_env(pool).await;
    let managed_host = dpf_ingested_host(&env).await;
    managed_host.mark_machine_for_updates().await;
    let host_id: MachineId = managed_host.id.into();

    env.api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap();
    env.api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Clear))
        .await
        .unwrap();

    let mut txn = env.db_txn().await;
    assert!(
        managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .reset_requested
            .is_none()
    );
    drop(txn);

    // Nothing pending matches no row, which has to surface as a precondition, not a raw
    // database error.
    let error = env
        .api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Clear))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);

    // Request again, then stamp it started the way the controller hinge does when it moves
    // the host into `Reset`.
    env.api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap();
    let mut txn = env.db_txn().await;
    db::machine::update_managed_host_reset_start_time(&mut txn, &host_id)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let error = env
        .api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Clear))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("already started"),
        "unexpected message: {}",
        error.message()
    );

    // A second `set` must not replace a started reset.
    let error = env
        .api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("already in progress"),
        "unexpected message: {}",
        error.message()
    );

    let mut txn = env.db_txn().await;
    let request = managed_host
        .host()
        .db_machine(&mut txn)
        .await
        .reset_requested
        .expect("the started reset should survive a refused clear and set");
    assert!(request.started_at.is_some());
}

/// Records a reset the way an operator would, then drops the host straight into
/// `DeletingCrs` so a single iteration exercises that substate on its own. `started_at` is
/// stamped because the hinge fires on any unstarted request and would otherwise pull the
/// host back to `DeletingInstance`.
async fn enter_deleting_crs(env: &TestEnv, managed_host: &TestManagedHost) {
    managed_host.mark_machine_for_updates().await;
    let host_id: MachineId = managed_host.id.into();
    env.api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap();

    let mut txn = env.db_txn().await;
    db::machine::update_managed_host_reset_start_time(&mut txn, &host_id)
        .await
        .unwrap();
    let host = managed_host.host().db_machine(&mut txn).await;
    let deleting_crs = ManagedHostState::Reset {
        reset_state: ResetState::DeletingCrs,
    };
    assert!(
        db::machine::advance(&host, &mut txn, &deleting_crs, None)
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();
}

async fn host_state(env: &TestEnv, managed_host: &TestManagedHost) -> ManagedHostState {
    let mut txn = env.db_txn().await;
    managed_host
        .host()
        .db_machine(&mut txn)
        .await
        .current_state()
        .clone()
}

/// Registration refuses a CR that still carries a `deletionTimestamp`, so `DeletingCrs`
/// waits for the delete to drain rather than for it to be accepted. Re-ingesting early
/// would recreate nothing and strand the host.
#[crate::sqlx_test]
async fn reset_holds_in_deleting_crs_while_the_dpf_crs_remain(pool: sqlx::PgPool) {
    let env = reset_controller_env(pool, DpfCrs::Present { dpu_count: 1 }).await;
    let managed_host = dpf_ingested_host(&env).await;
    enter_deleting_crs(&env, &managed_host).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let state = host_state(&env, &managed_host).await;
    assert!(
        matches!(
            state,
            ManagedHostState::Reset {
                reset_state: ResetState::DeletingCrs
            }
        ),
        "an undrained CR set has to hold the reset, got {state:?}"
    );
}

/// A drained host has to re-enter discovery at `Initializing`, the only substate that
/// reaches `DpfState::Provisioning` and so the only path that recreates the CRs this
/// state just deleted. The request is cleared with the handoff, or the hinge re-fires.
#[crate::sqlx_test]
async fn reset_re_enters_dpu_discovery_once_the_dpf_crs_are_gone(pool: sqlx::PgPool) {
    let env = reset_controller_env(pool, DpfCrs::Gone).await;
    let managed_host = dpf_ingested_host(&env).await;
    enter_deleting_crs(&env, &managed_host).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    match host_state(&env, &managed_host).await {
        ManagedHostState::DpuDiscoveringState { dpu_states } => {
            assert!(!dpu_states.states.is_empty());
            for (dpu_id, dpu_state) in &dpu_states.states {
                assert_eq!(
                    *dpu_state,
                    DpuDiscoveringState::Initializing,
                    "DPU {dpu_id} has to re-enter discovery from the start"
                );
            }
        }
        other => panic!("a drained reset re-enters DPU discovery, got {other:?}"),
    }

    let mut txn = env.db_txn().await;
    assert!(
        managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .reset_requested
            .is_none()
    );
}

/// The controller parks any host carrying a failure record in `Failed` before it dispatches on
/// state, and `Failed` refuses `Clear`, so a parked reset can neither finish nor be withdrawn.
#[crate::sqlx_test]
async fn reset_completes_while_the_host_carries_a_failure_record(pool: sqlx::PgPool) {
    let env = reset_controller_env(pool, DpfCrs::Gone).await;
    let managed_host = dpf_ingested_host(&env).await;
    enter_deleting_crs(&env, &managed_host).await;

    let mut txn = env.db_txn().await;
    let host = managed_host.host().db_machine(&mut txn).await;
    db::machine::update_failure_details(
        &host,
        &mut txn,
        FailureDetails {
            cause: model::machine::FailureCause::NVMECleanFailed {
                err: "failed before the reset was requested".to_string(),
            },
            source: model::machine::FailureSource::Scout,
            failed_at: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let state = host_state(&env, &managed_host).await;
    assert!(
        matches!(state, ManagedHostState::DpuDiscoveringState { .. }),
        "a failure record must not park a reset that is already tearing the host down, \
         got {state:?}"
    );
}

/// Reset retains the Instance and its addresses until both DPUs acknowledge
/// Admin, including when release has started or cleanup is disabled.
#[crate::sqlx_test]
async fn reset_retains_instance_until_every_dpu_acknowledges_admin(pool: sqlx::PgPool) {
    struct Case {
        name: &'static str,
        release_before_reset: bool,
        ignore_cleanup: bool,
    }

    let env = reset_controller_env(pool, DpfCrs::Present { dpu_count: 2 }).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    for case in [
        Case {
            name: "active Instance with host cleanup",
            release_before_reset: false,
            ignore_cleanup: false,
        },
        Case {
            name: "terminating Instance with cleanup disabled",
            release_before_reset: true,
            ignore_cleanup: true,
        },
    ] {
        let managed_host = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_multi(&env, 2))
            .await
            .expect("timed out during initial provisioning");
        let host_id: MachineId = managed_host.id.into();
        let instance = managed_host
            .instance_builer(&env)
            .single_interface_network_config(segment_id)
            .build()
            .await;

        let mut txn = env.db_txn().await;
        let snapshot = managed_host.snapshot(&mut txn).await;
        assert!(!snapshot.use_admin_network(), "{}", case.name);
        assert!(
            snapshot.managed_host_network_config_version_synced(),
            "{}",
            case.name
        );
        let tenant_version = snapshot.host_snapshot.network_config.version;
        let allocated_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|address| (address.address, address.prefix, address.vpc_id))
        .collect::<Vec<_>>();
        assert!(!allocated_addresses.is_empty(), "{}", case.name);
        txn.commit().await.unwrap();

        if case.release_before_reset {
            env.api
                .release_instance(Request::new(rpc::InstanceReleaseRequest {
                    id: Some(instance.id),
                    issue: None,
                    is_repair_tenant: None,
                    delete_attribution: None,
                }))
                .await
                .unwrap();
        }
        managed_host.mark_machine_for_updates().await;
        let mut request = reset_request(host_id, Mode::Set);
        request.get_mut().allow_reset_with_instance = !case.release_before_reset;
        request.get_mut().ignore_cleanup = case.ignore_cleanup;
        env.api.trigger_managed_host_reset(request).await.unwrap();

        let deleting_instance = ManagedHostState::Reset {
            reset_state: ResetState::DeletingInstance,
        };
        env.run_machine_state_controller_iteration_until_state_matches(
            &managed_host.host().id,
            3,
            deleting_instance.clone(),
        )
        .await;
        env.run_machine_state_controller_iteration().await;

        let mut txn = env.db_txn().await;
        let snapshot = managed_host.snapshot(&mut txn).await;
        assert!(snapshot.use_admin_network(), "{}", case.name);
        let admin_version = snapshot.host_snapshot.network_config.version;
        assert_ne!(admin_version, tenant_version, "{}", case.name);
        assert!(
            !snapshot.managed_host_network_config_version_synced(),
            "{}",
            case.name
        );
        txn.commit().await.unwrap();

        for dpu_id in &managed_host.dpu_ids {
            let response = env
                .api
                .get_managed_host_network_config(Request::new(
                    rpc::forge::ManagedHostNetworkConfigRequest {
                        dpu_machine_id: Some(*dpu_id),
                    },
                ))
                .await
                .unwrap()
                .into_inner();
            assert!(response.use_admin_network, "{}", case.name);
            assert!(response.tenant_interfaces.is_empty(), "{}", case.name);
            assert_eq!(
                response.managed_host_config_version,
                admin_version.to_string(),
                "{}",
                case.name
            );
        }

        // Reload persisted snapshots on each pass. Neither a stale observation
        // nor one DPU's acknowledgement permits releasing the tenant's addresses.
        for acknowledged_dpus in [0, 1] {
            if acknowledged_dpus == 1 {
                network_configured_with_health(&env, &managed_host.dpu_ids[0], None).await;
            }
            env.run_machine_state_controller_iteration().await;
            let mut txn = env.db_txn().await;
            let snapshot = managed_host.snapshot(&mut txn).await;
            assert_eq!(snapshot.managed_state, deleting_instance, "{}", case.name);
            assert_eq!(
                snapshot.instance.as_ref().unwrap().id,
                instance.id,
                "{}",
                case.name
            );
            assert_eq!(
                snapshot.host_snapshot.network_config.version, admin_version,
                "{}",
                case.name
            );
            assert!(
                !snapshot.managed_host_network_config_version_synced(),
                "{}",
                case.name
            );
            let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
                txn.as_mut(),
                &instance.id,
                &segment_id,
            )
            .await
            .unwrap()
            .into_iter()
            .map(|address| (address.address, address.prefix, address.vpc_id))
            .collect::<Vec<_>>();
            assert_eq!(retained_addresses, allocated_addresses, "{}", case.name);
            txn.commit().await.unwrap();
            assert_eq!(
                instance.rpc_instance().await.status().tenant(),
                rpc::TenantState::Terminating,
                "{}",
                case.name
            );
        }

        network_configured_with_health(&env, &managed_host.dpu_ids[1], None).await;
        let deleting_crs = ManagedHostState::Reset {
            reset_state: ResetState::DeletingCrs,
        };
        let next_state = if case.ignore_cleanup {
            deleting_crs.clone()
        } else {
            ManagedHostState::WaitingForCleanup {
                cleanup_state: CleanupState::HostCleanup {
                    boss_controller_id: None,
                },
                cleanup_context: CleanupContext::Reset,
            }
        };
        env.run_machine_state_controller_iteration_until_state_matches(
            &managed_host.host().id,
            5,
            next_state,
        )
        .await;

        let mut txn = env.db_txn().await;
        assert!(
            db::instance::find_id_by_machine_id(txn.as_mut(), &host_id)
                .await
                .unwrap()
                .is_none(),
            "{}",
            case.name
        );
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                txn.as_mut(),
                &instance.id,
                &segment_id,
            )
            .await
            .unwrap()
            .is_empty(),
            "{}",
            case.name
        );

        if !case.ignore_cleanup {
            // Scout's completion report permits the existing cleanup path to
            // hand the host over to DPF teardown.
            let host = managed_host.host().db_machine(&mut txn).await;
            db::machine::update_reboot_time(&host, &mut txn)
                .await
                .unwrap();
            db::machine::update_cleanup_time(&host, &mut txn)
                .await
                .unwrap();
        }
        txn.commit().await.unwrap();
        if !case.ignore_cleanup {
            env.run_machine_state_controller_iteration_until_state_matches(
                &managed_host.host().id,
                3,
                deleting_crs,
            )
            .await;
        }
    }
}
