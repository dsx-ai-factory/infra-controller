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
use std::error::Error as _;

use carbide_machine_controller::handler::MachineStateHandlerBuilder;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::{
    DpuMachineId, HostMachineIdSubtypeTrait, MachineId, MachineIdSubtypeTrait,
};
use chrono::Utc;
use common::api_fixtures::{
    create_managed_host_multi_dpu, create_managed_host_with_hardware_info_template,
    create_test_env, reboot_completed,
};
use libredfish::{EnabledDisabled, SystemPowerControl};
use model::instance::status::tenant::TenantState;
use model::machine::{
    DpuInitState, FailureCause, FailureDetails, FailureSource, InstallDpuOsState, InstanceState,
    Machine, MachineLastRebootRequestedMode, MachineState, ManagedHostState, PowerState,
    ReprovisionState, SetBootOrderInfo, SetBootOrderState, StateMachineArea, UnlockHostState,
};
use model::test_support::HardwareInfoTemplate;
use rpc::forge::MachineArchitecture;
use rpc::forge::dpu_reprovisioning_request::Mode;
use rpc::forge::forge_server::Forge;
use rpc::forge_agent_control_response::Action;
use rpc::model::instance::snapshot::instance_snapshot_derive_status;

use crate::errors::{CarbideError, CarbideErrorWithLocation};
use crate::tests::common;
use crate::tests::common::api_fixtures::dpu::create_dpu_machine_in_waiting_for_network_install;
use crate::tests::common::api_fixtures::instance::TestInstance;
use crate::tests::common::api_fixtures::rpc_instance::RpcInstance;
use crate::tests::common::api_fixtures::test_machine::TestMachineInterface;
use crate::tests::common::api_fixtures::test_managed_host::TestAnyManagedHost;
use crate::tests::common::api_fixtures::{
    TestEnv, TestMachine, TestManagedHost, create_managed_host, forge_agent_control,
    update_time_params,
};

const DGX_H100_INFO_JSON: &[u8] = br#"{
    "machine_type": "x86_64",
    "dmi_data": {
        "product_name": "DGXH100",
        "sys_vendor": "NVIDIA"
    }
}"#;

fn reprovision_set_host_boot_order_state(
    set_boot_order_state: SetBootOrderState,
) -> ReprovisionState {
    ReprovisionState::SetHostBootOrder {
        set_boot_order_info: SetBootOrderInfo {
            set_boot_order_jid: None,
            set_boot_order_state,
            retry_count: 0,
        },
    }
}

#[derive(Clone, Copy)]
enum ReprovisionHostBootRepairShape {
    SingleDpu,
    AssignedSingleDpu,
    FirstDpuOnly,
    AllDpus,
}

fn reprovision_host_boot_repair_states(
    mh: &TestManagedHost,
    shape: ReprovisionHostBootRepairShape,
) -> Vec<ManagedHostState> {
    let states = [
        ReprovisionState::PrepareHostBootRepair,
        ReprovisionState::UnlockHostForBootRepair {
            unlock_host_state: UnlockHostState::DisableLockdown,
        },
        ReprovisionState::CheckHostBootConfig,
        reprovision_set_host_boot_order_state(SetBootOrderState::SetBootOrder),
        reprovision_set_host_boot_order_state(SetBootOrderState::WaitForSetBootOrderJobScheduled),
        reprovision_set_host_boot_order_state(SetBootOrderState::RebootHost),
        reprovision_set_host_boot_order_state(SetBootOrderState::WaitForSetBootOrderJobCompletion),
        reprovision_set_host_boot_order_state(SetBootOrderState::CheckBootOrder),
        ReprovisionState::LockHostAfterBootRepair,
        ReprovisionState::RebootHostBmc,
    ];

    states
        .into_iter()
        .map(|state| match shape {
            ReprovisionHostBootRepairShape::SingleDpu => mh.new_dpu_reprovision_state(state),
            ReprovisionHostBootRepairShape::AssignedSingleDpu => {
                mh.new_dpu_assigned_reprovision_state(state)
            }
            ReprovisionHostBootRepairShape::FirstDpuOnly => {
                let not_under_reprovision = ReprovisionState::NotUnderReprovision;
                let mut states = vec![&state];
                states.extend((1..mh.dpu_ids.len()).map(|_| &not_under_reprovision));
                mh.new_dpus_reprovision_state(&states)
            }
            ReprovisionHostBootRepairShape::AllDpus => {
                mh.new_dpus_reprovision_state(&vec![&state; mh.dpu_ids.len()])
            }
        })
        .collect()
}

/// Return true when any DPU in a reprovisioning managed-host state matches.
fn has_dpu_reprovision_state(
    state: &ManagedHostState,
    matches_state: impl FnMut(&ReprovisionState) -> bool,
) -> bool {
    match state {
        ManagedHostState::DPUReprovision { dpu_states }
        | ManagedHostState::Assigned {
            instance_state: InstanceState::DPUReprovision { dpu_states },
        } => dpu_states.states.values().any(matches_state),
        _ => false,
    }
}

async fn assert_dpu_reprovision_host_boot_repair<ID>(
    env: &TestEnv,
    machine: &TestMachine<ID>,
    expected_states: Vec<ManagedHostState>,
) -> Machine<ID>
where
    ID: MachineIdSubtypeTrait,
    db::DatabaseError: From<<ID as TryFrom<MachineId>>::Error>,
{
    env.redfish_sim.set_lockdown(EnabledDisabled::Enabled);
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(false);

    let redfish_timepoint = env.redfish_sim.timepoint();

    // Drive the shared repair path; each expected state should be externally restartable.
    for expected_state in expected_states {
        let current_machine = machine.next_iteration_machine(env).await;
        assert_eq!(current_machine.current_state(), &expected_state);

        // Keep restart available so wedged BIOS/job/boot-order repair can be operator-restarted.
        assert!(
            current_machine.reprovision_requested.is_some(),
            "expected DPU reprovision request to remain present during host boot repair"
        );

        // Disable lockdown so Redfish reflects writable host BIOS/boot state.
        if has_dpu_reprovision_state(&expected_state, |state| {
            matches!(state, ReprovisionState::CheckHostBootConfig)
        }) {
            assert!(
                env.redfish_sim
                    .lockdown_states()
                    .contains(&EnabledDisabled::Disabled),
                "expected DPU reprovision host boot repair to disable lockdown before boot config checks"
            );
        }

        // Re-enable lockdown so DPU reprovision preserves the host profile's security posture.
        if has_dpu_reprovision_state(&expected_state, |state| {
            matches!(state, ReprovisionState::RebootHostBmc)
        }) {
            let lockdown_states = env.redfish_sim.lockdown_states();
            assert!(
                !lockdown_states.is_empty()
                    && lockdown_states
                        .iter()
                        .all(|state| *state == EnabledDisabled::Enabled),
                "expected DPU reprovision host boot repair to re-enable lockdown before rebooting the host BMC"
            );
        }
    }

    let actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .all_hosts();
    let (set_boot_order_pos, set_boot_order_mac) = actions
        .iter()
        .enumerate()
        .find_map(|(position, action)| match action {
            RedfishSimAction::SetBootOrderDpuFirst { boot_interface_mac } => {
                Some((position, boot_interface_mac))
            }
            _ => None,
        })
        .expect("expected DPU reprovision boot repair to set DPU-first boot order");
    let boot_order_checks = actions
        .iter()
        .enumerate()
        .filter_map(|(position, action)| match action {
            RedfishSimAction::IsBootOrderSetup { boot_interface_mac } => {
                Some((position, boot_interface_mac))
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(
        actions
            .iter()
            .all(|action| !matches!(action, RedfishSimAction::MachineSetup { .. })),
        "expected order-only DPU reprovision boot repair to preserve the configured HTTP boot device; got: {actions:?}"
    );
    assert!(
        boot_order_checks
            .iter()
            .filter(|(position, _)| *position < set_boot_order_pos)
            .count()
            >= 2,
        "expected both CheckHostBootConfig and SetBootOrder to inspect the order before writing it; got: {actions:?}"
    );
    assert!(
        boot_order_checks
            .iter()
            .any(|(position, _)| *position > set_boot_order_pos),
        "expected boot-order verification after the write; got: {actions:?}"
    );
    assert!(
        boot_order_checks
            .iter()
            .all(|(_, boot_interface_mac)| *boot_interface_mac == set_boot_order_mac),
        "expected every order check and write to target the same interface; got: {actions:?}"
    );

    let rebooting_machine = machine.next_iteration_machine(env).await;
    assert!(
        has_dpu_reprovision_state(rebooting_machine.current_state(), |state| {
            matches!(state, ReprovisionState::RebootHost)
        }),
        "expected DPU reprovision host boot repair to transition to RebootHost; got: {:?}",
        rebooting_machine.current_state()
    );
    assert!(
        rebooting_machine.reprovision_requested.is_some(),
        "expected DPU reprovision request to remain present until the final host reboot is handled"
    );

    // Clearing the request before RebootHost would make wedged repair work non-restartable.
    let final_reboot_timepoint = env.redfish_sim.timepoint();
    let terminal_machine = machine.next_iteration_machine(env).await;
    assert!(
        terminal_machine.reprovision_requested.is_none(),
        "expected DPU reprovision request to be cleared after final host reboot"
    );
    assert_eq!(
        env.redfish_sim
            .actions_since(&final_reboot_timepoint)
            .all_hosts(),
        vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)]
    );

    terminal_machine
}

async fn prepare_dpu_reprovision_host_boot_check(
    env: &TestEnv,
    mh: &TestManagedHost,
) -> TestMachine<DpuMachineId> {
    let dpu_machine = mh.dpu();
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::update_state(
        &mut txn,
        &mh.id,
        &mh.new_dpu_reprovision_state(ReprovisionState::CheckHostBootConfig),
    )
    .await
    .unwrap();
    db::machine::trigger_dpu_reprovisioning_request(&dpu_machine.id, &mut txn, "AdminCli", true)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    dpu_machine
}

async fn prepare_terminal_dpu_reprovision(
    env: &TestEnv,
    mh: &TestManagedHost,
) -> model::machine::ManagedHostStateSnapshot {
    use carbide_uuid::machine::AsMachineId;
    use state_controller::CheckApplied;

    assert_eq!(mh.dpu_ids.len(), 1);
    env.api
        .modify_dpf_state(tonic::Request::new(rpc::forge::ModifyDpfStateRequest {
            machine_id: Some(mh.id.into()),
            dpf_enabled: false,
        }))
        .await
        .expect("fixture should disable DPF");
    mh.network_configured(env).await;

    let mut txn = env.db_txn().await;
    let previous = mh.snapshot(&mut txn).await;
    // Mark earlier reboot receipts verified so the generic restart check
    // cannot preempt the terminal reprovision operation.
    for (id, receipt) in [
        (
            mh.id.to_machine_id(),
            previous.host_snapshot.status.last_reboot_requested,
        ),
        (
            mh.dpu_ids[0].to_machine_id(),
            previous.dpu_snapshots[0].status.last_reboot_requested,
        ),
    ] {
        if let Some(receipt) = receipt
            && receipt.restart_verified == Some(false)
        {
            db::machine::update_restart_verification_status(&id, receipt, Some(true), 0, &mut txn)
                .await
                .expect("fixture should verify its preceding reboot")
                .check_applied()
                .expect("fixture reboot receipt should still match");
        }
    }
    db::machine::trigger_dpu_reprovisioning_request(&mh.dpu_ids[0], txn.as_mut(), "AdminCli", true)
        .await
        .expect("fixture should request DPU reprovisioning");
    db::machine::update_dpu_reprovision_start_time(&mh.dpu_ids[0], txn.as_mut())
        .await
        .expect("fixture should start DPU reprovisioning");
    db::machine::approve_dpu_reprovision_request(&mh.dpu_ids[0], txn.as_mut())
        .await
        .expect("fixture should approve DPU reprovisioning");
    // Restart detection compares the request time with the state token.
    // Persist the terminal state after every request timestamp is set.
    db::machine::update_state(
        txn.as_mut(),
        &mh.id.into(),
        &mh.new_dpu_reprovision_state(ReprovisionState::RebootHost),
    )
    .await
    .expect("fixture should enter RebootHost");
    sqlx::query("UPDATE machines SET controller_state_outcome = NULL WHERE id = $1")
        .bind(mh.id)
        .execute(txn.as_mut())
        .await
        .expect("fixture should clear its preceding controller outcome");
    let snapshot = mh.snapshot(&mut txn).await;
    txn.commit().await.unwrap();

    let request = snapshot.dpu_snapshots[0]
        .reprovision_requested
        .as_ref()
        .expect("fixture should retain its complete reprovision request");
    for timestamp in [
        request.requested_at,
        request
            .started_at
            .expect("fixture request should be started"),
        request.restart_reprovision_requested_at,
    ] {
        assert!(timestamp < snapshot.host_snapshot.state.version.timestamp());
    }
    snapshot
}

/// Runs terminal reprovision against a public DPF update in both queue orders.
/// Request removal, successor state, and reboot bookkeeping must commit together
/// without repeating the accepted reboot while database completion waits.
#[crate::sqlx_test]
async fn terminal_reprovision_and_dpf_updates_do_not_deadlock(pool: sqlx::PgPool) {
    use carbide_uuid::machine::AsMachineId;
    use db::state_history::{StateHistoryTableId, find_by_object_ids};
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use model::controller_outcome::PersistentStateHandlerOutcome;
    use model::machine::Dpf;
    use tokio::time::{Duration as TimeoutDuration, timeout};

    use crate::tests::machine_states::{
        gate_controller_machine_write, remove_controller_machine_write_gate,
    };

    const RECEIPT_UPDATE: &str =
        "UPDATE machines SET last_reboot_requested=$1 WHERE id=$2 RETURNING id";

    async fn wait_for_terminal_host_write(
        pool: &sqlx::PgPool,
        blocker_pid: i32,
    ) -> (i32, &'static str) {
        // Queued reboot metadata is applied before the framework's state CAS.
        // Admission can wait for the host before returning that transaction.
        let receipt_update = RECEIPT_UPDATE;
        let isolated_host_lock = "SELECT id FROM machines WHERE id = $1 FOR UPDATE";
        tokio::select! {
            pid = wait_for_blocked_query(pool, blocker_pid, receipt_update) => (pid, receipt_update),
            pid = wait_for_blocked_query(pool, blocker_pid, isolated_host_lock) => (pid, isolated_host_lock),
        }
    }

    Box::pin(async move {
        let env = create_test_env(pool).await;
        let mh = create_managed_host(&env).await;
        assert_eq!(mh.dpu_ids.len(), 1);
        let machine_ids = [mh.id.to_machine_id(), mh.dpu_ids[0].to_machine_id()];
        let terminal_state = mh.new_dpu_reprovision_state(ReprovisionState::RebootHost);
        let successor = ManagedHostState::HostInit {
            machine_state: MachineState::Discovered {
                skip_reboot_wait: false,
            },
        };

        for (case, dpf_first) in [
            ("terminal admitted, public DPF waits", false),
            ("public DPF before isolated terminal wait", true),
        ] {
            let before = prepare_terminal_dpu_reprovision(&env, &mh).await;
            let mut txn = env.db_txn().await;
            let history_before =
                find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                    .await
                    .expect("fixture state history should be readable");
            txn.commit().await.unwrap();

            let request = before.dpu_snapshots[0]
                .reprovision_requested
                .as_ref()
                .expect("fixture should retain its complete reprovision request");
            assert_eq!(request.initiator, "AdminCli", "{case}");
            assert!(request.update_firmware, "{case}");
            assert!(request.user_approval_received, "{case}");
            for timestamp in [
                request.requested_at,
                request.started_at.expect("fixture request should be started"),
                request.restart_reprovision_requested_at,
            ] {
                assert!(
                    timestamp < before.host_snapshot.state.version.timestamp(),
                    "{case}"
                );
            }
            assert_eq!(before.host_snapshot.state.value, terminal_state, "{case}");
            assert_eq!(before.dpu_snapshots[0].state.value, terminal_state, "{case}");
            assert_eq!(
                before.dpu_snapshots[0].state.version,
                before.host_snapshot.state.version,
                "{case}"
            );
            assert!(
                before.host_snapshot.controller_state_outcome.is_none(),
                "{case}"
            );
            let host_endpoint = before.host_snapshot.bmc_addr().unwrap().ip().to_string();
            env.redfish_sim
                .set_power_state(&host_endpoint, libredfish::PowerState::On);
            let redfish_timepoint = env.redfish_sim.timepoint();
            let operation_started_at = Utc::now();

            let mut gate = if dpf_first {
                let mut gate = env.db_txn().await;
                db::machine::lock_by_id_for_update(gate.as_mut(), &machine_ids[0])
                    .await
                    .expect("fixture should lock its host");
                gate
            } else {
                gate_controller_machine_write(&env.pool, machine_ids[0])
                    .await
                    .expect("fixture should gate its admitted host receipt")
            };
            let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(gate.as_mut())
                .await
                .unwrap();
            // `maybe_done` retains either result if it finishes during a failed
            // readiness check. Both real futures must still be joined.
            let iteration = maybe_done(env.run_machine_state_controller_iteration_no_requeue());
            let update = maybe_done(env.api.modify_dpf_state(tonic::Request::new(
                rpc::forge::ModifyDpfStateRequest {
                    machine_id: Some(mh.id.into()),
                    dpf_enabled: true,
                },
            )));
            tokio::pin!(iteration, update);
            let mut completion_pid = None;
            let mut update_pid = None;
            let mut completion_query = None;
            let readiness = timeout(TimeoutDuration::from_secs(25), async {
                if dpf_first {
                    update_pid = Some(tokio::select! {
                        () = &mut update => return Err(format!(
                            "DPF update passed the host gate: {:?}", update.as_mut().output_mut()
                        )),
                        pid = wait_for_blocked_query(&env.pool, gate_pid, "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => pid,
                    });
                    let (pid, query) = tokio::select! {
                        () = &mut update => return Err(format!(
                            "DPF update passed the host gate: {:?}", update.as_mut().output_mut()
                        )),
                        () = &mut iteration => return Err("controller did not reach terminal completion".to_string()),
                        blocked = wait_for_terminal_host_write(&env.pool, update_pid.unwrap()) => blocked,
                    };
                    completion_pid = Some(pid);
                    completion_query = Some(query);
                } else {
                    let pid = tokio::select! {
                        () = &mut iteration => return Err("controller did not reach its admitted receipt write".to_string()),
                        pid = wait_for_blocked_query(&env.pool, gate_pid, RECEIPT_UPDATE) => pid,
                    };
                    completion_pid = Some(pid);
                    completion_query = Some(RECEIPT_UPDATE);
                    update_pid = Some(tokio::select! {
                        () = &mut iteration => return Err("controller passed its receipt gate".to_string()),
                        () = &mut update => return Err(format!(
                            "DPF update passed the host gate: {:?}", update.as_mut().output_mut()
                        )),
                        pid = wait_for_blocked_query(&env.pool, pid, "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => pid,
                    });
                }
                Ok::<(), String>(())
            })
            .await
            .unwrap_or_else(|error| Err(format!("terminal lock readiness timed out: {error}")));
            tracing::info!(
                case,
                ?gate_pid,
                ?completion_pid,
                ?update_pid,
                ?completion_query,
                "Observed terminal reprovision lock ordering"
            );

            let accepted_actions = env
                .redfish_sim
                .actions_since(&redfish_timepoint)
                .all_hosts();
            let mut txn = env.db_txn().await;
            let waiting = mh.snapshot(&mut txn).await;
            let waiting_history =
                find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids).await;
            txn.commit().await.unwrap();
            let gate_release = gate.commit().await;

            let completion_timed_out = {
                let joined = async { tokio::join!(&mut iteration, &mut update) };
                tokio::pin!(joined);
                if timeout(TimeoutDuration::from_secs(30), &mut joined)
                    .await
                    .is_ok()
                {
                    false
                } else {
                    // Failure cleanup only: cancel the two observed writers in
                    // this private database, then let the controller join its
                    // children before reporting the original failure.
                    let mut cancellation_results = Vec::new();
                    for pid in completion_pid.into_iter().chain(update_pid) {
                        cancellation_results.push((
                            pid,
                            sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                                .bind(pid)
                                .fetch_one(&env.pool)
                                .await,
                        ));
                    }
                    timeout(TimeoutDuration::from_secs(30), &mut joined)
                        .await
                        .unwrap_or_else(|error| {
                            panic!(
                                "{case}: writer cleanup did not settle: {error}; readiness: {readiness:?}; cancellations: {cancellation_results:?}"
                            )
                        });
                    true
                }
            };
            let trigger_cleanup = if dpf_first {
                Ok(())
            } else {
                remove_controller_machine_write_gate(&env.pool).await
            };
            let update_result = update
                .as_mut()
                .take_output()
                .expect("DPF future should be joined");
            assert!(
                !completion_timed_out,
                "{case}: terminal completion exceeded 30s; readiness: {readiness:?}; DPF: {update_result:?}"
            );
            gate_release.expect("database gate should release");
            readiness.unwrap_or_else(|error| panic!("{case}: {error}"));
            trigger_cleanup.expect("joined terminal row should remove its test trigger");
            let operation_completed_at = Utc::now();
            update_result.unwrap_or_else(|error| panic!("{case}: public DPF update failed: {error}"));

            assert_eq!(
                accepted_actions,
                vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)],
                "{case}: terminal host reboot must be accepted before database completion"
            );
            assert_eq!(
                env.redfish_sim
                    .actions_since(&redfish_timepoint)
                    .all_hosts(),
                accepted_actions,
                "{case}: completion must retain the accepted reboot"
            );
            assert_eq!(
                env.redfish_sim
                    .actions_since(&redfish_timepoint)
                    .for_host(&host_endpoint),
                accepted_actions,
                "{case}: the accepted reboot must target the host"
            );
            assert_eq!(
                serde_json::to_value(waiting_history.expect("paused history should be readable")).unwrap(),
                serde_json::to_value(&history_before).unwrap(),
                "{case}: terminal history must remain invisible before commit"
            );
            for (waiting_state, previous_state) in [
                (&waiting.host_snapshot.state, &before.host_snapshot.state),
                (&waiting.dpu_snapshots[0].state, &before.dpu_snapshots[0].state),
            ] {
                assert_eq!(waiting_state.value, previous_state.value, "{case}");
                assert_eq!(waiting_state.version, previous_state.version, "{case}");
            }
            assert_eq!(
                waiting.dpu_snapshots[0].reprovision_requested.as_ref(),
                Some(request),
                "{case}: the exact completed request remains visible until commit"
            );
            assert_eq!(
                waiting.host_snapshot.status.last_reboot_requested,
                before.host_snapshot.status.last_reboot_requested,
                "{case}: accepted reboot metadata must commit with terminal state"
            );
            assert!(
                waiting.host_snapshot.controller_state_outcome.is_none(),
                "{case}"
            );
            assert_eq!(
                waiting.host_snapshot.config.dpf,
                before.host_snapshot.config.dpf,
                "{case}"
            );
            assert_eq!(
                waiting.dpu_snapshots[0].config.dpf,
                before.dpu_snapshots[0].config.dpf,
                "{case}"
            );

            let mut txn = env.db_txn().await;
            let after = mh.snapshot(&mut txn).await;
            let next_version = after.host_snapshot.state.version;
            assert_eq!(
                after.host_snapshot.state.value,
                successor,
                "{case}: controller failures must not look like a completed iteration: {:?}",
                after.host_snapshot.controller_state_outcome
            );
            assert_eq!(
                next_version.version_nr(),
                before.host_snapshot.state.version.version_nr() + 1,
                "{case}"
            );
            assert_eq!(after.dpu_snapshots.len(), 1, "{case}");
            let dpu = &after.dpu_snapshots[0];
            assert_eq!(dpu.id, mh.dpu_ids[0], "{case}");
            assert_eq!(dpu.state.value, successor, "{case}");
            assert_eq!(dpu.state.version, next_version, "{case}");
            assert_eq!(
                dpu.state.version.version_nr(),
                before.dpu_snapshots[0].state.version.version_nr() + 1,
                "{case}"
            );
            assert!(
                dpu.reprovision_requested.is_none(),
                "{case}: the captured completed request must be cleared"
            );
            assert!(
                matches!(
                    after.host_snapshot.controller_state_outcome,
                    Some(PersistentStateHandlerOutcome::Transition { .. })
                ),
                "{case}: terminal state and a new Transition must commit together: {:?}",
                after.host_snapshot.controller_state_outcome
            );
            assert_eq!(
                after.host_snapshot.config.dpf,
                Dpf {
                    enabled: true,
                    ..before.host_snapshot.config.dpf.clone()
                },
                "{case}"
            );
            assert_eq!(
                dpu.config.dpf,
                Dpf {
                    enabled: true,
                    ..before.dpu_snapshots[0].config.dpf.clone()
                },
                "{case}"
            );
            assert_eq!(
                dpu.status.last_reboot_requested,
                before.dpu_snapshots[0].status.last_reboot_requested,
                "{case}: ForceRestart of the host must preserve the DPU receipt"
            );

            let receipt = after
                .host_snapshot
                .status
                .last_reboot_requested
                .expect("terminal host reboot should persist its receipt");
            assert_eq!(receipt.mode, MachineLastRebootRequestedMode::Reboot, "{case}");
            assert_eq!(receipt.restart_verified, Some(false), "{case}");
            assert_eq!(receipt.verification_attempts, Some(0), "{case}");
            assert!(
                receipt.time >= operation_started_at && receipt.time <= operation_completed_at,
                "{case}: reboot receipt must belong to the observed invocation: {receipt:?}"
            );
            if let Some(previous_receipt) = before.host_snapshot.status.last_reboot_requested {
                assert!(
                    receipt.time > previous_receipt.time,
                    "{case}: terminal completion must record a new accepted reboot"
                );
            }

            let history_after =
                find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                    .await
                    .expect("completed state history should be readable");
            for id in &machine_ids {
                let previous = &history_before[&id.to_string()];
                let current = &history_after[&id.to_string()];
                assert_eq!(current.len(), previous.len() + 1, "{case}: {id}");
                assert_eq!(
                    serde_json::to_value(&current[..previous.len()]).unwrap(),
                    serde_json::to_value(previous).unwrap(),
                    "{case}: {id}: retained history must remain exact"
                );
                let latest = current.last().unwrap();
                assert_eq!(latest.state_version, next_version, "{case}: {id}");
                assert_eq!(
                    serde_json::from_str::<ManagedHostState>(&latest.state).unwrap(),
                    successor,
                    "{case}: {id}"
                );
            }
            txn.commit().await.unwrap();
        }
    })
    .await;
}

