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

//! Persisted wait reasons for running Redfish tasks report the BMC's progress.

use std::collections::HashMap;

use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::firmware::FirmwareComponentType;
use model::machine::{
    DpuDiscoveringState, DpuDiscoveringStates, HostReprovisionState, ManagedHostState,
    SetSecureBootState,
};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

const FIRMWARE_TASK_ID: &str = "host-firmware-task";
const CERTIFICATE_TASK_ID: &str = "dpu-certificate-task";

/// One poll of a running task: the progress the BMC reports and the persisted reason.
struct ProgressCase {
    scenario: &'static str,
    percent_complete: Option<u32>,
    reason: String,
}

fn progress_cases(prefix: &str) -> Vec<ProgressCase> {
    vec![
        ProgressCase {
            scenario: "reported progress",
            percent_complete: Some(45),
            reason: format!("{prefix}: 45%"),
        },
        ProgressCase {
            scenario: "reported zero progress",
            percent_complete: Some(0),
            reason: format!("{prefix}: 0%"),
        },
        ProgressCase {
            scenario: "progress not reported",
            percent_complete: None,
            reason: prefix.to_string(),
        },
        ProgressCase {
            scenario: "progress advances on a later poll",
            percent_complete: Some(70),
            reason: format!("{prefix}: 70%"),
        },
    ]
}

async fn setup_host(pool: PgPool) -> (Env, TestManagedHost) {
    let env = Env::builder(pool).build().await;
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.test_harness.default_test_site_explorer();
    let mh = env
        .test_harness
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0;
    (env, mh)
}

/// Polls the task once per case and checks the persisted outcome; the state must not change.
async fn assert_progress_cases(
    env: &mut Env,
    mh: &TestManagedHost,
    task_id: &str,
    cases: Vec<ProgressCase>,
) {
    let waiting = mh.host.machine().await;
    for case in cases {
        env.redfish_sim
            .set_running_task(task_id, case.percent_complete);
        env.run_single_iteration().await;

        let machine = mh.host.machine().await;
        assert_eq!(
            machine.state.value, waiting.state.value,
            "{}: state changed",
            case.scenario
        );
        assert_eq!(
            machine.state.version, waiting.state.version,
            "{}: state version changed",
            case.scenario
        );
        let Some(PersistentStateHandlerOutcome::Wait { reason, .. }) =
            machine.controller_state_outcome
        else {
            panic!(
                "{}: expected a Wait outcome, got {:?}",
                case.scenario, machine.controller_state_outcome
            );
        };
        assert!(
            reason.ends_with(&case.reason),
            "{}: unexpected reason {reason:?}, expected it to end with {:?}",
            case.scenario,
            case.reason
        );
    }
}

#[sqlx_test]
async fn host_firmware_upgrade_wait_reports_task_progress(pool: PgPool) {
    let (mut env, mh) = setup_host(pool).await;
    mh.advance_state(ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::WaitingForFirmwareUpgrade {
            task_id: FIRMWARE_TASK_ID.to_string(),
            final_version: "1.2.3".to_string(),
            firmware_type: FirmwareComponentType::Bmc,
            power_drains_needed: None,
            firmware_number: None,
            started_waiting: Some(chrono::Utc::now()),
        },
        retry_count: 0,
    })
    .await;

    let prefix = format!("Waiting for firmware upgrade task {FIRMWARE_TASK_ID} to complete");
    assert_progress_cases(&mut env, &mh, FIRMWARE_TASK_ID, progress_cases(&prefix)).await;
}

#[sqlx_test]
async fn dpu_certificate_upload_wait_reports_task_progress(pool: PgPool) {
    let (mut env, mh) = setup_host(pool).await;
    mh.advance_state(ManagedHostState::DpuDiscoveringState {
        dpu_states: DpuDiscoveringStates {
            states: HashMap::from([(
                mh.dpu(0).id,
                DpuDiscoveringState::EnableSecureBoot {
                    enable_secure_boot_state: SetSecureBootState::WaitCertificateUpload {
                        task_id: CERTIFICATE_TASK_ID.to_string(),
                    },
                    count: 0,
                },
            )]),
        },
    })
    .await;

    let prefix = format!("Waiting for certificate upload task {CERTIFICATE_TASK_ID} to complete");
    assert_progress_cases(&mut env, &mh, CERTIFICATE_TASK_ID, progress_cases(&prefix)).await;
}
