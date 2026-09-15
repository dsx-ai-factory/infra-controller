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

use carbide_machine_controller::handler::MachineStateHandlerBuilder;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineIdSubtypeTrait};
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

use crate::tests::common;
use crate::tests::common::api_fixtures::dpu::create_dpu_machine_in_waiting_for_network_install;
use crate::tests::common::api_fixtures::instance::TestInstance;
use crate::tests::common::api_fixtures::rpc_instance::RpcInstance;
use crate::tests::common::api_fixtures::test_machine::TestMachineInterface;
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

impl TestManagedHost {
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