/// A DPU row wait must release the attempted cohort while the same invocation
/// keeps its accepted host reboot and commits the terminal transition.
#[crate::sqlx_test]
async fn terminal_reprovision_releases_locks_while_retaining_accepted_reboot(pool: sqlx::PgPool) {
    use carbide_uuid::machine::{AsMachineId, MachineInterfaceId};
    use db::state_history::{StateHistoryTableId, find_by_object_ids};
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use model::controller_outcome::PersistentStateHandlerOutcome;
    use model::machine::{HostReprovisionRequest, HostReprovisionState};
    use tokio::time::{Duration as TimeoutDuration, timeout};

    Box::pin(async move {
        let env = create_test_env(pool).await;
        let mh = create_managed_host(&env).await;
        let segment_id = env.create_vpc_and_tenant_segment().await;
        let instance = mh
            .instance_builer(&env)
            .single_interface_network_config(segment_id)
            .build()
            .await;
        prepare_terminal_dpu_reprovision(&env, &mh).await;
        let machine_ids = [mh.id.to_machine_id(), mh.dpu_ids[0].to_machine_id()];
        let mut txn = env.db_txn().await;
        let terminal_state = mh.new_dpu_assigned_reprovision_state(ReprovisionState::RebootHost);
        db::machine::update_state(txn.as_mut(), &mh.id.into(), &terminal_state)
            .await
            .expect("allocated fixture enters terminal DPU reprovision");
        let repeated_interface: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                 (machine_id, attached_dpu_machine_id, association_type, segment_id,
                  mac_address, primary_interface, hostname, interface_type)
             SELECT machine_id, attached_dpu_machine_id, association_type, segment_id,
                    '02:00:53:33:00:01'::macaddr, false, 'terminal-repeated', interface_type
             FROM machine_interfaces
             WHERE machine_id = $1 AND attached_dpu_machine_id = $2
               AND interface_type = 'Data'
             ORDER BY id LIMIT 1 RETURNING id",
        )
        .bind(mh.id)
        .bind(mh.dpu_ids[0])
        .fetch_one(txn.as_mut())
        .await
        .expect("the same DPU has a second nonprimary data attachment");
        let before = mh.snapshot(&mut txn).await;
        assert_eq!(before.dpu_snapshots.len(), 2);
        assert!(before.dpu_snapshots.iter().all(|dpu| dpu.id == mh.dpu_ids[0]));
        assert!(before.host_snapshot.status.interfaces.iter().any(|interface|
            interface.id == repeated_interface && !interface.primary_interface));
        assert!(before.host_snapshot.host_reprovision_requested.is_none());
        assert_eq!(before.host_snapshot.state.value, terminal_state);
        assert_eq!(before.instance.as_ref().unwrap().id, instance.id);
        let history_before =
            find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                .await
                .unwrap();
        let expected_interfaces = sqlx::query_scalar::<_, MachineInterfaceId>(
            "SELECT id FROM machine_interfaces WHERE machine_id = $1 ORDER BY id",
        )
        .bind(mh.id)
        .fetch_all(txn.as_mut())
        .await
        .unwrap();
        txn.commit().await.unwrap();
        let host_endpoint = before.host_snapshot.bmc_addr().unwrap().ip().to_string();
        env.redfish_sim
            .set_power_state(&host_endpoint, libredfish::PowerState::On);
        let redfish_timepoint = env.redfish_sim.timepoint();
        let operation_started_at = Utc::now();

        let mut gate = env.db_txn().await;
        db::machine::lock_by_id_for_update(gate.as_mut(), &machine_ids[1])
            .await
            .unwrap();
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await
            .unwrap();
        let iteration = maybe_done(env.run_machine_state_controller_iteration_no_requeue());
        tokio::pin!(iteration);
        let readiness = timeout(TimeoutDuration::from_secs(25), async {
            tokio::select! {
                () = &mut iteration => Err("controller finished before its isolated DPU wait".to_string()),
                pid = wait_for_blocked_query(
                    &env.pool,
                    gate_pid,
                    "SELECT id FROM machines WHERE id = $1 FOR UPDATE",
                ) => Ok(pid),
            }
        })
        .await
        .unwrap_or_else(|error| Err(format!("isolated DPU wait timed out: {error}")));
        let completion_pid = readiness.as_ref().ok().copied();
        let accepted_actions = env
            .redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts();

        // The DPU is the last row attempted in this one-DPU cohort. Its wait
        // must hold none of the host, interface, or history locks visited first.
        // Retain probe errors until the gate is released and children are joined.
        let host_probe = sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(mh.id)
            .execute(gate.as_mut())
            .await;
        let interface_probe = sqlx::query_scalar::<_, MachineInterfaceId>(
            "SELECT id FROM machine_interfaces WHERE machine_id = $1 ORDER BY id FOR UPDATE NOWAIT",
        )
        .bind(mh.id)
        .fetch_all(gate.as_mut())
        .await;
        let mut history_probes = Vec::new();
        for id in machine_ids {
            let acquired = sqlx::query_scalar::<_, bool>(
                "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 'machine_state_history'::regclass::bigint))",
            )
            .bind(id.to_string())
            .fetch_one(gate.as_mut())
            .await;
            history_probes.push((id, acquired));
        }
        // Host Set writes a request without advancing the controller token.
        // Completion must choose its successor from that newly committed request.
        let request_write = db::host_machine_update::trigger_host_reprovisioning_request(
            gate.as_mut(),
            "terminal-completion",
            &mh.id.into(),
        )
        .await;
        let written_request: Result<
            (config_version::ConfigVersion, sqlx::types::Json<HostReprovisionRequest>),
            sqlx::Error,
        > = sqlx::query_as(
            "SELECT controller_state_version, host_reprovisioning_requested
             FROM machines WHERE id = $1",
        )
        .bind(mh.id)
        .fetch_one(gate.as_mut())
        .await;
        let mut txn = env.db_txn().await;
        let waiting = mh.snapshot(&mut txn).await;
        txn.commit().await.unwrap();
        let gate_release = gate.commit().await;

        let completion_timed_out = if timeout(TimeoutDuration::from_secs(30), &mut iteration)
            .await
            .is_ok()
        {
            false
        } else {
            let mut cancellation = None;
            if let Some(pid) = completion_pid {
                cancellation = Some(
                    sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                        .bind(pid)
                        .fetch_one(&env.pool)
                        .await,
                );
            }
            timeout(TimeoutDuration::from_secs(30), &mut iteration)
                .await
                .unwrap_or_else(|error| {
                    panic!("controller cleanup did not settle: {error}; readiness: {readiness:?}; cancellation: {cancellation:?}")
                });
            true
        };
        let operation_completed_at = Utc::now();
        assert!(
            !completion_timed_out,
            "terminal completion exceeded 30s: {readiness:?}"
        );
        gate_release.expect("DPU gate and retained probes should release");
        readiness.unwrap_or_else(|error| panic!("{error}"));
        request_write.expect("the production Host Set writer stores its request");
        let (request_version, sqlx::types::Json(written_request)) =
            written_request.expect("read the request under the gate's host lock");
        assert_eq!(request_version, before.host_snapshot.state.version);
        assert_eq!(written_request.initiator, "terminal-completion");
        assert!(written_request.started_at.is_none());
        assert!(!written_request.user_approval_received);
        assert_eq!(written_request.request_reset, Some(false));
        assert!(
            written_request.requested_at >= operation_started_at
                && written_request.requested_at <= operation_completed_at
        );
        host_probe.expect("isolated DPU wait must release the host row");
        assert_eq!(
            interface_probe.expect("isolated DPU wait must release every host interface"),
            expected_interfaces
        );
        for (id, acquired) in history_probes {
            assert!(
                acquired.unwrap(),
                "isolated DPU wait must release history for {id}"
            );
        }
        assert_eq!(
            accepted_actions,
            vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)]
        );
        assert_eq!(
            env.redfish_sim
                .actions_since(&redfish_timepoint)
                .all_hosts(),
            accepted_actions,
            "database retries must retain the accepted reboot"
        );
        assert_eq!(
            env.redfish_sim
                .actions_since(&redfish_timepoint)
                .for_host(&host_endpoint),
            accepted_actions
        );
        assert_eq!(
            waiting.dpu_snapshots[0].reprovision_requested,
            before.dpu_snapshots[0].reprovision_requested
        );
        assert_eq!(
            waiting.host_snapshot.status.last_reboot_requested,
            before.host_snapshot.status.last_reboot_requested
        );
        assert!(waiting.host_snapshot.host_reprovision_requested.is_none());
        assert_eq!(waiting.host_snapshot.state.value, terminal_state);
        assert_eq!(
            waiting.host_snapshot.state.version,
            before.host_snapshot.state.version
        );

        let mut txn = env.db_txn().await;
        let after = mh.snapshot(&mut txn).await;
        let successor = ManagedHostState::Assigned {
            instance_state: InstanceState::HostReprovision {
                reprovision_state: HostReprovisionState::CheckingFirmwareV2 {
                    firmware_type: None,
                    firmware_number: None,
                },
            },
        };
        assert_eq!(
            serde_json::to_value(&after.host_snapshot.host_reprovision_requested).unwrap(),
            serde_json::to_value(Some(&written_request)).unwrap(),
            "terminal completion must retain the fresh Host Set request"
        );
        let retained_instance = after.instance.as_ref().expect("allocated Instance remains");
        assert_eq!(retained_instance.id, instance.id);
        assert_eq!(&retained_instance.machine_id, mh.id.as_host_machine_id());
        assert!(retained_instance.deleted.is_none());
        let next_version = after.host_snapshot.state.version;
        assert_eq!(
            after.host_snapshot.state.value,
            successor,
            "{:?}",
            after.host_snapshot.controller_state_outcome
        );
        assert_eq!(
            next_version.version_nr(),
            before.host_snapshot.state.version.version_nr() + 1
        );
        assert_eq!(after.dpu_snapshots.len(), 2);
        assert!(after.dpu_snapshots.iter().all(|dpu|
            dpu.id == mh.dpu_ids[0]
                && dpu.state.value == successor
                && dpu.state.version == next_version
                && dpu.reprovision_requested.is_none()));
        let dpu = &after.dpu_snapshots[0];
        assert_eq!(dpu.id, mh.dpu_ids[0]);
        assert_eq!(dpu.state.value, successor);
        assert_eq!(dpu.state.version, next_version);
        assert_eq!(
            dpu.state.version.version_nr(),
            before.dpu_snapshots[0].state.version.version_nr() + 1
        );
        assert!(dpu.reprovision_requested.is_none());
        assert!(matches!(
            after.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Transition { .. })
        ));
        assert_eq!(
            dpu.status.last_reboot_requested,
            before.dpu_snapshots[0].status.last_reboot_requested
        );
        let receipt = after.host_snapshot.status.last_reboot_requested.unwrap();
        assert_eq!(receipt.mode, MachineLastRebootRequestedMode::Reboot);
        assert_eq!(receipt.restart_verified, Some(false));
        assert_eq!(receipt.verification_attempts, Some(0));
        assert!(receipt.time >= operation_started_at && receipt.time <= operation_completed_at);
        if let Some(previous) = before.host_snapshot.status.last_reboot_requested {
            assert!(receipt.time > previous.time);
        }
        let history_after =
            find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                .await
                .unwrap();
        for id in machine_ids {
            let previous = &history_before[&id.to_string()];
            let current = &history_after[&id.to_string()];
            assert_eq!(current.len(), previous.len() + 1, "{id}");
            assert_eq!(
                serde_json::to_value(&current[..previous.len()]).unwrap(),
                serde_json::to_value(previous).unwrap(),
                "{id}"
            );
            let latest = current.last().unwrap();
            assert_eq!(latest.state_version, next_version, "{id}");
            assert_eq!(
                serde_json::from_str::<ManagedHostState>(&latest.state).unwrap(),
                successor,
                "{id}"
            );
        }
        txn.commit().await.unwrap();
    })
    .await;
}

/// Restart changes can revoke completion without changing the host token or
/// original request time. The winning full request and state must survive.
#[crate::sqlx_test]
async fn terminal_reprovision_rejects_restart_requested_while_completion_waits(pool: sqlx::PgPool) {
    use carbide_machine_controller::metrics::MachineMetrics;
    use carbide_uuid::machine::{AsMachineId, HostMachineId};
    use db::state_history::{StateHistoryTableId, find_by_object_ids};
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use state_controller::db_write_batch::DbWriteBatch;
    use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerError};
    use tokio::time::{Duration as TimeoutDuration, timeout};

    Box::pin(async move {
        let env = create_test_env(pool).await;
        let mh = create_managed_host(&env).await;
        let before = prepare_terminal_dpu_reprovision(&env, &mh).await;
        let captured_request = before.dpu_snapshots[0]
            .reprovision_requested
            .clone()
            .unwrap();
        let machine_ids = [mh.id.to_machine_id(), mh.dpu_ids[0].to_machine_id()];
        let mut txn = env.db_txn().await;
        let history_before =
            find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                .await
                .unwrap();
        txn.commit().await.unwrap();
        let host_endpoint = before.host_snapshot.bmc_addr().unwrap().ip().to_string();
        env.redfish_sim
            .set_power_state(&host_endpoint, libredfish::PowerState::On);
        let redfish_timepoint = env.redfish_sim.timepoint();

        let mut gate = env.db_txn().await;
        db::machine::lock_by_id_for_update(gate.as_mut(), &machine_ids[1])
            .await
            .unwrap();
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await
            .unwrap();
        let host_id: HostMachineId = mh.id.into();
        let mut snapshot = before.clone();
        let captured_state = snapshot.managed_state.clone();
        let mut services = env.machine_state_handler_services();
        let mut metrics = MachineMetrics::default();
        let mut pending_db_writes = DbWriteBatch::new();
        let (result, readiness, accepted_actions, winning, winning_history) = {
            let mut ctx = StateHandlerContext {
                services: &mut services,
                metrics: &mut metrics,
                pending_db_writes: &mut pending_db_writes,
            };
            let attempt = maybe_done(env.machine_state_handler.handle_object_state(
                &host_id,
                &mut snapshot,
                &captured_state,
                &mut ctx,
            ));
            tokio::pin!(attempt);
            let readiness = timeout(TimeoutDuration::from_secs(25), async {
                tokio::select! {
                    () = &mut attempt => Err("handler finished before its isolated DPU wait".to_string()),
                    pid = wait_for_blocked_query(
                        &env.pool,
                        gate_pid,
                        "SELECT id FROM machines WHERE id = $1 FOR UPDATE",
                    ) => Ok(pid),
                }
            })
            .await
            .unwrap_or_else(|error| Err(format!("isolated DPU wait timed out: {error}")));
            let completion_pid = readiness.as_ref().ok().copied();
            let accepted_actions = env
                .redfish_sim
                .actions_since(&redfish_timepoint)
                .all_hosts();

            // The gate owns D, so the real Restart helper can update its request
            // while completion owns only an isolated wait for that same row.
            db::machine::restart_dpu_reprovisioning(
                gate.as_mut(),
                &[&mh.dpu_ids[0]],
                captured_request.update_firmware,
            )
            .await
            .expect("gate owner should submit a real Restart request");
            let winning = mh.snapshot(&mut gate).await;
            let winning_history =
                find_by_object_ids(gate.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                    .await
                    .unwrap();
            let gate_release = gate.commit().await;
            let completion_timed_out = if timeout(TimeoutDuration::from_secs(30), &mut attempt)
                .await
                .is_ok()
            {
                false
            } else {
                let mut cancellation = None;
                if let Some(pid) = completion_pid {
                    cancellation = Some(
                        sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                            .bind(pid)
                            .fetch_one(&env.pool)
                            .await,
                    );
                }
                timeout(TimeoutDuration::from_secs(30), &mut attempt)
                    .await
                    .unwrap_or_else(|error| {
                        panic!("handler cleanup did not settle: {error}; readiness: {readiness:?}; cancellation: {cancellation:?}")
                    });
                true
            };
            let result = attempt
                .as_mut()
                .take_output()
                .expect("handler should be joined")
                .map(|outcome| outcome.to_string());
            assert!(
                !completion_timed_out,
                "terminal completion exceeded 30s: {readiness:?}; result: {result:?}"
            );
            gate_release.expect("Restart request should commit and release the DPU gate");
            (result, readiness, accepted_actions, winning, winning_history)
        };
        // The processor discards this unconsumed batch on invalidation. The
        // rejected completion must leave the persisted reboot receipt unchanged.
        drop(pending_db_writes);
        readiness.unwrap_or_else(|error| panic!("{error}"));
        assert!(
            matches!(result, Err(StateHandlerError::IterationInvalidated { .. })),
            "{result:?}"
        );
        assert_eq!(
            accepted_actions,
            vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)]
        );
        assert_eq!(
            env.redfish_sim
                .actions_since(&redfish_timepoint)
                .all_hosts(),
            accepted_actions
        );
        assert_eq!(
            env.redfish_sim
                .actions_since(&redfish_timepoint)
                .for_host(&host_endpoint),
            accepted_actions
        );
        let winning_request = winning.dpu_snapshots[0]
            .reprovision_requested
            .as_ref()
            .unwrap();
        assert_ne!(
            winning_request.restart_reprovision_requested_at,
            captured_request.restart_reprovision_requested_at
        );
        let mut expected_request = captured_request.clone();
        expected_request.restart_reprovision_requested_at =
            winning_request.restart_reprovision_requested_at;
        assert_eq!(
            winning_request,
            &expected_request,
            "Restart time must be the only changed field"
        );
        assert_eq!(
            winning.host_snapshot.state.version,
            before.host_snapshot.state.version
        );
        assert_eq!(
            winning.host_snapshot.state.value,
            before.host_snapshot.state.value
        );
        assert_eq!(
            winning.dpu_snapshots[0].state.version,
            before.dpu_snapshots[0].state.version
        );
        assert_eq!(
            winning.dpu_snapshots[0].state.value,
            before.dpu_snapshots[0].state.value
        );
        assert_eq!(
            serde_json::to_value(&winning_history).unwrap(),
            serde_json::to_value(&history_before).unwrap()
        );

        let mut txn = env.db_txn().await;
        let after = mh.snapshot(&mut txn).await;
        assert_eq!(
            after.host_snapshot.state.value,
            winning.host_snapshot.state.value
        );
        assert_eq!(
            after.host_snapshot.state.version,
            winning.host_snapshot.state.version
        );
        assert_eq!(
            after.host_snapshot.controller_state_outcome,
            winning.host_snapshot.controller_state_outcome
        );
        assert_eq!(
            after.host_snapshot.status.last_reboot_requested,
            before.host_snapshot.status.last_reboot_requested
        );
        assert_eq!(
            after.dpu_snapshots[0].state.value,
            winning.dpu_snapshots[0].state.value
        );
        assert_eq!(
            after.dpu_snapshots[0].state.version,
            winning.dpu_snapshots[0].state.version
        );
        assert_eq!(
            after.dpu_snapshots[0].controller_state_outcome,
            winning.dpu_snapshots[0].controller_state_outcome
        );
        assert_eq!(
            after.dpu_snapshots[0].reprovision_requested.as_ref(),
            Some(winning_request)
        );
        assert_eq!(
            after.dpu_snapshots[0].status.last_reboot_requested,
            before.dpu_snapshots[0].status.last_reboot_requested
        );
        let history_after =
            find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                .await
                .unwrap();
        assert_eq!(
            serde_json::to_value(history_after).unwrap(),
            serde_json::to_value(winning_history).unwrap()
        );
        txn.commit().await.unwrap();
    })
    .await;
}

