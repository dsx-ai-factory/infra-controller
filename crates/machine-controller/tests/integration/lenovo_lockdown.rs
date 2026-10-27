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
        .with_config(ManagedHostConfig::default())
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
    env.redfish_sim.set_lockdown_requires_host_reboot(true);
    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    let reboot = deconfiguring(DeconfiguringHostState::RebootAfterLockdown);
    assert_eq!(mh.host.machine().await.current_state(), &reboot);

    env.redfish_sim.fail_next_power_action(
        libredfish::SystemPowerControl::ForceRestart,
        "transient restart failure",
    );
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &reboot);
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    assert_eq!(machine.current_state(), &reboot);
    let requested = machine.status.last_reboot_requested.unwrap();
    assert!(requested.time >= machine.state.version.timestamp());

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
    env.redfish_sim.set_lockdown_requires_host_reboot(true);
    env.run_single_iteration().await;
    env.run_single_iteration().await;
    let requested = mh
        .host
        .machine()
        .await
        .status
        .last_reboot_requested
        .unwrap();
    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    assert_eq!(
        machine.current_state(),
        &deconfiguring(DeconfiguringHostState::RebootAfterLockdown)
    );
    assert_eq!(machine.status.last_reboot_requested.unwrap(), requested);
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
