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

//! Initial-ingestion mTLS reset: `DisableMtls` must complete before certificates
//! are installed.

use std::sync::Arc;

use carbide_secrets::credentials::{CredentialKey, CredentialWriter, Credentials};
use carbide_switch_controller::context::SwitchStateHandlerServices;
use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use carbide_uuid::switch::SwitchId;
use component_manager::error::ComponentManagerError;
use component_manager::mock::{MockNvSwitchManager, MockSwitchMtlsCall};
use component_manager::nv_switch_manager::{SwitchMtlsDisableJobStatus, SwitchMtlsDisableState};
use db::switch as db_switch;
use librms::protos::rack_manager::SwitchService;
use model::switch::{
    ConfigureCertificateState, ConfiguringState, DisableMtlsState, InitializingState,
    SwitchControllerState,
};

use crate::common::{
    ControllerEnv, default_switch_mtls_services, new_switch, set_switch_rack_id,
    transition_switch_controller_state,
};
use crate::state_controller::{mock_component_manager, run_switch_controller_with_services};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const DISABLE_JOB_ID: &str = "mock-mtls-disable-job";

fn disable_mtls_start() -> SwitchControllerState {
    SwitchControllerState::Configuring {
        config_state: ConfiguringState::DisableMtls {
            disable_mtls: DisableMtlsState::Start,
        },
    }
}

fn disable_mtls_wait() -> SwitchControllerState {
    SwitchControllerState::Configuring {
        config_state: ConfiguringState::DisableMtls {
            disable_mtls: DisableMtlsState::WaitForComplete {
                job_id: DISABLE_JOB_ID.to_string(),
            },
        },
    }
}

fn certificate_start() -> SwitchControllerState {
    SwitchControllerState::Configuring {
        config_state: ConfiguringState::ConfigureCertificate {
            configure_certificate: ConfigureCertificateState::Start,
        },
    }
}

/// The services ingestion installs for a non-primary switch with default
/// configuration; the disable request must match them.
fn ingestion_services() -> Vec<i32> {
    vec![
        SwitchService::NvueApi as i32,
        SwitchService::ScaleUpFabricTelemetryInterface as i32,
    ]
}

fn pending() -> Result<SwitchMtlsDisableJobStatus, ComponentManagerError> {
    Ok(SwitchMtlsDisableJobStatus {
        state: SwitchMtlsDisableState::Pending,
        error: None,
    })
}

fn completed() -> Result<SwitchMtlsDisableJobStatus, ComponentManagerError> {
    Ok(SwitchMtlsDisableJobStatus {
        state: SwitchMtlsDisableState::Completed,
        error: None,
    })
}

/// Creates a switch in `state`, optionally with a rack and NVOS credentials so
/// the disable step can resolve an endpoint.
async fn switch_in_state(
    env: &ControllerEnv,
    state: SwitchControllerState,
    with_rack: bool,
) -> TestResult<SwitchId> {
    let switch_id = new_switch(env, Some("Switch4".to_string()), None).await?;

    let bmc_mac_address = db_switch::find_switch_endpoints_by_ids(&env.pool, &[switch_id])
        .await?
        .first()
        .expect("switch endpoint row")
        .bmc_mac;

    env.test_credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin { bmc_mac_address },
            &Credentials::UsernamePassword {
                username: "nvos-admin".to_string(),
                password: "nvos-secret".to_string(),
            },
        )
        .await
        .expect("failed to seed NVOS credentials");

    let mut txn = env.pool.begin().await?;

    if with_rack {
        set_switch_rack_id(txn.as_mut(), &switch_id, &"rack-id-1".into()).await?;
    }

    transition_switch_controller_state(txn.as_mut(), &switch_id, state).await?;
    txn.commit().await?;

    Ok(switch_id)
}

/// Runs one controller iteration with a fresh controller, so no in-memory state
/// survives between calls, as after a restart.
async fn run_iteration(env: &ControllerEnv, mock: &Arc<MockNvSwitchManager>) {
    run_switch_controller_with_services(
        env.pool.clone(),
        env.api.work_lock_manager_handle(),
        SwitchStateHandlerServices {
            db_pool: env.pool.clone(),
            component_manager: Some(mock_component_manager(mock.clone())),
            credential_manager: env.test_credential_manager.clone(),
            switch_mtls_services: default_switch_mtls_services(),
            per_object_metrics_registry: env.per_object_metrics_registry.clone(),
            redfish_client_pool: env.redfish_sim.clone(),
            bmc_credential_ops: env.redfish_sim.clone(),
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            bmc_rotation_enabled: false,
        },
    )
    .await;
}