#[crate::sqlx_test]
async fn test_dpu_for_set_clear_reprovisioning(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(dpu.reprovision_requested.is_none(),);

    mh.mark_machine_for_updates().await;

    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;

    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert_eq!(&dpu.reprovision_requested.unwrap().initiator, "AdminCli");

    let res = env
        .api
        .list_dpu_waiting_for_reprovisioning(tonic::Request::new(
            ::rpc::forge::DpuReprovisioningListRequest {},
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(res.dpus.len(), 1);
    assert_eq!(res.dpus[0].id.unwrap().to_string(), mh.dpu().id.to_string());

    mh.dpu().trigger_dpu_reprovisioning(Mode::Clear, true).await;

    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(dpu.reprovision_requested.is_none());
}

#[crate::sqlx_test]
async fn test_dpu_reprovision_host_boot_repair_runs_machine_setup_when_bios_not_setup(
    pool: sqlx::PgPool,
) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let dpu_machine = prepare_dpu_reprovision_host_boot_check(&env, &mh).await;

    env.redfish_sim.set_is_boot_order_setup(true);
    env.redfish_sim.set_is_bios_setup(false);

    let redfish_timepoint = env.redfish_sim.timepoint();

    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::ConfigureHostBoot { retry_count: 0 })
    );

    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::PollingHostBiosSetup { retry_count: 0 })
    );

    let actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .all_hosts();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, RedfishSimAction::MachineSetup { .. })),
        "expected DPU reprovision host boot repair to run machine_setup when BIOS setup is false; got: {actions:?}"
    );
}

#[crate::sqlx_test]
async fn test_dpu_reprovision_viking_repairs_bios_before_boot_order_skip(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(DGX_H100_INFO_JSON),
    )
    .await;
    let dpu_machine = prepare_dpu_reprovision_host_boot_check(&env, &mh).await;

    env.redfish_sim.set_is_boot_order_setup(false);
    env.redfish_sim.set_is_bios_setup(false);

    let redfish_timepoint = env.redfish_sim.timepoint();

    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::ConfigureHostBoot { retry_count: 0 })
    );

    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::PollingHostBiosSetup { retry_count: 0 })
    );

    env.redfish_sim.set_is_bios_setup(true);
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::LockHostAfterBootRepair)
    );

    let actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .all_hosts();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, RedfishSimAction::MachineSetup { .. })),
        "expected DPU reprovision host boot repair to run machine_setup when Viking BIOS setup is false; got: {actions:?}"
    );
    assert!(
        actions.iter().all(|action| !matches!(
            action,
            RedfishSimAction::SetBootOrderDpuFirst { .. }
                | RedfishSimAction::IsBootOrderSetup { .. }
        )),
        "expected Viking DPU reprovision host boot repair to skip boot-order remediation after BIOS repair; got: {actions:?}"
    );
}

#[crate::sqlx_test]
async fn test_dpu_reprovision_viking_skips_boot_order_when_bios_setup(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(DGX_H100_INFO_JSON),
    )
    .await;
    let dpu_machine = prepare_dpu_reprovision_host_boot_check(&env, &mh).await;

    env.redfish_sim.set_is_boot_order_setup(false);
    env.redfish_sim.set_is_bios_setup(true);

    let redfish_timepoint = env.redfish_sim.timepoint();

    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::LockHostAfterBootRepair)
    );

    let actions = env
        .redfish_sim
        .actions_since(&redfish_timepoint)
        .all_hosts();
    assert!(
        actions.iter().all(|action| !matches!(
            action,
            RedfishSimAction::MachineSetup { .. }
                | RedfishSimAction::SetBootOrderDpuFirst { .. }
                | RedfishSimAction::IsBootOrderSetup { .. }
        )),
        "expected Viking DPU reprovision host boot repair to skip BIOS and boot-order remediation when BIOS setup is true; got: {actions:?}"
    );
}

#[crate::sqlx_test]
async fn test_dpu_reprovision_viking_finishes_parked_boot_order_recovery(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(DGX_H100_INFO_JSON),
    )
    .await;
    let dpu_machine = prepare_dpu_reprovision_host_boot_check(&env, &mh).await;

    // A controller upgrade can find a Viking in a persisted recovery substate
    // created before boot-order remediation was disabled for this platform.
    // Finish restoring host power before taking the safe terminal shortcut.
    let parked_recovery = mh.new_dpu_reprovision_state(reprovision_set_host_boot_order_state(
        SetBootOrderState::HandleJobFailure {
            failure: "persisted failed boot-order job".to_string(),
            power_state: PowerState::Off,
        },
    ));
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::update_state(&mut txn, &mh.id, &parked_recovery)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let redfish_timepoint = env.redfish_sim.timepoint();
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(dpu.current_state(), &parked_recovery);
    assert_eq!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts(),
        vec![RedfishSimAction::Power(SystemPowerControl::ForceOff)],
        "Viking policy must not abandon an in-flight power recovery"
    );

    let recovering_power_on = mh.new_dpu_reprovision_state(reprovision_set_host_boot_order_state(
        SetBootOrderState::HandleJobFailure {
            failure: "persisted failed boot-order job".to_string(),
            power_state: PowerState::On,
        },
    ));
    let redfish_timepoint = env.redfish_sim.timepoint();
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(dpu.current_state(), &recovering_power_on);
    assert_eq!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts(),
        vec![RedfishSimAction::BmcReset(None)],
        "parked recovery should reset the BMC after the host powers off"
    );

    // ForceOff records last_reboot_requested; backdate it so power_down_wait
    // elapses in-process before the controller restores host power.
    {
        let mut txn = env.db_txn().await;
        let host = mh.host().db_machine(&mut txn).await;
        update_time_params(&env.pool, &host, 1, None).await;
    }

    let redfish_timepoint = env.redfish_sim.timepoint();
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(dpu.current_state(), &recovering_power_on);
    assert_eq!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts(),
        vec![RedfishSimAction::Power(SystemPowerControl::On)],
        "parked recovery should restore host power after the BMC reset"
    );

    let safe_terminal = mh.new_dpu_reprovision_state(reprovision_set_host_boot_order_state(
        SetBootOrderState::CheckBootOrder,
    ));
    let redfish_timepoint = env.redfish_sim.timepoint();
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(dpu.current_state(), &safe_terminal);
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty(),
        "completed recovery should naturally reach CheckBootOrder"
    );

    // CheckBootOrder has no unfinished operation behind it, but its preceding
    // reboot may have reverted the HTTP boot device. Repair that BIOS drift
    // without touching the unsupported Viking boot-order APIs, and spend one
    // attempt from the shared convergence budget.
    env.redfish_sim.set_is_bios_setup(false);

    let redfish_timepoint = env.redfish_sim.timepoint();
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::ConfigureHostBoot { retry_count: 1 })
    );
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty(),
        "Viking BIOS recovery must not read or write boot order"
    );

    // Once BIOS is intact, the same safe terminal skips boot-order
    // verification and completes the repair.
    env.redfish_sim.set_is_bios_setup(true);
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::update_state(&mut txn, &mh.id, &safe_terminal)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let redfish_timepoint = env.redfish_sim.timepoint();
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::LockHostAfterBootRepair)
    );
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty(),
        "safe Viking completion should not touch Redfish boot order"
    );
}

#[crate::sqlx_test]
async fn test_dpu_for_reprovisioning_fail_if_maintenance_not_set(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(dpu.reprovision_requested.is_none(),);

    assert!(
        env.api
            .trigger_dpu_reprovisioning(tonic::Request::new(
                ::rpc::forge::DpuReprovisioningRequest {
                    dpu_id: None,
                    machine_id: Some(mh.dpu().id.into()),
                    mode: rpc::forge::dpu_reprovisioning_request::Mode::Set as i32,
                    initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                    update_firmware: true
                },
            ))
            .await
            .is_err()
    );
}

#[crate::sqlx_test]
async fn test_dpu_for_reprovisioning_fail_if_state_is_not_ready(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (_, dpu_machine_id) = create_managed_host(&env).await.into();

    assert!(
        env.api
            .trigger_dpu_reprovisioning(tonic::Request::new(
                ::rpc::forge::DpuReprovisioningRequest {
                    dpu_id: None,
                    machine_id: Some(dpu_machine_id.into()),
                    mode: rpc::forge::dpu_reprovisioning_request::Mode::Set as i32,
                    initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                    update_firmware: true
                },
            ))
            .await
            .is_err()
    );
}

#[crate::sqlx_test]
async fn test_dpu_agent_control_requires_requested_machine(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id: DpuMachineId = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
        .parse()
        .unwrap();

    let error = env
        .api
        .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(dpu_id.into()),
            nic_firmware_site_cache_supported: false,
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound);
    assert_eq!(error.message(), format!("machine not found: {dpu_id}"));
}

#[crate::sqlx_test]
async fn test_dpu_agent_control_preserves_database_error_details(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id: DpuMachineId = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
        .parse()
        .unwrap();

    // Fail transaction acquisition before the DPU lookup; retain operator metadata.
    env.pool.close().await;
    let error = env
        .api
        .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(dpu_id.into()),
            nic_firmware_site_cache_supported: false,
        }))
        .await
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-500"
    );
}

#[crate::sqlx_test]
async fn test_dpu_agent_control_preserves_lookup_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let dpu_id: DpuMachineId = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
        .parse()
        .unwrap();

    // Keep transaction acquisition working while find_dpu_id cannot read machines.id.
    sqlx::query("ALTER TABLE machines RENAME COLUMN id TO unavailable_id")
        .execute(&env.pool)
        .await
        .expect("rename the machine ID column");

    let response = env
        .api
        .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(dpu_id.into()),
            nic_firmware_site_cache_supported: false,
        }))
        .await;

    // Restore the schema before asserting so a failed test leaves the database inspectable.
    sqlx::query("ALTER TABLE machines RENAME COLUMN unavailable_id TO id")
        .execute(&env.pool)
        .await
        .expect("restore the machine ID column");

    let error = response.expect_err("the DPU lookup must fail");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(error.message(), "internal database error");
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-500"
    );
    assert_eq!(
        error.metadata().get("nico-error-text").unwrap(),
        "internal database error"
    );

    let source = error
        .source()
        .expect("status retains its error source")
        .downcast_ref::<CarbideErrorWithLocation>()
        .expect("status retains the CarbideError");
    let CarbideError::DBError(database_error) = &source.error else {
        panic!("expected the DPU lookup's database error: {source:?}");
    };
    assert!(
        database_error
            .to_string()
            .contains("query=SELECT id FROM machines WHERE id = $1."),
        "{database_error}"
    );
    assert_eq!(
        database_error
            .source
            .as_database_error()
            .expect("retain the Postgres error")
            .code()
            .as_deref(),
        Some("42703")
    );
}

#[crate::sqlx_test]
async fn test_dpu_agent_control_preserves_host_state_decode_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;

    // DPU requests decode the attached host's `controller_state`.
    sqlx::query("UPDATE machines SET controller_state = '{}'::jsonb WHERE id = $1")
        .bind(mh.id)
        .execute(&env.pool)
        .await
        .unwrap();
    let error = env
        .api
        .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(mh.dpu_ids[0].into()),
            nic_firmware_site_cache_supported: false,
        }))
        .await
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(error.message(), "internal database error");
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-500"
    );
    assert_eq!(
        error.metadata().get("nico-error-text").unwrap(),
        error.message()
    );
    let source = error
        .source()
        .unwrap()
        .downcast_ref::<CarbideErrorWithLocation>()
        .expect("status should retain the API error and conversion location");
    assert!(source.location.contains("handlers/machine_scout.rs:"));
    let CarbideError::DBError(annotated) = &source.error else {
        panic!("expected host state query error, got {:?}", source.error);
    };
    assert!(
        annotated
            .to_string()
            .contains("SELECT m.id, m.controller_state")
    );
    assert!(matches!(
        &annotated.source,
        sqlx::Error::ColumnDecode { index, .. } if index == "1"
    ));
}

#[crate::sqlx_test]
async fn test_host_agent_control_preserves_scout_contact_write_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    // Clear the fixture's contact time so the row satisfies the new constraint.
    sqlx::query("UPDATE machines SET last_scout_contact_time = NULL WHERE id = $1")
        .bind(mh.id)
        .execute(&env.pool)
        .await
        .unwrap();

    // The constraint rejects only this host's scout contact write.
    // DDL substitutes a typed machine ID, not caller-supplied SQL.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE machines ADD CONSTRAINT test_reject_scout_contact \
         CHECK (id <> '{}' OR last_scout_contact_time IS NULL)",
        mh.id,
    )))
    .execute(&env.pool)
    .await
    .unwrap();
    let error = env
        .api
        .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(mh.id.into()),
            nic_firmware_site_cache_supported: false,
        }))
        .await
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(error.message(), "internal database error");
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-500"
    );
    assert_eq!(
        error.metadata().get("nico-error-text").unwrap(),
        error.message()
    );
    let source = error
        .source()
        .unwrap()
        .downcast_ref::<CarbideErrorWithLocation>()
        .expect("status should retain the API error and conversion location");
    assert!(source.location.contains("handlers/machine_scout.rs:"));
    let CarbideError::DBError(annotated) = &source.error else {
        panic!("expected scout contact query error, got {:?}", source.error);
    };
    assert!(
        annotated
            .to_string()
            .contains("UPDATE machines SET last_scout_contact_time=NOW()")
    );
    let database_error = annotated.source.as_database_error().unwrap();
    assert_eq!(database_error.code().as_deref(), Some("23514"));
    assert_eq!(
        database_error.constraint(),
        Some("test_reject_scout_contact")
    );
}

#[crate::sqlx_test]
async fn test_dpu_agent_control_uses_host_state_and_requires_attachment(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let dpu_id = mh.dpu_ids[0];
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let dpu = mh.dpu().db_machine(&mut txn).await;

    db::machine::advance(
        &host,
        &mut txn,
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall),
        None,
    )
    .await
    .unwrap();
    db::machine::advance(&dpu, &mut txn, &ManagedHostState::Ready, None)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // The DPU's own `Ready` state maps to `Noop`; its host's state maps to `Discovery`.
    let response = env
        .api
        .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(dpu_id.into()),
            nic_firmware_site_cache_supported: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(response.action, Some(Action::Discovery(_))));

    let mut txn = env.pool.begin().await.unwrap();
    sqlx::query("UPDATE machines SET controller_state = '{}'::jsonb WHERE id = $1")
        .bind(dpu_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    db::machine::find_one(txn.as_mut(), &dpu_id, Default::default())
        .await
        .expect_err("a full DPU snapshot must reject malformed controller state");
    txn.commit().await.unwrap();

    // Agent control needs the host's state, even if the DPU's own state cannot decode.
    let response = env
        .api
        .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(dpu_id.into()),
            nic_firmware_site_cache_supported: false,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(response.action, Some(Action::Discovery(_))));

    let mut txn = env.pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE machine_interfaces SET attached_dpu_machine_id = NULL
         WHERE attached_dpu_machine_id = $1 AND machine_id != $1",
    )
    .bind(dpu_id)
    .execute(txn.as_mut())
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let error = env
        .api
        .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(dpu_id.into()),
            nic_firmware_site_cache_supported: false,
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound);
    assert_eq!(error.message(), format!("machine not found: {dpu_id}"));
}

#[crate::sqlx_test]
async fn test_agent_control_updates_scout_contact_only_for_host_requests(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let dpu_id = mh.dpu_ids[0];
    let stale_contact_time = "2020-01-01T00:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::update_state(&mut txn, &mh.id.into(), &ManagedHostState::Ready)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    for (scenario, machine_id, host_contact_updates) in [
        ("host request", MachineId::from(mh.id), true),
        ("DPU request", MachineId::from(dpu_id), false),
    ] {
        sqlx::query("UPDATE machines SET last_scout_contact_time = $1 WHERE id IN ($2, $3)")
            .bind(stale_contact_time)
            .bind(mh.id)
            .bind(dpu_id)
            .execute(&env.pool)
            .await
            .unwrap();

        env.api
            .forge_agent_control(tonic::Request::new(rpc::forge::ForgeAgentControlRequest {
                machine_id: Some(machine_id),
                nic_firmware_site_cache_supported: false,
            }))
            .await
            .unwrap();

        let mut txn = env.pool.begin().await.unwrap();
        let host_contact_time = mh
            .host()
            .db_machine(&mut txn)
            .await
            .status
            .last_scout_contact_time;
        let dpu_contact_time = mh
            .dpu()
            .db_machine(&mut txn)
            .await
            .status
            .last_scout_contact_time;
        txn.commit().await.unwrap();

        if host_contact_updates {
            assert!(
                host_contact_time.expect("host request records scout contact") > stale_contact_time,
                "{scenario}"
            );
        } else {
            assert_eq!(host_contact_time, Some(stale_contact_time), "{scenario}");
        }
        assert_eq!(dpu_contact_time, Some(stale_contact_time), "{scenario}");
    }
}

#[crate::sqlx_test]
async fn test_dpu_for_reprovisioning_with_no_firmware_upgrade(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(dpu.reprovision_requested.is_none(),);

    let dpu_interface = mh.dpu().first_interface(&mut txn).await;
    let dpu_arch = rpc::forge::MachineArchitecture::Arm;

    mh.mark_machine_for_updates().await;

    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, false).await;

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(dpu.reprovision_requested.unwrap().initiator, "AdminCli");

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB
        }),
    );

    env.run_machine_state_controller_iteration().await;

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert_ne!(pxe.pxe_script, "exit".to_string());

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Discovery(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Discovery as i32
    );
    mh.dpu().discovery_completed().await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::PoweringOffHost)
    );

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Noop as i32
    );

    for state in [
        ReprovisionState::PowerDown,
        ReprovisionState::VerifyFirmareVersions,
        ReprovisionState::WaitingForNetworkConfig,
    ] {
        let dpu = mh.dpu().next_iteration_machine(&env).await;
        assert_eq!(dpu.current_state(), &mh.new_dpu_reprovision_state(state));
    }

    let _response = mh.dpu().forge_agent_control().await;
    mh.network_configured(&env).await;

    // Repair host boot setup before the final BMC and host reboot sequence.
    let dpu_machine = mh.dpu();
    let dpu = assert_dpu_reprovision_host_boot_repair(
        &env,
        &dpu_machine,
        reprovision_host_boot_repair_states(&mh, ReprovisionHostBootRepairShape::SingleDpu),
    )
    .await;
    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::Discovered { .. },
        }
    ));

    let _response = mh.host().forge_agent_control().await;
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert!(matches!(dpu.current_state(), &ManagedHostState::Ready));
}

