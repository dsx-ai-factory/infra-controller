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
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType, PredictedHostMachineId};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::machine::{
    CleanupContext, CleanupState, DpuDiscoveringState, FailureCause, FailureDetails, FailureSource,
    ManagedHostState, ResetState, StateMachineArea,
};
use rpc::forge::forge_server::Forge;
use rpc::forge::managed_host_reset_request::Mode;
use rpc::forge::{ManagedHostResetListRequest, ManagedHostResetRequest, UpdateInitiator};
use tokio::time::timeout;
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::instance::seed_service_vpc_endpoint;
use crate::tests::common::api_fixtures::test_managed_host::{
    TestManagedHost, TestPredictedManagedHost,
};
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_managed_host_with_dpf,
    create_managed_host_with_dpf_multi, create_test_env, create_test_env_with_overrides,
    get_config, network_configured_with_health,
};
use crate::tests::dpf::phase_observation;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

mod reset_scout_races {
    use std::future::Future;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use futures_util::future::maybe_done;
    use model::firmware::FirmwareComponentType;
    use model::machine::{HostReprovisionState, ManagedHostStateSnapshot, ResetRequest};

    use super::*;

    async fn snapshot(env: &TestEnv, host: &TestManagedHost) -> ManagedHostStateSnapshot {
        let mut txn = env.db_txn().await;
        let snapshot = host.snapshot(&mut txn).await;
        txn.commit().await.unwrap();
        snapshot
    }

