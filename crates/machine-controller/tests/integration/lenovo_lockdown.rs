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

use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::machine::{DecommissioningState, DeconfiguringHostState, ManagedHostState};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

fn deconfiguring(step: DeconfiguringHostState) -> ManagedHostState {
    ManagedHostState::Decommissioning {
        decommissioning_state: DecommissioningState::DeconfiguringHost {
            deconfiguring_state: step,
        },
    }
}

async fn setup(pool: PgPool, boot_wait: chrono::Duration) -> (Env, TestManagedHost) {
    setup_with_config(pool, boot_wait, ManagedHostConfig::default()).await
}

async fn setup_with_config(
    pool: PgPool,
    boot_wait: chrono::Duration,
    config: ManagedHostConfig,
) -> (Env, TestManagedHost) {
    let env = Env::builder(pool)
        .configure_runtime(|config| config.machine_state_controller.uefi_boot_wait = boot_wait)
        .build()
        .await;
    let domain = env.test_harness.test_domain().await;
    let network = env.test_harness.network_controller();
    let underlay = network.create_underlay_segment(&domain).await;
    network.create_admin_segment(&domain).await;
    let explorer = env.test_harness.default_test_site_explorer();
    let mh = env
        .test_harness
        .managed_host_builder(&explorer, underlay)
        .with_config(config)
        .build()
        .await
        .0;
    mh.advance_state(deconfiguring(DeconfiguringHostState::DisableLockdown))
        .await;
    (env, mh)
}

#[sqlx_test]
async fn decommission_retries_runtime_unlock_reboot_before_advancing(pool: PgPool) {
    let (mut env, mh) = setup(pool, chrono::Duration::zero()).await;
    env.redfish_sim.set_lockdown_has_boot_latched_controls(true);
    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    let reboot = deconfiguring(DeconfiguringHostState::RebootAfterLockdown);
    assert_eq!(mh.host.machine().await.current_state(), &reboot);

    let previous_request = mh.host.machine().await.status.last_reboot_requested;
    env.redfish_sim.fail_next_power_action(
        libredfish::SystemPowerControl::ForceRestart,
        "transient restart failure",
    );
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &reboot);
    assert_eq!(
        mh.host.machine().await.status.last_reboot_requested,
        previous_request
    );
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    assert_eq!(machine.current_state(), &reboot);
    let requested = machine.status.last_reboot_requested.unwrap();
    assert!(requested.time >= machine.state.version.timestamp());

    // An accepted restart and zero wait interval still do not prove a boot.
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &reboot);
    mh.host.reboot_completed().await;
    env.run_single_iteration().await;
    assert_eq!(
        mh.host.machine().await.current_state(),
        &deconfiguring(DeconfiguringHostState::ClearSuperNicLockdown)
    );
    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert!(actions.contains(&RedfishSimAction::PowerFailed(
        libredfish::SystemPowerControl::ForceRestart
    )));
    assert_eq!(
        actions
            .iter()
            .filter(|action| **action
                == RedfishSimAction::Power(libredfish::SystemPowerControl::ForceRestart))
            .count(),
        1
    );
}

#[sqlx_test]
async fn decommission_waits_for_runtime_unlock_boot_without_restarting_again(pool: PgPool) {
    let (mut env, mh) = setup(pool, chrono::Duration::hours(1)).await;
    env.redfish_sim.set_lockdown_has_boot_latched_controls(true);
    env.run_single_iteration().await;
    env.run_single_iteration().await;
    let requested = mh
        .host
        .machine()
        .await
        .status
        .last_reboot_requested
        .unwrap();
    // Even a reported boot must respect the configured post-request interval.
    mh.host.reboot_completed().await;
    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    assert_eq!(
        machine.current_state(),
        &deconfiguring(DeconfiguringHostState::RebootAfterLockdown)
    );
    // Restart verification can update its bookkeeping while this step waits.
    // The reboot request itself must retain its identity and mode.
    let current = machine.status.last_reboot_requested.unwrap();
    assert_eq!(current.time, requested.time);
    assert_eq!(current.mode, requested.mode);
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty()
    );
}

#[sqlx_test]
async fn decommission_without_runtime_control_preserves_existing_no_reboot_path(pool: PgPool) {
    let (mut env, mh) = setup(pool, chrono::Duration::zero()).await;
    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_eq!(
        mh.host.machine().await.current_state(),
        &deconfiguring(DeconfiguringHostState::ClearSuperNicLockdown)
    );
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty()
    );
}

