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

use std::sync::{Arc, Mutex};

use carbide_secrets::credentials::{CredentialManager, Credentials};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_test_support::{Case, Outcome, check_cases_async};
use component_manager::component_manager::ComponentManager;
use component_manager::compute_tray_manager::{
    Backend, ComputeTrayEndpoint, ComputeTrayFirmwareUpdateStatus, ComputeTrayManager,
    ComputeTrayResult,
};
use component_manager::error::ComponentManagerError;
use component_manager::mock::{MockNvSwitchManager, MockPowerShelfManager};
use component_manager::types::FirmwareUpdateOptions;
use model::component_manager::{ComputeTrayComponent, PowerAction};
use model::machine::{
    FailureCause, FailureDetails, FailureSource, MachineMaintenanceOperation, ManagedHostState,
    StateMachineArea,
};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

#[derive(Clone, Copy, Debug)]
enum BackendOutcome {
    Success,
    Empty,
    NonSuccess,
    TransportFailure,
}

#[derive(Debug)]
pub(crate) struct ReconciliationComputeTrayManager {
    outcome: Mutex<BackendOutcome>,
    actions: Mutex<Vec<PowerAction>>,
}

impl ReconciliationComputeTrayManager {
    pub(crate) fn new() -> Self {
        Self {
            outcome: Mutex::new(BackendOutcome::Success),
            actions: Mutex::new(Vec::new()),
        }
    }

    fn set_outcome(&self, outcome: BackendOutcome) {
        *self.outcome.lock().unwrap() = outcome;
        self.actions.lock().unwrap().clear();
    }

    fn take_actions(&self) -> Vec<PowerAction> {
        std::mem::take(&mut *self.actions.lock().unwrap())
    }
}

#[async_trait::async_trait]
impl ComputeTrayManager for ReconciliationComputeTrayManager {
    fn name(&self) -> &str {
        "maintenance-test"
    }

    fn backend(&self) -> Backend {
        Backend::Mock
    }

    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        self.actions.lock().unwrap().push(action);
        match *self.outcome.lock().unwrap() {
            BackendOutcome::Success => Ok(vec![ComputeTrayResult {
                bmc_ip: endpoints[0].bmc_ip,
                bmc_mac: endpoints[0].bmc_mac,
                success: true,
                error: None,
            }]),
            BackendOutcome::Empty => Ok(Vec::new()),
            BackendOutcome::NonSuccess => Ok(vec![ComputeTrayResult {
                bmc_ip: endpoints[0].bmc_ip,
                bmc_mac: endpoints[0].bmc_mac,
                success: false,
                error: Some("test backend rejection".into()),
            }]),
            BackendOutcome::TransportFailure => Err(ComponentManagerError::Status(
                tonic::Status::unavailable("test transport failure"),
            )),
        }
    }

    async fn update_firmware(
        &self,
        _endpoints: &[ComputeTrayEndpoint],
        _target_version: &str,
        _components: &[ComputeTrayComponent],
        _options: &FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        unreachable!("firmware updates are not exercised by maintenance tests")
    }

    async fn get_firmware_status(
        &self,
        _endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError> {
        unreachable!("firmware status is not exercised by maintenance tests")
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        unreachable!("firmware bundles are not exercised by maintenance tests")
    }
}

#[derive(Clone, Copy, Debug)]
enum EntryState {
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug)]
struct ReconciliationCase {
    operation: MachineMaintenanceOperation,
    entry_state: EntryState,
    backend_outcome: BackendOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultingState {
    Ready,
    Failed,
}

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    request_cleared: bool,
    resulting_state: ResultingState,
    actions: Vec<PowerAction>,
}

fn expected_action(operation: MachineMaintenanceOperation) -> PowerAction {
    match operation {
        MachineMaintenanceOperation::PowerOn => PowerAction::On,
        MachineMaintenanceOperation::PowerOff => PowerAction::ForceOff,
        MachineMaintenanceOperation::Reset => PowerAction::ForceRestart,
    }
}

