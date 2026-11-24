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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Json;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse as _, Response};
use bmc_mock::test_support::{TEST_MAC_POOL, serve_https};
use bmc_mock::{DpuMachineInfo, DpuSettings, HardwareType, MachineInfo, MachineRouterOptions};
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_redfish::libredfish::{RedfishAuth, RedfishClientCreationError, RedfishClientPool};
use carbide_secrets::credentials::CredentialReader;
use carbide_test_harness::prelude::*;
use carbide_uuid::machine::DpuMachineId;
use libredfish::model::service_root::RedfishVendor;
use libredfish::{Endpoint, Redfish};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::machine::{DecommissioningState, DeconfiguringDpuState, ManagedHostState};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::env::Env;

const TASKS: &str = "/redfish/v1/TaskService/Tasks";
const EXPANDED_TASKS: &str = "/redfish/v1/TaskService/Tasks?$expand=.($levels=1)";
const SAVED_ID: &str = "tracked-bfb-task";
const SAVED_TASK: &str = "/redfish/v1/TaskService/Tasks/tracked-bfb-task";
const REPLACEMENT_TASK: &str = "/redfish/v1/TaskService/Tasks/operator-retry";
const IMAGE_URI: &str = "carbide-pxe.forge//public/blobs/internal/aarch64/preingestion.bfb";
const DPU_OS: &str = "redfish/v1/UpdateService/FirmwareInventory/DPU_OS";

#[derive(Default)]
struct TaskRequests {
    responses: VecDeque<(StatusCode, Value)>,
    requests: Vec<String>,
}

impl TaskRequests {
    async fn respond(script: Arc<Mutex<Self>>, request: Request, next: Next) -> Response {
        if !request.uri().path().starts_with(TASKS) {
            return next.run(request).await;
        }

        let mut script = script.lock().unwrap();
        script
            .requests
            .push(format!("{} {}", request.method(), request.uri()));
        let (status, body) = script
            .responses
            .pop_front()
            .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, Value::Null));
        (status, Json(body)).into_response()
    }
}

struct TaskBmcPool {
    endpoint: Endpoint,
    http: libredfish::RedfishClientPool,
    sim: Arc<RedfishSim>,
}

#[async_trait]
impl RedfishClientPool for TaskBmcPool {
    async fn create_client(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        if port != self.endpoint.port {
            return self.sim.create_client(host, port, auth, vendor).await;
        }
        self.http
            .create_client(self.endpoint.clone())
            .await
            .map_err(RedfishClientCreationError::RedfishError)
    }

    fn credential_reader(&self) -> &dyn CredentialReader {
        self.sim.credential_reader()
    }
}

struct Fixture {
    env: Env,
    host: TestManagedHost,
    script: Arc<Mutex<TaskRequests>>,
    server: bmc_mock::CombinedServer,
}

impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let machine = MachineInfo::Dpu(DpuMachineInfo::new(
            HardwareType::DellPowerEdgeR750,
            &mut TEST_MAC_POOL.lock().unwrap(),
            DpuSettings::default(),
        ));
        let (_actor, callbacks) =
            bmc_mock::simulated::SimulatedActor::new(CancellationToken::new().drop_guard());
        let (router, _) = bmc_mock::machine_router(
            &machine,
            Arc::new(callbacks),
            "vanilla-bfb-recovery".into(),
            false,
            MachineRouterOptions::default(),
        );
        let script = Arc::new(Mutex::new(TaskRequests::default()));
        let (server, url) = serve_https(
            "vanilla-bfb-recovery",
            router.layer(from_fn({
                let script = script.clone();
                move |request, next| TaskRequests::respond(script.clone(), request, next)
            })),
        );
        let port = url.port().unwrap();
        let env = Env::builder(pool)
            .with_controller_redfish_pool(move |sim, _credentials| {
                Arc::new(TaskBmcPool {
                    endpoint: Endpoint {
                        host: url.host_str().unwrap().to_owned(),
                        port: Some(port),
                        user: None,
                        password: None,
                    },
                    http: libredfish::RedfishClientPool::builder()
                        .danger_accept_invalid_certs()
                        .build()
                        .unwrap(),
                    sim,
                })
            })
            .build()
            .await;
        let domain = env.test_harness.test_domain().await;
        let network = env.test_harness.network_controller();
        let underlay = network.create_underlay_segment(&domain).await;
        network.create_admin_segment(&domain).await;
        let explorer = env.test_harness.default_test_site_explorer();
        let (host, _) = env
            .test_harness
            .managed_host_builder(&explorer, underlay)
            .build()
            .await;
        let dpu = host.dpu(0).machine().await;
        let mut bmc = dpu.status.bmc_info.clone();
        bmc.port = Some(port);
        let mut txn = env.test_harness.db_txn().await;
        db::bmc_metadata::update_bmc_network_into_machine_interfaces(&mut txn, &dpu.id, &mut bmc)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        host.advance_state(waiting_for_task(host.dpu(0).id, SAVED_ID))
            .await;
        Self {
            env,
            host,
            script,
            server,
        }
    }

    async fn tick(
        &mut self,
        expected: impl IntoIterator<Item = (&'static str, StatusCode, Value)>,
    ) {
        let expected_requests = {
            let mut script = self.script.lock().unwrap();
            script.requests.clear();
            expected
                .into_iter()
                .map(|(uri, status, body)| {
                    script.responses.push_back((status, body));
                    format!("GET {uri}")
                })
                .collect::<Vec<_>>()
        };
        self.env.run_single_iteration_without_requeue().await;
        let script = self.script.lock().unwrap();
        assert_eq!(script.requests, expected_requests);
        assert!(script.responses.is_empty());
    }

    async fn assert_error(&self, details: &[&str], includes_retry: bool) -> String {
        let machine = self.host.host.machine().await;
        assert_eq!(
            machine.current_state(),
            &waiting_for_task(self.host.dpu(0).id, SAVED_ID),
        );
        let Some(PersistentStateHandlerOutcome::Error { err, .. }) =
            machine.controller_state_outcome
        else {
            panic!(
                "expected a persisted controller error, got {:?}",
                machine.controller_state_outcome
            );
        };
        assert!(err.contains(SAVED_ID), "{err}");
        for detail in details {
            assert!(err.contains(detail), "expected {detail:?} in {err:?}");
        }
        assert_eq!(err.contains("nico-admin-cli"), includes_retry, "{err}");
        err
    }
}

fn deconfiguring(dpu: DpuMachineId, state: DeconfiguringDpuState) -> ManagedHostState {
    ManagedHostState::Decommissioning {
        decommissioning_state: DecommissioningState::DeconfiguringDpus {
            dpu_states: HashMap::from([(dpu, state)]),
        },
    }
}

fn waiting_for_task(dpu: DpuMachineId, task_id: &str) -> ManagedHostState {
    deconfiguring(
        dpu,
        DeconfiguringDpuState::WaitForInstallComplete {
            task_id: task_id.into(),
        },
    )
}

fn task(id: &str, state: &str, image: &str) -> Value {
    json!({
        "@odata.id": format!("{TASKS}/{id}"),
        "@odata.type": "#Task.v1_4_3.Task",
        "Id": id,
        "TaskState": state,
        "PercentComplete": 45,
        "Payload": {
            "HttpOperation": "POST",
            "TargetUri": "/redfish/v1/UpdateService/Actions/UpdateService.SimpleUpdate",
            "JsonBody": json!({"ImageURI": image, "Targets": [DPU_OS]}).to_string(),
        },
    })
}

fn collection(members: Vec<Value>) -> Value {
    json!({
        "@odata.id": TASKS,
        "@odata.type": "#TaskCollection.TaskCollection",
        "Name": "Task Collection",
        "Members@odata.count": members.len(),
        "Members": members,
    })
}

