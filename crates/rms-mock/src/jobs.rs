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
//! A job advances each time it is polled rather than with time.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{JobPacing, UnknownJobPolicy};
use crate::rms;

/// Where a job has got to; rendered per RPC as an enum or a string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
}

impl JobState {
    /// The `JobExecutionState` value; never `Unspecified`.
    pub(crate) fn as_execution_state(self) -> i32 {
        let state = match self {
            Self::Queued => rms::JobExecutionState::Queued,
            Self::Running => rms::JobExecutionState::Running,
            Self::Completed => rms::JobExecutionState::Completed,
            Self::Failed => rms::JobExecutionState::Failed,
        };
        state as i32
    }

    /// The lowercase spelling for the RPCs that report state as a string.
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
    prefix: String,
    pacing: JobPacing,
    unknown: UnknownJobPolicy,
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
    pub(crate) fn new(prefix: String, pacing: JobPacing, unknown: UnknownJobPolicy) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            prefix,
            pacing,
            unknown,
        }
    }

    /// Start a job that will complete, and return its id.
    pub(crate) fn start(&self, node_id: &str, rack_id: &str) -> String {
        self.insert(node_id, rack_id, None)
    }

    /// Start a job that will fail with `error`, and return its id.
    ///
    /// It is paced like any other job, so the caller's polling loop sees it
    /// queued and running before it reports [`JobState::Failed`] where a
    /// completing job reports [`JobState::Completed`].
    pub(crate) fn start_failing(&self, node_id: &str, rack_id: &str, error: String) -> String {
        self.insert(node_id, rack_id, Some(error))
    }

    fn insert(&self, node_id: &str, rack_id: &str, failure: Option<String>) -> String {
        let id = format!(
            "{}-{}",
            self.prefix,
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
    /// Returns `None` only when the job is unknown and the configured policy
    /// says to report that as an error.
    pub(crate) fn observe(&self, job_id: &str) -> Option<JobStatus> {
        let mut jobs = crate::lock(&self.jobs);
        let Some(job) = jobs.get_mut(job_id) else {
            return self.unknown_job(job_id);
        };

        job.observations += 1;
        let (state, error_message) = if job.observations >= self.pacing.terminal_after_observations
        {
            match &job.failure {
                Some(error) => (JobState::Failed, error.clone()),
                None => (JobState::Completed, String::new()),
            }
        } else if job.observations >= self.pacing.running_after_observations {
            (JobState::Running, String::new())
        } else {
            (JobState::Queued, String::new())
        };

        Some(JobStatus {
            state,
            node_id: job.node_id.clone(),
            rack_id: job.rack_id.clone(),
            error_message,
        })
    }

    /// How to answer for a job this process never issued.
    ///
    /// NICo persists job ids in its database, so after its host restarts the
    /// mock is polled for jobs that no longer exist. Reporting those as
    /// failures would strand every switch that had a configuration in flight,
    /// which is why the default is to call them complete. The proto says such
    /// a poll should fail; this is a deliberate divergence in favour of a mock
    /// that survives its host's restart.
    fn unknown_job(&self, job_id: &str) -> Option<JobStatus> {
        match self.unknown {
            UnknownJobPolicy::Complete => {
                tracing::debug!(job_id, "Reporting an unknown job as complete");
                Some(JobStatus {
                    state: JobState::Completed,
                    node_id: String::new(),
                    rack_id: String::new(),
                    error_message: String::new(),
                })
            }
            UnknownJobPolicy::Fail => Some(JobStatus {
                state: JobState::Failed,
                node_id: String::new(),
                rack_id: String::new(),
                error_message: String::new(),
            }),
            UnknownJobPolicy::NotFound => None,
        }
    }
}
