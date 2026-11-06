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

async fn setup(pool: PgPool, config: ManagedHostConfig) -> (Env, TestManagedHost) {
    let vendor = config.vendor.expect("test fixture must specify a vendor");
    let env = Env::builder(pool).build().await;
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
    // The controller reads the vendor from Scout discovery, not exploration.
    let mut hardware_info = mh.host.hardware_info();
    hardware_info.dmi_data.as_mut().unwrap().sys_vendor = vendor.to_pascalcase();
    let mut txn = env.test_harness.db_txn().await;
    db::machine_topology::set_topology_update_needed(&mut txn, &mh.host.id.into(), true)
        .await
        .unwrap();
    db::machine_topology::create_or_update(&mut txn, &mh.host.id.into(), &hardware_info)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(mh.host.machine().await.bmc_vendor(), vendor);
    mh.advance_state(deconfiguring(DeconfiguringHostState::DisableLockdown))
        .await;
    (env, mh)
}

#[sqlx_test]
async fn lenovo_decommission_unlocks_without_an_extra_reboot(pool: PgPool) {
    let config = ManagedHostConfig {
        vendor: Some(bmc_vendor::BMCVendor::Lenovo),
        ..ManagedHostConfig::default()
    };
    let (mut env, mh) = setup(pool, config).await;
    env.redfish_sim
        .set_lockdown(libredfish::EnabledDisabled::Enabled);
    let timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_eq!(
        mh.host.machine().await.current_state(),
        &deconfiguring(DeconfiguringHostState::ClearSuperNicLockdown)
    );
    assert!(
        env.redfish_sim
            .lockdown_states()
            .contains(&libredfish::EnabledDisabled::Disabled)
    );
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty()
    );
}

#[sqlx_test]
async fn supermicro_decommission_retains_unlock_reboot_and_retry(pool: PgPool) {
    let config = ManagedHostConfig {
        vendor: Some(bmc_vendor::BMCVendor::Supermicro),
        ..ManagedHostConfig::default()
    };
    let (mut env, mh) = setup(pool, config).await;
    env.run_single_iteration().await;
    let reboot = deconfiguring(DeconfiguringHostState::RebootAfterLockdown);
    assert_eq!(mh.host.machine().await.current_state(), &reboot);
    let timepoint = env.redfish_sim.timepoint();
    env.redfish_sim.fail_next_power_action(
        libredfish::SystemPowerControl::ForceRestart,
        "transient restart failure",
    );
    env.run_single_iteration().await;
    assert_eq!(mh.host.machine().await.current_state(), &reboot);
    env.run_single_iteration().await;
    assert_eq!(
        mh.host.machine().await.current_state(),
        &deconfiguring(DeconfiguringHostState::ClearSuperNicLockdown)
    );
    let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
    assert!(actions.contains(&RedfishSimAction::PowerFailed(
        libredfish::SystemPowerControl::ForceRestart
    )));
    assert!(actions.contains(&RedfishSimAction::Power(
        libredfish::SystemPowerControl::ForceRestart
    )));
}

#[sqlx_test]
async fn no_dpu_lockdown_enables_without_boot_wait_and_disable_still_polls(pool: PgPool) {
    let mut config = ManagedHostConfig {
        vendor: Some(bmc_vendor::BMCVendor::Lenovo),
        ..ManagedHostConfig::default()
    };
    config.dpus.clear();
    config.expected_machine_data = Some(model::expected_machine::ExpectedMachineData {
        serial_number: config.serial.clone(),
        dpu_policy: model::expected_machine::HostDpuPolicy::Ignore,
        ..Default::default()
    });
    let (mut env, mh) = setup(pool, config).await;
    assert!(mh.dpus.is_empty());
    for mode in [
        model::machine::LockdownMode::Enable,
        model::machine::LockdownMode::Disable,
    ] {
        mh.advance_state(ManagedHostState::HostInit {
            machine_state: model::machine::MachineState::WaitingForLockdown {
                lockdown_info: model::machine::LockdownInfo {
                    state: model::machine::LockdownState::TimeWaitForDPUDown,
                    mode: mode.clone(),
                },
            },
        })
        .await;
        let timepoint = env.redfish_sim.timepoint();
        env.run_single_iteration().await;
        let machine = mh.host.machine().await;
        if mode == model::machine::LockdownMode::Enable {
            assert!(matches!(
                machine.current_state(),
                ManagedHostState::BomValidating { .. }
            ));
        } else {
            assert_eq!(
                machine.current_state(),
                &ManagedHostState::HostInit {
                    machine_state: model::machine::MachineState::WaitingForLockdown {
                        lockdown_info: model::machine::LockdownInfo {
                            state: model::machine::LockdownState::PollingLockdownStatus,
                            mode,
                        },
                    },
                }
            );
        }
        let actions = env.redfish_sim.actions_since(&timepoint).all_hosts();
        assert!(
            actions.iter().all(|action| !matches!(
                action,
                RedfishSimAction::Power(_)
                    | RedfishSimAction::PowerFailed(_)
                    | RedfishSimAction::BmcReset(_)
                    | RedfishSimAction::ChassisReset { .. }
            )),
            "unexpected power or reset request: {actions:?}"
        );
    }
}