pub(crate) fn component_manager(
    compute_tray: Arc<dyn ComputeTrayManager>,
) -> Arc<ComponentManager> {
    Arc::new(ComponentManager::new(
        Arc::new(MockNvSwitchManager::default()),
        Arc::new(MockPowerShelfManager),
        compute_tray,
        false,
        false,
        true,
    ))
}

pub(crate) fn valid_credential_manager() -> Arc<dyn CredentialManager> {
    Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
        username: "root".into(),
        password: "password".into(),
    }))
}

async fn create_ready_host(
    env: &Env,
    underlay_segment: &carbide_test_harness::TestNetworkSegment,
) -> TestManagedHost {
    let site_explorer = env.test_harness.default_test_site_explorer();
    let host = env
        .test_harness
        .managed_host_builder(&site_explorer, *underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0;
    host.advance_to_converged_ready().await;
    host
}

async fn enter_requested_state(
    env: &Env,
    host: &TestManagedHost,
    operation: MachineMaintenanceOperation,
    entry_state: EntryState,
) {
    let mut txn = env.test_harness.db_txn().await;
    let initial_state = match entry_state {
        EntryState::Ready => {
            db::machine::clear_failure_details(&host.host.id, &mut txn)
                .await
                .unwrap();
            ManagedHostState::Ready
        }
        EntryState::Failed => {
            let details = FailureDetails {
                cause: FailureCause::UnhandledState {
                    err: "stale failure".into(),
                },
                failed_at: chrono::Utc::now(),
                source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
            };
            db::machine::update_failure_details_by_machine_id(
                &host.host.id,
                &mut txn,
                details.clone(),
            )
            .await
            .unwrap();
            ManagedHostState::Failed {
                details,
                machine_id: host.host.id.into(),
                retry_count: 1,
            }
        }
    };

    db::machine::set_machine_maintenance_requested(
        &mut txn,
        host.host.id,
        "maintenance-test",
        operation,
    )
    .await
    .unwrap();
    db::machine::update_state(&mut txn, &host.host.id, &initial_state)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

async fn reconcile(
    env: &mut Env,
    host: &TestManagedHost,
    backend: &ReconciliationComputeTrayManager,
    case: ReconciliationCase,
) -> Result<Observation, String> {
    backend.set_outcome(case.backend_outcome);
    enter_requested_state(env, host, case.operation, case.entry_state).await;

    // First iteration accepts the request from Ready or Failed.
    env.run_single_iteration().await;
    let entered = host.host.machine().await;
    if !matches!(
        entered.state.value,
        ManagedHostState::Maintenance { operation } if operation == case.operation
    ) {
        return Err(format!(
            "request did not enter Maintenance from {:?}: {:?}",
            case.entry_state, entered.state.value
        ));
    }

    // Second iteration reconciles the operation with the backend.
    env.run_single_iteration().await;
    let machine = host.host.machine().await;
    let resulting_state = match machine.state.value {
        ManagedHostState::Ready => ResultingState::Ready,
        ManagedHostState::Failed { .. } => ResultingState::Failed,
        other => return Err(format!("unexpected resulting state: {other:?}")),
    };

    Ok(Observation {
        request_cleared: machine.machine_maintenance_requested.is_none(),
        resulting_state,
        actions: backend.take_actions(),
    })
}

fn backend_cases() -> Vec<Case<ReconciliationCase, Observation, String>> {
    let mut cases = Vec::new();
    for operation in [
        MachineMaintenanceOperation::PowerOn,
        MachineMaintenanceOperation::PowerOff,
        MachineMaintenanceOperation::Reset,
    ] {
        for entry_state in [EntryState::Ready, EntryState::Failed] {
            for backend_outcome in [
                BackendOutcome::Success,
                BackendOutcome::Empty,
                BackendOutcome::NonSuccess,
                BackendOutcome::TransportFailure,
            ] {
                let succeeds = matches!(backend_outcome, BackendOutcome::Success);
                cases.push(Case {
                    scenario: Box::leak(
                        format!("{operation:?} / {entry_state:?} / {backend_outcome:?}")
                            .into_boxed_str(),
                    ),
                    input: ReconciliationCase {
                        operation,
                        entry_state,
                        backend_outcome,
                    },
                    expect: Outcome::Yields(Observation {
                        request_cleared: true,
                        resulting_state: if succeeds {
                            ResultingState::Ready
                        } else {
                            ResultingState::Failed
                        },
                        actions: vec![expected_action(operation)],
                    }),
                });
            }
        }
    }
    cases
}

#[sqlx_test]
async fn reconciles_all_power_operations_and_backend_outcomes(pool: PgPool) {
    let backend = Arc::new(ReconciliationComputeTrayManager::new());
    let env = Env::builder(pool)
        .with_component_manager(component_manager(backend.clone()))
        .with_credential_manager(valid_credential_manager())
        .build()
        .await;
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let host = create_ready_host(&env, &underlay_segment).await;
    let env = tokio::sync::Mutex::new(env);

    check_cases_async(backend_cases(), |case| {
        let env = &env;
        let backend = backend.clone();
        let host = &host;
        async move {
            let mut env = env.lock().await;
            reconcile(&mut env, host, backend.as_ref(), case).await
        }
    })
    .await;
}

async fn reconcile_precondition_failure(
    env: &mut Env,
    host: &TestManagedHost,
    operation: MachineMaintenanceOperation,
    entry_state: EntryState,
) -> Result<Observation, String> {
    enter_requested_state(env, host, operation, entry_state).await;
    env.run_single_iteration().await;
    env.run_single_iteration().await;

    let machine = host.host.machine().await;
    Ok(Observation {
        request_cleared: machine.machine_maintenance_requested.is_none(),
        resulting_state: if matches!(machine.state.value, ManagedHostState::Failed { .. }) {
            ResultingState::Failed
        } else {
            return Err(format!(
                "unexpected resulting state: {:?}",
                machine.state.value
            ));
        },
        actions: Vec::new(),
    })
}

fn precondition_cases() -> Vec<Case<(MachineMaintenanceOperation, EntryState), Observation, String>>
{
    let mut cases = Vec::new();
    for operation in [
        MachineMaintenanceOperation::PowerOn,
        MachineMaintenanceOperation::PowerOff,
        MachineMaintenanceOperation::Reset,
    ] {
        for entry_state in [EntryState::Ready, EntryState::Failed] {
            cases.push(Case {
                scenario: Box::leak(format!("{operation:?} / {entry_state:?}").into_boxed_str()),
                input: (operation, entry_state),
                expect: Outcome::Yields(Observation {
                    request_cleared: true,
                    resulting_state: ResultingState::Failed,
                    actions: Vec::new(),
                }),
            });
        }
    }
    cases
}

#[sqlx_test]
async fn clears_requests_when_component_manager_is_missing(pool: PgPool) {
    let env = Env::builder(pool)
        .with_credential_manager(valid_credential_manager())
        .build()
        .await;
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let host = create_ready_host(&env, &underlay_segment).await;
    let env = tokio::sync::Mutex::new(env);

    check_cases_async(precondition_cases(), |(operation, entry_state)| {
        let env = &env;
        let host = &host;
        async move {
            let mut env = env.lock().await;
            reconcile_precondition_failure(&mut env, host, operation, entry_state).await
        }
    })
    .await;
}

#[sqlx_test]
async fn clears_requests_when_credentials_are_missing(pool: PgPool) {
    let backend = Arc::new(ReconciliationComputeTrayManager::new());
    let env = Env::builder(pool)
        .with_component_manager(component_manager(backend.clone()))
        .with_credential_manager(Arc::new(TestCredentialManager::default()))
        .build()
        .await;
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let host = create_ready_host(&env, &underlay_segment).await;
    let env = tokio::sync::Mutex::new(env);

    check_cases_async(precondition_cases(), |(operation, entry_state)| {
        let env = &env;
        let backend = backend.clone();
        let host = &host;
        async move {
            backend.set_outcome(BackendOutcome::Success);
            let mut env = env.lock().await;
            let mut observation =
                reconcile_precondition_failure(&mut env, host, operation, entry_state).await?;
            observation.actions = backend.take_actions();
            Ok(observation)
        }
    })
    .await;
}
