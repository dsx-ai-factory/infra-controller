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

use std::sync::Arc;

use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use eyre::eyre;
use forge_dpu_agent_utils::utils::create_forge_client;
use rpc::forge::InstancePhoneHomeLastContactRequest;

use crate::state::FmdsState;

/// The terminal outcome of a phone-home operation. `RateLimited` (the outbound
/// governor rejected the attempt) and `InstanceNotFound` (no instance for this
/// machine yet) are the two failures the per-RPC RED instrumentation cannot
/// see: the first never reaches an RPC, the second is a successful lookup that
/// returned nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum PhoneHomeOutcome {
    Ok,
    RateLimited,
    InstanceNotFound,
    Error,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_fmds_phone_home_total",
    kind = counter,
    component = "fmds",
    describe = "Number of FMDS tenant phone-home operations, by outcome"
)]
struct FmdsPhoneHome {
    outcome: PhoneHomeOutcome,
}

/// `PhoneHomeSucceeded` writes the existing success record with the configured
/// machine ID and the timestamp returned by Forge. `PhoneHomeCompleted` below
/// retains the existing Event identity for failures, and both increment the
/// same metric.
#[derive(Event)]
#[event(
    event_name = "fmds_phone_home_succeeded",
    metric_family = FmdsPhoneHome,
    log = info,
    message = "Successfully phoned home"
)]
struct PhoneHomeSucceeded {
    #[label]
    outcome: PhoneHomeOutcome,
    #[context]
    machine_id: String,
    #[context]
    timestamp: String,
}

/// `PhoneHomeCompleted` retains the existing failure Event identity. Keeping
/// success and failure as separate types lets each log expose only the fields
/// it actually has while both increment the same `outcome` series.
#[derive(Event)]
#[event(
    event_name = "fmds_phone_home_completed",
    metric_family = FmdsPhoneHome,
    log = warn,
    message = "Phone home failed"
)]
struct PhoneHomeCompleted {
    #[label]
    outcome: PhoneHomeOutcome,
    #[context]
    error: String,
}

/// A phone-home failure tagged with the bounded outcome for the metric label,
/// carrying the eyre report for the log line and the caller. Any failure that
/// is not specifically rate-limited or instance-not-found maps to `Error`.
struct PhoneHomeError {
    outcome: PhoneHomeOutcome,
    source: eyre::Error,
}

impl From<eyre::Error> for PhoneHomeError {
    fn from(source: eyre::Error) -> Self {
        Self {
            outcome: PhoneHomeOutcome::Error,
            source,
        }
    }
}

impl From<tonic::Status> for PhoneHomeError {
    fn from(status: tonic::Status) -> Self {
        Self {
            outcome: PhoneHomeOutcome::Error,
            source: status.into(),
        }
    }
}

pub async fn phone_home(state: &Arc<FmdsState>) -> Result<(), eyre::Error> {
    complete_phone_home(attempt_phone_home(state).await)
}

struct PhoneHomeSuccess {
    machine_id: String,
    timestamp: String,
}

fn complete_phone_home(
    result: Result<PhoneHomeSuccess, PhoneHomeError>,
) -> Result<(), eyre::Error> {
    match result {
        Ok(success) => {
            emit(PhoneHomeSucceeded {
                outcome: PhoneHomeOutcome::Ok,
                machine_id: success.machine_id,
                timestamp: success.timestamp,
            });
            Ok(())
        }
        Err(failure) => {
            let PhoneHomeError { outcome, source } = failure;
            emit(PhoneHomeCompleted {
                outcome,
                error: format!("{source:#}"),
            });
            Err(source)
        }
    }
}

