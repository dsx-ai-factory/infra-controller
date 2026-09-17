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

use crate::rms;

/// Prefix of generated job ids.
const JOB_ID_PREFIX: &str = "rms-mock";

/// The poll on which a job reports its terminal state; earlier polls see it
/// running.
const TERMINAL_AFTER_OBSERVATIONS: u32 = 2;

/// Where a job has got to; rendered per RPC as an enum or a string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobState {
    Running,
    Completed,
    Failed,
}

impl JobState {
    /// The `JobExecutionState` value; never `Unspecified`.
    pub(crate) fn as_execution_state(self) -> i32 {
        let state = match self {
            Self::Running => rms::JobExecutionState::Running,
            Self::Completed => rms::JobExecutionState::Completed,
            Self::Failed => rms::JobExecutionState::Failed,
        };
        state as i32
    }

    /// The lowercase spelling for the RPCs that report state as a string.
    pub(crate) fn as_wire_str(self) -> &'static str {
        match self {
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

/// Every job handed out and not yet seen complete.
///
/// Starting a job for a node drops any earlier job for it, and a completed
/// job is forgotten.
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
    pub(crate) fn start(&self, node_id: &str, rack_id: &str) -> String {
        self.insert(node_id, rack_id, None)
    }

    /// Start a job that will fail with `error`, and return its id.
    pub(crate) fn start_failing(&self, node_id: &str, rack_id: &str, error: String) -> String {
        self.insert(node_id, rack_id, Some(error))
    }

    fn insert(&self, node_id: &str, rack_id: &str, failure: Option<String>) -> String {
        let id = format!(
            "{JOB_ID_PREFIX}-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let mut jobs = crate::lock(&self.jobs);
        jobs.retain(|_, job| job.node_id != node_id || job.rack_id != rack_id);
        jobs.insert(
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

    /// Poll a job, advancing it. A job this process never issued is reported
    /// complete.
    pub(crate) fn observe(&self, job_id: &str) -> JobStatus {
        let mut jobs = crate::lock(&self.jobs);
        let Some(job) = jobs.get_mut(job_id) else {
            tracing::warn!(job_id, "Reporting an unknown job as complete");
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
        } else {
            (JobState::Running, String::new())
        };

        let status = JobStatus {
            state,
            node_id: job.node_id.clone(),
            rack_id: job.rack_id.clone(),
            error_message,
        };
        if state == JobState::Completed {
            jobs.remove(job_id);
        }
        status
    }
}
