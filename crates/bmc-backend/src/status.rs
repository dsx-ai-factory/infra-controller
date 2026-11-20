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

use bmc_platform::{
    DriverOutcome, OperationReference, OperationStatus, PlatformError, VendorJobId,
};
use libredfish::model::task::{Task, TaskState};
use libredfish::{JobState, Redfish};

use crate::BackendBmc;
use crate::convert::platform_error;

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::operation_status`].
    pub async fn operation_status(
        &self,
        reference: &OperationReference,
    ) -> Result<OperationStatus, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                libredfish_operation_status(client.as_ref(), reference).await
            }
            // Boxed so callers' futures stay within the stack limit.
            Self::Drivers(bmc) => Box::pin(bmc.operation_status(reference)).await,
        }
    }
}

/// Where the work behind `reference` stands through libredfish, which reads
/// iDRAC jobs and the Tasks it knows the id of.
pub async fn libredfish_operation_status(
    client: &dyn Redfish,
    reference: &OperationReference,
) -> Result<OperationStatus, PlatformError> {
    match reference {
        OperationReference::VendorJob { job_id, .. } => client
            .get_job_state(job_id.as_str())
            .await
            .map(job_status)
            .map_err(platform_error),
        OperationReference::RedfishTask { .. } => {
            let task_id = redfish_task_id(reference).ok_or(PlatformError::Unsupported)?;
            let task = client.get_task(&task_id).await.map_err(platform_error)?;
            task_status(task)
        }
    }
}

/// The iDRAC configuration job known by `job_id`.
pub fn idrac_job(job_id: VendorJobId) -> OperationReference {
    OperationReference::VendorJob {
        uri: format!("/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/{job_id}").into(),
        job_id,
        retry_after_seconds: None,
    }
}

/// The iDRAC job a libredfish write started, by the id it returned.
pub fn idrac_outcome(job_id: Option<String>) -> DriverOutcome {
    job_id
        .and_then(|job_id| VendorJobId::new(job_id).ok())
        .map_or_else(DriverOutcome::complete, |job_id| {
            DriverOutcome::accepted(idrac_job(job_id))
        })
}

/// The Task resource known by `task_id`, for work tracked by libredfish's
/// Task id.
pub fn redfish_task(task_id: &str) -> OperationReference {
    OperationReference::RedfishTask {
        uri: format!("/redfish/v1/TaskService/Tasks/{task_id}").into(),
        retry_after_seconds: None,
        task: None,
    }
}

/// The libredfish Task id of `reference`, from the Task the BMC named or else
/// the URI it polls, when either lies under `TaskService/Tasks`.
pub fn redfish_task_id(reference: &OperationReference) -> Option<String> {
    let OperationReference::RedfishTask { uri, task, .. } = reference else {
        return None;
    };
    task.iter().chain([uri]).find_map(|id| {
        id.to_string()
            .split('/')
            .skip_while(|segment| *segment != "Tasks")
            .nth(1)
            .filter(|task_id| !task_id.is_empty())
            .map(str::to_string)
    })
}

/// An iDRAC job state libredfish does not recognize is still running.
fn job_status(state: JobState) -> OperationStatus {
    match state {
        JobState::Scheduled => OperationStatus::AwaitingReset,
        JobState::Running | JobState::Unknown => OperationStatus::Running,
        JobState::Completed => OperationStatus::Completed,
        JobState::ScheduledWithErrors | JobState::CompletedWithErrors | JobState::Failed => {
            OperationStatus::Failed {
                state: format!("{state:?}"),
                message: None,
            }
        }
    }
}

/// A Task that reports no state is an empty response.
fn task_status(task: Task) -> Result<OperationStatus, PlatformError> {
    Ok(match task.task_state.ok_or(PlatformError::NoContent)? {
        TaskState::New | TaskState::Starting | TaskState::Running | TaskState::Pending => {
            OperationStatus::Running
        }
        TaskState::Completed => OperationStatus::Completed,
        state => OperationStatus::Failed {
            state: format!("{state:?}"),
            message: task
                .messages
                .into_iter()
                .next()
                .map(|message| message.message),
        },
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn idrac_job_states_map_to_operation_statuses() {
        let failed = |state: &str| OperationStatus::Failed {
            state: state.to_string(),
            message: None,
        };
        value_scenarios!(job_status:
            "awaiting the host restart" {
                JobState::Scheduled => OperationStatus::AwaitingReset,
            }
            "running, including states libredfish does not recognize" {
                JobState::Running => OperationStatus::Running,
                JobState::Unknown => OperationStatus::Running,
            }
            "completed" {
                JobState::Completed => OperationStatus::Completed,
            }
            "ended without completing" {
                JobState::ScheduledWithErrors => failed("ScheduledWithErrors"),
                JobState::CompletedWithErrors => failed("CompletedWithErrors"),
                JobState::Failed => failed("Failed"),
            }
        );
    }

    #[test]
    fn a_libredfish_job_id_is_the_idrac_job_to_poll() {
        let job = || VendorJobId::new("JID_42".to_string()).expect("nonempty");
        value_scenarios!(idrac_outcome:
            "a started job" {
                Some("JID_42".to_string()) => DriverOutcome::accepted(idrac_job(job())),
            }
            "no job" {
                None => DriverOutcome::complete(),
                Some(" ".to_string()) => DriverOutcome::complete(),
            }
        );
    }

    #[test]
    fn a_task_is_known_by_the_id_under_its_task_service() {
        let task = |uri: &str, task: Option<&str>| OperationReference::RedfishTask {
            uri: uri.to_string().into(),
            retry_after_seconds: None,
            task: task.map(|task| task.to_string().into()),
        };
        let job = OperationReference::VendorJob {
            uri: "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/JID_42"
                .to_string()
                .into(),
            job_id: "JID_42".parse().expect("nonempty"),
            retry_after_seconds: None,
        };
        value_scenarios!(run = |reference: OperationReference| redfish_task_id(&reference);
            "the Task the BMC named" {
                task(
                    "/redfish/v1/TaskService/TaskMonitors/7",
                    Some("/redfish/v1/TaskService/Tasks/7"),
                ) => Some("7".to_string()),
            }
            "the URI it polls" {
                task("/redfish/v1/TaskService/Tasks/7/Monitor", None) => Some("7".to_string()),
                redfish_task("dpu-certificate-task") => Some("dpu-certificate-task".to_string()),
            }
            "no Task" {
                task("/redfish/v1/TaskService/TaskMonitors/7", None) => None,
                job => None,
            }
        );
    }

    #[test]
    fn task_states_map_to_operation_statuses() {
        let task = |task_state| Task {
            task_state,
            ..serde_json::from_value(serde_json::json!({
                "@odata.id": "/redfish/v1/TaskService/Tasks/7",
                "@odata.type": "#Task.v1_4_3.Task",
                "Id": "7",
            }))
            .expect("minimal task")
        };
        value_scenarios!(run = |task_state: Option<TaskState>| task_status(task(task_state));
            "running" {
                Some(TaskState::New) => Ok(OperationStatus::Running),
                Some(TaskState::Pending) => Ok(OperationStatus::Running),
            }
            "completed" {
                Some(TaskState::Completed) => Ok(OperationStatus::Completed),
            }
            "ended without completing" {
                Some(TaskState::Suspended) => Ok(OperationStatus::Failed {
                    state: "Suspended".to_string(),
                    message: None,
                }),
                Some(TaskState::Exception) => Ok(OperationStatus::Failed {
                    state: "Exception".to_string(),
                    message: None,
                }),
            }
            "reports no state" {
                None => Err(PlatformError::NoContent),
            }
        );
    }
}
