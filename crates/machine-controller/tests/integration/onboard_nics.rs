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
use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool as _};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use chrono::{Duration, Utc};
use config_version::ConfigVersion;
use libredfish::EnabledDisabled;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::machine::{DisableOnboardNicsState, MachineState, ManagedHostState};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

fn onboard_state(state: DisableOnboardNicsState) -> ManagedHostState {
    ManagedHostState::HostInit {
        machine_state: MachineState::DisableOnboardNics {
            disable_onboard_nics_state: state,
        },
    }
}

async fn setup_host(env: &Env, config: ManagedHostConfig) -> (TestManagedHost, String) {
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.test_harness.default_test_site_explorer();
    let (host, data) = env
        .test_harness
        .managed_host_builder(&site_explorer, underlay)
        .with_config(config)
        .build()
        .await;
    (host, data.host_bmc_ip().to_string())
}

async fn expire_wait(env: &Env, host: &TestManagedHost) {
    let machine = host.host.machine().await;
    let version: ConfigVersion = format!(
        "V{}-T{}",
        machine.state.version.version_nr(),
        (Utc::now() - Duration::seconds(11)).timestamp_micros()
    )
    .parse()
    .unwrap();
    let result = sqlx::query("UPDATE machines SET controller_state_version = $1 WHERE id = $2")
        .bind(version)
        .bind(host.host.id)
        .execute(&env.test_harness.api().database_connection)
        .await
        .unwrap();
    assert_eq!(result.rows_affected(), 1);
}

#[sqlx_test]
async fn onboard_nics_toggle_waits_and_retries_across_restart(pool: PgPool) {
    let mut env = Env::builder(pool.clone()).build().await;
    let (host, bmc) = setup_host(&env, ManagedHostConfig::default()).await;
    env.redfish_sim
        .set_service_root_vendor(Some("NVIDIA".into()));
    env.redfish_sim
        .set_service_root_product(Some("VR NVL72".into()));
    host.advance_state(ManagedHostState::HostInit {
        machine_state: MachineState::EnableIpmiOverLan,
    })
    .await;
    env.run_single_iteration().await;
    assert_eq!(
        host.host.machine().await.state.value,
        onboard_state(DisableOnboardNicsState::ConfigureOnboardNics { enable: true })
    );

    for (enable, target, operation) in [
        (true, EnabledDisabled::Enabled, "enable_onboard_nics"),
        (false, EnabledDisabled::Disabled, "disable_onboard_nics"),
    ] {
        let before = host.host.machine().await;
        let actions = env.redfish_sim.timepoint();
        env.redfish_sim
            .fail_next_onboard_nics_write("temporary NIC write failure");
        env.run_single_iteration().await;
        let failed = host.host.machine().await;
        assert_eq!(failed.state.value, before.state.value);
        assert_eq!(failed.state.version, before.state.version);
        assert!(matches!(failed.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Error { ref err, .. })
                if err.contains("temporary NIC write failure") && err.contains(operation)));
        assert_eq!(
            env.redfish_sim.actions_since(&actions).for_host(&bmc),
            vec![RedfishSimAction::SetEnableOnboardNicsFailed(target)]
        );

        env = Env::builder(pool.clone()).build_with_harness(env.test_harness, env.redfish_sim);
        env.run_single_iteration().await;
        let waiting = host.host.machine().await;
        assert_eq!(
            waiting.state.value,
            onboard_state(DisableOnboardNicsState::WaitForConfigToBeApplied {
                onboard_nics_disabled: !enable,
            })
        );
        assert_eq!(
            env.redfish_sim.actions_since(&actions).for_host(&bmc),
            vec![
                RedfishSimAction::SetEnableOnboardNicsFailed(target),
                RedfishSimAction::SetEnableOnboardNics(target),
            ]
        );

        // A new controller must honor the persisted wait without reissuing the write.
        env = Env::builder(pool.clone()).build_with_harness(env.test_harness, env.redfish_sim);
        let actions = env.redfish_sim.timepoint();
        env.run_single_iteration().await;
        let still_waiting = host.host.machine().await;
        assert_eq!(still_waiting.state.value, waiting.state.value);
        assert_eq!(still_waiting.state.version, waiting.state.version);
        assert!(matches!(still_waiting.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
                if reason == "Waiting for onboard NIC configuration to be applied"));
        assert!(
            env.redfish_sim
                .actions_since(&actions)
                .all_hosts()
                .is_empty()
        );

        expire_wait(&env, &host).await;
        env.run_single_iteration().await;
        let expected = if enable {
            onboard_state(DisableOnboardNicsState::ConfigureOnboardNics { enable: false })
        } else {
            ManagedHostState::HostInit {
                machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 0 },
            }
        };
        assert_eq!(host.host.machine().await.state.value, expected);
        assert!(
            env.redfish_sim
                .actions_since(&actions)
                .all_hosts()
                .is_empty()
        );
    }
}

#[sqlx_test]
async fn onboard_nics_skipped_without_managed_dpus(pool: PgPool) {
    let mut env = Env::builder(pool).build().await;
    let mut config = ManagedHostConfig::zero_dpu();
    config.expected_machine_data = Some(model::expected_machine::ExpectedMachineData {
        serial_number: config.serial.clone(),
        dpu_policy: model::expected_machine::HostDpuPolicy::Ignore,
        ..Default::default()
    });
    let (host, bmc) = setup_host(&env, config).await;
    env.redfish_sim
        .set_service_root_vendor(Some("NVIDIA".into()));
    env.redfish_sim
        .set_service_root_product(Some("VR NVL72".into()));
    let client = env
        .redfish_sim
        .create_client(&bmc, None, RedfishAuth::Anonymous, None)
        .await
        .unwrap();
    client
        .set_enable_onboard_nics(EnabledDisabled::Enabled)
        .await
        .unwrap();
    host.advance_state(ManagedHostState::HostInit {
        machine_state: MachineState::EnableIpmiOverLan,
    })
    .await;
    let actions = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_eq!(
        host.host.machine().await.state.value,
        ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 0 },
        }
    );
    assert!(
        env.redfish_sim
            .actions_since(&actions)
            .all_hosts()
            .is_empty()
    );
    assert_eq!(
        client.are_onboard_nics_enabled().await.unwrap(),
        Some(EnabledDisabled::Enabled)
    );
}

#[sqlx_test]
async fn onboard_nics_skipped_on_other_skus_with_managed_dpus(pool: PgPool) {
    let mut env = Env::builder(pool).build().await;
    let (host, _) = setup_host(&env, ManagedHostConfig::default()).await;
    env.redfish_sim
        .set_service_root_vendor(Some("NVIDIA".into()));
    env.redfish_sim
        .set_service_root_product(Some("GB200 NVL".into()));
    host.advance_state(ManagedHostState::HostInit {
        machine_state: MachineState::EnableIpmiOverLan,
    })
    .await;
    let actions = env.redfish_sim.timepoint();
    env.run_single_iteration().await;
    assert_eq!(
        host.host.machine().await.state.value,
        ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 0 },
        }
    );
    assert!(
        env.redfish_sim
            .actions_since(&actions)
            .all_hosts()
            .is_empty()
    );
}
