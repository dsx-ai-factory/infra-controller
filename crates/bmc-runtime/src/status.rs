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

//! One-shot status of asynchronous BMC work a driver handed back.

use std::sync::Arc;

use bmc_platform::{ClassifyBmcError, OperationReference, OperationStatus, PlatformError};
use nv_redfish::Bmc;
use nv_redfish::core::{EntityTypeRef, ModificationResponse, ODataId};
use nv_redfish::oem::dell::schema::dell_job::{DellJob, JobState as DellJobState};
use nv_redfish::schema::job::{Job, JobState};
use nv_redfish::schema::message::Message;
use nv_redfish::schema::task::{Task, TaskState};
use serde::Deserialize;
use serde_json::Value;

use crate::ConnectedBmc;
use crate::observe::observe;

/// The message of a `Scheduled` iDRAC job that will never start.
const JOB_INITIALIZATION_FAILURE: &str = "Job processing initialization failure.";

/// The message by which a task hands its work to the job named in its first argument.
const TRANSITIONED_TO_JOB: &str = "Update.1.0.OperationTransitionedToJob";

impl<B: Bmc + 'static> ConnectedBmc<B>
where
    B::Error: ClassifyBmcError,
{
    /// Reads where the work behind `reference` stands, without waiting.
    ///
    /// A task that handed its work to a job reports the job's status.
    pub async fn operation_status(
        &self,
        reference: &OperationReference,
    ) -> Result<OperationStatus, PlatformError> {
        observe(self.endpoint(), None, "operation_status", async {
            self.read_operation_status(reference).await
        })
        .await
    }

    async fn read_operation_status(
        &self,
        reference: &OperationReference,
    ) -> Result<OperationStatus, PlatformError> {
        match reference {
            OperationReference::RedfishTask { uri, task, .. } => {
                let mut polled = self.poll(uri).await;
                // A service can remove a task monitor once the work completes
                // (DSP0266 12.2); the Task its 202 named still reports how it
                // ended.
                if matches!(
                    polled,
                    Err(PlatformError::Bmc {
                        status: 404 | 410,
                        ..
                    })
                ) && let Some(task) = task
                {
                    polled = self.poll(task).await;
                }
                match task_poll(polled)? {
                    TaskPoll::Status(status) => Ok(status),
                    TaskPoll::Task(task) => match transitioned_job(&task) {
                        Some(job) => job_status(&*self.get::<Job>(&job).await?),
                        None => task_status(&task),
                    },
                }
            }
            OperationReference::VendorJob { uri, .. } => {
                dell_job_status(&*self.get::<DellJob>(uri).await?)
            }
        }
    }

    async fn poll(&self, uri: &ODataId) -> Result<ModificationResponse<Value>, PlatformError> {
        self.bmc()
            .poll::<Value>(uri)
            .await
            .map_err(ClassifyBmcError::classify)
    }

    async fn get<T>(&self, uri: &ODataId) -> Result<Arc<T>, PlatformError>
    where
        T: EntityTypeRef + for<'de> Deserialize<'de> + Send + Sync + 'static,
    {
        self.bmc()
            .get::<T>(uri)
            .await
            .map_err(ClassifyBmcError::classify)
    }
}

/// What one poll of a task URI says.
#[derive(Debug)]
enum TaskPoll {
    Status(OperationStatus),
    /// A Task resource, whose state and messages say where the work stands.
    Task(Box<Task>),
}

/// Reads one poll of a task URI, which names either a Task or a Task
/// Monitor. A Task answers with itself throughout. A monitor answers 202
/// while the work runs and, once it finishes, with the operation's own
/// response, such as Supermicro's `{"Success": …}`; a 4xx then means the
/// operation was rejected, except a 404 or 410, which means the monitor is
/// gone rather than that the work failed.
fn task_poll(
    polled: Result<ModificationResponse<Value>, PlatformError>,
) -> Result<TaskPoll, PlatformError> {
    match polled {
        Ok(ModificationResponse::Task(_)) => Ok(TaskPoll::Status(OperationStatus::Running)),
        Ok(ModificationResponse::Empty | ModificationResponse::Message(_)) => {
            Ok(TaskPoll::Status(OperationStatus::Completed))
        }
        Ok(ModificationResponse::Entity(body)) => Ok(match serde_json::from_value::<Task>(body) {
            Ok(task) => TaskPoll::Task(Box::new(task)),
            Err(_) => TaskPoll::Status(OperationStatus::Completed),
        }),
        Err(PlatformError::Bmc {
            status: status @ 400..=499,
            message,
            ..
        }) if !matches!(status, 404 | 410) => Ok(TaskPoll::Status(OperationStatus::Failed {
            state: format!("HTTP {status}"),
            message: Some(message),
        })),
        Err(error) => Err(error),
    }
}

