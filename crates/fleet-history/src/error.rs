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

use std::time::Duration;

/// Why a history call has no answer. Each kind is its own variant, so a
/// caller's port error keeps it: a timeout is never a failure, and a
/// request that can never be answered is never retried. Every text is this
/// crate's own, never store or probe reply text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    /// The request can never be answered as asked: a bad window, quantile
    /// or target. `field` names the parameter at fault.
    #[error("invalid {field}: {reason}")]
    InvalidInput { field: &'static str, reason: String },
    /// The filter or the probe's metadata is one this client cannot read.
    #[error("history cannot serve this request: {0}")]
    Unsupported(String),
    /// The stored series cannot answer: they are not what the probe
    /// publishes, or a sweep the summary reads lacks the capability its
    /// filter needs.
    #[error("history unavailable: {0}")]
    Unavailable(String),
    /// Another history read holds the store's read gate. `retry_after` is
    /// when to ask again, when known.
    #[error("history is busy")]
    Busy { retry_after: Option<Duration> },
    /// The call did not answer by its deadline.
    #[error("history request timed out")]
    Timeout,
    /// The caller cancelled the call.
    #[error("history request cancelled")]
    Cancelled,
}

impl HistoryError {
    pub(crate) fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidInput {
            field,
            reason: reason.into(),
        }
    }

    /// The stored series are not what the probe publishes.
    pub(crate) fn bad_store(reason: &str) -> Self {
        Self::Unavailable(format!("the stored series are not the probe's: {reason}"))
    }
}
