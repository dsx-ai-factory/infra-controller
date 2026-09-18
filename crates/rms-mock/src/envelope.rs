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

//! Response builders.
//!
//! A node the request names but no simulated device answers for, or that
//! the host refuses, is a per-node failure: its result says why, it counts
//! towards `failed_nodes`, and the batch fails.

use crate::jobs::{JobState, JobStatus};
use crate::resolve::NodeRef;
use crate::rms;

/// The per-node error for a node no simulated device answers for.
pub(crate) const UNMATCHED_NODE: &str = "no simulated device matches this node";

/// How each node in a batch fared: the echoed node id and, on failure, why.
pub(crate) type NodeResult<'a> = (&'a str, Result<(), String>);

/// Every matched node succeeded and every unmatched one failed.
pub(crate) fn matched_or_not<'a>(refs: &[NodeRef<'a>]) -> Vec<NodeResult<'a>> {
    refs.iter()
        .map(|r| {
            let outcome = if r.matched() {
                Ok(())
            } else {
                Err(UNMATCHED_NODE.to_owned())
            };
            (r.node_id, outcome)
        })
        .collect()
}

/// How a batch went, derived from its per-node results.
pub(crate) struct BatchOutcome {
    pub(crate) status: rms::ReturnCode,
    /// Names each failed node and its reason; empty on success.
    pub(crate) message: String,
    pub(crate) stats: rms::NodeOperationStats,
}

impl BatchOutcome {
    pub(crate) fn of(results: &[NodeResult<'_>]) -> Self {
        let failures: Vec<String> = results
            .iter()
            .filter_map(|(node_id, outcome)| {
                outcome
                    .as_ref()
                    .err()
                    .map(|reason| format!("{node_id}: {reason}"))
            })
            .collect();
        let total = results.len() as u32;
        let failed = failures.len() as u32;

        let (status, message) = if failures.is_empty() {
            (rms::ReturnCode::Success, String::new())
        } else {
            (
                rms::ReturnCode::Failure,
                format!("{failed} of {total} nodes failed: {}", failures.join("; ")),
            )
        };

        Self {
            status,
            message,
            stats: rms::NodeOperationStats {
                total_nodes: total,
                successful_nodes: total - failed,
                failed_nodes: failed,
            },
        }
    }
}

/// A batch with the given per-node results.
pub(crate) fn node_batch(results: &[NodeResult<'_>], job_id: &str) -> rms::NodeBatchResponse {
    let node_results = results
        .iter()
        .map(|(node_id, outcome)| rms::NodeOperationResult {
            node_id: (*node_id).to_owned(),
            status: match outcome {
                Ok(()) => rms::ReturnCode::Success as i32,
                Err(_) => rms::ReturnCode::Failure as i32,
            },
            error_message: outcome.as_ref().err().cloned().unwrap_or_default(),
        })
        .collect();

    let outcome = BatchOutcome::of(results);
    rms::NodeBatchResponse {
        status: outcome.status as i32,
        message: outcome.message,
        node_results,
        job_id: job_id.to_owned(),
        stats: Some(outcome.stats),
    }
}

/// The `job_states` of a `GetJobStatus` response: the polled job first, then
/// its children when asked for.
pub(crate) fn job_states(status: &JobStatus, include_children: bool) -> Vec<rms::JobStatus> {
    let mut states = vec![job_status(status)];
    if include_children {
        states.extend(status.children.iter().map(job_status));
    }
    states
}

/// One job as `GetJobStatus` reports it. `execution_state` is never the
/// proto3 default, and a parent is tied to no node.
fn job_status(status: &JobStatus) -> rms::JobStatus {
    let error_code = match status.state {
        JobState::Failed => rms::JobError::Other,
        JobState::Running | JobState::Completed => rms::JobError::Unspecified,
    };
    rms::JobStatus {
        job_id: status.job_id.clone(),
        parent_job_id: status.parent_job_id.clone(),
        child_job_ids: status.children.iter().map(|c| c.job_id.clone()).collect(),
        execution_state: status.state.as_execution_state(),
        error_message: status.error_message.clone(),
        error_code: error_code as i32,
        result_json: String::new(),
        state_description: status.state.as_wire_str().to_owned(),
        rack_id: non_empty(&status.rack_id),
        node_id: non_empty(&status.node_id),
        created_at: None,
        updated_at: None,
    }
}

/// Reject a status poll that names no job, which would otherwise read as a
/// completed one.
pub(crate) fn require_job_id(job_id: &str) -> Result<(), tonic::Status> {
    if job_id.trim().is_empty() {
        return Err(tonic::Status::invalid_argument("job_id is required"));
    }
    Ok(())
}

/// The `error_message` of a status RPC answering `RETURN_CODE_FAILURE` for a
/// job this process never issued.
pub(crate) fn job_not_found(job_id: &str) -> String {
    format!("job {job_id} not found")
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}