    async fn prepare(env: &TestEnv, host: &TestManagedHost) -> ManagedHostStateSnapshot {
        let waiting = ManagedHostState::HostReprovision {
            reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
                upgrade_task_id: "reset-scout-task".into(),
                firmware_type: FirmwareComponentType::Bmc,
                final_version: "1.2.3".into(),
                power_drains_needed: None,
                started_at: chrono::Utc::now(),
                deadline: chrono::Utc::now() + chrono::Duration::minutes(60),
                task_json: String::new(),
                result: None,
            },
            retry_count: 0,
        };
        let mut txn = env.db_txn().await;
        db::machine::update_state(txn.as_mut(), &host.id, &waiting)
            .await
            .unwrap();
        for id in [MachineId::from(host.id), host.dpu_ids[0].into()] {
            sqlx::query(
                "UPDATE machines SET reset_requested = NULL, reprovisioning_requested = NULL,
                     host_reprovisioning_requested = NULL, last_reboot_requested = NULL,
                     controller_state_outcome = NULL WHERE id = $1",
            )
            .bind(id)
            .execute(txn.as_mut())
            .await
            .unwrap();
        }
        txn.commit().await.unwrap();
        host.mark_machine_for_updates().await;
        network_configured_with_health(env, &host.dpu_ids[0], None).await;
        env.api
            .trigger_managed_host_reset(reset_request(host.id.into(), Mode::Set))
            .await
            .unwrap();
        let before = snapshot(env, host).await;
        assert_eq!(before.managed_state, waiting);
        assert!(before.instance.is_none());
        assert!(before.host_snapshot.config.dpf.used_for_ingestion);
        assert_eq!(
            before.host_snapshot.status.failure_details.cause,
            FailureCause::NoError
        );
        assert!(before.host_snapshot.status.last_reboot_requested.is_none());
        assert!(
            before
                .host_snapshot
                .reset_requested
                .as_ref()
                .unwrap()
                .started_at
                .is_none()
        );
        assert!(before.aggregate_health.alerts.iter().any(|alert| {
            alert.id == *model::machine_update_module::HOST_UPDATE_HEALTH_PROBE_ID
                && alert
                    .classifications
                    .contains(&health_report::HealthAlertClassification::prevent_allocations())
        }));
        for dpu in &before.dpu_snapshots {
            assert!(dpu.reprovision_requested.is_none());
            assert!(dpu.status.last_reboot_requested.is_none());
            let observation = dpu.network_status_observation.as_ref().unwrap();
            assert!(
                chrono::Utc::now().signed_duration_since(observation.observed_at)
                    < chrono::Duration::minutes(1)
            );
        }
        before
    }

    // Observe the actual blocked statement, including history-trigger advisory waits.
    async fn lock_wait(
        pool: &sqlx::PgPool,
        blocker: i32,
        excluded_pid: Option<i32>,
    ) -> Result<(i32, String, bool), String> {
        timeout(TEST_TIMEOUT, async {
            loop {
                let waiting = sqlx::query_as(
                    "SELECT a.pid, a.query,
                         EXISTS(SELECT 1 FROM pg_locks l WHERE l.pid = a.pid
                                AND l.locktype = 'advisory' AND NOT l.granted)
                     FROM pg_stat_activity a
                     WHERE a.datname = current_database() AND a.wait_event_type = 'Lock'
                       AND $1 = ANY(pg_blocking_pids(a.pid))
                       AND ($2::integer IS NULL OR a.pid != $2)
                     ORDER BY a.pid LIMIT 1",
                )
                .bind(blocker)
                .bind(excluded_pid)
                .fetch_optional(pool)
                .await
                .map_err(|error| format!("observe lock wait: {error:?}"))?;
                if let Some(waiting) = waiting {
                    return Ok(waiting);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|error| format!("lock wait missing: {error}"))?
    }

    async fn host_is_free(txn: &mut sqlx::PgConnection, id: MachineId) -> Result<(), String> {
        let mut probe = db::Transaction::begin_inner(txn)
            .await
            .map_err(|error| format!("probe savepoint: {error:?}"))?;
        let locked = sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(id)
            .execute(probe.as_pgconn())
            .await;
        let rollback = probe.rollback().await;
        locked.map_err(|error| format!("history waiter retained H: {error:?}"))?;
        rollback.map_err(|error| format!("probe rollback: {error:?}"))
    }

    async fn join_writers(
        env: &TestEnv,
        pids: [Option<i32>; 2],
        joined: impl Future<Output = ()>,
    ) -> bool {
        tokio::pin!(joined);
        if timeout(TEST_TIMEOUT, &mut joined).await.is_ok() {
            return false;
        }
        let mut cancellations = Vec::new();
        for pid in pids.into_iter().flatten() {
            cancellations.push((
                pid,
                sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                    .bind(pid)
                    .fetch_one(&env.pool)
                    .await,
            ));
        }
        timeout(TEST_TIMEOUT, &mut joined)
            .await
            .unwrap_or_else(|error| {
                panic!("writer cleanup did not settle: {error}; {cancellations:?}")
            });
        true
    }

    fn check(case: &str, failures: &mut Vec<String>, assertions: impl FnOnce()) {
        if let Err(panic) = catch_unwind(AssertUnwindSafe(assertions)) {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("assertion failed");
            failures.push(format!("{case}: {message}"));
        }
    }

    fn assert_started(request: &ResetRequest, pending: &ResetRequest) {
        assert!(request.started_at.is_some());
        assert_eq!(request.requested_at, pending.requested_at);
        assert_eq!(request.initiator, pending.initiator);
        assert_eq!(request.ignore_cleanup, pending.ignore_cleanup);
    }

    async fn histories(
        env: &TestEnv,
        host: &TestManagedHost,
    ) -> Vec<Vec<(config_version::ConfigVersion, String)>> {
        let mut txn = env.db_txn().await;
        let mut histories = Vec::new();
        for id in [MachineId::from(host.id), host.dpu_ids[0].into()] {
            histories.push(
                db::state_history::for_object(
                    txn.as_mut(),
                    db::state_history::StateHistoryTableId::Machine,
                    &id,
                )
                .await
                .unwrap()
                .into_iter()
                .map(|record| (record.state_version, record.state))
                .collect(),
            );
        }
        txn.commit().await.unwrap();
        histories
    }

    async fn assert_reset_history(
        env: &TestEnv,
        host: &TestManagedHost,
        since: config_version::ConfigVersion,
    ) {
        let state = snapshot(env, host).await;
        let expected = ManagedHostState::Reset {
            reset_state: ResetState::DeletingInstance,
        };
        assert_eq!(state.managed_state, expected);
        assert!(matches!(
            state.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Transition { .. })
        ));
        let mut txn = env.db_txn().await;
        for (id, version) in state
            .dpu_snapshots
            .iter()
            .map(|dpu| (MachineId::from(dpu.id), dpu.state.version))
            .chain([(host.id.into(), state.host_snapshot.state.version)])
        {
            let machine = db::machine::find_one(txn.as_mut(), &id, Default::default())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(machine.current_state(), &expected);
            let history = db::state_history::for_object(
                txn.as_mut(),
                db::state_history::StateHistoryTableId::Machine,
                &id,
            )
            .await
            .unwrap();
            assert_eq!(
                history
                    .iter()
                    .filter(|record| {
                        record.state_version.version_nr() > since.version_nr()
                            && serde_json::from_str::<ManagedHostState>(&record.state).unwrap()
                                == expected
                    })
                    .count(),
                1
            );
            let last = history.last().unwrap();
            assert_eq!(
                serde_json::from_str::<ManagedHostState>(&last.state).unwrap(),
                expected
            );
            assert_eq!(last.state_version, version);
        }
        txn.commit().await.unwrap();
    }

    #[crate::sqlx_test]
    async fn scout_report_and_reset_controller_preserve_history_order(pool: sqlx::PgPool) {
        Box::pin(async move {
            let mut mock = provisioning_mock();
            mock.expect_force_delete_host().times(0);
            let env = env_with_dpf_mock(pool, mock).await;
            let host = dpf_ingested_host(&env).await;
            let mut failures = Vec::new();
            for (case, scout_first) in [("Scout first", true), ("Reset first", false)] {
                let before = prepare(&env, &host).await;
                let pending = before.host_snapshot.reset_requested.clone().unwrap();
                let upgrade_task_id = match &before.managed_state {
                    ManagedHostState::HostReprovision {
                        reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
                            upgrade_task_id, ..
                        }, ..
                    } => upgrade_task_id.clone(),
                    other => panic!("waiting fixture changed: {other:?}"),
                };
                let mut gate = env.db_txn().await;
                if scout_first {
                    sqlx::query("SELECT pg_advisory_xact_lock(\
                        hashtextextended($1, 'machine_state_history'::regclass::bigint))")
                        .bind(host.id.to_string()).execute(gate.as_mut()).await.unwrap();
                } else {
                    db::machine::lock_by_id_for_update(gate.as_mut(), &host.id.into()).await.unwrap();
                }
                let gate_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(gate.as_mut()).await.unwrap();
                let checkpoint = env.redfish_sim.timepoint();
                let clients = env.redfish_sim.create_client_calls();
                let report = maybe_done(env.api.report_scout_firmware_upgrade_status(Request::new(
                    rpc::forge::ScoutFirmwareUpgradeStatusRequest {
                        machine_id: Some(host.id.into()), upgrade_task_id: upgrade_task_id.clone(),
                        success: true, exit_code: 0, stdout: "completed-task".into(),
                        stderr: String::new(), error: String::new(),
                    },
                )));
                let controller = maybe_done(env.run_machine_state_controller_iteration_no_requeue());
                tokio::pin!(report, controller);
                let mut report_pid = None;
                let mut controller_pid = None;
                let mut observations = Vec::new();
                let readiness = timeout(TEST_TIMEOUT, async {
                    if scout_first {
                        let first = tokio::select! {
                            () = &mut report => return Err("Scout passed history gate".to_string()),
                            observed = lock_wait(&env.pool, gate_pid, None) => observed?,
                        };
                        report_pid = Some(first.0);
                        observations.push(first);
                        let second = tokio::select! {
                            () = &mut report => return Err("Scout passed history gate".to_string()),
                            () = &mut controller => return Err("reset did not reach admission".to_string()),
                            observed = lock_wait(&env.pool, gate_pid, report_pid) => observed?,
                        };
                        controller_pid = Some(second.0);
                        observations.push(second);
                        host_is_free(gate.as_mut(), host.id.into()).await?;
                    } else {
                        let first = tokio::select! {
                            () = &mut controller => return Err("reset passed H gate".to_string()),
                            observed = lock_wait(&env.pool, gate_pid, None) => observed?,
                        };
                        controller_pid = Some(first.0);
                        observations.push(first);
                        let second = tokio::select! {
                            () = &mut controller => return Err("reset passed H gate".to_string()),
                            () = &mut report => return Err("Scout did not wait for reset".to_string()),
                            observed = async {
                                tokio::select! {
                                    observed = lock_wait(&env.pool, gate_pid, controller_pid) => observed,
                                    observed = lock_wait(&env.pool, controller_pid.unwrap(), None) => observed,
                                }
                            } => observed?,
                        };
                        report_pid = Some(second.0);
                        observations.push(second);
                    }
                    Ok::<(), String>(())
                }).await.unwrap_or_else(|error| Err(format!("readiness timeout: {error}")));
                let release = gate.commit().await;
                let timed_out = join_writers(&env, [report_pid, controller_pid], async {
                    tokio::join!(&mut report, &mut controller);
                }).await;
                let report_result = report.as_mut().take_output().unwrap();
                let intermediate = snapshot(&env, &host).await;
                let intermediate_request = intermediate.host_snapshot.reset_requested.clone();
                // The controller runner returns unit; preserve its database outcome too.
                let diagnostic = format!("{case}: {readiness:?}; waits={observations:?}; \
                    release={release:?}; timed_out={timed_out}; Scout={report_result:?}; \
                    state={:?}; outcome={:?}; reset={intermediate_request:?}",
                    intermediate.managed_state, intermediate.host_snapshot.controller_state_outcome);
                let retry = scout_first && report_result.is_ok()
                    && intermediate.managed_state != (ManagedHostState::Reset {
                        reset_state: ResetState::DeletingInstance,
                    })
                    && intermediate_request.as_ref().is_some_and(|request| request.started_at.is_none());
                if retry {
                    env.run_machine_state_controller_iteration_no_requeue().await;
                }
                let after = snapshot(&env, &host).await;
                let final_histories = histories(&env, &host).await;
                check(case, &mut failures, || {
                    assert!(readiness.is_ok() && release.is_ok() && !timed_out, "{diagnostic}");
                    assert!(!matches!(intermediate.host_snapshot.controller_state_outcome,
                        Some(PersistentStateHandlerOutcome::Error { .. })), "{diagnostic}");
                    if scout_first {
                        assert!(report_result.is_ok(), "{diagnostic}");
                        assert!(matches!(&intermediate.managed_state,
                            ManagedHostState::HostReprovision {
                                reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
                                    result: Some(result), ..
                                }, ..
                            } if result.success && result.stdout == "completed-task"), "{diagnostic}");
                        assert!(retry, "{diagnostic}");
                        assert!(intermediate_request.as_ref().unwrap().started_at.is_none());
                    } else {
                        assert_eq!(report_result.as_ref().unwrap_err().code(), Code::FailedPrecondition,
                            "{diagnostic}");
                    }
                    assert_eq!(after.managed_state, ManagedHostState::Reset {
                        reset_state: ResetState::DeletingInstance,
                    }, "{diagnostic}");
                    assert_started(after.host_snapshot.reset_requested.as_ref().unwrap(), &pending);
                    for (index, history) in final_histories.iter().enumerate() {
                        let reports = history.iter().filter(|(version, json)| {
                            version.version_nr() > before.host_snapshot.state.version.version_nr()
                                && matches!(serde_json::from_str::<ManagedHostState>(json).unwrap(),
                                    ManagedHostState::HostReprovision {
                                        reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
                                            result: Some(_), ..
                                        }, ..
                                    })
                        }).count();
                        assert_eq!(reports, usize::from(scout_first && index == 0),
                            "Scout reports update only H and cannot restore waiting after Reset");
                    }
                    assert_eq!(env.redfish_sim.create_client_calls(), clients);
                    assert!(env.redfish_sim.actions_since(&checkpoint).all_hosts().is_empty());
                });
                if after.managed_state == (ManagedHostState::Reset { reset_state: ResetState::DeletingInstance }) {
                    assert_reset_history(&env, &host, before.host_snapshot.state.version).await;
                }
            }
            assert!(failures.is_empty(), "{}", failures.join("\n"));
        }).await;
    }

    #[crate::sqlx_test]
    async fn scout_report_releases_history_and_rechecks_replaced_task(pool: sqlx::PgPool) {
        use db::state_history::{StateHistoryTableId, find_by_object_ids};

        let mut mock = provisioning_mock();
        mock.expect_force_delete_host().times(0);
        let env = env_with_dpf_mock(pool, mock).await;
        let host = dpf_ingested_host(&env).await;
        let before = prepare(&env, &host).await;
        let mut replacement_state = before.managed_state.clone();
        let reported_task = match &mut replacement_state {
            ManagedHostState::HostReprovision {
                reprovision_state:
                    HostReprovisionState::WaitingForScoutUpgrade {
                        upgrade_task_id,
                        result,
                        ..
                    },
                ..
            } => {
                assert!(result.is_none());
                let original = upgrade_task_id.clone();
                *upgrade_task_id = format!("{original}-replacement");
                original
            }
            other => panic!("waiting fixture changed: {other:?}"),
        };
        let machine_ids = [MachineId::from(host.id), host.dpu_ids[0].into()];
        let mut txn = env.db_txn().await;
        let history_before =
            find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                .await
                .unwrap();
        txn.commit().await.unwrap();
        let checkpoint = env.redfish_sim.timepoint();
        let clients = env.redfish_sim.create_client_calls();

        let mut gate = env.db_txn().await;
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
            .bind(host.id)
            .execute(gate.as_mut())
            .await
            .unwrap();
        let gate_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await
            .unwrap();
        let report = maybe_done(env.api.report_scout_firmware_upgrade_status(Request::new(
            rpc::forge::ScoutFirmwareUpgradeStatusRequest {
                machine_id: Some(host.id.into()),
                upgrade_task_id: reported_task,
                success: true,
                exit_code: 0,
                stdout: "obsolete-task-result".into(),
                stderr: String::new(),
                error: String::new(),
            },
        )));
        tokio::pin!(report);
        let waiting = tokio::select! {
            () = &mut report => Err("Scout passed the host gate".to_string()),
            observed = lock_wait(&env.pool, gate_pid, None) => observed,
        };
        // Scout must release A(H) before its isolated H wait. Record the probe
        // before releasing the gate, but join the request before asserting it.
        let history_available = sqlx::query_scalar::<_, bool>(
            "SELECT pg_try_advisory_xact_lock(\
                hashtextextended($1, 'machine_state_history'::regclass::bigint))",
        )
        .bind(host.id.to_string())
        .fetch_one(gate.as_mut())
        .await;
        let replacement = async {
            if !history_available.as_ref().is_ok_and(|available| *available) {
                return Ok(None);
            }
            // Use the real single-host state owner to replace the task. This
            // gate models a committed writer, not a controller iteration.
            let machine = db::machine::find_one(gate.as_mut(), &host.id, Default::default())
                .await?
                .ok_or_else(|| db::DatabaseError::NotFoundError {
                    kind: "machine",
                    id: host.id.to_string(),
                })?;
            let advanced =
                db::machine::advance(&machine, gate.as_mut(), &replacement_state, None).await?;
            let winner = db::machine::find_one(gate.as_mut(), &host.id, Default::default())
                .await?
                .ok_or_else(|| db::DatabaseError::NotFoundError {
                    kind: "machine",
                    id: host.id.to_string(),
                })?;
            let history =
                find_by_object_ids(gate.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                    .await?;
            Ok::<_, db::DatabaseError>(Some((advanced, winner, history)))
        }
        .await;
        let release = gate.commit().await;
        let timed_out = join_writers(
            &env,
            [waiting.as_ref().ok().map(|wait| wait.0), None],
            async { (&mut report).await },
        )
        .await;
        let report_result = report.as_mut().take_output().unwrap();
        let diagnostic = format!(
            "wait={waiting:?}; history_available={history_available:?}; \
             replacement={:?}; release={release:?}; timeout={timed_out}; Scout={report_result:?}",
            replacement.as_ref().map(|winner| winner.is_some()),
        );
        let (_, query, advisory_wait) = waiting.expect("Scout must reach its host row wait");
        assert!(
            query.contains("FOR NO KEY UPDATE") && !query.contains("NOWAIT") && !advisory_wait,
            "{diagnostic}"
        );
        assert!(history_available.unwrap(), "{diagnostic}");
        assert!(release.is_ok() && !timed_out, "{diagnostic}");
        let (advanced, winner, winning_history) = replacement.unwrap().unwrap();
        assert!(advanced, "{diagnostic}");
        let error = report_result.expect_err("the original task must not replace the current task");
        assert_eq!(error.code(), Code::FailedPrecondition, "{diagnostic}");
        assert_eq!(
            error.message(),
            format!(
                "scout firmware upgrade status task ID mismatch for machine {}",
                host.id
            ),
            "{diagnostic}"
        );

        let after = snapshot(&env, &host).await;
        assert_eq!(winner.state.value, replacement_state, "{diagnostic}");
        assert_eq!(
            after.host_snapshot.state.value, winner.state.value,
            "{diagnostic}"
        );
        assert_eq!(
            after.host_snapshot.state.version, winner.state.version,
            "{diagnostic}"
        );
        assert_eq!(
            winner.state.version.version_nr(),
            before.host_snapshot.state.version.version_nr() + 1,
            "{diagnostic}"
        );
        assert_eq!(
            after.host_snapshot.controller_state_outcome,
            before.host_snapshot.controller_state_outcome
        );
        assert_eq!(
            after.host_snapshot.health_reports,
            before.host_snapshot.health_reports
        );
        assert_eq!(
            after.host_snapshot.reset_requested,
            before.host_snapshot.reset_requested
        );
        assert!(after.host_snapshot.reprovision_requested.is_none());
        assert!(after.host_snapshot.host_reprovision_requested.is_none());
        assert_eq!(after.dpu_snapshots.len(), before.dpu_snapshots.len());
        for (dpu, original) in after.dpu_snapshots.iter().zip(&before.dpu_snapshots) {
            assert_eq!(dpu.id, original.id);
            assert_eq!(dpu.state.value, original.state.value);
            assert_eq!(dpu.state.version, original.state.version);
        }
        let mut txn = env.db_txn().await;
        let history_after =
            find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                .await
                .unwrap();
        assert_eq!(
            serde_json::to_value(&history_after).unwrap(),
            serde_json::to_value(&winning_history).unwrap(),
            "{diagnostic}: rejected Scout result must add no history"
        );
        let host_history = &history_after[&host.id.to_string()];
        let original_host_history = &history_before[&host.id.to_string()];
        assert_eq!(host_history.len(), original_host_history.len() + 1);
        assert_eq!(
            serde_json::to_value(&host_history[..original_host_history.len()]).unwrap(),
            serde_json::to_value(original_host_history).unwrap()
        );
        let last = host_history.last().unwrap();
        assert_eq!(last.state_version, winner.state.version);
        assert_eq!(
            serde_json::from_str::<ManagedHostState>(&last.state).unwrap(),
            replacement_state
        );
        let dpu_key = host.dpu_ids[0].to_string();
        assert_eq!(
            serde_json::to_value(&history_after[&dpu_key]).unwrap(),
            serde_json::to_value(&history_before[&dpu_key]).unwrap()
        );
        txn.commit().await.unwrap();
        assert_eq!(env.redfish_sim.create_client_calls(), clients);
        assert!(
            env.redfish_sim
                .actions_since(&checkpoint)
                .all_hosts()
                .is_empty()
        );
    }

    #[crate::sqlx_test]
    async fn scout_report_deadline_bounds_the_isolated_host_wait(pool: sqlx::PgPool) {
        let mut mock = provisioning_mock();
        mock.expect_force_delete_host().times(0);
        let env = env_with_dpf_mock(pool, mock).await;
        let host = dpf_ingested_host(&env).await;
        let before = prepare(&env, &host).await;
        let history_before = histories(&env, &host).await;
        let checkpoint = env.redfish_sim.timepoint();
        let clients = env.redfish_sim.create_client_calls();

        let mut gate = env.db_txn().await;
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
            .bind(host.id)
            .execute(gate.as_mut())
            .await
            .unwrap();
        let gate_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await
            .unwrap();
        let mut history_gate = env.db_txn().await;
        let history_gate_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(history_gate.as_mut())
            .await
            .unwrap();
        let budget = Duration::from_secs(60);
        let mut request = Request::new(rpc::forge::ScoutFirmwareUpgradeStatusRequest {
            machine_id: Some(host.id.into()),
            upgrade_task_id: "reset-scout-task".into(),
            success: true,
            exit_code: 0,
            stdout: "must-not-be-recorded-after-deadline".into(),
            stderr: String::new(),
            error: String::new(),
        });
        request.set_timeout(budget);
        let report = maybe_done(env.api.report_scout_firmware_upgrade_status(request));
        tokio::pin!(report);
        let waiting = tokio::select! {
            () = &mut report => Err("Scout completed before its isolated host wait".to_string()),
            observed = lock_wait(&env.pool, gate_pid, None) => observed,
        };
        let first_wait_observed_at = tokio::time::Instant::now();
        // Both advances affect every Tokio timer in this runtime. Keep time
        // resumed while PostgreSQL establishes each real lock boundary.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(2)).await;
        let survived_partial_budget = futures_util::poll!(&mut report).is_pending();
        tokio::time::resume();
        // The isolated H wait must have released history. Hold it separately
        // so Scout's fresh attempt has a second wait within the original budget.
        let history_acquired = sqlx::query_scalar::<_, bool>(
            "SELECT pg_try_advisory_xact_lock(\
                hashtextextended($1, 'machine_state_history'::regclass::bigint)\
            )",
        )
        .bind(host.id.to_string())
        .fetch_one(history_gate.as_mut())
        .await;
        let host_release = gate.commit().await;
        let history_wait = if matches!(&history_acquired, Ok(true)) {
            tokio::select! {
                () = &mut report => Err("Scout completed before its fresh history attempt".to_string()),
                observed = lock_wait(&env.pool, history_gate_pid, None) => observed,
            }
        } else {
            Err("Scout retained history at its isolated host wait".to_string())
        };
        tokio::time::pause();
        // First-wait time + budget is past the original deadline. Subtract
        // real I/O time so a fresh budget after the 2s advance cannot expire
        // during the following 1s probe.
        let remaining_from_first_wait = budget.checked_sub(first_wait_observed_at.elapsed());
        tokio::time::advance(
            remaining_from_first_wait.unwrap_or_default() + Duration::from_millis(100),
        )
        .await;
        let completed_before_release = timeout(Duration::from_secs(1), &mut report).await.is_ok();
        tokio::time::resume();
        let history_release = history_gate.rollback().await;
        let timed_out = join_writers(
            &env,
            [
                waiting.as_ref().ok().map(|wait| wait.0),
                history_wait.as_ref().ok().map(|wait| wait.0),
            ],
            async { (&mut report).await },
        )
        .await;
        let result = report.as_mut().take_output().unwrap();
        let diagnostic = format!(
            "first_wait={waiting:?}; survived_partial_budget={survived_partial_budget}; \
             history_acquired={history_acquired:?}; host_release={host_release:?}; \
             second_wait={history_wait:?}; remaining_from_first_wait={remaining_from_first_wait:?}; \
             completed_before_history_release={completed_before_release}; \
             history_release={history_release:?}; join_timeout={timed_out}; Scout={result:?}"
        );
        let (_, query, advisory_wait) = waiting.expect("Scout must reach its isolated host wait");
        assert!(
            query.contains("FOR NO KEY UPDATE") && !query.contains("NOWAIT") && !advisory_wait,
            "{diagnostic}"
        );
        let (_, query, advisory_wait) =
            history_wait.expect("Scout must reach a fresh history attempt");
        assert!(
            query.contains("pg_advisory_xact_lock") && advisory_wait,
            "{diagnostic}"
        );
        assert!(
            survived_partial_budget
                && remaining_from_first_wait.is_some()
                && matches!(history_acquired, Ok(true))
                && host_release.is_ok()
                && completed_before_release
                && history_release.is_ok()
                && !timed_out,
            "{diagnostic}"
        );
        let error = result
            .expect_err("the original request budget must expire while history remains locked");
        assert_eq!(error.code(), Code::DeadlineExceeded, "{diagnostic}");
        assert_eq!(
            error.message(),
            "machine database preparation deadline exceeded"
        );

        let after = snapshot(&env, &host).await;
        assert_eq!(
            after.host_snapshot.state.value,
            before.host_snapshot.state.value
        );
        assert_eq!(
            after.host_snapshot.state.version,
            before.host_snapshot.state.version
        );
        assert_eq!(
            after.host_snapshot.controller_state_outcome,
            before.host_snapshot.controller_state_outcome
        );
        assert_eq!(
            after.host_snapshot.health_reports,
            before.host_snapshot.health_reports
        );
        assert_eq!(
            after.host_snapshot.reset_requested,
            before.host_snapshot.reset_requested
        );
        assert_eq!(
            after.host_snapshot.reprovision_requested,
            before.host_snapshot.reprovision_requested
        );
        assert_eq!(
            serde_json::to_value(&after.host_snapshot.host_reprovision_requested).unwrap(),
            serde_json::to_value(&before.host_snapshot.host_reprovision_requested).unwrap()
        );
        assert_eq!(after.dpu_snapshots.len(), before.dpu_snapshots.len());
        for (dpu, original) in after.dpu_snapshots.iter().zip(&before.dpu_snapshots) {
            assert_eq!(dpu.id, original.id);
            assert_eq!(dpu.state.value, original.state.value);
            assert_eq!(dpu.state.version, original.state.version);
            assert_eq!(
                dpu.controller_state_outcome,
                original.controller_state_outcome
            );
        }
        assert_eq!(histories(&env, &host).await, history_before);
        assert_eq!(env.redfish_sim.create_client_calls(), clients);
        assert!(
            env.redfish_sim
                .actions_since(&checkpoint)
                .all_hosts()
                .is_empty()
        );
    }

    #[crate::sqlx_test]
    async fn scout_report_deadline_includes_pool_acquisition(pool: sqlx::PgPool) {
        use carbide_uuid::machine::HostMachineId;

        use crate::test_support::builder::TestApiBuilder;

        #[derive(Clone, Copy, Debug)]
        enum Case {
            Scout,
            Dpf,
            ForceDelete,
        }

        let env = create_test_env(pool.clone()).await;
        let limited_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(pool.connect_options().as_ref().clone())
            .await
            .unwrap();
        let api = TestApiBuilder::new(
            limited_pool.clone(),
            env.common_pools.clone(),
            env.api.work_lock_manager_handle.clone(),
        )
        .build();
        let host_id: HostMachineId = "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30"
            .parse()
            .unwrap();
        let budget = Duration::from_secs(5);
        let mut held = Some(limited_pool.acquire().await.unwrap());
        let mut results = Vec::new();
        for case in [Case::Scout, Case::Dpf, Case::ForceDelete] {
            if held.is_none() {
                held = Some(limited_pool.acquire().await.unwrap());
            }
            let call = maybe_done(async {
                match case {
                    Case::Scout => {
                        let mut request =
                            Request::new(rpc::forge::ScoutFirmwareUpgradeStatusRequest {
                                machine_id: Some(host_id),
                                upgrade_task_id: "pool-budget".into(),
                                ..Default::default()
                            });
                        request.set_timeout(budget);
                        api.report_scout_firmware_upgrade_status(request)
                            .await
                            .map(|_| ())
                    }
                    Case::Dpf => {
                        let mut request = Request::new(rpc::forge::ModifyDpfStateRequest {
                            machine_id: Some(host_id),
                            dpf_enabled: true,
                        });
                        request.set_timeout(budget);
                        api.modify_dpf_state(request).await.map(|_| ())
                    }
                    Case::ForceDelete => {
                        let mut request =
                            Request::new(rpc::forge::AdminForceDeleteMachineRequest {
                                host_query: host_id.to_string(),
                                ..Default::default()
                            });
                        request.set_timeout(budget);
                        api.admin_force_delete_machine(request).await.map(|_| ())
                    }
                }
            });
            tokio::pin!(call);
            // All three RPCs wait for this one held connection. Its acquire
            // timeout exceeds the caller's budget and cannot supply this result.
            let acquisition_pending = futures_util::poll!(&mut call).is_pending();
            tokio::time::pause();
            tokio::time::advance(budget + Duration::from_millis(1)).await;
            let completed_before_release = timeout(Duration::from_secs(1), &mut call).await.is_ok();
            tokio::time::resume();
            // A failed deadline assertion must still release the pool slot
            // before joining the real request. Successful rows keep it held.
            if !completed_before_release {
                drop(held.take());
            }
            let timed_out = join_writers(&env, [None, None], async { (&mut call).await }).await;
            let result = call.as_mut().take_output().unwrap();
            results.push((
                case,
                acquisition_pending,
                completed_before_release,
                timed_out,
                result,
            ));
        }
        drop(held);
        limited_pool.close().await;
        for (case, acquisition_pending, completed_before_release, timed_out, result) in results {
            assert!(
                acquisition_pending,
                "{case:?}: the request must wait for the held pool slot"
            );
            assert!(
                completed_before_release && !timed_out,
                "{case:?}: {result:?}"
            );
            let error = result.expect_err("pool acquisition consumes the preparation budget");
            assert_eq!(error.code(), Code::DeadlineExceeded, "{case:?}");
            assert_eq!(
                error.message(),
                "machine database preparation deadline exceeded",
                "{case:?}"
            );
        }
    }

    #[crate::sqlx_test]
    async fn reset_preemption_rejects_withdrawn_or_replaced_request(pool: sqlx::PgPool) {
        Box::pin(async move {
            let mut mock = provisioning_mock();
            mock.expect_force_delete_host().times(0);
            let env = env_with_dpf_mock(pool, mock).await;
            let host = dpf_ingested_host(&env).await;
            let mut failures = Vec::new();
            for (case, replace) in [("Clear wins", false), ("replacement wins", true)] {
                let before = prepare(&env, &host).await;
                let original = before.host_snapshot.reset_requested.clone().unwrap();
                let history_before = histories(&env, &host).await;
                let mut gate = env.db_txn().await;
                db::machine::lock_by_id_for_update(gate.as_mut(), &host.id.into())
                    .await
                    .unwrap();
                let gate_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(gate.as_mut())
                    .await
                    .unwrap();
                let checkpoint = env.redfish_sim.timepoint();
                let clients = env.redfish_sim.create_client_calls();
                let controller =
                    maybe_done(env.run_machine_state_controller_iteration_no_requeue());
                tokio::pin!(controller);
                let waiting = tokio::select! {
                    () = &mut controller => Err("reset passed H gate".to_string()),
                    observed = lock_wait(&env.pool, gate_pid, None) => observed,
                };
                // Exercise the same Set/Clear producer on the gate's connection.
                // The public API's sequential authorization/wiring has retained coverage.
                let produced = if replace {
                    db::machine::trigger_managed_host_reset_request(
                        gate.as_mut(),
                        "replacement-reset",
                        &host.id.into(),
                        true,
                    )
                    .await
                } else {
                    db::machine::clear_managed_host_reset_request(
                        gate.as_mut(),
                        &host.id.into(),
                        true,
                    )
                    .await
                };
                let current = db::machine::find_one(gate.as_mut(), &host.id, Default::default())
                    .await
                    .and_then(|machine| {
                        machine.ok_or_else(|| db::DatabaseError::NotFoundError {
                            kind: "machine",
                            id: host.id.to_string(),
                        })
                    });
                let current_version = current.as_ref().map(|machine| machine.current_version());
                let replacement = current
                    .as_ref()
                    .ok()
                    .and_then(|machine| machine.reset_requested.clone());
                let release = gate.commit().await;
                let timed_out = join_writers(
                    &env,
                    [waiting.as_ref().ok().map(|wait| wait.0), None],
                    async {
                        (&mut controller).await;
                    },
                )
                .await;
                let intermediate = snapshot(&env, &host).await;
                let history_after = histories(&env, &host).await;
                let diagnostic = format!(
                    "{case}: wait={waiting:?}; producer={produced:?}; \
                    current_version={current_version:?}; replacement={replacement:?}; \
                    read_error={:?}; release={release:?}; timeout={timed_out}; \
                    state={:?}; reset={:?}; outcome={:?}",
                    current.as_ref().err(),
                    intermediate.managed_state,
                    intermediate.host_snapshot.reset_requested,
                    intermediate.host_snapshot.controller_state_outcome
                );
                let should_retry = replace
                    && intermediate.managed_state == before.managed_state
                    && intermediate
                        .host_snapshot
                        .reset_requested
                        .as_ref()
                        .is_some_and(|request| request.started_at.is_none());
                if should_retry {
                    env.run_machine_state_controller_iteration_no_requeue()
                        .await;
                }
                let after = snapshot(&env, &host).await;
                check(case, &mut failures, || {
                    assert!(
                        waiting.is_ok()
                            && produced.as_ref().is_ok_and(|matched| *matched)
                            && release.is_ok()
                            && !timed_out,
                        "{diagnostic}"
                    );
                    assert_eq!(current_version.unwrap(), before.host_snapshot.state.version);
                    assert_eq!(
                        intermediate.managed_state, before.managed_state,
                        "{diagnostic}"
                    );
                    assert_eq!(
                        intermediate.host_snapshot.state.version,
                        before.host_snapshot.state.version
                    );
                    assert_eq!(
                        history_after, history_before,
                        "stale reset must leave no H/D history"
                    );
                    assert!(
                        !matches!(
                            intermediate.host_snapshot.controller_state_outcome,
                            Some(PersistentStateHandlerOutcome::Error { .. })
                        ),
                        "{diagnostic}"
                    );
                    if replace {
                        let replacement = replacement.as_ref().unwrap();
                        assert_ne!(replacement.requested_at, original.requested_at);
                        assert_ne!(replacement.ignore_cleanup, original.ignore_cleanup);
                        assert_eq!(replacement.initiator, "replacement-reset");
                        assert!(replacement.started_at.is_none());
                        let retained = intermediate.host_snapshot.reset_requested.as_ref().unwrap();
                        assert!(retained.started_at.is_none());
                        assert_eq!(
                            serde_json::to_value(retained).unwrap(),
                            serde_json::to_value(replacement).unwrap()
                        );
                        assert!(should_retry, "{diagnostic}");
                        assert_started(
                            after.host_snapshot.reset_requested.as_ref().unwrap(),
                            replacement,
                        );
                        assert_eq!(
                            after.managed_state,
                            ManagedHostState::Reset {
                                reset_state: ResetState::DeletingInstance,
                            }
                        );
                    } else {
                        assert!(intermediate.host_snapshot.reset_requested.is_none());
                        assert!(after.host_snapshot.reset_requested.is_none());
                    }
                    assert_eq!(env.redfish_sim.create_client_calls(), clients);
                    assert!(
                        env.redfish_sim
                            .actions_since(&checkpoint)
                            .all_hosts()
                            .is_empty()
                    );
                });
                if should_retry {
                    assert_reset_history(&env, &host, before.host_snapshot.state.version).await;
                }
            }
            assert!(failures.is_empty(), "{}", failures.join("\n"));
        })
        .await;
    }
}

