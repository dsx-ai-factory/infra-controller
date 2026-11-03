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

use std::collections::BTreeSet;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use bmc_mock::test_support::TestBmc;
use bmc_mock::test_support::axum_http_client::AxumRouterHttpClient;
use carbide_test_support::value_scenarios;
use nv_redfish::ServiceRoot;
use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};
use nv_redfish::core::AsyncTask;
use serde_json::{Value, json};
use url::Url;

use super::*;
use crate::test_support::endpoint;

fn with_envelope(uri: &str, id: &str, fields: Value) -> Value {
    let mut body = json!({"@odata.id": uri, "Id": id, "Name": id});
    body.as_object_mut()
        .expect("body is an object")
        .extend(fields.as_object().expect("fields are an object").clone());
    body
}

fn task(fields: Value) -> Task {
    serde_json::from_value(with_envelope(
        "/redfish/v1/TaskService/Tasks/7",
        "7",
        fields,
    ))
    .expect("task body")
}

fn job(fields: Value) -> Job {
    serde_json::from_value(with_envelope("/redfish/v1/JobService/Jobs/3", "3", fields))
        .expect("job body")
}

fn dell_job(fields: Value) -> DellJob {
    serde_json::from_value(with_envelope(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/JID_1",
        "JID_1",
        fields,
    ))
    .expect("Dell job body")
}

fn failed_with(state: &str, message: &str) -> Result<OperationStatus, ()> {
    Ok(OperationStatus::Failed {
        state: state.to_string(),
        message: Some(message.to_string()),
    })
}

fn needs_intervention(state: &str) -> Result<OperationStatus, ()> {
    Ok(OperationStatus::NeedsIntervention {
        state: state.to_string(),
    })
}

#[test]
fn task_states_classify_by_whether_the_work_can_still_finish() {
    value_scenarios!(run = |fields: Value| task_status(&task(fields)).map_err(drop);
        "still running" {
            json!({}) => Ok(OperationStatus::Running),
            json!({"TaskState": "Running"}) => Ok(OperationStatus::Running),
            json!({"TaskState": "Interrupted"}) => Ok(OperationStatus::Running),
        }
        "terminal" {
            json!({"TaskState": "Completed"}) => Ok(OperationStatus::Completed),
            json!({
                "TaskState": "Exception",
                "Messages": [
                    {"MessageId": "Base.1.0.GeneralError", "Message": "flash failed"},
                    {"MessageId": "Base.1.0.GeneralError", "Message": "rolled back"},
                ],
            }) => failed_with("Exception", "flash failed; rolled back"),
        }
        "unrecognized" {
            json!({"TaskState": "Exploded"}) => Err(()),
        }
    );
}

#[test]
fn job_states_separate_operator_waits_from_failures() {
    value_scenarios!(run = |fields: Value| job_status(&job(fields)).map_err(drop);
        "still running" {
            json!({"JobState": "Interrupted"}) => Ok(OperationStatus::Running),
        }
        "terminal" {
            json!({"JobState": "Completed"}) => Ok(OperationStatus::Completed),
            json!({
                "JobState": "Invalid",
                "Messages": [{"MessageId": "Base.1.0.GeneralError", "Message": "bad payload"}],
            }) => failed_with("Invalid", "bad payload"),
        }
        "operator" {
            json!({"JobState": "UserIntervention"}) => needs_intervention("UserIntervention"),
            json!({"JobState": "Suspended"}) => needs_intervention("Suspended"),
        }
    );
}

#[test]
fn dell_job_states_report_jobs_that_wait_for_a_host_reset() {
    value_scenarios!(run = |fields: Value| dell_job_status(&dell_job(fields)).map_err(drop);
        "awaiting reset" {
            json!({"JobState": "Scheduled", "Message": "Task successfully scheduled."})
                => Ok(OperationStatus::AwaitingReset),
            json!({"JobState": "PendingActivation"}) => Ok(OperationStatus::AwaitingReset),
        }
        "still running" {
            json!({"JobState": "Running"}) => Ok(OperationStatus::Running),
        }
        "terminal" {
            json!({"JobState": "RebootCompleted"}) => Ok(OperationStatus::Completed),
            json!({"JobState": "CompletedWithErrors", "Message": "SYS051"})
                => failed_with("CompletedWithErrors", "SYS051"),
            json!({"JobState": "Scheduled", "Message": JOB_INITIALIZATION_FAILURE})
                => failed_with("ScheduledWithErrors", JOB_INITIALIZATION_FAILURE),
        }
        "operator" {
            json!({"JobState": "Paused"}) => needs_intervention("Paused"),
        }
        "invalid" {
            json!({}) => Err(()),
            json!({"JobState": "Unknown"}) => Err(()),
        }
    );
}

/// One poll's result and the status it should read as.
type Case = (
    &'static str,
    Result<ModificationResponse<Value>, PlatformError>,
    Result<OperationStatus, ()>,
);