async fn controller_state(
    env: &ControllerEnv,
    switch_id: &SwitchId,
) -> TestResult<SwitchControllerState> {
    let mut conn = env.pool.acquire().await?;

    Ok(db_switch::find_by_id(&mut conn, switch_id)
        .await?
        .expect("switch should exist")
        .controller_state
        .value)
}

/// Parks a finished case's switch in `Error`, where the controller leaves it
/// alone, so later cases sharing the database only see their own switch.
async fn park_switch(env: &ControllerEnv, switch_id: &SwitchId) -> TestResult {
    let mut txn = env.pool.begin().await?;

    transition_switch_controller_state(
        txn.as_mut(),
        switch_id,
        SwitchControllerState::Error {
            cause: "case finished".to_string(),
        },
    )
    .await?;

    txn.commit().await?;

    Ok(())
}

fn certificate_submitted(calls: &[MockSwitchMtlsCall]) -> bool {
    calls
        .iter()
        .any(|call| matches!(call, MockSwitchMtlsCall::CertificateSubmitted { .. }))
}

#[sqlx_test]
async fn test_initializing_enters_disable_mtls_before_certificates(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;

    let switch_id = switch_in_state(
        &env,
        SwitchControllerState::Initializing {
            initializing_state: InitializingState::WaitForOsMachineInterface,
        },
        false,
    )
    .await?;

    run_iteration(&env, &Arc::new(MockNvSwitchManager::default())).await;

    assert_eq!(
        controller_state(&env, &switch_id).await?,
        disable_mtls_start()
    );

    Ok(())
}

#[sqlx_test]
async fn test_disable_mtls_start_submits_certificate_services(pool: sqlx::PgPool) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = switch_in_state(&env, disable_mtls_start(), true).await?;

    let mock = Arc::new(MockNvSwitchManager::default().with_mtls_disable_job(DISABLE_JOB_ID));

    run_iteration(&env, &mock).await;

    assert_eq!(
        controller_state(&env, &switch_id).await?,
        disable_mtls_wait()
    );

    assert_eq!(
        mock.mtls_calls(),
        vec![MockSwitchMtlsCall::DisableSubmitted {
            services: ingestion_services()
        }]
    );

    Ok(())
}

/// Cases where the disable step submits nothing or fails at submission.
#[sqlx_test]
async fn test_disable_mtls_start_skips_or_fails_without_resubmitting(
    pool: sqlx::PgPool,
) -> TestResult {
    struct Case {
        scenario: &'static str,
        with_rack: bool,
        mock: MockNvSwitchManager,
        iterations: usize,
        expected_submissions: usize,
        check: fn(&SwitchControllerState) -> bool,
    }

    let cases = [
        Case {
            scenario: "no rack association skips to certificate configuration",
            with_rack: false,
            mock: MockNvSwitchManager::default().with_mtls_disable_job(DISABLE_JOB_ID),
            iterations: 1,
            expected_submissions: 0,
            check: |state| *state == certificate_start(),
        },
        Case {
            scenario: "unsupported backend skips to certificate configuration",
            with_rack: true,
            mock: MockNvSwitchManager::default(),
            iterations: 1,
            expected_submissions: 0,
            check: |state| *state == certificate_start(),
        },
        Case {
            scenario: "unknown submission outcome errors and is not resubmitted",
            with_rack: true,
            mock: MockNvSwitchManager::default().with_mtls_disable_outcome_unknown("lost reply"),
            // The second iteration proves an errored switch does not resubmit.
            iterations: 2,
            expected_submissions: 1,
            check: |state| {
                matches!(
                    state,
                    SwitchControllerState::Error { cause }
                        if cause.contains("unknown outcome")
                            && cause.contains("certificate configuration was not attempted")
                )
            },
        },
    ];

    let env = ControllerEnv::new(pool.clone()).await;

    for case in cases {
        let switch_id = switch_in_state(&env, disable_mtls_start(), case.with_rack).await?;
        let mock = Arc::new(case.mock);

        for _ in 0..case.iterations {
            run_iteration(&env, &mock).await;
        }

        let state = controller_state(&env, &switch_id).await?;
        assert!((case.check)(&state), "{}: got {state:?}", case.scenario);

        let calls = mock.mtls_calls();
        assert_eq!(calls.len(), case.expected_submissions, "{}", case.scenario);
        assert!(!certificate_submitted(&calls), "{}", case.scenario);

        park_switch(&env, &switch_id).await?;
    }

    Ok(())
}