async fn attempt_phone_home(state: &Arc<FmdsState>) -> Result<PhoneHomeSuccess, PhoneHomeError> {
    state
        .outbound_governor
        .clone()
        .check()
        .map_err(|e| PhoneHomeError {
            outcome: PhoneHomeOutcome::RateLimited,
            source: eyre!("rate limit exceeded for phone_home; {}\n", e),
        })?;

    let forge_client_config = state
        .forge_client_config
        .as_ref()
        .ok_or_else(|| eyre!("phone_home not configured: no forge client config"))?;

    let mut client = create_forge_client(&state.forge_api, forge_client_config).await?;

    let machine_id = state
        .machine_id
        .load_full()
        .ok_or_else(|| eyre!("phone_home: no machine_id available yet"))?;

    // Look up the instance for this machine
    let request = tonic::Request::new(*machine_id);

    let response = client.find_instance_by_machine_id(request).await?;
    let instance = response
        .into_inner()
        .instances
        .first()
        .cloned()
        .ok_or_else(|| PhoneHomeError {
            outcome: PhoneHomeOutcome::InstanceNotFound,
            source: eyre!("no instance found for machine {}", machine_id),
        })?;

    let instance_id = instance.id;

    let request = tonic::Request::new(InstancePhoneHomeLastContactRequest { instance_id });
    let response = client
        .update_instance_phone_home_last_contact(request)
        .await?;
    let timestamp = response
        .into_inner()
        .timestamp
        .ok_or_else(|| eyre!("timestamp is empty in response"))?;

    Ok(PhoneHomeSuccess {
        machine_id: machine_id.to_string(),
        timestamp: timestamp.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{CapturedFieldKind, MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};

    use super::*;

    const PHONE_HOME_METRIC: &str = "carbide_fmds_phone_home_total";
    const MACHINE_ID: &str = "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg";
    const TIMESTAMP: &str = "2026-07-21T18:42:00Z";

    enum CompletionCase {
        Success,
        Failure {
            outcome: PhoneHomeOutcome,
            error: &'static str,
        },
    }

    impl CompletionCase {
        fn metric_label(&self) -> &'static str {
            match self {
                Self::Success => "ok",
                Self::Failure { outcome, .. } => match outcome {
                    PhoneHomeOutcome::Ok => "ok",
                    PhoneHomeOutcome::RateLimited => "rate_limited",
                    PhoneHomeOutcome::InstanceNotFound => "instance_not_found",
                    PhoneHomeOutcome::Error => "error",
                },
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct CompletionObservation {
        returned_error: Option<String>,
        metric_delta: f64,
        logs: Vec<LogObservation>,
    }

    #[derive(Debug, PartialEq)]
    struct LogObservation {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        outcome: Option<String>,
        machine_id: Option<String>,
        machine_id_kind: Option<CapturedFieldKind>,
        timestamp: Option<String>,
        timestamp_kind: Option<CapturedFieldKind>,
        error: Option<String>,
        error_kind: Option<CapturedFieldKind>,
    }

    fn expected_log(
        metadata_name: &str,
        level: tracing::Level,
        message: &str,
        outcome: &str,
        machine_id: Option<&str>,
        timestamp: Option<&str>,
        error: Option<&str>,
    ) -> Vec<LogObservation> {
        vec![LogObservation {
            metadata_name: metadata_name.to_string(),
            level,
            message: message.to_string(),
            event_name: Some(metadata_name.to_string()),
            metric_name: Some(PHONE_HOME_METRIC.to_string()),
            outcome: Some(outcome.to_string()),
            machine_id: machine_id.map(str::to_string),
            machine_id_kind: machine_id.map(|_| CapturedFieldKind::Debug),
            timestamp: timestamp.map(str::to_string),
            timestamp_kind: timestamp.map(|_| CapturedFieldKind::Debug),
            error: error.map(str::to_string),
            error_kind: error.map(|_| CapturedFieldKind::Debug),
        }]
    }

    fn observe_completion(case: CompletionCase) -> CompletionObservation {
        let outcome = case.metric_label();
        let result = match case {
            CompletionCase::Success => Ok(PhoneHomeSuccess {
                machine_id: MACHINE_ID.to_string(),
                timestamp: TIMESTAMP.to_string(),
            }),
            CompletionCase::Failure { outcome, error } => Err(PhoneHomeError {
                outcome,
                source: eyre!("{error}"),
            }),
        };

        let metrics = MetricsCapture::start();
        let mut completion = None;
        let logs = capture_logs(|| {
            completion = Some(complete_phone_home(result));
        })
        .into_iter()
        .map(|log| {
            let event_name = log.field("event_name").map(str::to_string);
            let metric_name = log.field("metric_name").map(str::to_string);
            let outcome = log.field("outcome").map(str::to_string);
            let machine_id = log.field("machine_id").map(str::to_string);
            let machine_id_kind = log.field_kind("machine_id");
            let timestamp = log.field("timestamp").map(str::to_string);
            let timestamp_kind = log.field_kind("timestamp");
            let error = log.field("error").map(str::to_string);
            let error_kind = log.field_kind("error");
            LogObservation {
                metadata_name: log.metadata_name,
                level: log.level,
                message: log.message,
                event_name,
                metric_name,
                outcome,
                machine_id,
                machine_id_kind,
                timestamp,
                timestamp_kind,
                error,
                error_kind,
            }
        })
        .collect();

        CompletionObservation {
            returned_error: completion.unwrap().err().map(|error| error.to_string()),
            metric_delta: metrics.counter_delta(PHONE_HOME_METRIC, &[("outcome", outcome)]),
            logs,
        }
    }

    #[test]
    fn terminal_events_log_and_count_each_phone_home_result() {
        check_values(
            [
                Check {
                    scenario: "success keeps its machine and timestamp fields",
                    input: CompletionCase::Success,
                    expect: CompletionObservation {
                        returned_error: None,
                        metric_delta: 1.0,
                        logs: expected_log(
                            "fmds_phone_home_succeeded",
                            tracing::Level::INFO,
                            "Successfully phoned home",
                            "ok",
                            Some(MACHINE_ID),
                            Some(TIMESTAMP),
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "rate limiting keeps its warning and error field",
                    input: CompletionCase::Failure {
                        outcome: PhoneHomeOutcome::RateLimited,
                        error: "rate limit exceeded",
                    },
                    expect: CompletionObservation {
                        returned_error: Some("rate limit exceeded".to_string()),
                        metric_delta: 1.0,
                        logs: expected_log(
                            "fmds_phone_home_completed",
                            tracing::Level::WARN,
                            "Phone home failed",
                            "rate_limited",
                            None,
                            None,
                            Some("rate limit exceeded"),
                        ),
                    },
                },
                Check {
                    scenario: "a missing instance keeps its bounded metric label",
                    input: CompletionCase::Failure {
                        outcome: PhoneHomeOutcome::InstanceNotFound,
                        error: "instance not found",
                    },
                    expect: CompletionObservation {
                        returned_error: Some("instance not found".to_string()),
                        metric_delta: 1.0,
                        logs: expected_log(
                            "fmds_phone_home_completed",
                            tracing::Level::WARN,
                            "Phone home failed",
                            "instance_not_found",
                            None,
                            None,
                            Some("instance not found"),
                        ),
                    },
                },
                Check {
                    scenario: "other failures use the generic error label",
                    input: CompletionCase::Failure {
                        outcome: PhoneHomeOutcome::Error,
                        error: "forge unavailable",
                    },
                    expect: CompletionObservation {
                        returned_error: Some("forge unavailable".to_string()),
                        metric_delta: 1.0,
                        logs: expected_log(
                            "fmds_phone_home_completed",
                            tracing::Level::WARN,
                            "Phone home failed",
                            "error",
                            None,
                            None,
                            Some("forge unavailable"),
                        ),
                    },
                },
            ],
            observe_completion,
        );
    }
}