#[crate::sqlx_test]
async fn test_instance_reprov_with_firmware_upgrade(pool: sqlx::PgPool) {
    Box::pin(test_instance_reprov_with_firmware_upgrade_impl(pool)).await;
}

async fn test_instance_reprov_with_firmware_upgrade_impl(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let (tinstance, rpc_instance) = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build_and_return()
        .await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_interface = mh.dpu().first_interface(&mut txn).await;
    txn.commit().await.unwrap();

    instance_reprov_start(&env, &mh, &tinstance, &rpc_instance).await;
    instance_reprov_install_dpu_os(&env, &mh, &tinstance, &dpu_interface).await;
    instance_reprov_complete(&env, &mh, &tinstance).await;
}

async fn instance_reprov_start(
    env: &TestEnv,
    mh: &TestManagedHost,
    tinstance: &TestInstance<'_, '_>,
    rpc_instance: &RpcInstance,
) {
    mh.mark_machine_for_updates().await;
    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;
    env.api
        .invoke_instance_power(tonic::Request::new(::rpc::forge::InstancePowerRequest {
            instance_id: tinstance.id.into(),
            apply_updates_on_reboot: true,
            boot_with_custom_ipxe: false,
            operation: 0,
        }))
        .await
        .unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(dpu.reprovision_requested.unwrap().initiator, "AdminCli");
    txn.commit().await.unwrap();

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    mh.network_configured(env).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    mh.network_configured(env).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage { .. }
        }
    ));
    txn.commit().await.unwrap();

    assert_reprov_tenant_state(env, mh, tinstance, TenantState::Updating).await;

    _ = forge_agent_control(env, rpc_instance.machine_id()).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}

async fn instance_reprov_install_dpu_os(
    env: &TestEnv,
    mh: &TestManagedHost,
    tinstance: &TestInstance<'_, '_>,
    dpu_interface: &TestMachineInterface,
) {
    let dpu_arch = MachineArchitecture::Arm;

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB,
        }),
    );
    txn.commit().await.unwrap();

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let dpu = mh.dpu().next_iteration_machine(env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    assert_reprov_tenant_state(env, mh, tinstance, TenantState::Updating).await;

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert_ne!(pxe.pxe_script, "exit".to_string());

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Discovery(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Discovery as i32
    );
    mh.dpu().discovery_completed().await;

    let mut txn = env.pool.begin().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::PoweringOffHost),
    );
    txn.commit().await.unwrap();

    assert_reprov_tenant_state(env, mh, tinstance, TenantState::Updating).await;

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Assigned/Reprovision/PoweringOffHost")
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    let mut txn = env.pool.begin().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::PowerDown)
    );
    txn.commit().await.unwrap();

    assert_reprov_tenant_state(env, mh, tinstance, TenantState::Updating).await;

    let mut txn = env.pool.begin().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::VerifyFirmareVersions)
    );
    txn.commit().await.unwrap();

    assert_reprov_tenant_state(env, mh, tinstance, TenantState::Updating).await;

    let mut txn = env.pool.begin().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::WaitingForNetworkConfig)
    );
    txn.commit().await.unwrap();

    assert_reprov_tenant_state(env, mh, tinstance, TenantState::Updating).await;

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(pxe.pxe_script.contains("exit"));
}

async fn instance_reprov_complete(
    env: &TestEnv,
    mh: &TestManagedHost,
    tinstance: &TestInstance<'_, '_>,
) {
    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Noop as i32
    );
    mh.network_configured(env).await;

    // Repair host boot setup before returning the assigned host to service.
    let dpu_machine = mh.dpu();
    let dpu = assert_dpu_reprovision_host_boot_repair(
        env,
        &dpu_machine,
        reprovision_host_boot_repair_states(mh, ReprovisionHostBootRepairShape::AssignedSingleDpu),
    )
    .await;
    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));

    assert_reprov_tenant_state(env, mh, tinstance, TenantState::Configuring).await;
}

async fn assert_reprov_tenant_state(
    env: &TestEnv,
    mh: &TestManagedHost,
    tinstance: &TestInstance<'_, '_>,
    expected: TenantState,
) {
    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let db_instance = tinstance.db_instance(&mut txn).await;
    txn.commit().await.unwrap();

    let device_id_maps = host.get_dpu_device_and_id_mappings().unwrap();
    assert_eq!(
        instance_snapshot_derive_status(
            &db_instance,
            &[],
            device_id_maps.1,
            host.primary_attached_dpu_machine_id(),
            host.state.clone().value,
            None,
            None,
            None,
            None,
            &host.health_reports,
        )
        .unwrap()
        .tenant
        .unwrap()
        .state,
        expected
    );
}

#[crate::sqlx_test]
async fn test_instance_reprov_without_firmware_upgrade(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_interface = mh.dpu().first_interface(&mut txn).await;
    let host_interface = mh.host().first_interface(&mut txn).await;
    let dpu_arch = MachineArchitecture::Arm;
    let host_arch = MachineArchitecture::X86;

    mh.mark_machine_for_updates().await;

    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, false).await;
    env.api
        .invoke_instance_power(tonic::Request::new(::rpc::forge::InstancePowerRequest {
            instance_id: tinstance.id.into(),
            apply_updates_on_reboot: true,
            boot_with_custom_ipxe: false,
            operation: 0,
        }))
        .await
        .unwrap();

    let current_instance = tinstance.rpc_instance().await;
    assert!(current_instance.status().inner().update.is_some());

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        &dpu.reprovision_requested.as_ref().unwrap().initiator,
        "AdminCli"
    );

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    mh.network_configured(&env).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    mh.network_configured(&env).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage { .. }
        }
    ));

    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert!(pxe.pxe_script.contains("scout.efi"));

    _ = forge_agent_control(&env, current_instance.inner().machine_id.unwrap()).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    // Since DPU reprovisioning is started, we can't allow user to reboot host in between. It
    // should be prevented from cloud itself.
    assert!(
        env.api
            .invoke_instance_power(tonic::Request::new(::rpc::forge::InstancePowerRequest {
                instance_id: tinstance.id.into(),
                apply_updates_on_reboot: true,
                boot_with_custom_ipxe: false,
                operation: 0,
            }))
            .await
            .is_err()
    );
    txn.commit().await.unwrap();

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB,
        })
    );

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Discovery(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Discovery as i32
    );
    mh.dpu().discovery_completed().await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::PoweringOffHost)
    );

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Assigned/Reprovision/PoweringOffHost")
    );

    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    for state in [
        ReprovisionState::PowerDown,
        ReprovisionState::VerifyFirmareVersions,
        ReprovisionState::WaitingForNetworkConfig,
    ] {
        let dpu = mh.dpu().next_iteration_machine(&env).await;
        assert_eq!(
            dpu.current_state(),
            &mh.new_dpu_assigned_reprovision_state(state)
        );
    }

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Assigned/Reprovision/WaitingForNetworkConfig")
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Noop as i32
    );
    mh.network_configured(&env).await;

    // Repair host boot setup before returning the assigned host to service.
    let dpu_machine = mh.dpu();
    let dpu = assert_dpu_reprovision_host_boot_repair(
        &env,
        &dpu_machine,
        reprovision_host_boot_repair_states(&mh, ReprovisionHostBootRepairShape::AssignedSingleDpu),
    )
    .await;
    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));

    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert!(pxe.pxe_script.contains("exit"));
}

#[crate::sqlx_test]
async fn test_dpu_for_set_but_clear_failed(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert!(dpu.reprovision_requested.is_none(),);

    mh.mark_machine_for_updates().await;
    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(dpu.reprovision_requested.unwrap().initiator, "AdminCli");

    let res = env
        .api
        .list_dpu_waiting_for_reprovisioning(tonic::Request::new(
            ::rpc::forge::DpuReprovisioningListRequest {},
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(res.dpus.len(), 1);
    assert_eq!(res.dpus[0].id, Some(mh.dpu().id));

    db::machine::update_dpu_reprovision_start_time(&mh.dpu().id, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert!(
        env.api
            .trigger_dpu_reprovisioning(tonic::Request::new(
                ::rpc::forge::DpuReprovisioningRequest {
                    dpu_id: None,
                    machine_id: Some(mh.dpu().id.into()),
                    mode: rpc::forge::dpu_reprovisioning_request::Mode::Clear as i32,
                    initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                    update_firmware: true
                },
            ))
            .await
            .is_err()
    );

    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert!(dpu.reprovision_requested.is_some(),);
}

#[crate::sqlx_test]
async fn test_reboot_retry(pool: sqlx::PgPool) {
    // The reconcile-heavy body builds a future too large for the test
    // thread's stack in debug builds; box it like its siblings above.
    Box::pin(test_reboot_retry_impl(pool)).await;
}

async fn test_reboot_retry_impl(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(dpu.reprovision_requested.is_none(),);

    mh.mark_machine_for_updates().await;
    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(dpu.reprovision_requested.unwrap().initiator, "AdminCli");
    let last_reboot_requested_time = dpu.status.last_reboot_requested.as_ref();
    for _ in 0..3 {
        env.run_machine_state_controller_iteration().await;
    }

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_ne!(
        dpu.status.last_reboot_requested.unwrap().time,
        last_reboot_requested_time.unwrap().time
    );

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );
    txn.commit().await.unwrap();

    // no reboots should be forced during firmware update
    for _ in 1..5 {
        update_time_params(&env.pool, &dpu, 1, None).await;
        let dpu = mh.dpu().next_iteration_machine(&env).await;
        assert_eq!(
            dpu.current_state(),
            &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
        );
    }

    reboot_completed(&env, dpu.id).await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    update_time_params(&env.pool, &dpu, 1, None).await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    update_time_params(&env.pool, &dpu, 1, None).await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    // Retry 1
    update_time_params(&env.pool, &dpu, 1, None).await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert!(matches!(
        dpu.status.last_reboot_requested.as_ref().unwrap().mode,
        MachineLastRebootRequestedMode::Reboot
    ));

    // Retry 2
    update_time_params(&env.pool, &dpu, 2, None).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu_ = mh.dpu().db_machine(&mut txn).await;

    assert!(matches!(
        dpu_.status.last_reboot_requested.as_ref().unwrap().mode,
        MachineLastRebootRequestedMode::Reboot
    ));
    txn.commit().await.unwrap();

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_ne!(
        dpu_.status.last_reboot_requested.as_ref().unwrap().time,
        dpu.status.last_reboot_requested.as_ref().unwrap().time
    );
    assert!(matches!(
        dpu.status.last_reboot_requested.as_ref().unwrap().mode,
        MachineLastRebootRequestedMode::Reboot
    ));

    // Retry 3
    update_time_params(&env.pool, &dpu, 3, None).await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert!(matches!(
        dpu.status.last_reboot_requested.as_ref().unwrap().mode,
        MachineLastRebootRequestedMode::Reboot
    ));

    // Retry 4
    update_time_params(&env.pool, &dpu, 4, None).await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert!(matches!(
        dpu.status.last_reboot_requested.as_ref().unwrap().mode,
        MachineLastRebootRequestedMode::PowerOff
    ));

    // Retry 5
    update_time_params(&env.pool, &dpu, 5, None).await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert!(matches!(
        dpu.status.last_reboot_requested.as_ref().unwrap().mode,
        MachineLastRebootRequestedMode::PowerOn
    ));

    // Retry 6
    update_time_params(&env.pool, &dpu, 5, None).await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert!(matches!(
        dpu.status.last_reboot_requested.as_ref().unwrap().mode,
        MachineLastRebootRequestedMode::Reboot
    ));
}

#[crate::sqlx_test]
async fn test_reboot_no_retry_during_firmware_update(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(dpu.reprovision_requested.is_none(),);

    mh.mark_machine_for_updates().await;

    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        &dpu.reprovision_requested.as_ref().unwrap().initiator,
        "AdminCli"
    );

    let last_reboot_requested_time = dpu.status.last_reboot_requested.as_ref();

    let handler = MachineStateHandlerBuilder::builder()
        .hardware_models(env.config.get_firmware_config())
        .dpu_up_threshold(chrono::Duration::minutes(5))
        .dpu_nic_firmware_initial_update_enabled(true)
        .dpu_nic_firmware_reprovision_update_enabled(true)
        .reachability_params(env.reachability_params)
        .dpu_enable_secure_boot(true)
        .attestation_enabled(env.attestation_enabled)
        .power_options_config(env.config.power_manager_options.clone().into())
        .build();
    env.override_machine_state_controller_handler(handler).await;

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_ne!(
        dpu.status.last_reboot_requested.as_ref().unwrap().time,
        last_reboot_requested_time.unwrap().time
    );

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB
        })
    );

    env.run_machine_state_controller_iteration().await;

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    txn.commit().await.unwrap();

    reboot_completed(&env, dpu.id).await;

    env.run_machine_state_controller_iteration().await;

    let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let dpu = mh.dpu().db_machine(&mut txn).await;
    let last_reboot_requested = host.status.last_reboot_requested.as_ref().unwrap();

    tracing::info!(?last_reboot_requested, "power request",);
    assert!(matches!(
        host.status.last_reboot_requested.as_ref().unwrap().mode,
        MachineLastRebootRequestedMode::Reboot
    ));

    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    txn.rollback().await.unwrap();
}

#[crate::sqlx_test]
async fn test_clear_with_function_call(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    txn.commit().await.unwrap();
    assert!(dpu.reprovision_requested.is_none(),);

    mh.mark_machine_for_updates().await;

    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;

    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        db::machine::clear_dpu_reprovisioning_request(&mut txn, &dpu.id, true)
            .await
            .is_ok()
    );
    txn.rollback().await.unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::update_dpu_reprovision_start_time(&dpu.id, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        db::machine::clear_dpu_reprovisioning_request(&mut txn, &dpu.id, true)
            .await
            .is_err()
    );
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn test_clear_maintenance_when_reprov_is_set(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    txn.commit().await.unwrap();
    assert!(dpu.reprovision_requested.is_none(),);

    mh.mark_machine_for_updates().await;

    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;

    assert!(
        env.api
            .set_maintenance(tonic::Request::new(::rpc::forge::MaintenanceRequest {
                host_id: Some(mh.id.into()),
                operation: 1,
                reference: Some("no reference".to_string()),
            }))
            .await
            .is_err()
    );
}

#[crate::sqlx_test]
async fn test_dpu_reset(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();

    let mh = create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;

    let agent_control_response = mh.dpu().forge_agent_control().await;
    assert!(matches!(
        agent_control_response.action,
        Some(Action::Noop(_))
    ));
    assert_eq!(
        agent_control_response.legacy_action,
        rpc::forge_agent_control_response::LegacyAction::Noop as i32
    );

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        4,
        ManagedHostState::DPUInit {
            dpu_states: model::machine::DpuInitStates {
                states: HashMap::from([(mh.dpu_ids[0], DpuInitState::WaitingForNetworkConfig)]),
            },
        },
    )
    .await;
}

#[crate::sqlx_test]
async fn test_restart_dpu_reprov(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    txn.commit().await.unwrap();
    assert!(dpu.reprovision_requested.is_none(),);

    mh.mark_machine_for_updates().await;

    assert!(
        env.api
            .trigger_dpu_reprovisioning(tonic::Request::new(
                ::rpc::forge::DpuReprovisioningRequest {
                    dpu_id: None,
                    machine_id: Some(mh.id.into()),
                    mode: Mode::Restart as i32,
                    initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                    update_firmware: false,
                },
            ))
            .await
            .is_err()
    );

    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;

    for _ in 0..3 {
        env.run_machine_state_controller_iteration().await;
    }

    let mut txn = env.pool.begin().await.unwrap();

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    let restart_time = dpu
        .reprovision_requested
        .as_ref()
        .unwrap()
        .restart_reprovision_requested_at;
    txn.commit().await.unwrap();

    mh.host()
        .trigger_dpu_reprovisioning(Mode::Restart, true)
        .await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_ne!(
        restart_time,
        dpu.reprovision_requested
            .as_ref()
            .unwrap()
            .restart_reprovision_requested_at
    );

    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB
        }),
    );

    // change the mode
    mh.host()
        .trigger_dpu_reprovisioning(Mode::Restart, false)
        .await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_ne!(
        restart_time,
        dpu.reprovision_requested
            .as_ref()
            .unwrap()
            .restart_reprovision_requested_at
    );

    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB
        }),
    );
}

#[crate::sqlx_test]
async fn test_restart_dpu_reprov_unassigned_host_boot_failure(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    let dpu_machine = mh.dpu();
    mh.mark_machine_for_updates().await;

    let failed_at = Utc::now();
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::trigger_dpu_reprovisioning_request(&dpu_machine.id, &mut txn, "AdminCli", true)
        .await
        .unwrap();
    db::machine::update_dpu_reprovision_explicit_start_time(&dpu_machine.id, failed_at, &mut txn)
        .await
        .unwrap();
    db::machine::update_state(
        &mut txn,
        &mh.id,
        &ManagedHostState::Failed {
            machine_id: mh.id.into(),
            retry_count: 0,
            details: FailureDetails {
                cause: FailureCause::BiosSetupFailed {
                    err: "host boot repair exhausted retries".to_string(),
                },
                failed_at,
                source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
            },
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Restart detection is intentionally gated on a request newer than the failed state.
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    mh.host()
        .trigger_dpu_reprovisioning(Mode::Restart, true)
        .await;

    // The repair failure preserves the DPU request so operators can restart from top-level Failed.
    let redfish_timepoint = env.redfish_sim.timepoint();
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB
        }),
    );
    assert!(
        dpu.reprovision_requested
            .as_ref()
            .is_some_and(|request| request.started_at.is_some())
    );
    assert_eq!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts(),
        vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)]
    );
}

#[crate::sqlx_test]
async fn test_dpu_for_reprovisioning_with_firmware_upgrade_multidpu_onedpu_reprov(
    pool: sqlx::PgPool,
) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpus = mh.dpu_db_machines(&mut txn).await;
    assert!(dpus[0].reprovision_requested.is_none(),);

    let dpu0_interface = mh.dpu_n(0).first_interface(&mut txn).await;
    let dpu_arch = rpc::forge::MachineArchitecture::Arm;

    mh.mark_machine_for_updates().await;

    mh.dpu_n(0)
        .trigger_dpu_reprovisioning(Mode::Set, true)
        .await;

    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_eq!(
        &dpu.reprovision_requested.as_ref().unwrap().initiator,
        "AdminCli"
    );

    let last_reboot_requested_time = dpu.status.last_reboot_requested.as_ref();

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpus_reprovision_state(&[
            &ReprovisionState::InstallDpuOs {
                substate: InstallDpuOsState::InstallingBFB
            },
            &ReprovisionState::NotUnderReprovision,
        ])
    );

    env.run_machine_state_controller_iteration().await;

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_ne!(
        dpu.status.last_reboot_requested.as_ref().unwrap().time,
        last_reboot_requested_time.unwrap().time
    );
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpus_reprovision_state(&[
            &ReprovisionState::WaitingForNetworkInstall,
            &ReprovisionState::NotUnderReprovision,
        ])
    );

    let pxe = dpu0_interface.get_pxe_instructions(dpu_arch).await;
    assert_ne!(pxe.pxe_script, "exit".to_string());

    let _response = mh.dpu_n(0).forge_agent_control().await;
    mh.dpu_n(0).discovery_completed().await;

    for state in [
        ReprovisionState::PoweringOffHost,
        ReprovisionState::PowerDown,
    ] {
        let dpu = mh.dpu_n(0).next_iteration_machine(&env).await;
        assert_eq!(
            dpu.current_state(),
            &mh.new_dpus_reprovision_state(&[&state, &state])
        );
    }

    for state in [
        ReprovisionState::VerifyFirmareVersions,
        ReprovisionState::WaitingForNetworkConfig,
    ] {
        let dpu = mh.dpu_n(0).next_iteration_machine(&env).await;
        assert_eq!(
            dpu.current_state(),
            &mh.new_dpus_reprovision_state(&[&state, &ReprovisionState::NotUnderReprovision])
        );
    }

    let pxe = dpu0_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Reprovisioning/WaitingForNetworkConfig")
    );

    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    let dpu = mh.dpu_n(0).next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpus_reprovision_state(&[
            &ReprovisionState::WaitingForNetworkConfig,
            &ReprovisionState::NotUnderReprovision
        ])
    );

    let pxe = dpu0_interface.get_pxe_instructions(dpu_arch).await;
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    let response = mh.dpu_n(0).forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Noop as i32
    );
    mh.network_configured(&env).await;

    // Host boot repair is host-scoped, but reprovision ownership stays limited to the requested DPU.
    let dpu_machine = mh.dpu_n(0);
    let dpu = assert_dpu_reprovision_host_boot_repair(
        &env,
        &dpu_machine,
        reprovision_host_boot_repair_states(&mh, ReprovisionHostBootRepairShape::FirstDpuOnly),
    )
    .await;
    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::Discovered { .. },
        }
    ));

    let _response = mh.host().forge_agent_control().await;

    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert!(matches!(dpu.current_state(), &ManagedHostState::Ready));
}

