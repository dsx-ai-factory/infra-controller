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

mod lifecycle_request_admission {
    use std::time::Duration;

    use carbide_uuid::machine::{HostMachineId, MachineIdSource, MachineInterfaceId, MachineType};
    use db::test_support::postgres::wait_for_blocked_query;
    use model::machine::machine_search_config::MachineSearchConfig;
    use tokio::time::timeout;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const COMPLETION_TIMEOUT: Duration = Duration::from_secs(25);
    const MACHINE_WAIT: &str = "SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE";

    struct HostWithDpus {
        host: HostMachineId,
        dpus: [DpuMachineId; 2],
        spare: DpuMachineId,
        interfaces: [MachineInterfaceId; 2],
    }

    // A `Ready` host with two data interfaces, a spare DPU, and the `host-update`
    // marker required by `Restart`. Raw rows let the tests change admission
    // dependencies without advancing controllers.
    async fn seed_host(
        pool: &sqlx::PgPool,
        seed: u8,
    ) -> Result<HostWithDpus, Box<dyn std::error::Error>> {
        let id = |offset, kind| {
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [seed + offset; 32],
                kind,
            )
        };
        let host = HostMachineId::try_from(id(0, MachineType::Host))?;
        let mut dpus = [
            DpuMachineId::try_from(id(0, MachineType::Dpu))?,
            DpuMachineId::try_from(id(1, MachineType::Dpu))?,
        ];
        dpus.sort_unstable();
        let spare = DpuMachineId::try_from(id(2, MachineType::Dpu))?;
        let mut txn = pool.begin().await?;
        // Oppose heap/snapshot order to the native order used for admission.
        for machine_id in [host.into(), dpus[1].into(), dpus[0].into(), spare.into()] {
            db::machine::create(
                txn.as_mut(),
                None,
                &machine_id,
                ManagedHostState::Ready,
                None,
                model::machine::CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
        }
        let segment: carbide_uuid::network::NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version) VALUES ($1, 'V1-T0') RETURNING id",
        )
        .bind(format!("lifecycle-admission-{seed}"))
        .fetch_one(txn.as_mut())
        .await?;
        let mut interfaces = Vec::new();
        for (index, dpu) in dpus.iter().enumerate() {
            let link = sqlx::query_scalar(
                "INSERT INTO machine_interfaces
                    (machine_id, attached_dpu_machine_id, association_type, segment_id,
                     mac_address, primary_interface, hostname, interface_type)
                 VALUES ($1, $2, 'Machine', $3, $4::macaddr, $5, 'admission', 'Data')
                 RETURNING id",
            )
            .bind(host)
            .bind(dpu)
            .bind(segment)
            .bind(format!("02:89:07:{seed:02x}:00:{index:02x}"))
            .bind(index == 1)
            .fetch_one(txn.as_mut())
            .await?;
            interfaces.push(link);
        }
        let report = health_report::HealthReport {
            source: "host-update".to_string(),
            triggered_by: None,
            observed_at: None,
            successes: Vec::new(),
            alerts: vec![health_report::HealthProbeAlert {
                id: "HostUpdateInProgress".parse()?,
                target: None,
                in_alert_since: None,
                message: "Update".to_string(),
                tenant_message: None,
                classifications: vec![
                    health_report::HealthAlertClassification::prevent_allocations(),
                ],
            }],
        };
        db::machine::insert_health_report(
            txn.as_mut(),
            &host.into(),
            health_report::HealthReportApplyMode::Merge,
            &report,
            false,
        )
        .await?;
        txn.commit().await?;
        Ok(HostWithDpus {
            host,
            dpus,
            spare,
            interfaces: [interfaces[0], interfaces[1]],
        })
    }

    fn dpf_request(
        host: HostMachineId,
        dpf_enabled: bool,
    ) -> tonic::Request<rpc::forge::ModifyDpfStateRequest> {
        tonic::Request::new(rpc::forge::ModifyDpfStateRequest {
            machine_id: Some(host),
            dpf_enabled,
        })
    }

    fn reprovision_request(
        id: MachineId,
        mode: Mode,
    ) -> tonic::Request<rpc::forge::DpuReprovisioningRequest> {
        tonic::Request::new(rpc::forge::DpuReprovisioningRequest {
            machine_id: Some(id),
            mode: mode as i32,
            update_firmware: false,
            ..Default::default()
        })
    }

    // Walk the retained direct database error to its SQLx diagnostic.
    fn sqlx_error_from_status(status: &tonic::Status) -> Option<&sqlx::Error> {
        let mut source: &(dyn std::error::Error + 'static) = status;
        loop {
            if let Some(error) = source.downcast_ref::<sqlx::Error>() {
                return Some(error);
            }
            source = source.source()?;
        }
    }

    async fn enabled_flags(
        pool: &sqlx::PgPool,
        ids: &[MachineId],
    ) -> Result<Vec<bool>, sqlx::Error> {
        sqlx::query_scalar(
            r#"SELECT (dpf->>'enabled')::boolean FROM machines
               WHERE id = ANY($1) ORDER BY id COLLATE "C""#,
        )
        .bind(ids)
        .fetch_all(pool)
        .await
    }

    // Ingestion can change before initial admission or during an isolated DPU
    // wait after a rejected attempt. Both paths must use the new marker.
    #[crate::sqlx_test]
    async fn dpf_disable_rechecks_ingestion_after_waiting(pool: sqlx::PgPool) -> TestResult {
        #[derive(Clone, Copy, Debug)]
        enum Wait {
            Host,
            LaterDpu,
        }
        let env = create_test_env(pool).await;
        for (seed, wait) in [(10, Wait::Host), (13, Wait::LaterDpu)] {
            let host = seed_host(&env.pool, seed).await?;
            let mut gate = env.pool.begin().await?;
            let (gate_id, wait_query) = match wait {
                Wait::Host => (
                    host.host.into(),
                    "SELECT id FROM machines WHERE id = $1 FOR UPDATE",
                ),
                Wait::LaterDpu => (host.dpus[1].into(), MACHINE_WAIT),
            };
            db::machine::lock_by_id_for_update(gate.as_mut(), &gate_id).await?;
            let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(gate.as_mut())
                .await?;
            let update = env.api.modify_dpf_state(dpf_request(host.host, false));
            tokio::pin!(update);
            tokio::select! {
                result = &mut update => panic!("{wait:?}: disable passed the gate: {result:?}"),
                _ = wait_for_blocked_query(&env.pool, pid, wait_query) => {}
            }
            sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                .bind(host.host)
                .fetch_one(gate.as_mut())
                .await?;
            db::machine::mark_machine_ingestion_done_with_dpf(gate.as_mut(), &host.host.into())
                .await?;
            gate.commit().await?;
            let error = timeout(COMPLETION_TIMEOUT, update)
                .await?
                .expect_err("ingestion forbids disable");
            assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{wait:?}");
            assert!(
                error.message().contains("machine was ingested via DPF"),
                "{wait:?}"
            );
            assert_eq!(
                enabled_flags(
                    &env.pool,
                    &[host.host.into(), host.dpus[0].into(), host.dpus[1].into()]
                )
                .await?,
                [true; 3],
                "{wait:?}"
            );
        }
        Ok(())
    }

    // Rebind an attachment while the RPC waits for the later DPU in isolation.
    // The retry must update the spare and leave the detached DPU alone.
    #[crate::sqlx_test]
    async fn dpf_late_contention_releases_prefix_and_reloads_cohort(
        pool: sqlx::PgPool,
    ) -> TestResult {
        let env = create_test_env(pool).await;
        let host = seed_host(&env.pool, 20).await?;
        let independent = seed_host(&env.pool, 30).await?;
        let mut first_gate = env.pool.begin().await?;
        db::machine::lock_by_id_for_update(first_gate.as_mut(), &host.dpus[0].into()).await?;
        let first_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(first_gate.as_mut())
            .await?;
        let mut later_gate = env.pool.begin().await?;
        db::machine::lock_by_id_for_update(later_gate.as_mut(), &host.dpus[1].into()).await?;
        let later_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(later_gate.as_mut())
            .await?;
        let update = env.api.modify_dpf_state(dpf_request(host.host, false));
        tokio::pin!(update);
        tokio::select! {
            result = &mut update => panic!("DPF passed the first native DPU gate: {result:?}"),
            _ = wait_for_blocked_query(&env.pool, first_pid, MACHINE_WAIT) => {}
        }
        first_gate.commit().await?;
        tokio::select! {
            result = &mut update => panic!("DPF passed the later DPU gate: {result:?}"),
            _ = wait_for_blocked_query(&env.pool, later_pid, MACHINE_WAIT) => {}
        }
        // The isolated later-DPU wait must retain none of the admitted prefix.
        sqlx::query("SELECT id FROM machines WHERE id = ANY($1) FOR UPDATE NOWAIT")
            .bind([MachineId::from(host.host), host.dpus[0].into()].as_slice())
            .fetch_all(later_gate.as_mut())
            .await?;
        let locked_links: Vec<MachineInterfaceId> = sqlx::query_scalar(
            "SELECT id FROM machine_interfaces WHERE machine_id = $1 FOR UPDATE NOWAIT",
        )
        .bind(host.host)
        .fetch_all(later_gate.as_mut())
        .await?;
        assert_eq!(locked_links.len(), host.interfaces.len());
        assert_eq!(
            enabled_flags(
                &env.pool,
                &[host.host.into(), host.dpus[0].into(), host.dpus[1].into()]
            )
            .await?,
            [true; 3]
        );
        timeout(
            COMPLETION_TIMEOUT,
            env.api
                .modify_dpf_state(dpf_request(independent.host, false)),
        )
        .await??;
        assert_eq!(
            enabled_flags(
                &env.pool,
                &[
                    independent.host.into(),
                    independent.dpus[0].into(),
                    independent.dpus[1].into()
                ]
            )
            .await?,
            [false; 3]
        );
        db::machine_interface::associate_interface_with_dpu_machine(
            &host.interfaces[1],
            &host.spare.into(),
            later_gate.as_mut(),
        )
        .await?;
        later_gate.commit().await?;
        timeout(COMPLETION_TIMEOUT, update).await??;
        assert_eq!(
            enabled_flags(
                &env.pool,
                &[host.host.into(), host.dpus[0].into(), host.spare.into()]
            )
            .await?,
            [false; 3]
        );
        assert_eq!(
            enabled_flags(&env.pool, &[host.dpus[1].into()]).await?,
            [true]
        );
        Ok(())
    }

    // A BMC interface becomes a data attachment while the RPC waits. Persisted
    // flags identify whether the retry used the changed classification.
    #[crate::sqlx_test]
    async fn dpf_reloads_bmc_classification_after_interface_wait(pool: sqlx::PgPool) -> TestResult {
        let env = create_test_env(pool).await;
        let host = seed_host(&env.pool, 70).await?;
        sqlx::query("UPDATE machine_interfaces SET interface_type = 'Bmc' WHERE id = $1")
            .bind(host.interfaces[1])
            .execute(&env.pool)
            .await?;
        let mut gate = env.pool.begin().await?;
        sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
            .bind(host.interfaces[1])
            .fetch_one(gate.as_mut())
            .await?;
        let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await?;
        let update = env.api.modify_dpf_state(dpf_request(host.host, false));
        tokio::pin!(update);
        tokio::select! {
            result = &mut update => panic!("DPF failed to admit the BMC interface: {result:?}"),
            _ = wait_for_blocked_query(&env.pool, pid, "SELECT id FROM machine_interfaces WHERE id = $1 FOR NO KEY UPDATE") => {}
        }
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(host.host)
            .fetch_one(gate.as_mut())
            .await?;
        sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(host.interfaces[0])
            .fetch_one(gate.as_mut())
            .await?;
        sqlx::query("UPDATE machine_interfaces SET interface_type = 'Data', attached_dpu_machine_id = $2 WHERE id = $1")
            .bind(host.interfaces[1]).bind(host.spare).execute(gate.as_mut()).await?;
        gate.commit().await?;
        timeout(COMPLETION_TIMEOUT, update).await??;
        assert_eq!(
            enabled_flags(
                &env.pool,
                &[host.host.into(), host.dpus[0].into(), host.spare.into()]
            )
            .await?,
            [false; 3]
        );
        assert_eq!(
            enabled_flags(&env.pool, &[host.dpus[1].into()]).await?,
            [true]
        );
        Ok(())
    }

    // Pool admission, row waits, retries, and an in-flight commit consume one
    // caller budget. Wait for backend cleanup before checking state and recovery.
    #[crate::sqlx_test]
    async fn dpf_timeout_bounds_pool_waits_and_retries(pool: sqlx::PgPool) -> TestResult {
        #[derive(Clone, Copy, Debug)]
        enum Wait {
            Host,
            Pool,
            Retry,
            Commit,
        }
        async fn wait_until_idle(pool: &sqlx::PgPool, pid: i32) -> TestResult {
            timeout(COMPLETION_TIMEOUT, async {
                loop {
                    let idle: bool = sqlx::query_scalar(
                        "SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity
                         WHERE pid = $1 AND (state <> 'idle' OR xact_start IS NOT NULL))",
                    )
                    .bind(pid)
                    .fetch_one(pool)
                    .await?;
                    if idle {
                        return Ok::<_, sqlx::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            Ok(())
        }
        let control = pool.clone();
        let env = create_test_env(pool).await;
        for (seed, wait, budget) in [
            (40, Wait::Host, Duration::from_secs(2)),
            (43, Wait::Pool, Duration::from_millis(200)),
            (46, Wait::Retry, Duration::from_secs(3)),
            (49, Wait::Commit, Duration::from_secs(2)),
        ] {
            let host = seed_host(&env.pool, seed).await?;
            let mut gate = None;
            let mut gate_pid = None;
            let mut held = Vec::new();
            if matches!(wait, Wait::Pool) {
                for _ in 0..env.api.pg_pool().options().get_max_connections() {
                    held.push(timeout(COMPLETION_TIMEOUT, env.api.pg_pool().acquire()).await??);
                }
                assert_eq!(env.api.pg_pool().num_idle(), 0);
            } else if matches!(wait, Wait::Commit) {
                // Deferred trigger work executes in COMMIT, after all flag writes.
                sqlx::raw_sql(
                    "CREATE TABLE test_dpf_commit_target (id text PRIMARY KEY);
                     CREATE FUNCTION gate_test_dpf_commit() RETURNS trigger AS $$
                     BEGIN
                         IF EXISTS(SELECT 1 FROM test_dpf_commit_target WHERE id = NEW.id) THEN
                             PERFORM pg_advisory_xact_lock(8907002);
                         END IF;
                         RETURN NEW;
                     END;
                     $$ LANGUAGE plpgsql;
                     CREATE CONSTRAINT TRIGGER gate_test_dpf_commit AFTER UPDATE ON machines
                     DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
                     WHEN (NEW.dpf IS DISTINCT FROM OLD.dpf)
                     EXECUTE FUNCTION gate_test_dpf_commit()",
                )
                .execute(&env.pool)
                .await?;
                sqlx::query("INSERT INTO test_dpf_commit_target (id) VALUES ($1)")
                    .bind(host.dpus[1])
                    .execute(&env.pool)
                    .await?;
                let mut txn = env.pool.begin().await?;
                sqlx::query("SELECT pg_advisory_xact_lock(8907002)")
                    .execute(txn.as_mut())
                    .await?;
                gate_pid = Some(
                    sqlx::query_scalar("SELECT pg_backend_pid()")
                        .fetch_one(txn.as_mut())
                        .await?,
                );
                gate = Some(txn);
            } else {
                let mut txn = env.pool.begin().await?;
                let id = if matches!(wait, Wait::Host) {
                    host.host.into()
                } else {
                    host.dpus[1].into()
                };
                db::machine::lock_by_id_for_update(txn.as_mut(), &id).await?;
                gate_pid = Some(
                    sqlx::query_scalar("SELECT pg_backend_pid()")
                        .fetch_one(txn.as_mut())
                        .await?,
                );
                gate = Some(txn);
            }
            let mut request = dpf_request(host.host, false);
            request.set_timeout(budget);
            let started = tokio::time::Instant::now();
            let update = env.api.modify_dpf_state(request);
            tokio::pin!(update);
            let mut waiter = None;
            if let Some(pid) = gate_pid {
                let query = match wait {
                    Wait::Host => "SELECT id FROM machines WHERE id = $1 FOR UPDATE",
                    Wait::Commit => "COMMIT",
                    Wait::Retry => MACHINE_WAIT,
                    Wait::Pool => unreachable!(),
                };
                waiter = Some(tokio::select! {
                    result = &mut update => panic!("{wait:?}: request never reached its lock wait: {result:?}"),
                    pid = wait_for_blocked_query(&control, pid, query) => pid,
                });
            }
            if matches!(wait, Wait::Retry) {
                let mut host_gate = env.pool.begin().await?;
                sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                    .bind(host.host)
                    .fetch_one(host_gate.as_mut())
                    .await?;
                let host_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(host_gate.as_mut())
                    .await?;
                tokio::time::sleep_until(started + Duration::from_secs(2)).await;
                gate.take().unwrap().commit().await?;
                waiter = Some(tokio::select! {
                    result = &mut update => panic!("retry did not reach the new host wait: {result:?}"),
                    pid = wait_for_blocked_query(&control, host_pid, "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => pid,
                });
                gate = Some(host_gate);
            }
            let result = if matches!(wait, Wait::Retry) {
                // Resetting the three-second budget on retry would expire after
                // five seconds; the one original budget expires after three.
                tokio::time::timeout_at(started + Duration::from_secs(4), &mut update).await?
            } else {
                timeout(COMPLETION_TIMEOUT, &mut update).await?
            };
            let error = result.expect_err("the finite caller budget expires");
            assert_eq!(error.code(), tonic::Code::DeadlineExceeded, "{wait:?}");
            assert_eq!(
                error.metadata().get("nico-error-code").unwrap(),
                "NICO-API-504"
            );
            if let Some(txn) = gate {
                txn.rollback().await?;
            }
            drop(held);
            if let Some(pid) = waiter {
                wait_until_idle(&control, pid).await?;
            }
            if matches!(wait, Wait::Commit) {
                sqlx::raw_sql("DROP TRIGGER gate_test_dpf_commit ON machines; DROP FUNCTION gate_test_dpf_commit(); DROP TABLE test_dpf_commit_target")
                    .execute(&env.pool).await?;
            }
            let ids = [
                MachineId::from(host.host),
                host.dpus[0].into(),
                host.dpus[1].into(),
            ];
            let flags = enabled_flags(&env.pool, &ids).await?;
            if matches!(wait, Wait::Commit) {
                // A commit already sent to PostgreSQL can succeed despite the
                // deadline response. Either complete outcome is allowed.
                assert!(flags == [true; 3] || flags == [false; 3], "{flags:?}");
            } else {
                assert_eq!(flags, [true; 3], "{wait:?}");
            }
            timeout(
                COMPLETION_TIMEOUT,
                env.api.modify_dpf_state(dpf_request(host.host, false)),
            )
            .await??;
            assert_eq!(
                enabled_flags(&env.pool, &ids).await?,
                [false; 3],
                "{wait:?}: a fresh update recovers"
            );
        }
        Ok(())
    }

    // Change requests or their authorization while `Restart` waits in isolation.
    // Comparing the selected request detects stale writes and requires replacement
    // fields to survive its merge. The database test checks multi-row atomicity.
    #[crate::sqlx_test]
    async fn restart_revalidates_requests_authorization_and_attachments(
        pool: sqlx::PgPool,
    ) -> TestResult {
        #[derive(Debug, Clone, Copy)]
        enum Change {
            Clear,
            Expand,
            Replace,
            RemoveMarker,
            Rebind,
            RebindDuringInterfaceWait,
        }
        let env = create_test_env(pool).await;
        for (seed, change, expected_code) in [
            (50, Change::Clear, Some(tonic::Code::FailedPrecondition)),
            (53, Change::Expand, Some(tonic::Code::FailedPrecondition)),
            (56, Change::Replace, None),
            (59, Change::RemoveMarker, Some(tonic::Code::InvalidArgument)),
            (62, Change::Rebind, Some(tonic::Code::FailedPrecondition)),
            (
                65,
                Change::RebindDuringInterfaceWait,
                Some(tonic::Code::FailedPrecondition),
            ),
        ] {
            let host = seed_host(&env.pool, seed).await?;
            let mut set_request = reprovision_request(host.dpus[0].into(), Mode::Set);
            set_request.get_mut().dpu_id = Some(host.dpus[1].into());
            env.api.trigger_dpu_reprovisioning(set_request).await?;
            let mut gate = env.pool.begin().await?;
            let wait_query = if matches!(change, Change::RebindDuringInterfaceWait) {
                sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
                    .bind(host.interfaces[1])
                    .fetch_one(gate.as_mut())
                    .await?;
                "SELECT id FROM machine_interfaces WHERE id = $1 FOR NO KEY UPDATE"
            } else {
                db::machine::lock_by_id_for_update(gate.as_mut(), &host.dpus[1].into()).await?;
                MACHINE_WAIT
            };
            let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(gate.as_mut())
                .await?;
            // One row first proves that Restart reaches the lower native DPU
            // before contending on the higher primary DPU below.
            let mut first_gate = if matches!(change, Change::Clear) {
                let mut first = env.pool.begin().await?;
                db::machine::lock_by_id_for_update(first.as_mut(), &host.dpus[0].into()).await?;
                Some(first)
            } else {
                None
            };
            let first_pid: Option<i32> = if let Some(first) = &mut first_gate {
                Some(
                    sqlx::query_scalar("SELECT pg_backend_pid()")
                        .fetch_one(first.as_mut())
                        .await?,
                )
            } else {
                None
            };
            let restart = env
                .api
                .trigger_dpu_reprovisioning(reprovision_request(host.host.into(), Mode::Restart));
            tokio::pin!(restart);
            if let Some(first_pid) = first_pid {
                tokio::select! {
                    result = &mut restart => panic!("Restart missed the first native DPU: {result:?}"),
                    _ = wait_for_blocked_query(&env.pool, first_pid, MACHINE_WAIT) => {}
                }
                first_gate.take().unwrap().commit().await?;
            }
            tokio::select! {
                result = &mut restart => panic!("{change:?}: Restart passed the DPU gate: {result:?}"),
                _ = wait_for_blocked_query(&env.pool, pid, wait_query) => {}
            }
            // These mutations must be possible while Restart waits in isolation.
            sqlx::query("SELECT id FROM machines WHERE id = ANY($1) FOR UPDATE NOWAIT")
                .bind([MachineId::from(host.host), host.dpus[0].into()].as_slice())
                .fetch_all(gate.as_mut())
                .await?;
            let locked_interfaces: Vec<MachineInterfaceId> = sqlx::query_scalar(
                "SELECT id FROM machine_interfaces WHERE machine_id = $1 FOR UPDATE NOWAIT",
            )
            .bind(host.host)
            .fetch_all(gate.as_mut())
            .await?;
            assert_eq!(locked_interfaces.len(), host.interfaces.len(), "{change:?}");
            match change {
                Change::Clear => {
                    db::machine::clear_dpu_reprovisioning_request(
                        gate.as_mut(),
                        &host.dpus[0],
                        true,
                    )
                    .await?
                }
                Change::Expand => {
                    db::machine::trigger_dpu_reprovisioning_request(
                        &host.dpus[1],
                        gate.as_mut(),
                        "expanded",
                        true,
                    )
                    .await?
                }
                Change::Replace => {
                    db::machine::trigger_dpu_reprovisioning_request(
                        &host.dpus[0],
                        gate.as_mut(),
                        "replacement",
                        true,
                    )
                    .await?;
                }
                Change::RemoveMarker => {
                    db::machine::remove_health_report(
                        gate.as_mut(),
                        &host.host.into(),
                        health_report::HealthReportApplyMode::Merge,
                        "host-update",
                    )
                    .await?
                }
                Change::Rebind | Change::RebindDuringInterfaceWait => {
                    db::machine_interface::associate_interface_with_dpu_machine(
                        &host.interfaces[1],
                        &host.spare.into(),
                        gate.as_mut(),
                    )
                    .await?;
                }
            }
            let mut expected = Vec::new();
            for id in host.dpus {
                expected.push(
                    db::machine::find_one(gate.as_mut(), &id, MachineSearchConfig::default())
                        .await?
                        .unwrap()
                        .reprovision_requested,
                );
            }
            gate.commit().await?;
            let result = timeout(COMPLETION_TIMEOUT, restart).await?;
            assert_eq!(
                result.as_ref().err().map(tonic::Status::code),
                expected_code,
                "{change:?}"
            );
            if let Err(error) = &result {
                let expected = match change {
                    Change::Clear | Change::Expand => "request selection changed",
                    Change::Rebind | Change::RebindDuringInterfaceWait => "attached DPUs changed",
                    Change::RemoveMarker => "HostUpdateInProgress",
                    Change::Replace => unreachable!(),
                };
                assert!(error.message().contains(expected), "{change:?}: {error}");
            }
            let mut check = env.pool.begin().await?;
            let mut actual = Vec::new();
            for id in host.dpus {
                actual.push(
                    db::machine::find_one(check.as_mut(), &id, MachineSearchConfig::default())
                        .await?
                        .unwrap()
                        .reprovision_requested,
                );
            }
            check.rollback().await?;
            if expected_code.is_none() {
                let replacement = expected[0].as_mut().unwrap();
                assert_eq!(replacement.initiator, "replacement");
                replacement.update_firmware = false;
                replacement.restart_reprovision_requested_at =
                    actual[0].as_ref().unwrap().restart_reprovision_requested_at;
            }
            assert_eq!(
                actual, expected,
                "{change:?}: preserve exact concurrent requests except the accepted Restart merge"
            );
        }
        Ok(())
    }

    // Fail the final DPU write after the host and first DPU update. A later
    // accepted enable must preserve every ingestion marker as well.
    #[crate::sqlx_test]
    async fn dpf_flag_updates_are_atomic_and_preserve_ingestion(pool: sqlx::PgPool) -> TestResult {
        let env = create_test_env(pool).await;
        let host = seed_host(&env.pool, 80).await?;
        let ids = [
            MachineId::from(host.host),
            host.dpus[0].into(),
            host.dpus[1].into(),
        ];
        let before: Vec<serde_json::Value> = sqlx::query_scalar(
            r#"SELECT dpf FROM machines WHERE id = ANY($1)
               ORDER BY id COLLATE "C""#,
        )
        .bind(ids.as_slice())
        .fetch_all(&env.pool)
        .await?;
        sqlx::raw_sql(
            "CREATE TABLE test_dpf_failure_target (id text PRIMARY KEY, fail boolean NOT NULL);
             CREATE FUNCTION reject_test_dpf_write() RETURNS trigger AS $$
             BEGIN
                 IF EXISTS(SELECT 1 FROM test_dpf_failure_target WHERE id = NEW.id AND fail) THEN
                     IF EXISTS(SELECT 1 FROM machines m
                               JOIN test_dpf_failure_target target ON target.id = m.id
                               WHERE NOT target.fail AND m.dpf->>'enabled' <> 'false') THEN
                         RAISE EXCEPTION 'test fault did not follow earlier flag writes';
                     END IF;
                     RAISE EXCEPTION 'test final DPU flag failure' USING ERRCODE = '23514';
                 END IF;
                 RETURN NEW;
             END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER reject_test_dpf_write BEFORE UPDATE OF dpf ON machines
             FOR EACH ROW WHEN (NEW.dpf->>'enabled' = 'false')
             EXECUTE FUNCTION reject_test_dpf_write()",
        )
        .execute(&env.pool)
        .await?;
        sqlx::query("INSERT INTO test_dpf_failure_target (id, fail) VALUES ($1, true), ($2, false), ($3, false)")
            .bind(host.dpus[1])
            .bind(host.host)
            .bind(host.dpus[0])
            .execute(&env.pool)
            .await?;
        let error = timeout(
            COMPLETION_TIMEOUT,
            env.api.modify_dpf_state(dpf_request(host.host, false)),
        )
        .await?
        .expect_err("the final DPU write must fail");
        assert_eq!(
            sqlx_error_from_status(&error)
                .and_then(sqlx::Error::as_database_error)
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23514"),
            "{error:?}"
        );
        let after: Vec<serde_json::Value> = sqlx::query_scalar(
            r#"SELECT dpf FROM machines WHERE id = ANY($1)
               ORDER BY id COLLATE "C""#,
        )
        .bind(ids.as_slice())
        .fetch_all(&env.pool)
        .await?;
        assert_eq!(
            after, before,
            "a later DPU failure must roll back every flag write"
        );
        sqlx::raw_sql("DROP TRIGGER reject_test_dpf_write ON machines; DROP FUNCTION reject_test_dpf_write(); DROP TABLE test_dpf_failure_target")
            .execute(&env.pool).await?;
        timeout(
            COMPLETION_TIMEOUT,
            env.api.modify_dpf_state(dpf_request(host.host, false)),
        )
        .await??;
        assert_eq!(enabled_flags(&env.pool, &ids).await?, [false; 3]);
        let mut marker = env.pool.begin().await?;
        for id in [host.host.into(), host.dpus[1].into()] {
            db::machine::mark_machine_ingestion_done_with_dpf(marker.as_mut(), &id).await?;
        }
        marker.commit().await?;
        let before: Vec<serde_json::Value> = sqlx::query_scalar(
            r#"SELECT dpf FROM machines WHERE id = ANY($1)
               ORDER BY id COLLATE "C""#,
        )
        .bind(ids.as_slice())
        .fetch_all(&env.pool)
        .await?;
        timeout(
            COMPLETION_TIMEOUT,
            env.api.modify_dpf_state(dpf_request(host.host, true)),
        )
        .await??;
        let after: Vec<serde_json::Value> = sqlx::query_scalar(
            r#"SELECT dpf FROM machines WHERE id = ANY($1)
               ORDER BY id COLLATE "C""#,
        )
        .bind(ids.as_slice())
        .fetch_all(&env.pool)
        .await?;
        let expected: Vec<_> = before
            .into_iter()
            .map(|mut settings| {
                settings["enabled"] = serde_json::Value::Bool(true);
                settings
            })
            .collect();
        assert_eq!(
            after, expected,
            "enable changes every flag from false to true and preserves other settings"
        );
        Ok(())
    }

    // Approval retains primary-first snapshot order. Host Clear must wait for
    // that primary without holding the lower secondary against approval.
    #[crate::sqlx_test]
    async fn host_clear_preserves_approval_lock_order(pool: sqlx::PgPool) -> TestResult {
        let env = create_test_env(pool).await;
        let host = seed_host(&env.pool, 90).await?;
        env.api
            .trigger_dpu_reprovisioning(reprovision_request(host.host.into(), Mode::Set))
            .await?;
        let mut approval = env.pool.begin().await?;
        db::machine::approve_dpu_reprovision_request(&host.dpus[1], approval.as_mut()).await?;
        let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(approval.as_mut())
            .await?;
        let clear = env
            .api
            .trigger_dpu_reprovisioning(reprovision_request(host.host.into(), Mode::Clear));
        tokio::pin!(clear);
        tokio::select! {
            result = &mut clear => panic!("Clear passed the primary approval gate: {result:?}"),
            _ = wait_for_blocked_query(&env.pool, pid, "UPDATE machines SET reprovisioning_requested=NULL") => {}
        }
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(host.dpus[0])
            .fetch_one(approval.as_mut())
            .await?;
        db::machine::approve_dpu_reprovision_request(&host.dpus[0], approval.as_mut()).await?;
        approval.commit().await?;
        timeout(COMPLETION_TIMEOUT, clear).await??;
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM machines WHERE id = ANY($1)
             AND reprovisioning_requested IS NOT NULL",
        )
        .bind(host.dpus.as_slice())
        .fetch_one(&env.pool)
        .await?;
        assert_eq!(pending, 0);
        Ok(())
    }
}