/// Each poll outcome decides whether certificate configuration may start.
#[sqlx_test]
async fn test_disable_mtls_wait_outcomes_gate_certificate_configuration(
    pool: sqlx::PgPool,
) -> TestResult {
    struct Case {
        scenario: &'static str,
        poll: Result<SwitchMtlsDisableJobStatus, ComponentManagerError>,
        check: fn(&SwitchControllerState) -> bool,
    }

    let cases = [
        Case {
            scenario: "pending job keeps waiting",
            poll: pending(),
            check: |state| *state == disable_mtls_wait(),
        },
        Case {
            scenario: "transient observation failure retries from the same state",
            poll: Err(ComponentManagerError::Unavailable("rms down".to_string())),
            check: |state| *state == disable_mtls_wait(),
        },
        Case {
            scenario: "completed job proceeds to certificate configuration",
            poll: completed(),
            check: |state| *state == certificate_start(),
        },
        Case {
            scenario: "failed job errors without certificate configuration",
            poll: Ok(SwitchMtlsDisableJobStatus {
                state: SwitchMtlsDisableState::Failed,
                error: Some("ssh login failed".to_string()),
            }),
            check: |state| {
                matches!(
                    state,
                    SwitchControllerState::Error { cause }
                        if cause.contains("ssh login failed")
                            && cause.contains("certificate configuration was not attempted")
                )
            },
        },
        Case {
            scenario: "unresolved outcome errors without certificate configuration",
            poll: Err(ComponentManagerError::OperationOutcomeUnknown(
                "job expired".to_string(),
            )),
            check: |state| {
                matches!(
                    state,
                    SwitchControllerState::Error { cause } if cause.contains("job expired")
                )
            },
        },
    ];

    let env = ControllerEnv::new(pool.clone()).await;

    for case in cases {
        let switch_id = switch_in_state(&env, disable_mtls_wait(), true).await?;

        let mock = Arc::new(
            MockNvSwitchManager::default()
                .with_mtls_disable_job(DISABLE_JOB_ID)
                .with_mtls_disable_job_status_responses([case.poll]),
        );

        run_iteration(&env, &mock).await;

        let state = controller_state(&env, &switch_id).await?;
        assert!((case.check)(&state), "{}: got {state:?}", case.scenario);

        assert!(
            !certificate_submitted(&mock.mtls_calls()),
            "{}",
            case.scenario
        );

        park_switch(&env, &switch_id).await?;
    }

    Ok(())
}

/// Walks one switch through disable and certificate installation using a fresh
/// controller per iteration, so each stage resumes from persisted state alone.
#[sqlx_test]
async fn test_certificates_are_submitted_only_after_disable_completes(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = switch_in_state(&env, disable_mtls_start(), true).await?;

    let mock = Arc::new(
        MockNvSwitchManager::default()
            .with_mtls_disable_job(DISABLE_JOB_ID)
            .with_mtls_disable_job_status_responses([pending(), completed()]),
    );

    // Submit, poll while pending, poll to completion, then submit certificates.
    for _ in 0..4 {
        run_iteration(&env, &mock).await;
    }

    let status_read = MockSwitchMtlsCall::DisableStatusRead {
        job_id: DISABLE_JOB_ID.to_string(),
    };

    assert_eq!(
        mock.mtls_calls(),
        vec![
            MockSwitchMtlsCall::DisableSubmitted {
                services: ingestion_services()
            },
            status_read.clone(),
            status_read,
            MockSwitchMtlsCall::CertificateSubmitted {
                services: ingestion_services()
            },
        ]
    );

    assert!(matches!(
        controller_state(&env, &switch_id).await?,
        SwitchControllerState::Configuring {
            config_state: ConfiguringState::ConfigureCertificate {
                configure_certificate: ConfigureCertificateState::WaitForComplete { .. },
            },
        }
    ));

    Ok(())
}
