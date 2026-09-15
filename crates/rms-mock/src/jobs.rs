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

//! Asynchronous job tracking.
//!
//! RMS answers long-running requests with a job id that the caller then polls.
//! The mock models that faithfully but deterministically: a job advances by
//! being observed rather than by elapsed time, so a test never waits and never
//! races.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::rms;

/// Prefix of generated job ids, so a job id in a log is recognisable as the
/// mock's.
const JOB_ID_PREFIX: &str = "rms-mock";

/// Jobs advance per poll: the first poll sees a job running and the second
/// sees its terminal state, which exercises the caller's polling loop without
/// stalling ingestion.
const RUNNING_AFTER_OBSERVATIONS: u32 = 1;
const TERMINAL_AFTER_OBSERVATIONS: u32 = 2;

/// Where a job has got to.
///
/// The wire encoding differs per RPC - some report an enum, others a free-form
/// lowercase string - so the state is kept abstract here and rendered at the
/// edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
}

impl JobState {
    /// The `JobExecutionState` enum value for the RPCs that report state as an
    /// enum rather than a string.
    ///
    /// Never `Unspecified`: that is the proto3 default, and a caller that
    /// receives it cannot tell the job apart from one whose state was never
    /// set, so it reports the outcome as unknown.
    pub(crate) fn as_execution_state(self) -> i32 {
        let state = match self {
            Self::Queued => rms::JobExecutionState::Queued,
            Self::Running => rms::JobExecutionState::Running,
            Self::Completed => rms::JobExecutionState::Completed,
            Self::Failed => rms::JobExecutionState::Failed,
        };
        state as i32
    }

    /// The lowercase spelling used by the RPCs that report state as a string.
    ///
    /// These exact words matter: a caller that does not recognise the string
    /// treats the job as still in progress and polls forever, so an
    /// unrecognised spelling is a silent hang rather than an error.
    pub(crate) fn as_wire_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

struct Job {
    node_id: String,
    rack_id: String,
    /// Why the job is to end in failure rather than completion, when it is.
    failure: Option<String>,
    observations: u32,
}

/// Every job the mock has handed out.
pub(crate) struct JobStore {
    jobs: Mutex<HashMap<String, Job>>,
    next_id: AtomicU64,
}

/// What a poll of a job returned.
pub(crate) struct JobStatus {
    pub(crate) state: JobState,
    pub(crate) node_id: String,
    pub(crate) rack_id: String,
    /// Why the job failed; empty unless `state` is [`JobState::Failed`].
    pub(crate) error_message: String,
}

impl JobStore {
    pub(crate) fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Start a job that will complete, and return its id.
    ///
    /// Ids are sequential rather than random so that a failing test names the
    /// same job every run.
    pub(crate) fn start(&self, node_id: &str, rack_id: &str) -> String {
        self.insert(node_id, rack_id, None)
    }

    /// Start a job that will fail with `error`, and return its id.
    ///
    /// It is paced like a completing job and reports [`JobState::Failed`]
    /// where that reports [`JobState::Completed`].
    pub(crate) fn start_failing(&self, node_id: &str, rack_id: &str, error: String) -> String {
        self.insert(node_id, rack_id, Some(error))
    }

    fn insert(&self, node_id: &str, rack_id: &str, failure: Option<String>) -> String {
        let id = format!(
            "{JOB_ID_PREFIX}-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed)
        );
        crate::lock(&self.jobs).insert(
            id.clone(),
            Job {
                node_id: node_id.to_owned(),
                rack_id: rack_id.to_owned(),
                failure,
                observations: 0,
            },
        );
        id
    }

    /// Poll a job, advancing it.
    ///
    /// A job this process never issued is reported complete. NICo persists job
    /// ids in its database, so after its host restarts the mock is polled for
    /// jobs that no longer exist, and failing those polls would strand every
    /// switch that had a configuration in flight. The proto says such a poll
    /// should fail; this is a deliberate divergence in favour of a mock that
    /// survives its host's restart.
    pub(crate) fn observe(&self, job_id: &str) -> JobStatus {
        let mut jobs = crate::lock(&self.jobs);
        let Some(job) = jobs.get_mut(job_id) else {
            tracing::debug!(job_id, "Reporting an unknown job as complete");
            return JobStatus {
                state: JobState::Completed,
                node_id: String::new(),
                rack_id: String::new(),
                error_message: String::new(),
            };
        };

        job.observations += 1;
        let (state, error_message) = if job.observations >= TERMINAL_AFTER_OBSERVATIONS {
            match &job.failure {
                Some(error) => (JobState::Failed, error.clone()),
                None => (JobState::Completed, String::new()),
            }
        } else if job.observations >= RUNNING_AFTER_OBSERVATIONS {
            (JobState::Running, String::new())
        } else {
            (JobState::Queued, String::new())
        };

        JobStatus {
            state,
            node_id: job.node_id.clone(),
            rack_id: job.rack_id.clone(),
            error_message,
        }
    }
}