#[crate::sqlx_test]
async fn test_dpu_for_reprovisioning_with_firmware_upgrade_multidpu_bothdpu(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = env.pool.begin().await.unwrap();
    let dpus = mh.dpu_db_machines(&mut txn).await;
    assert!(dpus[0].reprovision_requested.is_none());

    let dpu0_interface = mh.dpu_n(0).first_interface(&mut txn).await;
    let dpu_arch = rpc::forge::MachineArchitecture::Arm;

    mh.mark_machine_for_updates().await;

    mh.dpu_n(0)
        .trigger_dpu_reprovisioning(Mode::Set, true)
        .await;
    mh.dpu_n(1)
        .trigger_dpu_reprovisioning(Mode::Set, true)
        .await;

    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_eq!(
        &dpu.reprovision_requested.as_ref().unwrap().initiator,
        "AdminCli"
    );

    let last_reboot_requested_time = dpu.status.last_reboot_requested.as_ref();

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_ne!(
        dpu.status.last_reboot_requested.as_ref().unwrap().time,
        last_reboot_requested_time.unwrap().time
    );
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpus_reprovision_state(&[
            &ReprovisionState::InstallDpuOs {
                substate: InstallDpuOsState::InstallingBFB
            },
            &ReprovisionState::InstallDpuOs {
                substate: InstallDpuOsState::InstallingBFB
            }
        ]),
    );

    for _ in 0..5 {
        env.run_machine_state_controller_iteration().await;
    }

    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_ne!(
        dpu.status.last_reboot_requested.as_ref().unwrap().time,
        last_reboot_requested_time.unwrap().time
    );
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpus_reprovision_state(&[
            &ReprovisionState::WaitingForNetworkInstall,
            &ReprovisionState::WaitingForNetworkInstall
        ])
    );

    let pxe = dpu0_interface.get_pxe_instructions(dpu_arch).await;
    assert_ne!(pxe.pxe_script, "exit".to_string());

    mh.dpu_n(0).forge_agent_control().await;
    mh.dpu_n(1).forge_agent_control().await;
    mh.dpu_n(0).discovery_completed().await;
    mh.dpu_n(1).discovery_completed().await;

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpus_reprovision_state(&[
            &ReprovisionState::PoweringOffHost,
            &ReprovisionState::PoweringOffHost
        ])
    );

    let pxe = dpu0_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Reprovisioning/PoweringOffHost")
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpus_reprovision_state(&[
            &ReprovisionState::PowerDown,
            &ReprovisionState::PowerDown
        ])
    );

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let dpu = mh.dpu_n(0).db_machine(&mut txn).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpus_reprovision_state(&[
            &ReprovisionState::WaitingForNetworkConfig,
            &ReprovisionState::WaitingForNetworkConfig
        ])
    );

    let pxe = dpu0_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Reprovisioning/WaitingForNetworkConfig")
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    let response = mh.dpu_n(0).forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Noop as i32
    );
    mh.network_configured(&env).await;

    // Repair host boot setup across all reprovisioned DPUs.
    let dpu_machine = mh.dpu_n(0);
    let dpu = assert_dpu_reprovision_host_boot_repair(
        &env,
        &dpu_machine,
        reprovision_host_boot_repair_states(&mh, ReprovisionHostBootRepairShape::AllDpus),
    )
    .await;
    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::Discovered { .. },
        }
    ));

    mh.host().forge_agent_control().await;
    let dpu = dpu_machine.next_iteration_machine(&env).await;
    assert!(matches!(dpu.current_state(), &ManagedHostState::Ready));
}

#[crate::sqlx_test]
async fn test_instance_reprov_restart_failed(pool: sqlx::PgPool) {
    Box::pin(test_instance_reprov_restart_failed_impl(pool)).await;
}

async fn test_instance_reprov_restart_failed_impl(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_interface = mh.dpu().first_interface(&mut txn).await;
    let host_interface = mh.host().first_interface(&mut txn).await;
    let dpu_arch = MachineArchitecture::Arm;
    let host_arch = MachineArchitecture::X86;

    mh.mark_machine_for_updates().await;

    mh.dpu().trigger_dpu_reprovisioning(Mode::Set, false).await;
    env.api
        .invoke_instance_power(tonic::Request::new(::rpc::forge::InstancePowerRequest {
            instance_id: tinstance.id.into(),
            apply_updates_on_reboot: true,
            boot_with_custom_ipxe: false,
            operation: 0,
        }))
        .await
        .unwrap();

    let current_instance = tinstance.rpc_instance().await;
    assert!(current_instance.status().inner().update.is_some());

    let dpu = mh.dpu().db_machine(&mut txn).await;
    assert_eq!(
        &dpu.reprovision_requested.as_ref().unwrap().initiator,
        "AdminCli"
    );

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    mh.network_configured(&env).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    mh.network_configured(&env).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let dpu = mh.dpu().db_machine(&mut txn).await;

    tracing::info!(
        machine_id = %dpu.id,
        dpu_state = %dpu.current_state(),
        "current DPU state",
    );

    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage { .. }
        }
    ));

    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert!(pxe.pxe_script.contains("scout.efi"));

    forge_agent_control(&env, current_instance.inner().machine_id.unwrap()).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Since DPU reprovisioning is started, we can't allow user to reboot host in between. It
    // should be prevented from cloud itself.
    assert!(
        env.api
            .invoke_instance_power(tonic::Request::new(::rpc::forge::InstancePowerRequest {
                instance_id: tinstance.id.into(),
                apply_updates_on_reboot: true,
                boot_with_custom_ipxe: false,
                operation: 0,
            }))
            .await
            .is_err()
    );
    txn.commit().await.unwrap();

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB,
        })
    );

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Discovery(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Discovery as i32
    );
    mh.dpu().discovery_completed().await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::PoweringOffHost)
    );

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Assigned/Reprovision/PoweringOffHost")
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::PowerDown)
    );

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;
    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::WaitingForNetworkConfig)
    );

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Assigned/Reprovision/WaitingForNetworkConfig")
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Noop as i32
    );

    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;
    let failed_at = Utc::now();
    let deserialized = FailureDetails {
        cause: model::machine::FailureCause::NVMECleanFailed {
            err: "error1".to_string(),
        },
        source: model::machine::FailureSource::Scout,
        failed_at,
    };
    db::machine::update_failure_details(&dpu, &mut txn, deserialized)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Failed {
                details: FailureDetails {
                    cause: model::machine::FailureCause::NVMECleanFailed {
                        err: "error1".to_string()
                    },
                    source: model::machine::FailureSource::Scout,
                    failed_at
                },
                machine_id: dpu.id.into()
            }
        }
    );

    assert!(
        env.api
            .trigger_dpu_reprovisioning(tonic::Request::new(
                ::rpc::forge::DpuReprovisioningRequest {
                    dpu_id: None,
                    machine_id: Some(mh.id.into()),
                    mode: Mode::Restart as i32,
                    initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                    update_firmware: false,
                },
            ))
            .await
            .is_ok()
    );

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB,
        },)
    );

    env.run_machine_state_controller_iteration().await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::WaitingForNetworkInstall)
    );

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(pxe.pxe_script.contains("internal/aarch64/carbide.efi"));

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Discovery(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Discovery as i32
    );
    mh.dpu().discovery_completed().await;

    let dpu = mh.dpu().next_iteration_machine(&env).await;
    assert_eq!(
        dpu.current_state(),
        &mh.new_dpu_assigned_reprovision_state(ReprovisionState::PoweringOffHost)
    );

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Assigned/Reprovision/PoweringOffHost")
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    for state in [
        ReprovisionState::PowerDown,
        ReprovisionState::VerifyFirmareVersions,
        ReprovisionState::WaitingForNetworkConfig,
    ] {
        let dpu = mh.dpu().next_iteration_machine(&env).await;
        assert_eq!(
            dpu.current_state(),
            &mh.new_dpu_assigned_reprovision_state(state)
        );
    }

    let pxe = dpu_interface.get_pxe_instructions(dpu_arch).await;
    assert!(
        pxe.pxe_script
            .contains("Current state: Assigned/Reprovision/WaitingForNetworkConfig")
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    let response = mh.dpu().forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(
        response.legacy_action,
        rpc::forge::forge_agent_control_response::LegacyAction::Noop as i32
    );
    mh.network_configured(&env).await;

    // Repair host boot setup before returning the assigned host to service.
    let dpu_machine = mh.dpu();
    let dpu = assert_dpu_reprovision_host_boot_repair(
        &env,
        &dpu_machine,
        reprovision_host_boot_repair_states(&mh, ReprovisionHostBootRepairShape::AssignedSingleDpu),
    )
    .await;
    assert!(matches!(
        dpu.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));

    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert!(pxe.pxe_script.contains("exit"));
}

#[crate::sqlx_test]
async fn test_dpu_for_reprovisioning_cannot_restart_if_not_started(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = common::api_fixtures::create_managed_host(&env).await;
    mh.mark_machine_for_updates().await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu = mh.dpu().db_machine(&mut txn).await;

    assert!(dpu.reprovision_requested.is_none(),);

    match env
        .api
        .trigger_dpu_reprovisioning(tonic::Request::new(
            ::rpc::forge::DpuReprovisioningRequest {
                dpu_id: None,
                machine_id: Some(mh.id.into()),
                mode: rpc::forge::dpu_reprovisioning_request::Mode::Restart as i32,
                initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                update_firmware: true,
            },
        ))
        .await
    {
        Ok(_) => panic!("Request to restart provisioning should have failed"),
        Err(e) => {
            assert!(matches!(e.code(), tonic::Code::InvalidArgument));
        }
    }
}

/// A public Clear queued ahead of Restart must leave the request absent.
/// Restart must reject the current NULL after waiting, rather than report a
/// successful JSON merge from its earlier non-NULL snapshot.
#[crate::sqlx_test]
async fn restart_rejects_request_cleared_during_its_dpu_wait(pool: sqlx::PgPool) {
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use tokio::time::{Duration as TimeoutDuration, timeout};

    use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};

    let mut config = common::api_fixtures::get_config();
    config.dpf.enabled = false;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let mh = create_managed_host(&env).await;
    assert_eq!(mh.dpu_ids.len(), 1);
    mh.mark_machine_for_updates().await;
    mh.host().trigger_dpu_reprovisioning(Mode::Set, true).await;
    let dpu_id = mh.dpu_ids[0];
    let mut before_txn = env.db_txn().await;
    let before = mh.dpu().db_machine(&mut before_txn).await;
    assert!(
        before
            .reprovision_requested
            .as_ref()
            .unwrap()
            .started_at
            .is_none()
    );
    before_txn.commit().await.unwrap();

    let mut gate = env.db_txn().await;
    db::machine::lock_by_id_for_update(gate.as_mut(), &dpu_id.into())
        .await
        .unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await
        .unwrap();
    let clear = maybe_done(env.api.trigger_dpu_reprovisioning(tonic::Request::new(
        rpc::forge::DpuReprovisioningRequest {
            machine_id: Some(mh.id.into()),
            mode: Mode::Clear as i32,
            initiator: rpc::forge::UpdateInitiator::AdminCli as i32,
            update_firmware: true,
            ..Default::default()
        },
    )));
    let restart = maybe_done(env.api.trigger_dpu_reprovisioning(tonic::Request::new(
        rpc::forge::DpuReprovisioningRequest {
            machine_id: Some(mh.id.into()),
            mode: Mode::Restart as i32,
            initiator: rpc::forge::UpdateInitiator::AdminCli as i32,
            update_firmware: false,
            ..Default::default()
        },
    )));
    tokio::pin!(clear, restart);
    let mut clear_pid = None;
    let mut restart_pid = None;
    let readiness = timeout(TimeoutDuration::from_secs(25), async {
        let pid = tokio::select! {
            () = &mut clear => return Err(format!("Clear did not reach the DPU gate: {:?}", clear.as_mut().output_mut())),
            pid = wait_for_blocked_query(&env.pool, gate_pid, "SET reprovisioning_requested=NULL") => pid,
        };
        clear_pid = Some(pid);
        restart_pid = Some(tokio::select! {
            () = &mut clear => return Err("Clear passed the DPU gate".to_string()),
            () = &mut restart => return Err(format!("Restart did not wait behind Clear: {:?}", restart.as_mut().output_mut())),
            pid = wait_for_blocked_query(
                &env.pool, pid, "SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE"
            ) => pid,
        });
        Ok::<(), String>(())
    })
    .await
    .unwrap_or_else(|error| Err(format!("Clear/Restart readiness timed out: {error}")));
    tracing::info!(
        ?gate_pid,
        ?clear_pid,
        ?restart_pid,
        ?readiness,
        "Observed public Clear before Restart"
    );
    let released = gate.commit().await;
    let finished = {
        let joined = async { tokio::join!(&mut clear, &mut restart) };
        tokio::pin!(joined);
        let finished = timeout(TimeoutDuration::from_secs(30), &mut joined)
            .await
            .is_ok();
        if !finished {
            for pid in clear_pid.into_iter().chain(restart_pid) {
                sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                    .bind(pid)
                    .fetch_one(&env.pool)
                    .await
                    .expect("cancel only the observed stalled public request");
            }
            timeout(TimeoutDuration::from_secs(30), &mut joined)
                .await
                .expect("cancelled public requests must join");
        }
        finished
    };
    let clear_result = clear.as_mut().take_output().expect("Clear joined");
    let restart_result = restart.as_mut().take_output().expect("Restart joined");
    let request_is_null: bool =
        sqlx::query_scalar("SELECT reprovisioning_requested IS NULL FROM machines WHERE id = $1")
            .bind(dpu_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    tracing::info!(
        ?clear_result,
        ?restart_result,
        request_is_null,
        "Joined Clear/Restart and reloaded the request"
    );
    released.expect("release the DPU gate");
    readiness.expect("observe both public request waits");
    assert!(finished, "public requests required timeout cancellation");
    clear_result.expect("Clear wins the DPU request write");
    let error =
        restart_result.expect_err("Restart must reject the request cleared during its wait");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        request_is_null,
        "Restart must preserve SQL NULL rather than recreate the request"
    );
}

/// A disable blocked on H must observe ingestion committed by that lock owner
/// before changing either the host or its attached DPUs.
#[crate::sqlx_test]
async fn dpf_disable_rejects_ingestion_committed_during_host_wait(pool: sqlx::PgPool) {
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use tokio::time::{Duration as TimeoutDuration, timeout};

    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    env.api
        .modify_dpf_state(tonic::Request::new(rpc::forge::ModifyDpfStateRequest {
            machine_id: Some(mh.id.into()),
            dpf_enabled: true,
        }))
        .await
        .expect("the fixture accepts enabling DPF before the ingestion marker");
    let mut before_txn = env.db_txn().await;
    let before = mh.snapshot(&mut before_txn).await;
    assert!(before.host_snapshot.config.dpf.enabled);
    assert!(!before.host_snapshot.config.dpf.used_for_ingestion);
    before_txn.commit().await.unwrap();

    let mut gate = env.db_txn().await;
    db::machine::lock_by_id_for_update(gate.as_mut(), &mh.id.into())
        .await
        .unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await
        .unwrap();
    let disable = maybe_done(env.api.modify_dpf_state(tonic::Request::new(
        rpc::forge::ModifyDpfStateRequest {
            machine_id: Some(mh.id.into()),
            dpf_enabled: false,
        },
    )));
    tokio::pin!(disable);
    let mut disable_pid = None;
    let readiness = timeout(TimeoutDuration::from_secs(25), async {
        disable_pid = Some(tokio::select! {
            () = &mut disable => return Err(format!("disable did not reach the host gate: {:?}", disable.as_mut().output_mut())),
            pid = wait_for_blocked_query(&env.pool, gate_pid, "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => pid,
        });
        Ok::<(), String>(())
    })
    .await
    .unwrap_or_else(|error| Err(format!("DPF disable readiness timed out: {error}")));
    let marker =
        db::machine::mark_machine_ingestion_done_with_dpf(gate.as_mut(), &mh.id.into()).await;
    let released = gate.commit().await;
    let finished = if timeout(TimeoutDuration::from_secs(30), &mut disable)
        .await
        .is_ok()
    {
        true
    } else {
        if let Some(pid) = disable_pid {
            sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                .bind(pid)
                .fetch_one(&env.pool)
                .await
                .expect("cancel only the observed stalled disable request");
        }
        timeout(TimeoutDuration::from_secs(30), &mut disable)
            .await
            .expect("cancelled disable request must join");
        false
    };
    let result = disable.as_mut().take_output().expect("disable joined");
    let mut check = env.db_txn().await;
    let after = mh.snapshot(&mut check).await;
    check.commit().await.unwrap();
    tracing::info!(
        ?gate_pid,
        ?disable_pid,
        ?readiness,
        ?result,
        dpf = ?after.host_snapshot.config.dpf,
        "Joined DPF disable after ingestion committed"
    );
    marker.expect("the lock owner commits the real DPF ingestion marker");
    released.expect("release the host gate");
    readiness.expect("observe the actual public disable wait");
    assert!(finished, "disable required timeout cancellation");
    let error = result.expect_err("disable must reject ingestion committed during its host wait");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("machine was ingested via DPF"));
    assert!(after.host_snapshot.config.dpf.enabled);
    assert!(after.host_snapshot.config.dpf.used_for_ingestion);
    assert_eq!(
        after.host_snapshot.state.value,
        before.host_snapshot.state.value
    );
    assert_eq!(
        after.host_snapshot.state.version,
        before.host_snapshot.state.version
    );
    assert_eq!(after.dpu_snapshots.len(), before.dpu_snapshots.len());
    for dpu in &after.dpu_snapshots {
        let prior = before
            .dpu_snapshots
            .iter()
            .find(|prior| prior.id == dpu.id)
            .unwrap();
        assert_eq!(dpu.config.dpf, prior.config.dpf);
        assert_eq!(dpu.state.value, prior.state.value);
        assert_eq!(dpu.state.version, prior.state.version);
    }
}