/// Expectations for the initial provisioning flow, shared by every env below.
fn provisioning_mock() -> MockDpfOperations {
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(DpuPhase::Ready)));
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

/// A preparation failure is safe to recover, but reset must still wait for
/// independent lockdown readback and the controller's persisted Failed phase.
#[crate::sqlx_test]
async fn reset_waits_for_nic_firmware_lockdown_restoration(pool: sqlx::PgPool) {
    use carbide_libmlx_model::firmware::operation::{
        FirmwareOperation, FirmwareOperationPhase, FirmwareOperationState,
    };
    use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
    use model::machine::nic_firmware::NicFirmwarePhase;
    use model::test_support::nic_firmware as fixtures;
    use rpc::protos::mlx_device as pb;
    use rpc::protos::mlx_device::mlx_device_lockdown_result::Result as LockdownResult;

    use crate::auth::AuthContext;

    let env = dpf_test_env(pool).await;
    let managed_host = dpf_ingested_host(&env).await;
    managed_host.mark_machine_for_updates().await;
    let host_id: MachineId = managed_host.id.into();

    // A reset queued before firmware recovery resumed remains withdrawable.
    env.api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap();
    let mut admitted = fixtures::operation();
    admitted.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    let target = &mut admitted.targets[0];
    target.supervisor_instance_id = Some("reset-test-helper".into());
    target.receipt = Some(FirmwareOperation {
        operation_id: admitted.operation_id.clone(),
        state: FirmwareOperationState::Failed,
        phase: Some(FirmwareOperationPhase::Prepare),
        report: FirmwareFlashReport {
            expected_version: Some(target.resolution.entry.firmware.version.clone()),
            ..Default::default()
        },
        diagnostic: Some("image preparation failed before mutation".into()),
    });
    admitted.validate().expect("valid admitted operation");
    let mut txn = env.db_txn().await;
    db::machine::update_state(
        &mut txn,
        managed_host.id.as_host_machine_id(),
        &ManagedHostState::NicFirmwareUpdating {
            operation: admitted,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    env.api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Clear))
        .await
        .unwrap();
    let error = env
        .api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(error.message().contains("NIC firmware recovery"));
    let active = db::machine::find_one(&env.pool, &managed_host.id, Default::default())
        .await
        .unwrap()
        .unwrap();
    assert!(active.reset_requested.is_none());
    assert!(matches!(
        active.state.value,
        ManagedHostState::NicFirmwareUpdating {
            operation: model::machine::nic_firmware::NicFirmwareOperation {
                phase: NicFirmwarePhase::UpdatingFirmware { .. },
                ..
            }
        }
    ));

    env.run_machine_state_controller_iteration_without_requeue()
        .await;
    let restoring = db::machine::find_one(&env.pool, &managed_host.id, Default::default())
        .await
        .unwrap()
        .unwrap();
    let ManagedHostState::NicFirmwareUpdating { operation } = &restoring.state.value else {
        panic!("preparation failure must restore lockdown before allowing reset");
    };
    assert!(matches!(
        operation.phase,
        NicFirmwarePhase::RestoringLockdown {
            failure: Some(_),
            ..
        }
    ));
    assert!(operation.lockdown.result.is_none());

    // The reset API must preserve restoration ownership until its independent
    // status read has committed, even though preparation already failed safely.
    let error = env
        .api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .expect_err("reset must wait for lockdown restoration");
    assert_eq!(error.code(), Code::FailedPrecondition);
    let retained = db::machine::find_one(&env.pool, &managed_host.id, Default::default())
        .await
        .unwrap()
        .unwrap();
    assert!(retained.reset_requested.is_none());
    assert_eq!(retained.state.value, restoring.state.value);
    assert_eq!(retained.state.version, restoring.state.version);

    // Report a separate status read for the restoration revision before the
    // controller can persist Failed and release the reset guard.
    let target = &operation.targets[0];
    let model::nic_firmware::NicFirmwareCardSelector::Mac(mac) = target.selector else {
        panic!("fixture selects its card by MAC")
    };
    let observed_device = pb::MlxDeviceInfo {
        pci_name: target.pci_name.clone(),
        base_mac: mac.to_string(),
        ..Default::default()
    };
    let mut request = Request::new(pb::PublishMlxObservationReportRequest {
        report: Some(pb::MlxObservationReport {
            machine_id: Some(host_id),
            observations: vec![pb::MlxObservation {
                device_info: Some(observed_device),
                lockdown_result: Some(pb::MlxDeviceLockdownResult {
                    controller_state_version: restoring.state.version.to_string(),
                    pci_name: target.pci_name.clone(),
                    expected_device: Some(target.selector.clone().into()),
                    result: Some(LockdownResult::StatusReport(pb::StatusReport {
                        device_id: target.pci_name.clone(),
                        status: pb::LockStatus::Unlocked.into(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                    })),
                }),
                ..Default::default()
            }],
            ..Default::default()
        }),
    });
    let mut auth = AuthContext::default();
    auth.principals
        .push(carbide_authn::middleware::Principal::SpiffeMachineIdentifier(host_id.to_string()));
    request.extensions_mut().insert(auth);
    env.api
        .publish_mlx_observation_report(request)
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    let failed = db::machine::find_one(&env.pool, &managed_host.id, Default::default())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        failed.state.value,
        ManagedHostState::NicFirmwareUpdating {
            operation: model::machine::nic_firmware::NicFirmwareOperation {
                phase: NicFirmwarePhase::Failed { .. },
                ..
            }
        }
    ));
    assert!(failed.reset_requested.is_none());
    env.api
        .trigger_managed_host_reset(reset_request(host_id, Mode::Set))
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    let resetting = db::machine::find_one(&env.pool, &managed_host.id, Default::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        resetting.state.value,
        ManagedHostState::Reset {
            reset_state: ResetState::DeletingInstance,
        }
    );
    assert!(
        resetting
            .reset_requested
            .expect("accepted reset")
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
    let captured = managed_host.host().db_machine(&mut txn).await;
    assert_eq!(
        db::machine::try_start_managed_host_reset(
            &mut txn,
            &captured.id,
            captured.state.version,
            captured
                .reset_requested
                .as_ref()
                .expect("the requested reset is pending"),
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(()),
    );
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
    let captured = managed_host.host().db_machine(&mut txn).await;
    assert_eq!(
        db::machine::try_start_managed_host_reset(
            &mut txn,
            &captured.id,
            captured.state.version,
            captured
                .reset_requested
                .as_ref()
                .expect("the requested reset is pending"),
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(()),
    );
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

/// A predicted host in Failed/DpfProvisioning with dpf.used_for_ingestion=true
/// and the HostUpdateInProgress alert can be reset.
#[crate::sqlx_test]
async fn reset_set_accepts_predicted_host_after_dpf_failure(pool: sqlx::PgPool) {
    let env = dpf_test_env(pool).await;
    let managed_host = dpf_ingested_host(&env).await;
    // Preserve the DPF fixture's persisted attachments, replacing only its host identity.
    let predicted_id: PredictedHostMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0xAC; 32],
        MachineType::PredictedHost,
    )
    .try_into()
    .unwrap();
    let mut txn = env.db_txn().await;
    let renamed = sqlx::query("UPDATE machines SET id = $1 WHERE id = $2")
        .bind(predicted_id)
        .bind(managed_host.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    assert_eq!(renamed.rows_affected(), 1);
    let predicted_host = TestPredictedManagedHost {
        id: predicted_id,
        dpu_ids: managed_host.dpu_ids,
        api: env.api.clone(),
    };
    let predicted_host_id: MachineId = predicted_host.id.into();

    db::machine::update_state(
        &mut txn,
        predicted_id.as_host_machine_id(),
        &ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::DpfProvisioning {
                    err: "test failure".to_string(),
                },
                failed_at: chrono::Utc::now(),
                source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
            },
            machine_id: predicted_host_id,
            retry_count: 0,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let mut txn = env.db_txn().await;
    let snapshot = predicted_host.snapshot(&mut txn).await;
    assert_eq!(
        snapshot.host_snapshot.id.machine_type(),
        MachineType::PredictedHost
    );
    assert!(snapshot.host_snapshot.config.dpf.used_for_ingestion);
    assert!(snapshot.instance.is_none());
    assert!(matches!(
        snapshot.managed_state,
        ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::DpfProvisioning { .. },
                ..
            },
            ..
        }
    ));
    drop(txn);

    let error = env
        .api
        .trigger_managed_host_reset(reset_request(predicted_host_id, Mode::Set))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(error.message().contains("HostUpdateInProgress"));

    predicted_host.mark_machine_for_updates().await;

    env.api
        .trigger_managed_host_reset(reset_request(predicted_host_id, Mode::Set))
        .await
        .unwrap();

    let mut txn = env.db_txn().await;
    let request = predicted_host
        .host()
        .db_machine(&mut txn)
        .await
        .reset_requested
        .expect("the reset request should persist on the host row");
    assert_eq!(request.initiator, UpdateInitiator::AdminCli.as_str_name());
    assert!(request.started_at.is_none(), "a fresh request is unstarted");

    drop(txn);

    // The same host-only gate protects Clear; withdraw before the controller picks it up.
    env.api
        .trigger_managed_host_reset(reset_request(predicted_host_id, Mode::Clear))
        .await
        .unwrap();
    let mut txn = env.db_txn().await;
    assert!(
        predicted_host
            .host()
            .db_machine(&mut txn)
            .await
            .reset_requested
            .is_none()
    );
    drop(txn);
    env.api
        .trigger_managed_host_reset(reset_request(predicted_host_id, Mode::Set))
        .await
        .unwrap();

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.db_txn().await;
    let snapshot = predicted_host.snapshot(&mut txn).await;
    assert_eq!(
        snapshot.managed_state,
        ManagedHostState::Reset {
            reset_state: ResetState::DeletingInstance,
        }
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

/// Verifies reset applies Admin networking before retained service endpoints block Instance
/// deletion, allowing dataplane removal without releasing endpoint ownership.
#[crate::sqlx_test]
async fn reset_applies_admin_before_waiting_for_service_vpc_cleanup(pool: sqlx::PgPool) {
    // Create a reset-capable host whose instance has durable endpoint ownership.
    let env = dpf_test_env(pool).await;
    let managed_host = dpf_ingested_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let endpoint =
        seed_service_vpc_endpoint(&env, instance.id, managed_host.dpu_ids[0], segment_id).await;

    // Capture the tenant generation so the test proves reset publishes a distinct Admin generation.
    let mut txn = env.db_txn().await;
    let tenant_version = managed_host
        .snapshot(&mut txn)
        .await
        .host_snapshot
        .network_config
        .version;
    txn.commit().await.unwrap();

    // Record an acknowledged reset and enter its instance-deletion substate.
    managed_host.mark_machine_for_updates().await;
    env.api
        .trigger_managed_host_reset(reset_request_allowing_instance(managed_host.id.into()))
        .await
        .unwrap();
    let deleting_instance = ManagedHostState::Reset {
        reset_state: ResetState::DeletingInstance,
    };
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        3,
        deleting_instance.clone(),
    )
    .await;

    // Reset requests Admin networking while retaining the endpoint and Instance.
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, deleting_instance);
    assert!(snapshot.use_admin_network());
    assert!(!snapshot.managed_host_network_config_version_synced());
    assert_ne!(
        snapshot.host_snapshot.network_config.version,
        tenant_version
    );
    assert!(
        matches!(
            &snapshot.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason
                == "waiting for DPUs to apply Admin networking before deleting the Instance"
        ),
        "expected the Admin-network acknowledgement wait, got {:?}",
        snapshot.host_snapshot.controller_state_outcome
    );
    let persisted = snapshot.instance.expect("instance remains assigned");
    assert!(persisted.deleted.is_none());
    assert_eq!(
        persisted.config.network.service_interfaces,
        vec![endpoint.clone()]
    );
    txn.commit().await.unwrap();

    // Acknowledging Admin advances reset to the cleanup guard without releasing ownership.
    network_configured_with_health(&env, &managed_host.dpu_ids[0], None).await;
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, deleting_instance);
    assert!(snapshot.use_admin_network());
    assert!(snapshot.managed_host_network_config_version_synced());
    assert!(
        matches!(
            &snapshot.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason
                == "waiting for service VPC network cleanup before deleting the Instance"
        ),
        "expected the service-network cleanup wait, got {:?}",
        snapshot.host_snapshot.controller_state_outcome
    );
    let persisted = snapshot.instance.expect("instance remains assigned");
    assert!(persisted.deleted.is_none());
    assert_eq!(persisted.config.network.service_interfaces, vec![endpoint]);
    txn.commit().await.unwrap();
}
