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

//! The Postgres secrets backend's instrumentation events, under
//! `carbide_api_secrets_*` with an `operation` label (get/set/create/delete):
//! an operation is counted the moment it starts, then counted again as a
//! success or a failure and timed once. Every event is metric-only -- the
//! secrets code logs its own detail elsewhere -- so each is `log = off`.

use std::time::Instant;

use carbide_instrument::{Event, LabelValue, emit};

/// The secrets operation, as the bounded `operation` metric label. Each
/// variant renders to the exact string the counters have always reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub enum SecretsOperation {
    Get,
    Set,
    Create,
    Delete,
}

/// A Postgres secrets operation was attempted; counted the moment it starts.
#[derive(Event)]
#[event(
    event_name = "secrets_request_started",
    metric_name = "carbide_api_secrets_requests_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of Postgres secrets operations attempted."
)]
struct SecretsRequestStarted {
    #[label]
    operation: SecretsOperation,
}

/// A Postgres secrets operation completed successfully.
#[derive(Event)]
#[event(
    event_name = "secrets_request_succeeded",
    metric_name = "carbide_api_secrets_requests_succeeded_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of successful Postgres secrets operations."
)]
struct SecretsRequestSucceeded {
    #[label]
    operation: SecretsOperation,
}

/// A Postgres secrets operation failed -- it left scope without succeeding.
#[derive(Event)]
#[event(
    event_name = "secrets_request_failed",
    metric_name = "carbide_api_secrets_requests_failed_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of failed Postgres secrets operations."
)]
struct SecretsRequestFailed {
    #[label]
    operation: SecretsOperation,
}

/// How long a Postgres secrets operation took, in whole milliseconds,
/// recorded once on either the success or the failure path. The elapsed time
/// is truncated to whole milliseconds before it is observed, so the recorded
/// sample is the same integer the counter has always reported.
#[derive(Event)]
#[event(
    event_name = "secrets_request_duration",
    metric_name = "carbide_api_secrets_request_duration_milliseconds",
    component = "nico-api",
    log = off,
    metric = histogram,
    describe = "Duration of Postgres secrets operations, in milliseconds."
)]
struct SecretsRequestDuration {
    #[label]
    operation: SecretsOperation,
    #[observation]
    duration_ms: u64,
}

/// Times one secrets operation and records its outcome exactly once: call
/// [`OperationTimer::succeed`] on the success path, and any other way out of
/// scope -- early `?` returns included -- records a failure on drop. The
/// events are metric-only; with no meter provider installed, `emit` is a
/// no-op.
pub struct OperationTimer {
    operation: SecretsOperation,
    started: Instant,
    completed: bool,
}

impl OperationTimer {
    /// Start timing an operation, counting the attempt immediately.
    pub fn start(operation: SecretsOperation) -> Self {
        emit(SecretsRequestStarted { operation });
        Self {
            operation,
            started: Instant::now(),
            completed: false,
        }
    }

    /// Record a successful operation and its duration.
    pub fn succeed(mut self) {
        self.completed = true;
        let duration_ms = self.started.elapsed().as_millis() as u64;
        emit(SecretsRequestSucceeded {
            operation: self.operation,
        });
        emit(SecretsRequestDuration {
            operation: self.operation,
            duration_ms,
        });
    }
}

impl Drop for OperationTimer {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let duration_ms = self.started.elapsed().as_millis() as u64;
        emit(SecretsRequestFailed {
            operation: self.operation,
        });
        emit(SecretsRequestDuration {
            operation: self.operation,
            duration_ms,
        });
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::LabelValue;
    use carbide_test_support::{Check, check_values};

    use super::SecretsOperation;

    /// The `operation` label values are the metric contract: each variant
    /// renders to the exact snake_case string the counters have always
    /// reported.
    ///
    /// The emit conditions -- which counter fires on success versus failure,
    /// and that the duration records once on each path without logging -- are
    /// pinned in the `secrets_metrics` integration-test binary, which runs
    /// alone in its own process so no other emitter moves the global series
    /// between a capture baseline and its delta.
    #[test]
    fn secrets_operation_renders_expected_label_values() {
        check_values(
            [
                Check {
                    scenario: "get",
                    input: SecretsOperation::Get,
                    expect: "get".to_string(),
                },
                Check {
                    scenario: "set",
                    input: SecretsOperation::Set,
                    expect: "set".to_string(),
                },
                Check {
                    scenario: "create",
                    input: SecretsOperation::Create,
                    expect: "create".to_string(),
                },
                Check {
                    scenario: "delete",
                    input: SecretsOperation::Delete,
                    expect: "delete".to_string(),
                },
            ],
            |operation| operation.label_value().to_string(),
        );
    }
}