#[crate::sqlx_test]
async fn dpf_later_dpu_contention_releases_the_whole_admission_prefix(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::panic::{AssertUnwindSafe, resume_unwind};

    use carbide_uuid::machine::{HostMachineId, MachineIdSource, MachineInterfaceId, MachineType};
    use carbide_uuid::network::NetworkSegmentId;
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::FutureExt;
    use futures_util::future::maybe_done;
    use tokio::time::{Duration, timeout};

    async fn flags(
        pool: &sqlx::PgPool,
        ids: &[MachineId],
    ) -> Result<Vec<(MachineId, bool, bool)>, sqlx::Error> {
        sqlx::query_as(
            "SELECT id, (dpf->>'enabled')::boolean, (dpf->>'used_for_ingestion')::boolean
             FROM machines WHERE id = ANY($1) ORDER BY id COLLATE \"C\"",
        )
        .bind(ids)
        .fetch_all(pool)
        .await
    }

    let env = create_test_env(pool).await;
    let machine_id =
        |seed, kind| MachineId::new(MachineIdSource::ProductBoardChassisSerial, [seed; 32], kind);
    let host_id = HostMachineId::try_from(machine_id(0x61, MachineType::Host))?;
    let independent_host_id = HostMachineId::try_from(machine_id(0x62, MachineType::Host))?;
    let independent_dpu_id = DpuMachineId::try_from(machine_id(0x63, MachineType::Dpu))?;
    let mut dpu_ids = [
        DpuMachineId::try_from(machine_id(0x61, MachineType::Dpu))?,
        DpuMachineId::try_from(machine_id(0x62, MachineType::Dpu))?,
    ];
    dpu_ids.sort_unstable();
    let [first_dpu_id, later_dpu_id] = dpu_ids;
    let target_ids = [host_id.into(), first_dpu_id.into(), later_dpu_id.into()];
    let independent_ids = [independent_host_id.into(), independent_dpu_id.into()];
    let mut setup = env.pool.begin().await?;
    // The primary host link selects the later native DPU, which snapshot
    // loading puts first. Admission must still start with the earlier ID.
    // Insert DPU rows in that same opposing heap order.
    for id in [
        host_id.into(),
        later_dpu_id.into(),
        first_dpu_id.into(),
        independent_host_id.into(),
        independent_dpu_id.into(),
    ] {
        db::machine::create(
            setup.as_mut(),
            None,
            &id,
            ManagedHostState::Ready,
            None,
            model::machine::CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
    }
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version)
         VALUES ('dpf-later-dpu-contention', 'V1-T0') RETURNING id",
    )
    .fetch_one(setup.as_mut())
    .await?;
    let mut target_interfaces = Vec::new();
    for (index, (owner_id, dpu_id)) in [
        (host_id, later_dpu_id),
        (host_id, first_dpu_id),
        (independent_host_id, independent_dpu_id),
    ]
    .into_iter()
    .enumerate()
    {
        let interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                (machine_id, attached_dpu_machine_id, association_type, segment_id,
                 mac_address, primary_interface, hostname, interface_type)
             VALUES ($1, $2, 'Machine', $3, $4::macaddr, $5, 'dpf-contention', 'Data')
             RETURNING id",
        )
        .bind(owner_id)
        .bind(dpu_id)
        .bind(segment_id)
        .bind(format!("02:53:33:00:61:{index:02x}"))
        .bind(index != 1)
        .fetch_one(setup.as_mut())
        .await?;
        if owner_id == host_id {
            target_interfaces.push(interface_id);
        }
    }
    target_interfaces.sort_unstable();
    let heap_ids: Vec<DpuMachineId> =
        sqlx::query_scalar("SELECT id FROM machines WHERE id = ANY($1) ORDER BY ctid")
            .bind([MachineId::from(first_dpu_id), later_dpu_id.into()].as_slice())
            .fetch_all(setup.as_mut())
            .await?;
    let snapshot = db::managed_host::load_snapshot(setup.as_mut(), &host_id, Default::default())
        .await?
        .expect("seeded target host");
    assert_eq!(heap_ids, [later_dpu_id, first_dpu_id]);
    assert_eq!(
        snapshot
            .dpu_snapshots
            .iter()
            .map(|dpu| dpu.id)
            .collect::<Vec<_>>(),
        [later_dpu_id, first_dpu_id]
    );
    setup.commit().await?;
    let before = flags(&env.pool, &target_ids).await?;
    assert_eq!(before.len(), target_ids.len());
    assert!(
        before
            .iter()
            .all(|(_, enabled, ingestion)| *enabled && !*ingestion)
    );
    let independent_before = flags(&env.pool, &independent_ids).await?;
    assert_eq!(independent_before.len(), independent_ids.len());
    assert!(
        independent_before
            .iter()
            .all(|(_, enabled, ingestion)| *enabled && !*ingestion)
    );

    let mut first_gate = env.pool.begin().await?;
    db::machine::lock_by_id_for_update(first_gate.as_mut(), &first_dpu_id.into()).await?;
    let first_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(first_gate.as_mut())
        .await?;
    let mut later_gate = env.pool.begin().await?;
    db::machine::lock_by_id_for_update(later_gate.as_mut(), &later_dpu_id.into()).await?;
    let later_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(later_gate.as_mut())
        .await?;
    let update_task = tokio::spawn({
        let api = env.api.clone();
        async move {
            api.modify_dpf_state(tonic::Request::new(rpc::forge::ModifyDpfStateRequest {
                machine_id: Some(host_id),
                dpf_enabled: false,
            }))
            .await
        }
    });
    let update_abort = update_task.abort_handle();
    let update = maybe_done(update_task);
    tokio::pin!(update);
    let isolated_wait = "SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE";
    // The first gate proves native order before the later-member rejection;
    // it owns no host/interface locks that could conceal a retained prefix.
    let first_wait = AssertUnwindSafe(timeout(Duration::from_secs(25), async {
        tokio::select! {
            () = &mut update => Err("DPF passed the first native DPU gate".to_string()),
            pid = wait_for_blocked_query(&env.pool, first_gate_pid, isolated_wait) => Ok(pid),
        }
    }))
    .catch_unwind()
    .await;
    let first_release = first_gate.commit().await;
    let later_wait = if matches!(&first_wait, Ok(Ok(Ok(_)))) && first_release.is_ok() {
        Some(
            AssertUnwindSafe(timeout(Duration::from_secs(25), async {
                tokio::select! {
                    () = &mut update => Err("DPF passed the later native DPU gate".to_string()),
                    pid = wait_for_blocked_query(&env.pool, later_gate_pid, isolated_wait) => Ok(pid),
                }
            }))
            .catch_unwind()
            .await,
        )
    } else {
        None
    };
    let released_prefix = if matches!(&later_wait, Some(Ok(Ok(Ok(_))))) {
        Some(
            async {
                sqlx::query_scalar::<_, MachineId>(
                    "SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT",
                )
                .bind(host_id)
                .fetch_one(later_gate.as_mut())
                .await?;
                sqlx::query_scalar::<_, MachineId>(
                    "SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT",
                )
                .bind(first_dpu_id)
                .fetch_one(later_gate.as_mut())
                .await?;
                sqlx::query_scalar::<_, MachineInterfaceId>(
                    "SELECT id FROM machine_interfaces WHERE machine_id = $1
                     ORDER BY id FOR UPDATE NOWAIT",
                )
                .bind(host_id)
                .fetch_all(later_gate.as_mut())
                .await
            }
            .await,
        )
    } else {
        None
    };
    let independent_task = tokio::spawn({
        let api = env.api.clone();
        async move {
            api.modify_dpf_state(tonic::Request::new(rpc::forge::ModifyDpfStateRequest {
                machine_id: Some(independent_host_id),
                dpf_enabled: false,
            }))
            .await
        }
    });
    let independent_abort = independent_task.abort_handle();
    let independent = maybe_done(independent_task);
    tokio::pin!(independent);
    let independent_progress = if matches!(&later_wait, Some(Ok(Ok(Ok(_))))) {
        Some(
            timeout(Duration::from_secs(25), async {
                tokio::select! {
                    () = &mut update => Err("target DPF passed its held later DPU".to_string()),
                    () = &mut independent => Ok(()),
                }
            })
            .await,
        )
    } else {
        None
    };
    let waiting_flags = flags(&env.pool, &target_ids).await;
    let independent_flags = flags(&env.pool, &independent_ids).await;
    let later_release = later_gate.commit().await;
    let finished = timeout(Duration::from_secs(30), async {
        tokio::join!(&mut update, &mut independent);
    })
    .await
    .is_ok();
    let settled = if finished {
        true
    } else {
        update_abort.abort();
        independent_abort.abort();
        timeout(Duration::from_secs(30), async {
            tokio::join!(&mut update, &mut independent);
        })
        .await
        .is_ok()
    };
    let update_result = update.as_mut().take_output();
    let independent_result = independent.as_mut().take_output();
    let diagnostic = format!(
        "first_wait={first_wait:?}; later_wait={later_wait:?}; \
         released_prefix={released_prefix:?}; independent_progress={independent_progress:?}; \
         waiting_flags={waiting_flags:?}; independent_flags={independent_flags:?}; \
         first_release={first_release:?}; later_release={later_release:?}; \
         update_result={update_result:?}; independent_result={independent_result:?}"
    );
    tracing::info!(%diagnostic, finished, settled, "Joined public DPF admission participants");
    assert!(settled, "DPF participants did not settle: {diagnostic}");
    assert!(
        finished,
        "DPF participants required cancellation: {diagnostic}"
    );
    let first_wait = first_wait.unwrap_or_else(|panic| resume_unwind(panic));
    let later_wait = later_wait.map(|result| result.unwrap_or_else(|panic| resume_unwind(panic)));
    assert!(
        matches!(first_wait, Ok(Ok(_)))
            && matches!(later_wait, Some(Ok(Ok(_))))
            && matches!(independent_progress, Some(Ok(Ok(())))),
        "public DPF waits/progress were not observed: {diagnostic}"
    );
    first_release?;
    later_release?;
    assert_eq!(
        released_prefix.expect("later DPU wait was observed")?,
        target_interfaces,
        "{diagnostic}"
    );
    assert_eq!(waiting_flags?, before, "{diagnostic}");
    let independent_flags = independent_flags?;
    assert_eq!(independent_flags.len(), independent_ids.len());
    assert!(
        independent_flags
            .iter()
            .all(|(_, enabled, ingestion)| !*enabled && !*ingestion)
    );
    update_result.expect("target joined")??;
    independent_result.expect("independent host joined")??;

    let expected = before
        .into_iter()
        .map(|(id, _, ingestion)| (id, false, ingestion))
        .collect::<Vec<_>>();
    assert_eq!(flags(&env.pool, &target_ids).await?, expected);
    let mut check = env.pool.begin().await?;
    let current = db::machine::find_dpus_by_host_machine_id(check.as_mut(), &host_id)
        .await?
        .into_iter()
        .map(|dpu| dpu.id)
        .collect::<Vec<_>>();
    check.commit().await?;
    assert_eq!(current, dpu_ids);
    Ok(())
}