#[sqlx_test]
async fn vanilla_bfb_recovers_after_task_history_is_lost(pool: PgPool) {
    let mut fixture = Fixture::new(pool).await;
    // Core still tracks the install, but the BMC task history is already empty.
    fixture
        .tick([
            (SAVED_TASK, StatusCode::NOT_FOUND, Value::Null),
            (EXPANDED_TASKS, StatusCode::OK, collection(vec![])),
        ])
        .await;
    let dpu = fixture.host.dpu(0).machine().await;
    let dpu_id = fixture.host.dpu(0).id;
    let address = dpu.bmc_addr().unwrap();
    assert_eq!(address.port(), fixture.server.address.port());
    let error = fixture.assert_error(&["HTTP 404"], true).await;
    let command = format!(
        "nico-admin-cli redfish --address {address} --username \"$BMC_USERNAME\" --password \"$BMC_PASSWORD\" simple-update --image-uri {IMAGE_URI} --target {DPU_OS}"
    );
    assert!(error.contains(&command), "{error}");

    let completed = task("operator-retry", "Completed", IMAGE_URI);
    fixture
        .tick([
            (SAVED_TASK, StatusCode::NOT_FOUND, Value::Null),
            (
                EXPANDED_TASKS,
                StatusCode::OK,
                collection(vec![completed.clone()]),
            ),
        ])
        .await;
    assert_eq!(
        fixture.host.host.machine().await.current_state(),
        &waiting_for_task(dpu_id, "operator-retry")
    );

    fixture
        .tick([(REPLACEMENT_TASK, StatusCode::OK, completed)])
        .await;
    assert_eq!(
        fixture.host.host.machine().await.current_state(),
        &deconfiguring(dpu_id, DeconfiguringDpuState::WaitingForBootAfterBfbInstall)
    );
}

#[sqlx_test]
async fn vanilla_bfb_retains_diagnostics_for_a_reused_id_or_failed_install(pool: PgPool) {
    let mut fixture = Fixture::new(pool).await;
    let unrelated = task(SAVED_ID, "Completed", "http://bmc/forge.bfb");
    let mut failed = task(SAVED_ID, "Exception", IMAGE_URI);
    failed["Messages"] = json!([
        {"MessageId": "Update.1.0.TransferFailed", "Message": "BFB transfer failed"},
        {"MessageId": "Update.1.0.InstallFailed", "Message": "DPU restart rejected"}
    ]);

    let cases = [
        (
            unrelated,
            &["does not identify the expected SimpleUpdate"][..],
        ),
        (
            failed,
            &["Exception", "BFB transfer failed", "DPU restart rejected"][..],
        ),
    ];
    for (tracked, diagnostics) in cases {
        fixture
            .tick([
                (SAVED_TASK, StatusCode::OK, tracked.clone()),
                (EXPANDED_TASKS, StatusCode::OK, collection(vec![tracked])),
            ])
            .await;
        fixture.assert_error(diagnostics, true).await;
    }
}

#[sqlx_test]
async fn vanilla_bfb_retries_a_transient_poll_and_reports_failed_scans(pool: PgPool) {
    let mut fixture = Fixture::new(pool).await;
    fixture
        .tick([(SAVED_TASK, StatusCode::SERVICE_UNAVAILABLE, Value::Null)])
        .await;
    fixture
        .assert_error(&["Failed redfish operation: get_task", "503"], false)
        .await;

    fixture
        .tick([(
            SAVED_TASK,
            StatusCode::OK,
            task(SAVED_ID, "Running", IMAGE_URI),
        )])
        .await;
    let machine = fixture.host.host.machine().await;
    assert_eq!(
        machine.current_state(),
        &waiting_for_task(fixture.host.dpu(0).id, SAVED_ID)
    );
    let Some(PersistentStateHandlerOutcome::Wait { reason, .. }) = machine.controller_state_outcome
    else {
        panic!(
            "expected a persisted progress outcome, got {:?}",
            machine.controller_state_outcome
        );
    };
    assert!(
        reason.contains(SAVED_ID) && reason.ends_with("45%"),
        "{reason}"
    );

    for (status, response, error) in [
        (StatusCode::SERVICE_UNAVAILABLE, Value::Null, "503"),
        (
            StatusCode::OK,
            collection(vec![json!({"@odata.id": REPLACEMENT_TASK})]),
            "missing field",
        ),
    ] {
        fixture
            .tick([
                (SAVED_TASK, StatusCode::NOT_FOUND, Value::Null),
                (EXPANDED_TASKS, status, response),
            ])
            .await;
        fixture
            .assert_error(
                &[
                    "Failed redfish operation: get_collection",
                    "HTTP 404",
                    "failed to scan",
                    error,
                ],
                false,
            )
            .await;
    }
}