/// A task URI can name a Task Monitor, which answers with the operation's
/// own response once the work finishes, rather than with a Task.
#[test]
fn task_polls_read_monitors_and_tasks_alike() {
    let pending = || {
        Ok(ModificationResponse::Task(AsyncTask {
            location: ODataId::from("/redfish/v1/TaskMonitor/1".to_string()).into(),
            retry_after: None,
            task: None,
        }))
    };
    let status = |polled| match task_poll(polled) {
        Ok(TaskPoll::Status(status)) => Ok(status),
        Ok(TaskPoll::Task(task)) => task_status(&task).map_err(|_| "unmapped task".to_string()),
        Err(error) => Err(error.to_string()),
    };
    let bmc_error = |status| {
        Err(PlatformError::Bmc {
            status,
            message_id: None,
            message: "refused".to_string(),
        })
    };
    let cases: [Case; 7] = [
        (
            "monitor still running",
            pending(),
            Ok(OperationStatus::Running),
        ),
        (
            "monitor finished without a body",
            Ok(ModificationResponse::Empty),
            Ok(OperationStatus::Completed),
        ),
        (
            "monitor finished with Supermicro's response",
            Ok(ModificationResponse::Entity(json!({
                "Success": {"code": "Base.1.10.Success", "message": "Successfully Completed Request."}
            }))),
            Ok(OperationStatus::Completed),
        ),
        (
            "a Task resource still running",
            Ok(ModificationResponse::Entity(with_envelope(
                "/redfish/v1/TaskService/Tasks/7",
                "7",
                json!({"TaskState": "Running"}),
            ))),
            Ok(OperationStatus::Running),
        ),
        (
            "monitor reports the operation rejected",
            bmc_error(400),
            Ok(OperationStatus::Failed {
                state: "HTTP 400".to_string(),
                message: Some("refused".to_string()),
            }),
        ),
        ("monitor gone", bmc_error(404), Err(())),
        ("monitor removed", bmc_error(410), Err(())),
    ];
    for (scenario, polled, expected) in cases {
        assert_eq!(status(polled).map_err(drop), expected, "{scenario}");
    }
}

#[test]
fn a_task_that_transitioned_to_a_job_names_that_job() {
    let transitioned = task(json!({
        "TaskState": "Running",
        "Messages": [{
            "MessageId": TRANSITIONED_TO_JOB,
            "MessageArgs": ["/redfish/v1/JobService/Jobs/3"],
        }],
    }));
    assert_eq!(
        transitioned_job(&transitioned),
        Some(ODataId::from("/redfish/v1/JobService/Jobs/3".to_string()))
    );
    assert_eq!(
        transitioned_job(&task(json!({"TaskState": "Running"}))),
        None
    );
}

/// A BMC whose task 2 finished: its monitor is gone and the task remains.
async fn finished_task_bmc() -> ConnectedBmc<TestBmc> {
    let router = Router::new()
        .route(
            "/redfish/v1",
            get(|| async {
                axum::Json(json!({
                    "@odata.id": "/redfish/v1", "Id": "RootService", "Name": "Root Service",
                    "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}},
                }))
            }),
        )
        .route(
            "/redfish/v1/TaskService/TaskMonitors/2",
            get(|| async { StatusCode::NOT_FOUND }),
        )
        .route(
            "/redfish/v1/TaskService/Tasks/2",
            get(|| async {
                axum::Json(with_envelope(
                    "/redfish/v1/TaskService/Tasks/2",
                    "2",
                    json!({"TaskState": "Completed", "TaskStatus": "OK"}),
                ))
            }),
        );
    let bmc = Arc::new(HttpBmc::new(
        AxumRouterHttpClient::new(router),
        Url::parse("https://bmc.test").expect("valid URL"),
        BmcCredentials::new("root".to_string(), "password".to_string()),
        CacheSettings::with_capacity(0),
    ));
    let root = ServiceRoot::new(bmc.clone())
        .await
        .expect("test BMC serves a service root");
    ConnectedBmc::new(endpoint(BTreeSet::new()), bmc, Arc::new(root), None)
}

/// A removed monitor is read through the Task its `202` named; without one,
/// the monitor's 404 is the answer.
#[tokio::test]
async fn a_removed_monitor_reads_the_task_its_202_named() {
    let monitor = |task: Option<&str>| OperationReference::RedfishTask {
        uri: ODataId::from("/redfish/v1/TaskService/TaskMonitors/2".to_string()),
        retry_after_seconds: None,
        task: task.map(|task| ODataId::from(task.to_string())),
    };
    let cases = [
        (
            "the 202 named its Task",
            monitor(Some("/redfish/v1/TaskService/Tasks/2")),
            Ok(OperationStatus::Completed),
        ),
        ("the 202 named no Task", monitor(None), Err(())),
    ];
    let bmc = finished_task_bmc().await;
    for (scenario, reference, expected) in cases {
        assert_eq!(
            bmc.operation_status(&reference).await.map_err(drop),
            expected,
            "{scenario}"
        );
    }
}
