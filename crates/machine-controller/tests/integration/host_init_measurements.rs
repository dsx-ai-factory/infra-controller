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
use measured_boot::pcr::PcrRegisterValue;
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

const HOST_INIT_PENDING_BUNDLE: ManagedHostState = ManagedHostState::HostInit {
    machine_state: MachineState::Measuring {
        measuring_state: MeasuringState::PendingBundle,
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
    // The test retry interval is one second; age the prior request so a retry is due.
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

async fn insert_verified_ek(env: &Env, host: &TestManagedHost) {
    let mut txn = env
        .test_harness
        .api()
        .database_connection
        .begin()
        .await
        .unwrap();
    db::attestation::ek_cert_verification_status::insert(
        txn.as_mut(),
        b"test-ek-sha256",
        "test-serial",
        true,
        None,
        b"test-issuer",
        "test-issuer-access-info",
        *host.host.id,
    )
    .await
    .unwrap()
    .expect("verified EK status should be inserted");
    txn.commit().await.unwrap();
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
    insert_verified_ek(&env, &host).await;
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
async fn missing_ek_status_reboots_before_measurement_timeout(pool: PgPool) {
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
        "missing EK status should retry the host boot before timeout"
    );
}

#[sqlx_test]
async fn pending_bundle_does_not_reboot(pool: PgPool) {
    let mut env = Env::builder(pool)
        .configure_runtime(|config| {
            config.attestation_enabled = true;
        })
        .build()
        .await;
    let host = setup_waiting_host(&env, Duration::minutes(10)).await;
    insert_verified_ek(&env, &host).await;
    let mut txn = env
        .test_harness
        .api()
        .database_connection
        .begin()
        .await
        .unwrap();
    db::machine_topology::set_topology_update_needed(&mut txn, &host.host.id, true)
        .await
        .unwrap();
    db::machine_topology::create_or_update(&mut txn, &host.host.id, &host.host.hardware_info())
        .await
        .unwrap();
    db::measured_boot::report::new(
        &mut txn,
        *host.host.id,
        &[PcrRegisterValue {
            pcr_register: 0,
            sha_any: "test-pcr".to_string(),
        }],
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    host.advance_state(HOST_INIT_PENDING_BUNDLE.clone()).await;
    let timepoint = env.redfish_sim.timepoint();

    env.run_single_iteration().await;

    assert_eq!(
        host.host.machine().await.current_state(),
        &HOST_INIT_PENDING_BUNDLE
    );
    assert!(
        env.redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty(),
        "PendingBundle should not reboot the host"
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