#[sqlx_test]
async fn no_dpu_lockdown_waits_for_boot_only_when_runtime_controls_exist(pool: PgPool) {
    let mut config = ManagedHostConfig::default();
    config.dpus.clear();
    config.expected_machine_data = Some(model::expected_machine::ExpectedMachineData {
        serial_number: config.serial.clone(),
        dpu_policy: model::expected_machine::HostDpuPolicy::Ignore,
        ..Default::default()
    });
    let (mut env, mh) = setup_with_config(pool, chrono::Duration::hours(1), config).await;
    assert!(mh.dpus.is_empty());
    let waiting = ManagedHostState::HostInit {
        machine_state: model::machine::MachineState::WaitingForLockdown {
            lockdown_info: model::machine::LockdownInfo {
                state: model::machine::LockdownState::TimeWaitForDPUDown,
                mode: model::machine::LockdownMode::Enable,
            },
        },
    };
    mh.advance_state(waiting.clone()).await;
    // A boot reported before this policy's restart request is insufficient.
    mh.host.reboot_completed().await;
    let mut txn = env.test_harness.db_txn().await;
    db::machine::update_reboot_requested_explicit_time(
        &mh.host.id.into(),
        &mut txn,
        model::machine::MachineLastRebootRequestedMode::Reboot,
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Older firmware retains the established completion path without a new boot.
    env.redfish_sim
        .set_lockdown_has_boot_latched_controls(false);
    env.run_single_iteration().await;
    assert!(matches!(
        mh.host.machine().await.current_state(),
        ManagedHostState::BomValidating { .. }
    ));

    mh.advance_state(waiting.clone()).await;
    env.redfish_sim.set_lockdown_has_boot_latched_controls(true);
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &waiting);
    assert!(matches!(
        mh.host.machine().await.controller_state_outcome,
        Some(model::controller_outcome::PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason.contains("Waiting for host boot to apply lockdown")
    ));

    // A powered-off host that never reported the boot is retried after the
    // existing recovery interval, rather than waiting indefinitely.
    let machine = mh.host.machine().await;
    let entered_at = chrono::Utc::now() - chrono::Duration::minutes(2);
    let stale_version: config_version::ConfigVersion = format!(
        "V{}-T{}",
        machine.state.version.version_nr(),
        entered_at.timestamp_micros()
    )
    .parse()
    .unwrap();
    let mut request = machine.status.last_reboot_requested.unwrap();
    request.time = entered_at;
    request.restart_verified = None;
    sqlx::query("UPDATE machines SET controller_state_version = $1, last_reboot_requested = $2, last_reboot_time = NULL WHERE id = $3")
        .bind(stale_version)
        .bind(sqlx::types::Json(request))
        .bind(mh.host.id)
        .execute(&env.test_harness.api().database_connection)
        .await.unwrap();
    env.redfish_sim.set_power_state(
        &machine
            .status
            .bmc_info
            .ip
            .expect("fixture host BMC IP")
            .to_string(),
        libredfish::PowerState::Off,
    );
    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &waiting);
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .contains(&RedfishSimAction::Power(libredfish::SystemPowerControl::On))
    );
    mh.host.reboot_completed().await;
    env.run_single_iteration().await;
    let polling = ManagedHostState::HostInit {
        machine_state: model::machine::MachineState::WaitingForLockdown {
            lockdown_info: model::machine::LockdownInfo {
                state: model::machine::LockdownState::PollingLockdownStatus,
                mode: model::machine::LockdownMode::Enable,
            },
        },
    };
    assert_eq!(mh.host.machine().await.current_state(), &polling);
    // A successful boot cannot substitute for the BMC applying lockdown.
    env.redfish_sim
        .set_lockdown(libredfish::EnabledDisabled::Disabled);
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &polling);
    env.redfish_sim
        .set_lockdown(libredfish::EnabledDisabled::Enabled);
    env.run_single_iteration().await;
    assert!(matches!(
        mh.host.machine().await.current_state(),
        ManagedHostState::BomValidating { .. }
    ));
}

#[sqlx_test]
async fn decommission_powers_on_off_host_and_waits_for_reported_boot(pool: PgPool) {
    let (mut env, mh) = setup(pool, chrono::Duration::zero()).await;
    env.redfish_sim.set_lockdown_has_boot_latched_controls(true);
    env.run_single_iteration().await;
    let reboot = deconfiguring(DeconfiguringHostState::RebootAfterLockdown);
    let machine = mh.host.machine().await;
    let bmc_ip = machine
        .status
        .bmc_info
        .ip
        .expect("fixture host BMC IP")
        .to_string();
    env.redfish_sim
        .set_power_state(&bmc_ip, libredfish::PowerState::Off);
    env.redfish_sim
        .fail_next_power_action(libredfish::SystemPowerControl::On, "power-on rejected");
    let previous_request = machine.status.last_reboot_requested;
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &reboot);
    assert_eq!(
        mh.host.machine().await.status.last_reboot_requested,
        previous_request
    );

    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    assert_eq!(machine.current_state(), &reboot);
    let request = machine.status.last_reboot_requested.unwrap();
    assert_eq!(
        request.mode,
        model::machine::MachineLastRebootRequestedMode::PowerOn
    );
    assert!(request.time >= machine.state.version.timestamp());
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &reboot);

    // Missing boot reports use the existing bounded retry instead of advancing.
    let entered_at = chrono::Utc::now() - chrono::Duration::minutes(2);
    let stale_version: config_version::ConfigVersion = format!(
        "V{}-T{}",
        machine.state.version.version_nr(),
        entered_at.timestamp_micros()
    )
    .parse()
    .unwrap();
    let mut stale_request = request;
    stale_request.time = entered_at;
    stale_request.restart_verified = None;
    sqlx::query("UPDATE machines SET controller_state_version = $1, last_reboot_requested = $2 WHERE id = $3")
        .bind(stale_version).bind(sqlx::types::Json(stale_request)).bind(mh.host.id)
        .execute(&env.test_harness.api().database_connection).await.unwrap();
    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &reboot);
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .contains(&RedfishSimAction::Power(
                libredfish::SystemPowerControl::ForceRestart
            ))
    );
    mh.host.reboot_completed().await;
    env.run_single_iteration().await;
    assert_eq!(
        mh.host.machine().await.current_state(),
        &deconfiguring(DeconfiguringHostState::ClearSuperNicLockdown)
    );
}
