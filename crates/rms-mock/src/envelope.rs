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