/// Existing-D discovery with complete loopbacks can change a host link without
/// updating H. DPF must own that attachment set through its dependent writes.
#[crate::sqlx_test]
async fn dpf_updates_admit_the_current_discovery_cohort(pool: sqlx::PgPool) {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use carbide_uuid::machine::{HostMachineId, MachineInterfaceId};
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use model::hardware_info::HardwareInfo;
    use model::machine::machine_id::from_hardware_info;
    use model::machine::machine_search_config::MachineSearchConfig;
    use model::machine_interface_address::MachineInterfaceAssociation;
    use model::resource_pool::common::LOOPBACK_IP_V6;
    use model::resource_pool::{ResourcePoolDef, ResourcePoolType};
    use tokio::time::{Duration, timeout};

    use crate::tests::common::api_fixtures::dpu::dpu_discover_dhcp;

    struct Observation {
        machines: HashMap<MachineId, model::machine::AnyMachine>,
        link: model::machine::MachineInterfaceSnapshot,
        dpu_ids: Vec<DpuMachineId>,
        new_reference_owner: Option<MachineId>,
    }

    async fn observe(
        pool: &sqlx::PgPool,
        host_id: HostMachineId,
        ids: &[MachineId],
        link_id: MachineInterfaceId,
        new_reference_id: Option<MachineInterfaceId>,
    ) -> Result<Observation, Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machines = db::machine::find(
            txn.as_mut(),
            db::ObjectFilter::List(ids),
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .into_iter()
        .map(|machine| (machine.id, machine))
        .collect();
        let link = db::machine_interface::find_one(txn.as_mut(), link_id).await?;
        let dpu_ids = db::machine::find_dpus_by_host_machine_id(txn.as_mut(), &host_id)
            .await?
            .into_iter()
            .map(|machine| machine.id)
            .collect();
        let new_reference_owner = match new_reference_id {
            Some(id) => {
                db::machine_interface::find_one(txn.as_mut(), id)
                    .await?
                    .machine_id
            }
            None => None,
        };
        txn.commit().await?;
        Ok(Observation {
            machines,
            link,
            dpu_ids,
            new_reference_owner,
        })
    }

    async fn gate_admitted_dpf_flag_write(
        pool: &sqlx::PgPool,
        dpu_id: DpuMachineId,
    ) -> Result<sqlx::PgTransaction<'static>, sqlx::Error> {
        let statement = format!(
            r#"
            CREATE FUNCTION pause_admitted_dpf_flag_write()
            RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM pg_advisory_xact_lock(5333, 52);
                RETURN NEW;
            END;
            $$;

            CREATE TRIGGER pause_admitted_dpf_flag_write
            BEFORE UPDATE OF dpf ON machines
            FOR EACH ROW
            WHEN (OLD.id = '{dpu_id}')
            EXECUTE FUNCTION pause_admitted_dpf_flag_write();
            "#
        );
        // Only a canonical typed DPU ID is interpolated into this test DDL.
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(pool)
            .await?;
        let mut gate = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(5333, 52)")
            .execute(gate.as_mut())
            .await?;
        Ok(gate)
    }

    let env = create_test_env(pool).await;
    let mut setup = env.db_txn().await;
    db::resource_pool::define(
        setup.as_mut(),
        LOOPBACK_IP_V6,
        &ResourcePoolDef {
            pool_type: ResourcePoolType::Ipv6,
            prefix: Some("2001:db8::/124".to_string()),
            ranges: vec![],
            delegate_prefix_len: None,
        },
    )
    .await
    .unwrap();
    setup.commit().await.unwrap();

    let mut failures = Vec::new();
    for (scenario, dpf_first) in [
        ("DPF admitted first", true),
        ("discovery queued first", false),
    ] {
        let mh = create_managed_host(&env).await;
        let host_id: HostMachineId = mh.id.into();
        let dpu1_id = mh.dpu_ids[0];
        let mut setup = env.db_txn().await;
        let snapshot = mh.snapshot(&mut setup).await;
        let host_link = snapshot
            .host_snapshot
            .status
            .interfaces
            .iter()
            .find(|interface| interface.attached_dpu_machine_id == Some(dpu1_id))
            .expect("the fixture host has its D1 link")
            .clone();
        assert!(host_link.primary_interface && !host_link.addresses.is_empty());
        assert!(!snapshot.host_snapshot.config.dpf.used_for_ingestion);
        setup.commit().await.unwrap();

        let standalone_config = env.managed_host_config();
        let mut dpu2_config = standalone_config.get_and_assert_single_dpu().clone();
        dpu2_config.host_mac_address = host_link.mac_address;
        let hardware = HardwareInfo::from(&dpu2_config);
        let dpu2_machine_id = from_hardware_info(&hardware).unwrap();
        let dpu2_id = DpuMachineId::try_from(dpu2_machine_id).unwrap();
        assert_ne!(dpu2_id, dpu1_id);
        let oob_id = dpu_discover_dhcp(&env, &dpu2_config.oob_mac_address.to_string()).await;
        let mut setup = env.db_txn().await;
        let oob = db::machine_interface::find_one(setup.as_mut(), oob_id)
            .await
            .unwrap();
        assert!(oob.machine_id.is_none());
        let dpu2 = db::machine::get_or_create(
            setup.as_mut(),
            Some(&env.common_pools),
            &dpu2_machine_id,
            &oob,
        )
        .await
        .unwrap();
        db::machine_interface::associate_interface_with_dpu_machine(
            &oob_id,
            &dpu2_machine_id,
            setup.as_mut(),
        )
        .await
        .unwrap();
        db::machine_topology::create_or_update(setup.as_mut(), &dpu2_machine_id, &hardware)
            .await
            .unwrap();
        let mut config = dpu2.network_config.value;
        config.loopback_ip = Some(
            db::resource_pool::allocate(
                &env.common_pools.ethernet.pool_loopback_ip,
                setup.as_mut(),
                model::resource_pool::OwnerType::Machine,
                &dpu2_machine_id.to_string(),
                None,
            )
            .await
            .unwrap(),
        );
        assert!(
            config.loopback_ip_v6.is_some(),
            "IPv6 is enabled and reserved for D2"
        );
        assert_eq!(
            db::machine::try_update_network_config(
                setup.as_mut(),
                &dpu2_machine_id,
                dpu2.network_config.version,
                &config,
            )
            .await
            .unwrap(),
            db::ConditionalWrite::Applied(())
        );
        let ids = [
            MachineId::from(host_id),
            MachineId::from(dpu1_id),
            dpu2_machine_id,
        ];
        for id in ids {
            db::machine::modify_dpf_state(setup.as_mut(), &id, false)
                .await
                .unwrap();
        }
        assert!(
            db::machine::find_host_by_dpu_machine_id(setup.as_mut(), &dpu2_id)
                .await
                .unwrap()
                .is_none(),
            "D2 must be standalone before discovery"
        );
        let new_reference_id: Option<MachineInterfaceId> = if dpf_first {
            Some(
                sqlx::query_scalar(
                    "INSERT INTO machine_interfaces
                     (segment_id, mac_address, primary_interface, hostname, interface_type)
                 VALUES ($1, $2, false, 'dpf-new-reference', 'Data') RETURNING id",
                )
                .bind(host_link.segment_id)
                .bind(
                    standalone_config
                        .get_and_assert_single_dpu()
                        .host_mac_address,
                )
                .fetch_one(setup.as_mut())
                .await
                .unwrap(),
            )
        } else {
            None
        };
        setup.commit().await.unwrap();
        let before = observe(&env.pool, host_id, &ids, host_link.id, new_reference_id)
            .await
            .unwrap();
        assert_eq!(before.dpu_ids, vec![dpu1_id]);
        assert_eq!(before.link.machine_id, Some(host_id.into()));
        assert_eq!(before.link.attached_dpu_machine_id, Some(dpu1_id));
        assert_eq!(before.new_reference_owner, None);
        let caller = db::machine_interface::find_one(&env.pool, oob_id)
            .await
            .unwrap();
        assert_eq!(caller.machine_id, Some(dpu2_machine_id));
        assert_eq!(caller.mac_address, dpu2_config.oob_mac_address);
        assert!(
            before.machines[&dpu2_machine_id]
                .network_config
                .loopback_ip
                .is_some()
        );
        assert!(
            before.machines[&dpu2_machine_id]
                .network_config
                .loopback_ip_v6
                .is_some()
        );

        let mut dpu_gate = if dpf_first {
            gate_admitted_dpf_flag_write(&env.pool, dpu1_id)
                .await
                .unwrap()
        } else {
            let mut gate = env.db_txn().await;
            db::machine::lock_by_id_for_update(gate.as_mut(), &dpu1_id.into())
                .await
                .unwrap();
            gate
        };
        let dpu_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(dpu_gate.as_mut())
            .await
            .unwrap();
        let mut link_gate = if dpf_first {
            None
        } else {
            Some(env.db_txn().await)
        };
        let link_gate_pid: Option<i32> = if let Some(gate) = &mut link_gate {
            sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
                .bind(host_link.id)
                .execute(gate.as_mut())
                .await
                .unwrap();
            Some(
                sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(gate.as_mut())
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let mut fk_txn = if dpf_first {
            Some(env.db_txn().await)
        } else {
            None
        };
        let fk_pid: Option<i32> = if let Some(txn) = &mut fk_txn {
            Some(
                sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(txn.as_mut())
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let update = maybe_done(env.api.modify_dpf_state(tonic::Request::new(
            rpc::forge::ModifyDpfStateRequest {
                machine_id: Some(host_id),
                dpf_enabled: true,
            },
        )));
        let discovery = maybe_done(env.api.discover_machine(tonic::Request::new(
            rpc::forge::MachineDiscoveryInfo {
                machine_interface_id: Some(oob_id),
                discovery_data: Some(rpc::DiscoveryData::Info(
                    rpc::DiscoveryInfo::try_from(hardware).unwrap(),
                )),
                create_machine: false,
                ..Default::default()
            },
        )));
        let new_reference = maybe_done(async {
            let Some(mut txn) = fk_txn else {
                return Ok::<_, Box<dyn std::error::Error>>(None);
            };
            let id = db::machine_interface::associate_interface_with_machine(
                &new_reference_id.expect("one real new-reference probe"),
                MachineInterfaceAssociation::Machine(host_id.into()),
                txn.as_mut(),
            )
            .await?;
            txn.commit().await?;
            Ok(Some(id))
        });
        tokio::pin!(update, discovery, new_reference);
        let mut update_pid = None;
        let mut discovery_pid = None;
        let mut discovery_blocked = false;
        let mut new_reference_blocked = false;
        let mut wait_arm = None;
        let readiness = timeout(Duration::from_secs(25), async {
            if dpf_first {
                update_pid = Some(tokio::select! {
                    () = &mut update => return Err("DPF did not reach its admitted D1 flag write".to_string()),
                    pid = wait_for_blocked_query(&env.pool, dpu_gate_pid, "UPDATE machines set dpf") => pid,
                });
                tokio::select! {
                    () = &mut discovery => {},
                    pid = wait_for_blocked_query(&env.pool, update_pid.unwrap(), "UPDATE machine_interfaces SET attached_dpu_machine_id") => {
                        discovery_pid = Some(pid);
                        discovery_blocked = true;
                    }
                }
                tokio::select! {
                    () = &mut new_reference => {},
                    _ = wait_for_blocked_query(&env.pool, update_pid.unwrap(), "UPDATE machine_interfaces SET machine_id") => {
                        new_reference_blocked = true;
                    }
                }
                wait_arm = Some("admitted D1 flag write");
            } else {
                discovery_pid = Some(tokio::select! {
                    () = &mut discovery => return Err("discovery passed its interface gate".to_string()),
                    pid = wait_for_blocked_query(&env.pool, link_gate_pid.unwrap(), "UPDATE machine_interfaces SET attached_dpu_machine_id") => pid,
                });
                let (pid, arm) = tokio::select! {
                    () = &mut discovery => return Err("discovery passed its interface gate".to_string()),
                    () = &mut update => return Err("DPF completed before the discovery cohort was admitted".to_string()),
                    pid = wait_for_blocked_query(&env.pool, dpu_gate_pid, "UPDATE machines set dpf") => (pid, "captured DPU flag update"),
                    pid = wait_for_blocked_query(&env.pool, discovery_pid.unwrap(), "SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE") => (pid, "isolated interface wait"),
                };
                update_pid = Some(pid);
                wait_arm = Some(arm);
            }
            Ok::<(), String>(())
        }).await.unwrap_or_else(|error| Err(format!("cohort readiness: {error}")));
        let at_gate = observe(&env.pool, host_id, &ids, host_link.id, new_reference_id).await;
        let released_prefix = if !dpf_first {
            Some(
                async {
                    let mut probe = env.pool.begin().await?;
                    let result =
                        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                            .bind(host_id)
                            .execute(probe.as_mut())
                            .await;
                    probe.rollback().await?;
                    result.map(|_| ())
                }
                .await,
            )
        } else {
            None
        };
        let link_release = match link_gate.take() {
            Some(gate) => Some(gate.commit().await),
            None => None,
        };
        let discovery_joined_before_dpu_release = if !dpf_first && readiness.is_ok() {
            Some(
                timeout(Duration::from_secs(30), async {
                    tokio::select! {
                        () = &mut discovery => {},
                        () = &mut update => { (&mut discovery).await; }
                    }
                })
                .await,
            )
        } else {
            None
        };
        let after_rebind = if !dpf_first
            && discovery_joined_before_dpu_release
                .as_ref()
                .is_some_and(Result::is_ok)
        {
            Some(observe(&env.pool, host_id, &ids, host_link.id, new_reference_id).await)
        } else {
            None
        };
        let dpu_release = dpu_gate.commit().await;
        let finished = {
            let joined = async {
                tokio::join!(&mut update, &mut discovery, &mut new_reference);
            };
            tokio::pin!(joined);
            let finished = timeout(Duration::from_secs(30), &mut joined).await.is_ok();
            if !finished {
                for pid in update_pid.into_iter().chain(discovery_pid).chain(fk_pid) {
                    sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                        .bind(pid)
                        .fetch_one(&env.pool)
                        .await
                        .expect("cancel observed cohort writer");
                }
                timeout(Duration::from_secs(30), &mut joined)
                    .await
                    .expect("cancelled DPF/discovery/reference writers must join");
            }
            finished
        };
        let gate_cleanup = if dpf_first {
            sqlx::raw_sql(
                "DROP TRIGGER pause_admitted_dpf_flag_write ON machines;
                 DROP FUNCTION pause_admitted_dpf_flag_write();",
            )
            .execute(&env.pool)
            .await
            .map(|_| ())
        } else {
            Ok(())
        };
        let update_result = update.as_mut().take_output().expect("DPF joined");
        let discovery_result = discovery.as_mut().take_output().expect("discovery joined");
        let reference_result = new_reference
            .as_mut()
            .take_output()
            .expect("reference probe joined");
        let after = observe(&env.pool, host_id, &ids, host_link.id, new_reference_id)
            .await
            .unwrap();
        tracing::info!(scenario, ?update_pid, ?discovery_pid, ?fk_pid, ?wait_arm, ?readiness,
            discovery_blocked, new_reference_blocked, ?released_prefix,
            ?update_result, ?discovery_result, ?reference_result,
            before_cohort = ?at_gate.as_ref().map(|value| &value.dpu_ids), after_cohort = ?after.dpu_ids,
            "Joined actual DPF/discovery cohort participants");
        let checked = catch_unwind(AssertUnwindSafe(|| {
            if let Some(release) = link_release {
                release.expect("release the interface gate");
            }
            dpu_release.expect("release the D1 gate");
            gate_cleanup.expect("remove the joined DPF flag gate");
            readiness.expect("observe the actual owner/discovery query arms");
            if !dpf_first {
                assert_eq!(wait_arm, Some("isolated interface wait"), "{scenario}");
            }
            assert!(finished, "{scenario}: requests required cancellation");
            update_result.expect("the public DPF update succeeds");
            let discovered = discovery_result
                .expect("the accepted existing-D discovery succeeds")
                .into_inner();
            assert_eq!(discovered.machine_id, Some(dpu2_machine_id));
            reference_result.expect("the actual new owner-FK primitive succeeds after release");
            let at_gate = at_gate.expect("observe before the D1 gate releases");
            if dpf_first {
                assert!(
                    discovery_blocked,
                    "{scenario}: discovery committed an attachment change inside DPF's captured-cohort phase"
                );
                assert_eq!(at_gate.link.attached_dpu_machine_id, Some(dpu1_id));
                assert_eq!(at_gate.dpu_ids, vec![dpu1_id]);
                assert!(
                    new_reference_blocked,
                    "{scenario}: a new owner FK bypassed DPF's strong host FOR UPDATE lock"
                );
                assert_eq!(at_gate.new_reference_owner, None);
            } else {
                assert_eq!(wait_arm, Some("isolated interface wait"));
                released_prefix
                    .unwrap()
                    .expect("the rejected attempt releases strong H before waiting on I");
                discovery_joined_before_dpu_release
                    .unwrap()
                    .expect("discovery joins while old D1 remains gated");
                let rebound = after_rebind
                    .unwrap()
                    .expect("observe the committed rebind before releasing old D1");
                assert_eq!(rebound.link.attached_dpu_machine_id, Some(dpu2_id));
                assert_eq!(rebound.dpu_ids, vec![dpu2_id]);
            }
            assert_eq!(after.link.machine_id, Some(host_id.into()));
            assert_eq!(after.link.attached_dpu_machine_id, Some(dpu2_id));
            assert_eq!(after.dpu_ids, vec![dpu2_id]);
            assert!(after.machines[&ids[0]].config.dpf.enabled);
            assert_eq!(after.machines[&ids[1]].config.dpf.enabled, dpf_first);
            assert_eq!(after.machines[&ids[2]].config.dpf.enabled, !dpf_first);
            for id in ids {
                let old = &before.machines[&id];
                let current = &after.machines[&id];
                assert_eq!(current.state.value, old.state.value);
                assert_eq!(current.state.version, old.state.version);
                assert_eq!(current.network_config.value, old.network_config.value);
                assert_eq!(current.network_config.version, old.network_config.version);
                assert_eq!(
                    serde_json::to_value(&current.history).unwrap(),
                    serde_json::to_value(&old.history).unwrap()
                );
            }
        }));
        if let Err(panic) = checked {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("cohort assertion failed");
            failures.push(format!("{scenario}: {message}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Restart must reject a cohort that accepted discovery replaced during its DPU wait.
#[crate::sqlx_test]
async fn restart_rejects_discovery_rebind_during_dpu_wait(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use model::hardware_info::HardwareInfo;
    use model::machine::machine_id::from_hardware_info;
    use model::machine::machine_search_config::MachineSearchConfig;
    use model::resource_pool::common::LOOPBACK_IP_V6;
    use model::resource_pool::{ResourcePoolDef, ResourcePoolType};
    use tokio::time::{Duration as TimeoutDuration, timeout};

    use crate::tests::common::api_fixtures::dpu::dpu_discover_dhcp;
    use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};

    let mut site_config = common::api_fixtures::get_config();
    site_config.dpf.enabled = false;
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::with_config(site_config)).await;
    let mut setup = env.db_txn().await;
    db::resource_pool::define(
        setup.as_mut(),
        LOOPBACK_IP_V6,
        &ResourcePoolDef {
            pool_type: ResourcePoolType::Ipv6,
            prefix: Some("2001:db8::/124".to_string()),
            ranges: vec![],
            delegate_prefix_len: None,
        },
    )
    .await?;
    setup.commit().await?;

    let mh = create_managed_host(&env).await;
    mh.mark_machine_for_updates().await;
    mh.host().trigger_dpu_reprovisioning(Mode::Set, true).await;
    let dpu1_id = mh.dpu_ids[0];
    let mut setup = env.db_txn().await;
    let host = mh.snapshot(&mut setup).await;
    let host_link = host
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|interface| interface.attached_dpu_machine_id == Some(dpu1_id))
        .expect("the fixture owns its addressed D1 link")
        .clone();
    assert!(host_link.primary_interface && !host_link.addresses.is_empty());
    assert!(!host.host_snapshot.config.dpf.used_for_ingestion);
    let original_request = host.dpu_snapshots[0]
        .reprovision_requested
        .clone()
        .expect("Set stored the selected request");
    assert!(original_request.started_at.is_none());
    setup.commit().await?;

    let standalone_config = env.managed_host_config();
    let mut dpu2_config = standalone_config.get_and_assert_single_dpu().clone();
    dpu2_config.host_mac_address = host_link.mac_address;
    let hardware = HardwareInfo::from(&dpu2_config);
    let dpu2_machine_id = from_hardware_info(&hardware)?;
    let dpu2_id = DpuMachineId::try_from(dpu2_machine_id)?;
    assert_ne!(dpu2_id, dpu1_id);
    let oob_id = dpu_discover_dhcp(&env, &dpu2_config.oob_mac_address.to_string()).await;
    let mut setup = env.db_txn().await;
    let oob = db::machine_interface::find_one(setup.as_mut(), oob_id).await?;
    assert!(oob.machine_id.is_none());
    let dpu2 = db::machine::get_or_create(
        setup.as_mut(),
        Some(&env.common_pools),
        &dpu2_machine_id,
        &oob,
    )
    .await?;
    db::machine_interface::associate_interface_with_dpu_machine(
        &oob_id,
        &dpu2_machine_id,
        setup.as_mut(),
    )
    .await?;
    db::machine_topology::create_or_update(setup.as_mut(), &dpu2_machine_id, &hardware).await?;
    let mut config = dpu2.network_config.value;
    config.loopback_ip = Some(
        db::resource_pool::allocate(
            &env.common_pools.ethernet.pool_loopback_ip,
            setup.as_mut(),
            model::resource_pool::OwnerType::Machine,
            &dpu2_machine_id.to_string(),
            None,
        )
        .await?,
    );
    assert!(
        config.loopback_ip_v6.is_some(),
        "D2 reserves enabled IPv6 too"
    );
    assert_eq!(
        db::machine::try_update_network_config(
            setup.as_mut(),
            &dpu2_machine_id,
            dpu2.network_config.version,
            &config,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    assert!(
        db::machine::find_host_by_dpu_machine_id(setup.as_mut(), &dpu2_id)
            .await?
            .is_none(),
        "D2 must be standalone before its real discovery request"
    );
    let caller = db::machine_interface::find_one(setup.as_mut(), oob_id).await?;
    assert_eq!(caller.machine_id, Some(dpu2_machine_id));
    assert_eq!(caller.mac_address, dpu2_config.oob_mac_address);
    let machine_ids = [MachineId::from(mh.id), dpu1_id.into(), dpu2_machine_id];
    let before = db::machine::find(
        setup.as_mut(),
        db::ObjectFilter::List(&machine_ids),
        MachineSearchConfig {
            include_history: true,
            ..Default::default()
        },
    )
    .await?
    .into_iter()
    .map(|machine| (machine.id, machine))
    .collect::<HashMap<_, _>>();
    assert!(
        before[&dpu2_machine_id]
            .network_config
            .loopback_ip
            .is_some()
    );
    assert!(
        before[&dpu2_machine_id]
            .network_config
            .loopback_ip_v6
            .is_some()
    );
    assert!(before[&dpu2_machine_id].reprovision_requested.is_none());
    setup.commit().await?;
    let redfish_timepoint = env.redfish_sim.timepoint();

    let mut gate = env.db_txn().await;
    db::machine::lock_by_id_for_update(gate.as_mut(), &dpu1_id.into()).await?;
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await?;
    let restart = maybe_done(env.api.trigger_dpu_reprovisioning(tonic::Request::new(
        rpc::forge::DpuReprovisioningRequest {
            machine_id: Some(mh.id.into()),
            mode: Mode::Restart as i32,
            initiator: rpc::forge::UpdateInitiator::AdminCli as i32,
            update_firmware: false,
            ..Default::default()
        },
    )));
    let discovery = maybe_done(env.api.discover_machine(tonic::Request::new(
        rpc::forge::MachineDiscoveryInfo {
            machine_interface_id: Some(oob_id),
            discovery_data: Some(rpc::DiscoveryData::Info(rpc::DiscoveryInfo::try_from(
                hardware,
            )?)),
            create_machine: false,
            ..Default::default()
        },
    )));
    tokio::pin!(restart, discovery);
    let mut restart_pid = None;
    let mut discovery_pid = None;
    let mut query_arm = None;
    let readiness = timeout(TimeoutDuration::from_secs(25), async {
        let (pid, arm) = tokio::select! {
            () = &mut restart => return Err(format!("Restart passed its D1 gate: {:?}", restart.as_mut().output_mut())),
            pid = wait_for_blocked_query(&env.pool, gate_pid, "reprovisioning_requested IS NOT NULL") => (pid, "pending request row selector"),
            pid = wait_for_blocked_query(&env.pool, gate_pid, "SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE") => (pid, "isolated non-key DPU wait"),
        };
        restart_pid = Some(pid);
        query_arm = Some(arm);
        tokio::select! {
            () = &mut discovery => Ok(()),
            () = &mut restart => Err(format!("Restart finished before D1 released: {:?}", restart.as_mut().output_mut())),
            pid = wait_for_blocked_query(&env.pool, pid, "UPDATE machine_interfaces SET attached_dpu_machine_id") => {
                discovery_pid = Some(pid);
                Err("Restart retained an interface while waiting for D1".to_string())
            },
        }
    })
    .await
    .unwrap_or_else(|error| Err(format!("Restart/discovery readiness timed out: {error}")));
    let mut probe = env.db_txn().await;
    let host_probe = sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
        .bind(mh.id)
        .execute(probe.as_mut())
        .await;
    let probe_release = probe.rollback().await;
    let mut paused = env.db_txn().await;
    let paused_link = db::machine_interface::find_one(paused.as_mut(), host_link.id).await;
    let paused_cohort = db::machine::find_dpus_by_host_machine_id(paused.as_mut(), &mh.id.into())
        .await
        .map(|machines| {
            machines
                .into_iter()
                .map(|machine| machine.id)
                .collect::<Vec<_>>()
        });
    let paused_release = paused.commit().await;
    let gate_release = gate.commit().await;
    let finished = {
        let joined = async { tokio::join!(&mut restart, &mut discovery) };
        tokio::pin!(joined);
        let finished = timeout(TimeoutDuration::from_secs(30), &mut joined)
            .await
            .is_ok();
        if !finished {
            for pid in restart_pid.into_iter().chain(discovery_pid) {
                sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                    .bind(pid)
                    .fetch_one(&env.pool)
                    .await
                    .expect("cancel only an observed stalled cohort writer");
            }
            timeout(TimeoutDuration::from_secs(30), &mut joined)
                .await
                .expect("released or cancelled Restart/discovery participants must join");
        }
        finished
    };
    let restart_result = restart.as_mut().take_output().expect("Restart joined");
    let discovery_result = discovery.as_mut().take_output().expect("Discovery joined");
    let mut check = env.db_txn().await;
    let after = db::machine::find(
        check.as_mut(),
        db::ObjectFilter::List(&machine_ids),
        MachineSearchConfig {
            include_history: true,
            ..Default::default()
        },
    )
    .await?
    .into_iter()
    .map(|machine| (machine.id, machine))
    .collect::<HashMap<_, _>>();
    check.commit().await?;
    tracing::info!(?gate_pid, ?restart_pid, ?discovery_pid, ?query_arm, ?readiness,
        ?restart_result, ?discovery_result, cohort = ?paused_cohort,
        "Joined Restart after accepted existing-D discovery changed its cohort");

    probe_release?;
    paused_release?;
    gate_release?;
    readiness.expect("real discovery must commit before old D1 releases");
    assert_eq!(query_arm, Some("isolated non-key DPU wait"));
    assert!(finished, "participants required timeout cancellation");
    host_probe.expect("Restart must release H before waiting on D1");
    let paused_link = paused_link?;
    assert_eq!(paused_link.machine_id, Some(mh.id.into()));
    assert_eq!(paused_link.attached_dpu_machine_id, Some(dpu2_id));
    assert_eq!(paused_cohort?, vec![dpu2_id]);
    let discovered = discovery_result?.into_inner();
    assert_eq!(discovered.machine_id, Some(dpu2_machine_id));
    let error = restart_result.expect_err("Restart must reject its detached selected DPU");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        after[&MachineId::from(dpu1_id)]
            .reprovision_requested
            .as_ref(),
        Some(&original_request)
    );
    assert!(after[&dpu2_machine_id].reprovision_requested.is_none());
    for id in machine_ids {
        let old = &before[&id];
        let current = &after[&id];
        assert_eq!(
            current.reprovision_requested, old.reprovision_requested,
            "{id}"
        );
        assert_eq!(current.state.value, old.state.value, "{id}");
        assert_eq!(current.state.version, old.state.version, "{id}");
        assert_eq!(
            current.network_config.value, old.network_config.value,
            "{id}"
        );
        assert_eq!(
            current.network_config.version, old.network_config.version,
            "{id}"
        );
        assert_eq!(
            serde_json::to_value(&current.history)?,
            serde_json::to_value(&old.history)?,
            "{id}"
        );
        assert_eq!(
            serde_json::to_value(&current.controller_state_outcome)?,
            serde_json::to_value(&old.controller_state_outcome)?,
            "{id}"
        );
    }
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty()
    );
    Ok(())
}

/// Host Restart must revalidate eligible IDs and the host update marker after its wait.
#[crate::sqlx_test]
async fn restart_revalidates_pending_selection_and_host_marker_after_wait(pool: sqlx::PgPool) {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use model::machine::machine_search_config::MachineSearchConfig;
    use tokio::time::{Duration as TimeoutDuration, timeout};

    use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};

    #[derive(Clone, Copy, Debug)]
    enum ConcurrentChange {
        HostSetAddsIdleSibling,
        TargetSetReplacesSelectedRequest,
        RemoveHostUpdateMarker,
    }

    let cases = [
        (
            "host Set adds the idle sibling",
            ConcurrentChange::HostSetAddsIdleSibling,
        ),
        (
            "target Set replaces only the selected request",
            ConcurrentChange::TargetSetReplacesSelectedRequest,
        ),
        (
            "host update marker is removed",
            ConcurrentChange::RemoveHostUpdateMarker,
        ),
    ];
    let mut config = common::api_fixtures::get_config();
    config.dpf.enabled = false;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    // Build every Ready fixture before any row stores a pending request, so
    // later fixture setup cannot advance an earlier row's reprovision state.
    let mut hosts = Vec::new();
    for _ in &cases {
        hosts.push(create_managed_host_multi_dpu(&env, 2).await);
    }

    let mut failures = Vec::new();
    for ((scenario, change), mh) in cases.into_iter().zip(&hosts) {
        let set_writer = !matches!(change, ConcurrentChange::RemoveHostUpdateMarker);
        let successful_merge = matches!(change, ConcurrentChange::TargetSetReplacesSelectedRequest);
        mh.mark_machine_for_updates().await;
        mh.dpu_n(0)
            .trigger_dpu_reprovisioning(Mode::Set, true)
            .await;
        let dpu1_id = mh.dpu_ids[0];
        let dpu2_id = mh.dpu_ids[1];
        let machine_ids = [MachineId::from(mh.id), dpu1_id.into(), dpu2_id.into()];
        let mut txn = env.db_txn().await;
        let before = db::machine::find(
            txn.as_mut(),
            db::ObjectFilter::List(&machine_ids),
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|machine| (machine.id, machine))
        .collect::<HashMap<_, _>>();
        assert_eq!(before[&machine_ids[0]].state.value, ManagedHostState::Ready);
        assert!(!before[&machine_ids[0]].config.dpf.used_for_ingestion);
        let initial = before[&machine_ids[1]]
            .reprovision_requested
            .as_ref()
            .unwrap();
        assert!(initial.started_at.is_none() && initial.update_firmware);
        assert!(before[&machine_ids[2]].reprovision_requested.is_none());
        txn.commit().await.unwrap();
        let redfish_timepoint = env.redfish_sim.timepoint();

        let mut gate = env.db_txn().await;
        db::machine::lock_by_id_for_update(gate.as_mut(), &dpu1_id.into())
            .await
            .unwrap();
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await
            .unwrap();
        let mut host_gate = Some(env.db_txn().await);
        let host_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(host_gate.as_mut().unwrap().as_mut())
            .await
            .unwrap();
        let writer = maybe_done(async {
            match change {
                ConcurrentChange::HostSetAddsIdleSibling => {
                    env.api
                        .trigger_dpu_reprovisioning(tonic::Request::new(
                            rpc::forge::DpuReprovisioningRequest {
                                machine_id: Some(mh.id.into()),
                                mode: Mode::Set as i32,
                                initiator: rpc::forge::UpdateInitiator::AdminCli as i32,
                                update_firmware: false,
                                ..Default::default()
                            },
                        ))
                        .await
                }
                ConcurrentChange::TargetSetReplacesSelectedRequest => {
                    env.api
                        .trigger_dpu_reprovisioning(tonic::Request::new(
                            rpc::forge::DpuReprovisioningRequest {
                                machine_id: Some(dpu1_id.into()),
                                dpu_id: Some(dpu2_id.into()),
                                mode: Mode::Set as i32,
                                initiator: rpc::forge::UpdateInitiator::AdminCli as i32,
                                update_firmware: false,
                            },
                        ))
                        .await
                }
                ConcurrentChange::RemoveHostUpdateMarker => {
                    env.api
                        .remove_machine_health_report(tonic::Request::new(
                            rpc::forge::RemoveMachineHealthReportRequest {
                                machine_id: Some(mh.id.into()),
                                source: "host-update".to_string(),
                            },
                        ))
                        .await
                }
            }
        });
        let restart = maybe_done(env.api.trigger_dpu_reprovisioning(tonic::Request::new(
            rpc::forge::DpuReprovisioningRequest {
                machine_id: Some(mh.id.into()),
                mode: Mode::Restart as i32,
                initiator: rpc::forge::UpdateInitiator::AdminCli as i32,
                update_firmware: true,
                ..Default::default()
            },
        )));
        tokio::pin!(writer, restart);
        let operation_started_at = Utc::now();
        let mut writer_pid = None;
        let mut restart_pid = None;
        let mut query_arm = None;
        let readiness = timeout(TimeoutDuration::from_secs(25), async {
            let blocker_pid = if set_writer {
                let pid = tokio::select! {
                    () = &mut writer => return Err(format!("Set passed its D1 gate: {:?}", writer.as_mut().output_mut())),
                    pid = wait_for_blocked_query(&env.pool, gate_pid, "SELECT row_to_json") => pid,
                };
                writer_pid = Some(pid);
                pid
            } else {
                gate_pid
            };
            let (pid, arm) = tokio::select! {
                () = &mut restart => return Err(format!("Restart passed its DPU gate: {:?}", restart.as_mut().output_mut())),
                pid = wait_for_blocked_query(&env.pool, blocker_pid, "reprovisioning_requested IS NOT NULL") => (pid, "pending request row selector"),
                pid = wait_for_blocked_query(&env.pool, blocker_pid, "SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE") => (pid, "isolated non-key DPU wait"),
            };
            restart_pid = Some(pid);
            query_arm = Some(arm);
            Ok::<(), String>(())
        }).await.unwrap_or_else(|error| Err(format!("Restart authorization readiness: {error}")));
        let host_probe = sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(mh.id)
            .execute(host_gate.as_mut().unwrap().as_mut())
            .await;

        let marker_gate_release = if !set_writer {
            Some(host_gate.take().unwrap().rollback().await)
        } else {
            None
        };
        let marker_completed = if !set_writer && readiness.is_ok() {
            Some(timeout(TimeoutDuration::from_secs(25), async {
                tokio::select! {
                    () = &mut writer => Ok(()),
                    () = &mut restart => Err("Restart passed D1 before marker removal".to_string()),
                    pid = wait_for_blocked_query(&env.pool, restart_pid.unwrap(), "UPDATE machines SET health_reports") => {
                        writer_pid = Some(pid);
                        Err("Restart retained H while waiting for D1".to_string())
                    },
                }
            }).await)
        } else {
            None
        };
        let marker_winning = if !set_writer {
            let mut txn = env.db_txn().await;
            let machines = db::machine::find(
                txn.as_mut(),
                db::ObjectFilter::List(&machine_ids),
                MachineSearchConfig::default(),
            )
            .await;
            let release = txn.commit().await;
            Some((machines, release))
        } else {
            None
        };
        let dpu_gate_release = gate.commit().await;
        let set_completed = if set_writer && readiness.is_ok() {
            Some(timeout(TimeoutDuration::from_secs(25), async {
                tokio::select! {
                    () = &mut writer => {},
                    () = &mut restart => { (&mut writer).await; }
                }
                tokio::select! {
                    () = &mut restart => Ok::<_, String>("Restart completed"),
                    pid = wait_for_blocked_query(&env.pool, host_gate_pid, "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => {
                        restart_pid = Some(pid);
                        Ok::<_, String>("fresh host admission wait")
                    },
                }
            }).await)
        } else {
            None
        };
        let set_winning = if set_writer {
            let mut txn = env.db_txn().await;
            let machines = db::machine::find(
                txn.as_mut(),
                db::ObjectFilter::List(&machine_ids),
                MachineSearchConfig::default(),
            )
            .await;
            let release = txn.commit().await;
            Some((machines, release))
        } else {
            None
        };
        let mut key_share_gate = if successful_merge
            && readiness.is_ok()
            && matches!(&set_completed, Some(Ok(Ok("fresh host admission wait"))))
        {
            let mut txn = env.db_txn().await;
            let lock = sqlx::query_scalar::<_, MachineId>(
                "SELECT id FROM machines WHERE id = $1 FOR KEY SHARE NOWAIT",
            )
            .bind(dpu1_id)
            .fetch_one(txn.as_mut())
            .await;
            Some((txn, lock))
        } else {
            None
        };
        let host_gate_release = match host_gate.take() {
            Some(txn) => Some(txn.rollback().await),
            None => None,
        };
        let finished_before_reader_release = {
            let joined = async { tokio::join!(&mut writer, &mut restart) };
            tokio::pin!(joined);
            timeout(TimeoutDuration::from_secs(30), &mut joined)
                .await
                .is_ok()
        };
        let key_share_release = match key_share_gate.take() {
            Some((txn, lock)) => Some((lock, txn.rollback().await)),
            None => None,
        };
        if !finished_before_reader_release {
            for pid in writer_pid.into_iter().chain(restart_pid) {
                sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                    .bind(pid)
                    .fetch_one(&env.pool)
                    .await
                    .expect("cancel only an observed stalled authorization writer");
            }
        }
        timeout(TimeoutDuration::from_secs(30), async {
            tokio::join!(&mut writer, &mut restart);
        })
        .await
        .expect("released or cancelled authorization writers must join");
        let writer_result = writer
            .as_mut()
            .take_output()
            .expect("concurrent writer joined");
        let restart_result = restart.as_mut().take_output().expect("Restart joined");
        let operation_completed_at = Utc::now();
        let mut txn = env.db_txn().await;
        let after = db::machine::find(
            txn.as_mut(),
            db::ObjectFilter::List(&machine_ids),
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|machine| (machine.id, machine))
        .collect::<HashMap<_, _>>();
        txn.commit().await.unwrap();
        tracing::info!(
            scenario,
            ?change,
            ?gate_pid,
            ?writer_pid,
            ?restart_pid,
            ?query_arm,
            ?readiness,
            ?set_completed,
            ?marker_completed,
            ?writer_result,
            ?restart_result,
            "Joined Restart after a concurrent authorization change"
        );

        let checked = catch_unwind(AssertUnwindSafe(|| {
            dpu_gate_release.expect("release the selected DPU gate");
            if let Some(release) = marker_gate_release {
                release.expect("release the marker probe");
            }
            if let Some(release) = host_gate_release {
                release.expect("release the Set observation gate");
            }
            if let Some((lock, release)) = key_share_release {
                lock.expect("the selected DPU reader acquires KEY SHARE");
                release.expect("release the selected DPU reader");
            }
            readiness.expect("observe real Set/Restart or Restart waits");
            assert_eq!(query_arm, Some("isolated non-key DPU wait"), "{scenario}");
            assert!(
                finished_before_reader_release,
                "{scenario}: request did not finish under its compatible reader"
            );
            host_probe.expect("Restart must release H before its isolated DPU wait");
            writer_result.expect("the concurrent public writer succeeds");
            let (winning, release) = if set_writer {
                assert_eq!(
                    set_completed
                        .unwrap()
                        .expect("Set reaches its post-commit barrier")
                        .unwrap(),
                    "fresh host admission wait"
                );
                set_winning.unwrap()
            } else {
                marker_completed
                    .unwrap()
                    .expect("marker removal finishes before D1 releases")
                    .expect("marker removal has no retained H prefix");
                marker_winning.unwrap()
            };
            release.expect("finish the winning request observation");
            let winning = winning
                .expect("reload the concurrent writer's committed values")
                .into_iter()
                .map(|machine| (machine.id, machine))
                .collect::<HashMap<_, _>>();
            if set_writer {
                let request = winning[&machine_ids[1]]
                    .reprovision_requested
                    .as_ref()
                    .unwrap();
                assert!(!request.update_firmware && request.started_at.is_none());
                assert_eq!(
                    request.requested_at,
                    request.restart_reprovision_requested_at
                );
                assert_ne!(request.update_firmware, initial.update_firmware);
            }
            match change {
                ConcurrentChange::HostSetAddsIdleSibling => {
                    assert!(winning[&machine_ids[2]].reprovision_requested.is_some());
                    let error =
                        restart_result.expect_err("Restart must reject the newly eligible sibling");
                    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
                }
                ConcurrentChange::TargetSetReplacesSelectedRequest => {
                    assert!(
                        winning[&machine_ids[2]].reprovision_requested.is_none(),
                        "machine_id must win over the deprecated sibling ID"
                    );
                    restart_result.expect("Some-to-Some replacement remains restartable");
                    let actual = after[&machine_ids[1]]
                        .reprovision_requested
                        .as_ref()
                        .unwrap();
                    let mut expected = winning[&machine_ids[1]]
                        .reprovision_requested
                        .clone()
                        .unwrap();
                    expected.update_firmware = true;
                    expected.restart_reprovision_requested_at =
                        actual.restart_reprovision_requested_at;
                    assert_eq!(actual, &expected);
                    assert!(
                        actual.restart_reprovision_requested_at >= operation_started_at
                            && actual.restart_reprovision_requested_at <= operation_completed_at
                    );
                }
                ConcurrentChange::RemoveHostUpdateMarker => {
                    assert!(
                        !winning[&machine_ids[0]]
                            .health_reports
                            .merges
                            .contains_key("host-update")
                    );
                    let error =
                        restart_result.expect_err("Restart must recheck the current update marker");
                    assert_eq!(error.code(), tonic::Code::InvalidArgument);
                    assert!(error.message().contains("HostUpdateInProgress"));
                }
            }
            for id in machine_ids {
                let old = &before[&id];
                let current = &after[&id];
                if !successful_merge || id != machine_ids[1] {
                    assert_eq!(
                        current.reprovision_requested, winning[&id].reprovision_requested,
                        "{scenario}: {id}"
                    );
                }
                assert_eq!(current.state.value, old.state.value, "{scenario}: {id}");
                assert_eq!(current.state.version, old.state.version, "{scenario}: {id}");
                assert_eq!(
                    current.network_config.value, old.network_config.value,
                    "{scenario}: {id}"
                );
                assert_eq!(
                    current.network_config.version, old.network_config.version,
                    "{scenario}: {id}"
                );
                assert_eq!(
                    serde_json::to_value(&current.history).unwrap(),
                    serde_json::to_value(&old.history).unwrap(),
                    "{scenario}: {id}"
                );
                assert_eq!(
                    serde_json::to_value(&current.controller_state_outcome).unwrap(),
                    serde_json::to_value(&old.controller_state_outcome).unwrap(),
                    "{scenario}: {id}"
                );
            }
            assert!(
                env.redfish_sim
                    .actions_since(&redfish_timepoint)
                    .all_hosts()
                    .is_empty()
            );
        }));
        if let Err(panic) = checked {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("authorization assertion failed");
            failures.push(format!("{scenario}: {message}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// An accepted DPU restart must finish its queued start and receipt against public DPF changes.
#[crate::sqlx_test]
async fn restarted_dpu_request_and_dpf_updates_do_not_deadlock(pool: sqlx::PgPool) {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use carbide_uuid::machine::AsMachineId;
    use db::state_history::{StateHistoryTableId, find_by_object_ids};
    use db::test_support::postgres::wait_for_blocked_query;
    use futures_util::future::maybe_done;
    use model::controller_outcome::PersistentStateHandlerOutcome;
    use model::machine::Dpf;
    use state_controller::CheckApplied;
    use tokio::time::{Duration as TimeoutDuration, timeout};

    use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};
    use crate::tests::machine_states::{
        gate_controller_machine_write, remove_controller_machine_write_gate,
    };

    const RECEIPT: &str = "UPDATE machines SET last_reboot_requested=$1 WHERE id=$2 RETURNING id";
    const DPF_WRITE: &str = "UPDATE machines set dpf = jsonb_set";
    const DPU_WAIT: &str = "SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE";
    const START_WRITE: &str = "SET reprovisioning_requested=";

    Box::pin(async move {
        let mut config = common::api_fixtures::get_config();
        config.dpf.enabled = false;
        let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
        // Finish fixture creation before either host stores a pending request.
        let hosts = [create_managed_host(&env).await, create_managed_host(&env).await];
        let mut failures = Vec::new();
        for ((case, dpf_first), mh) in [
            ("controller receipt before DPF", false),
            ("DPF waits before the controller start stamp", true),
        ].into_iter().zip(&hosts) {
            env.api.modify_dpf_state(tonic::Request::new(rpc::forge::ModifyDpfStateRequest {
                machine_id: Some(mh.id.into()), dpf_enabled: false,
            })).await.expect("fixture should start with DPF disabled");
            mh.mark_machine_for_updates().await;
            mh.dpu().trigger_dpu_reprovisioning(Mode::Set, true).await;
            for _ in 0..3 {
                env.run_machine_state_controller_iteration().await;
            }
            let machine_ids = [mh.id.to_machine_id(), mh.dpu_ids[0].to_machine_id()];
            let mut txn = env.db_txn().await;
            let setup = mh.snapshot(&mut txn).await;
            assert_eq!(setup.managed_state, mh.new_dpu_reprovision_state(ReprovisionState::WaitingForNetworkInstall));
            assert!(!setup.host_snapshot.config.dpf.used_for_ingestion);
            assert!(setup.dpu_snapshots[0].reprovision_requested.as_ref().unwrap().started_at.is_some());
            // Earlier setup receipts must not divert this invocation into verification Wait.
            for (id, receipt) in [
                (machine_ids[0], setup.host_snapshot.status.last_reboot_requested),
                (machine_ids[1], setup.dpu_snapshots[0].status.last_reboot_requested),
            ] {
                if let Some(receipt) = receipt && receipt.restart_verified == Some(false) {
                    db::machine::update_restart_verification_status(&id, receipt, Some(true), 0, &mut txn)
                        .await.expect("verify the captured setup receipt")
                        .check_applied().expect("setup receipt still matches");
                }
            }
            txn.commit().await.unwrap();
            mh.host().trigger_dpu_reprovisioning(Mode::Restart, false).await;
            let mut txn = env.db_txn().await;
            let before = mh.snapshot(&mut txn).await;
            let history_before = find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                .await.unwrap();
            txn.commit().await.unwrap();
            let request = before.dpu_snapshots[0].reprovision_requested.as_ref().unwrap();
            assert!(request.started_at.is_some() && !request.update_firmware);
            assert!(request.restart_reprovision_requested_at > before.host_snapshot.state.version.timestamp());
            assert!(!before.host_snapshot.config.dpf.enabled);
            assert!(!before.dpu_snapshots[0].config.dpf.enabled);
            let dpu_endpoint = before.dpu_snapshots[0].bmc_addr().unwrap().ip().to_string();
            let redfish_timepoint = env.redfish_sim.timepoint();
            let started_at = Utc::now();
            let mut gate = if dpf_first {
                let mut txn = env.db_txn().await;
                db::machine::lock_by_id_for_update(txn.as_mut(), &machine_ids[1]).await.unwrap();
                txn
            } else {
                gate_controller_machine_write(&env.pool, machine_ids[1]).await.unwrap()
            };
            let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(gate.as_mut()).await.unwrap();
            let iteration = maybe_done(env.run_machine_state_controller_iteration_no_requeue());
            let update = maybe_done(env.api.modify_dpf_state(tonic::Request::new(
                rpc::forge::ModifyDpfStateRequest { machine_id: Some(mh.id.into()), dpf_enabled: true },
            )));
            tokio::pin!(iteration, update);
            let mut controller_pid = None;
            let mut dpf_pid = None;
            let mut query_arm = None;
            let readiness = timeout(TimeoutDuration::from_secs(25), async {
                if dpf_first {
                    let (pid, arm) = tokio::select! {
                        () = &mut update => return Err(format!("DPF passed the D gate: {:?}", update.as_mut().output_mut())),
                        pid = wait_for_blocked_query(&env.pool, gate_pid, DPF_WRITE) => (pid, "DPF flag write"),
                        pid = wait_for_blocked_query(&env.pool, gate_pid, DPU_WAIT) => (pid, "isolated DPF wait"),
                    };
                    dpf_pid = Some(pid);
                    query_arm = Some(arm);
                    controller_pid = Some(tokio::select! {
                        () = &mut update => return Err("DPF escaped its D gate".to_string()),
                        () = &mut iteration => return Err("controller did not reach its actual D start write".to_string()),
                        pid = wait_for_blocked_query(&env.pool, pid, START_WRITE) => pid,
                    });
                } else {
                    let pid = tokio::select! {
                        () = &mut iteration => return Err("controller did not reach the accepted D receipt".to_string()),
                        pid = wait_for_blocked_query(&env.pool, gate_pid, RECEIPT) => pid,
                    };
                    controller_pid = Some(pid);
                    let (pid, arm) = tokio::select! {
                        () = &mut iteration => return Err("controller passed its receipt gate".to_string()),
                        () = &mut update => return Err(format!("DPF did not wait for the controller: {:?}", update.as_mut().output_mut())),
                        pid = wait_for_blocked_query(&env.pool, pid, DPF_WRITE) => (pid, "DPF flag write"),
                        pid = wait_for_blocked_query(&env.pool, pid, DPU_WAIT) => (pid, "isolated DPF wait"),
                    };
                    dpf_pid = Some(pid);
                    query_arm = Some(arm);
                }
                Ok::<(), String>(())
            }).await.unwrap_or_else(|error| Err(format!("restart/DPF readiness: {error}")));
            let accepted_actions = env.redfish_sim.actions_since(&redfish_timepoint).for_host(&dpu_endpoint);
            let released_prefix = if !dpf_first {
                let mut txn = env.db_txn().await;
                let lock = sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                    .bind(mh.id).execute(txn.as_mut()).await;
                Some((lock, txn.rollback().await))
            } else { None };
            let mut txn = env.db_txn().await;
            let paused = mh.snapshot(&mut txn).await;
            let paused_history = find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids).await;
            let paused_read_release = txn.commit().await;
            let gate_release = gate.commit().await;
            let timed_out = {
                let joined = async { tokio::join!(&mut iteration, &mut update); };
                tokio::pin!(joined);
                if timeout(TimeoutDuration::from_secs(30), &mut joined).await.is_ok() {
                    false
                } else {
                    let mut cancellations = Vec::new();
                    for pid in controller_pid.into_iter().chain(dpf_pid) {
                        cancellations.push((pid, sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                            .bind(pid).fetch_one(&env.pool).await));
                    }
                    timeout(TimeoutDuration::from_secs(30), &mut joined).await
                        .unwrap_or_else(|error| panic!("{case}: cleanup did not join: {error}; {readiness:?}; {cancellations:?}"));
                    true
                }
            };
            let trigger_cleanup = if dpf_first { Ok(()) }
                else { remove_controller_machine_write_gate(&env.pool).await };
            iteration.as_mut().take_output().expect("controller iteration joined its children");
            let update_result = update.as_mut().take_output().expect("DPF request joined");
            let completed_at = Utc::now();
            let mut txn = env.db_txn().await;
            let after = mh.snapshot(&mut txn).await;
            let history_after = find_by_object_ids(txn.as_mut(), StateHistoryTableId::Machine, &machine_ids).await;
            let final_read_release = txn.commit().await;
            tracing::info!(case, ?gate_pid, ?controller_pid, ?dpf_pid, ?query_arm, ?readiness,
                ?update_result, outcome = ?after.host_snapshot.controller_state_outcome,
                "Joined accepted D restart and public DPF update");
            let checked = catch_unwind(AssertUnwindSafe(|| {
                gate_release.expect("release the actual D/receipt gate");
                trigger_cleanup.expect("remove the joined receipt gate");
                paused_read_release.expect("release the paused observation");
                final_read_release.expect("release the final observation");
                assert!(!timed_out, "{case}: participants exceeded their completion deadline");
                readiness.expect("observe both actual writer queues");
                assert_eq!(query_arm, Some("isolated DPF wait"), "{case}");
                if let Some((lock, release)) = released_prefix {
                    release.expect("release the H ownership probe");
                    lock.expect("DPF releases H before isolated D wait");
                }
                update_result.expect("public DPF update commits");
                assert_eq!(accepted_actions, vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)]);
                assert_eq!(env.redfish_sim.actions_since(&redfish_timepoint).for_host(&dpu_endpoint), accepted_actions);
                assert_eq!(paused.host_snapshot.state.value, before.host_snapshot.state.value);
                assert_eq!(paused.host_snapshot.state.version, before.host_snapshot.state.version);
                assert_eq!(paused.dpu_snapshots[0].reprovision_requested.as_ref(), Some(request));
                assert_eq!(paused.dpu_snapshots[0].status.last_reboot_requested,
                    before.dpu_snapshots[0].status.last_reboot_requested);
                assert_eq!(serde_json::to_value(paused_history.unwrap()).unwrap(), serde_json::to_value(&history_before).unwrap());
                let successor = mh.new_dpu_reprovision_state(ReprovisionState::InstallDpuOs {
                    substate: InstallDpuOsState::InstallingBFB,
                });
                let version = after.host_snapshot.state.version;
                assert_eq!(after.managed_state, successor, "{:?}", after.host_snapshot.controller_state_outcome);
                assert_eq!(version.version_nr(), before.host_snapshot.state.version.version_nr() + 1);
                assert!(matches!(after.host_snapshot.controller_state_outcome,
                    Some(PersistentStateHandlerOutcome::Transition { .. })));
                let dpu = &after.dpu_snapshots[0];
                assert_eq!(dpu.state.value, successor);
                assert_eq!(dpu.state.version, version);
                let actual = dpu.reprovision_requested.as_ref().unwrap();
                let mut expected = (*request).clone();
                expected.started_at = actual.started_at;
                assert_eq!(actual, &expected);
                assert!(actual.started_at.is_some_and(|time| time >= started_at && time <= completed_at));
                let receipt = dpu.status.last_reboot_requested.expect("accepted D restart receipt commits");
                assert_eq!(receipt.mode, MachineLastRebootRequestedMode::Reboot);
                assert_eq!(receipt.restart_verified, Some(false));
                assert_eq!(receipt.verification_attempts, Some(0));
                assert!(receipt.time >= started_at && receipt.time <= completed_at);
                assert_eq!(after.host_snapshot.status.last_reboot_requested, before.host_snapshot.status.last_reboot_requested);
                assert_eq!(after.host_snapshot.config.dpf, Dpf { enabled: true, ..before.host_snapshot.config.dpf.clone() });
                assert_eq!(dpu.config.dpf, Dpf { enabled: true, ..before.dpu_snapshots[0].config.dpf.clone() });
                for (current, prior) in [
                    (&after.host_snapshot.network_config, &before.host_snapshot.network_config),
                    (&dpu.network_config, &before.dpu_snapshots[0].network_config),
                ] {
                    assert_eq!(current.value, prior.value);
                    assert_eq!(current.version, prior.version);
                }
                for id in machine_ids {
                    let history = &history_after.as_ref().unwrap()[&id.to_string()];
                    let prior = &history_before[&id.to_string()];
                    assert_eq!(history.len(), prior.len() + 1);
                    assert_eq!(serde_json::to_value(&history[..prior.len()]).unwrap(), serde_json::to_value(prior).unwrap());
                    assert_eq!(history.last().unwrap().state_version, version);
                    assert_eq!(serde_json::from_str::<ManagedHostState>(&history.last().unwrap().state).unwrap(), successor);
                }
            }));
            if let Err(panic) = checked {
                let message = panic.downcast_ref::<String>().map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("restart/DPF assertion failed");
                failures.push(format!("{case}: {message}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }).await;
}

impl<ID: HostMachineIdSubtypeTrait> TestAnyManagedHost<ID> {
    pub(in crate::tests) async fn mark_machine_for_updates(&self) {
        self.api
            .insert_machine_health_report(tonic::Request::new(
                rpc::forge::InsertMachineHealthReportRequest {
                    machine_id: Some(self.id.into()),
                    health_report_entry: Some(rpc::forge::HealthReportEntry {
                        report: Some(
                            health_report::HealthReport {
                                source: "host-update".to_string(),
                                triggered_by: None,
                                observed_at: None,
                                successes: Vec::new(),
                                alerts: vec![health_report::HealthProbeAlert {
                                    id: "HostUpdateInProgress".parse().unwrap(),
                                    target: None,
                                    in_alert_since: None,
                                    message: "Update".to_string(),
                                    tenant_message: None,
                                    classifications: vec![
                                        health_report::HealthAlertClassification::prevent_allocations(),
                                    ],
                                }],
                            }
                            .into(),
                        ),
                        mode: rpc::forge::HealthReportApplyMode::Merge.into(),
                    }),
                },
            ))
            .await
            .unwrap();
    }
}
