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
use chrono::{Duration, Utc};
use config_version::ConfigVersion;
use model::machine::{
    FailureCause, MachineLastRebootRequested, MachineLastRebootRequestedMode, MachineState,
    ManagedHostState, MeasuringState,
};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

const HOST_INIT_MEASURING: ManagedHostState = ManagedHostState::HostInit {
    machine_state: MachineState::Measuring {
        measuring_state: MeasuringState::WaitingForMeasurements,
    },
};

async fn setup_waiting_host(env: &Env, age: Duration) -> TestManagedHost {
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.test_harness.default_test_site_explorer();
    let host = env
        .test_harness
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0;

    host.advance_state(HOST_INIT_MEASURING.clone()).await;
    let machine = host.host.machine().await;
    let entered_at = Utc::now() - age;
    let stale_version: ConfigVersion = format!(
        "V{}-T{}",
        machine.state.version.version_nr(),
        entered_at.timestamp_micros()
    )
    .parse()
    .unwrap();
    let last_reboot_requested = MachineLastRebootRequested {
        time: entered_at,
        mode: MachineLastRebootRequestedMode::Reboot,
        restart_verified: None,
        verification_attempts: None,
    };
    sqlx::query(
        "UPDATE machines SET controller_state_version = $1, last_reboot_requested = $2 WHERE id = $3",
    )
    .bind(stale_version)
    .bind(sqlx::types::Json(last_reboot_requested))
    .bind(host.host.id)
    .execute(&env.test_harness.api().database_connection)
    .await
    .unwrap();

    host
}

#[sqlx_test]
async fn host_init_reboots_while_waiting_for_measurements(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.attestation_enabled = true;
            config
                .machine_state_controller
                .waiting_for_measurements_timeout = Duration::hours(1);
        })
        .build()
        .await;
    let host = setup_waiting_host(&env, Duration::minutes(10)).await;
    let updated = sqlx::query(
        "UPDATE ek_cert_verification_status SET signing_ca_found = TRUE WHERE machine_id = $1",
    )
    .bind(host.host.id)
    .execute(&env.test_harness.api().database_connection)
    .await
    .unwrap();
    assert_eq!(
        updated.rows_affected(),
        1,
        "fixture should create EK status"
    );
    let timepoint = env.redfish_sim.timepoint();

    env.run_single_iteration().await;

    assert_eq!(
        host.host.machine().await.current_state(),
        &HOST_INIT_MEASURING
    );
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .contains(&RedfishSimAction::Power(
                libredfish::SystemPowerControl::ForceRestart,
            )),
        "waiting for Scout measurements should retry the host boot"
    );
}

#[sqlx_test]
async fn missing_ek_status_cannot_bypass_measurement_timeout(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.attestation_enabled = true;
            config
                .machine_state_controller
                .waiting_for_measurements_timeout = Duration::minutes(5);
        })
        .build()
        .await;
    let host = setup_waiting_host(&env, Duration::minutes(10)).await;
    sqlx::query("DELETE FROM ek_cert_verification_status WHERE machine_id = $1")
        .bind(host.host.id)
        .execute(&env.test_harness.api().database_connection)
        .await
        .unwrap();
    let timepoint = env.redfish_sim.timepoint();

    env.run_single_iteration().await;

    let machine = host.host.machine().await;
    assert!(matches!(
        &machine.state.value,
        ManagedHostState::Failed { details, .. }
            if matches!(&details.cause, FailureCause::MeasurementsNotReceived { .. })
    ));
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "an expired measurement wait should fail without another reboot"
    );
}