/// The job a task handed its work to, if it did.
fn transitioned_job(task: &Task) -> Option<ODataId> {
    task.messages
        .iter()
        .flatten()
        .find(|message| message.message_id == TRANSITIONED_TO_JOB)?
        .message_args
        .as_ref()?
        .first()
        .map(|job| ODataId::from(job.clone()))
}

fn failed(state: String, message: Option<String>) -> OperationStatus {
    OperationStatus::Failed { state, message }
}

fn joined_messages(messages: Option<&Vec<Message>>) -> Option<String> {
    let text: Vec<&str> = messages
        .into_iter()
        .flatten()
        .filter_map(|message| message.message.as_deref())
        .collect();
    (!text.is_empty()).then(|| text.join("; "))
}

fn unrecognized(kind: &str, uri: &ODataId) -> PlatformError {
    PlatformError::InvalidResponse {
        message: format!("{kind} {uri} reports a state this schema does not define"),
    }
}

/// `Interrupted` is not terminal: the Redfish schema expects interrupted
/// work to restart.
fn task_status(task: &Task) -> Result<OperationStatus, PlatformError> {
    Ok(match task.task_state {
        None
        | Some(
            TaskState::New
            | TaskState::Starting
            | TaskState::Running
            | TaskState::Suspended
            | TaskState::Interrupted
            | TaskState::Pending
            | TaskState::Stopping
            | TaskState::Service
            | TaskState::Cancelling,
        ) => OperationStatus::Running,
        Some(TaskState::Completed) => OperationStatus::Completed,
        Some(state @ (TaskState::Killed | TaskState::Exception | TaskState::Cancelled)) => failed(
            format!("{state:?}"),
            joined_messages(task.messages.as_ref()),
        ),
        Some(TaskState::UnsupportedValue) => return Err(unrecognized("task", &task.odata_id)),
    })
}

/// A suspended job resumes only through its `Resume` action, so it waits on
/// an operator like one in `UserIntervention`.
fn job_status(job: &Job) -> Result<OperationStatus, PlatformError> {
    Ok(match job.job_state {
        None
        | Some(
            JobState::New
            | JobState::Starting
            | JobState::Running
            | JobState::Interrupted
            | JobState::Pending
            | JobState::Stopping
            | JobState::Service
            | JobState::Continue
            | JobState::Validating,
        ) => OperationStatus::Running,
        Some(JobState::Completed) => OperationStatus::Completed,
        Some(state @ (JobState::Exception | JobState::Cancelled | JobState::Invalid)) => {
            failed(format!("{state:?}"), joined_messages(job.messages.as_ref()))
        }
        Some(state @ (JobState::UserIntervention | JobState::Suspended)) => {
            OperationStatus::NeedsIntervention {
                state: format!("{state:?}"),
            }
        }
        Some(JobState::UnsupportedValue) => return Err(unrecognized("job", &job.odata_id)),
    })
}

/// Only Dell iDRAC produces [`OperationReference::VendorJob`]. A job that
/// reports no `JobState` is an invalid response, not a running job.
fn dell_job_status(job: &DellJob) -> Result<OperationStatus, PlatformError> {
    let state = job
        .job_state
        .flatten()
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("Dell job {} reports no JobState", job.id),
        })?;
    let message = job.message.clone().flatten();
    Ok(match state {
        DellJobState::Scheduled if message.as_deref() == Some(JOB_INITIALIZATION_FAILURE) => {
            failed("ScheduledWithErrors".to_string(), message)
        }
        DellJobState::Scheduled | DellJobState::PendingActivation => OperationStatus::AwaitingReset,
        DellJobState::New
        | DellJobState::Scheduling
        | DellJobState::Downloading
        | DellJobState::Downloaded
        | DellJobState::ReadyForExecution
        | DellJobState::Waiting
        | DellJobState::Running
        | DellJobState::RebootPending => OperationStatus::Running,
        DellJobState::Completed | DellJobState::RebootCompleted => OperationStatus::Completed,
        DellJobState::Failed | DellJobState::CompletedWithErrors | DellJobState::RebootFailed => {
            failed(format!("{state:?}"), message)
        }
        DellJobState::Paused | DellJobState::UserIntervention => {
            OperationStatus::NeedsIntervention {
                state: format!("{state:?}"),
            }
        }
        DellJobState::Unknown | DellJobState::UnsupportedValue => {
            return Err(unrecognized("Dell job", &job.odata_id));
        }
    })
}

#[cfg(test)]
mod tests;
