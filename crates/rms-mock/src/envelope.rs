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
//! Every field these set is load-bearing, and proto3 defaults are the wrong
//! answer for all of them: an unset `status` is `UNSPECIFIED`, which callers
//! read as failure, and an unset job id makes a caller give up with
//! "succeeded but returned no job id". Building responses in one place keeps
//! that contract in one place too, rather than in every handler.
//!
//! One rule for a node the request names but no simulated device answers
//! for: it is a per-node failure. Its result says why, it counts towards
//! `failed_nodes`, and the batch fails, since the proto has a batch succeed
//! only when every targeted node did. A caller learns which nodes were not
//! found instead of being handed a success it cannot act on.

use crate::resolve::NodeRef;
use crate::rms;

/// The per-node error for a node no simulated device answers for.
pub(crate) const UNMATCHED_NODE: &str = "no simulated device matches this node";

/// How a batch over resolved nodes went: every matched node succeeded and
/// every unmatched one failed.
pub(crate) struct BatchOutcome {
    pub(crate) status: rms::ReturnCode,
    /// Names the nodes that were not found; empty on success. The compute
    /// caller records this as a missing node's error.
    pub(crate) message: String,
    pub(crate) stats: rms::NodeOperationStats,
}

impl BatchOutcome {
    pub(crate) fn of(refs: &[NodeRef<'_>]) -> Self {
        let unmatched: Vec<&str> = refs
            .iter()
            .filter(|r| !r.matched())
            .map(|r| r.node_id)
            .collect();
        let total = refs.len() as u32;
        let failed = unmatched.len() as u32;

        let (status, message) = if unmatched.is_empty() {
            (rms::ReturnCode::Success, String::new())
        } else {
            (
                rms::ReturnCode::Failure,
                format!(
                    "{failed} of {total} nodes did not match any simulated device: {}",
                    unmatched.join(", ")
                ),
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

/// A batch over the given nodes.
///
/// The caller checks three things independently - the batch status, the
/// per-node results, and `stats.failed_nodes` - and treats a disagreement
/// between them as failure, so all three are derived from the same
/// matched-or-not split.
pub(crate) fn node_batch(refs: &[NodeRef<'_>], job_id: &str) -> rms::NodeBatchResponse {
    let node_results = refs
        .iter()
        .map(|r| {
            let (status, error_message) = if r.matched() {
                (rms::ReturnCode::Success, String::new())
            } else {
                (rms::ReturnCode::Failure, UNMATCHED_NODE.to_owned())
            };
            rms::NodeOperationResult {
                node_id: r.node_id.to_owned(),
                status: status as i32,
                error_message,
            }
        })
        .collect();

    let outcome = BatchOutcome::of(refs);
    rms::NodeBatchResponse {
        status: outcome.status as i32,
        message: outcome.message,
        node_results,
        job_id: job_id.to_owned(),
        stats: Some(outcome.stats),
    }
}
