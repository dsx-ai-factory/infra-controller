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
//! Startup registration of expected inventory records with bounded retry.
//!
//! Every record is registered independently. Transient API failures are
//! retried with exponential backoff, a record the API already holds counts
//! as present, and any other failure is reported once in the summary.

use std::future::Future;
use std::time::Duration;

use futures::{StreamExt, stream};
use rand::RngExt;
use serde::Serialize;
use tonic::Code;

use crate::api_client::{ClientApiError, ExpectedRecord};
use crate::config::ExpectedInventoryRegistrationConfig;

/// Fragments of the messages nico-api returns for an expected record whose
/// MAC address is already registered. The status code depends on the
/// handler: a duplicate machine BMC MAC and a duplicate switch NVOS MAC
/// arrive as `FailedPrecondition`, while a duplicate switch or power shelf
/// BMC MAC arrives as `Internal`, so the status code alone cannot identify
/// them.
const DUPLICATE_RECORD_MARKERS: [&str; 2] = [
    "duplicate MAC address for expected host BMC interface",
    "NVOS MAC address is already claimed by another expected switch",
];

/// Number of failed identifiers included in the summary error log line.
const LOGGED_FAILURE_LIMIT: usize = 10;

/// How a failed registration attempt is handled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    /// The API already holds the record. This also covers a retry whose
    /// previous attempt was committed after the client gave up waiting.
    AlreadyPresent,
    /// The failure may clear on its own; retry after a backoff.
    Retry,
    /// The failure will not clear by retrying.
    Fail,
}

fn classify(error: &ClientApiError) -> Disposition {
    match error {
        ClientApiError::ConnectFailed(_) => Disposition::Retry,
        ClientApiError::ConfigError(_) => Disposition::Fail,
        ClientApiError::InvocationError(status) => {
            if status.code() == Code::AlreadyExists
                || DUPLICATE_RECORD_MARKERS
                    .iter()
                    .any(|marker| status.message().contains(marker))
            {
                return Disposition::AlreadyPresent;
            }
            match status.code() {
                Code::Unavailable
                | Code::DeadlineExceeded
                | Code::ResourceExhausted
                | Code::Internal
                | Code::Unknown
                | Code::Aborted
                | Code::Cancelled => Disposition::Retry,
                _ => Disposition::Fail,
            }
        }
    }
}

/// Delay before retry number `retry` (zero-based). The base doubles per retry
/// from `initial_backoff` and is capped at `max_backoff`; `jitter` in
/// `[0.0, 1.0]` places the result between half of the base and the base.
fn backoff_delay(
    config: &ExpectedInventoryRegistrationConfig,
    retry: u32,
    jitter: f64,
) -> Duration {
    let base = config
        .initial_backoff
        .saturating_mul(2_u32.saturating_pow(retry))
        .min(config.max_backoff);
    let half = base / 2;
    half + half.mul_f64(jitter.clamp(0.0, 1.0))
}

/// Successful outcome of registering one record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Registration {
    Registered,
    AlreadyPresent,
}

/// Runs `attempt` until it succeeds, reports the record as already present,
/// fails permanently, or exhausts `config.max_attempts`.
pub(crate) async fn register_with_retry<F, Fut>(
    identifier: &str,
    config: &ExpectedInventoryRegistrationConfig,
    mut attempt: F,
) -> Result<Registration, ClientApiError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), ClientApiError>>,
{
    let mut attempts = 0_u32;
    loop {
        attempts += 1;
        let error = match attempt().await {
            Ok(()) => return Ok(Registration::Registered),
            Err(error) => error,
        };
        match classify(&error) {
            Disposition::AlreadyPresent => return Ok(Registration::AlreadyPresent),
            Disposition::Fail => return Err(error),
            Disposition::Retry => {}
        }
        if attempts >= config.max_attempts {
            return Err(error);
        }
        let delay = backoff_delay(config, attempts - 1, rand::rng().random::<f64>());
        tracing::warn!(
            identifier,
            attempt = attempts,
            max_attempts = config.max_attempts,
            retry_delay_milliseconds = delay.as_millis(),
            error = %error,
            "transient error registering expected inventory record; retrying"
        );
        tokio::time::sleep(delay).await;
    }
}

/// Counts from one startup registration pass, exposed on
/// `/expected-inventory/status`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ExpectedInventorySummary {
    /// Records created by this pass.
    pub registered: usize,
    /// Records the API already held.
    pub already_present: usize,
    /// Records that could not be registered.
    pub failed: usize,
    /// Identifiers of the failed records, sorted.
    pub failed_identifiers: Vec<String>,
}

impl ExpectedInventorySummary {
    fn record(&mut self, identifier: String, result: Result<Registration, ClientApiError>) {
        match result {
            Ok(Registration::Registered) => self.registered += 1,
            Ok(Registration::AlreadyPresent) => self.already_present += 1,
            Err(error) => {
                tracing::error!(
                    identifier,
                    error = %error,
                    "failed to register expected inventory record"
                );
                self.failed += 1;
                self.failed_identifiers.push(identifier);
            }
        }
    }

    /// Emits one summary line, plus an error line naming failed records.
    pub(crate) fn log(&self) {
        tracing::info!(
            registered_count = self.registered,
            already_present_count = self.already_present,
            failed_count = self.failed,
            "expected inventory registration finished"
        );
        if self.failed > 0 {
            let shown = self
                .failed_identifiers
                .iter()
                .take(LOGGED_FAILURE_LIMIT)
                .collect::<Vec<_>>();
            tracing::error!(
                failed_count = self.failed,
                shown_count = shown.len(),
                failed_identifiers = ?shown,
                "expected inventory records were not registered; their racks cannot become ready"
            );
        }
    }
}

/// Registers every record with at most `config.concurrency` in flight and
/// returns the aggregate outcome.
pub(crate) async fn register_all<F, Fut>(
    records: Vec<ExpectedRecord>,
    config: &ExpectedInventoryRegistrationConfig,
    register: F,
) -> ExpectedInventorySummary
where
    F: Fn(ExpectedRecord) -> Fut,
    Fut: Future<Output = Result<(), ClientApiError>>,
{
    let register = &register;
    let results = stream::iter(records)
        .map(|record| async move {
            let identifier = record.identifier();
            let result =
                register_with_retry(&identifier, config, || register(record.clone())).await;
            (identifier, result)
        })
        .buffer_unordered(config.concurrency)
        .collect::<Vec<_>>()
        .await;

    let mut summary = ExpectedInventorySummary::default();
    for (identifier, result) in results {
        summary.record(identifier, result);
    }
    summary.failed_identifiers.sort();
    summary
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use carbide_test_support::Outcome::{self, Fails, Yields};
    use carbide_test_support::{Check, assert_outcome, check_values};
    use tonic::Status;

    use super::*;

    type Responses = Arc<Mutex<VecDeque<Result<(), ClientApiError>>>>;

    fn invocation(status: Status) -> ClientApiError {
        ClientApiError::InvocationError(status)
    }

    fn fast_config(max_attempts: u32, concurrency: usize) -> ExpectedInventoryRegistrationConfig {
        ExpectedInventoryRegistrationConfig {
            max_attempts,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
            concurrency,
        }
    }

    fn machine_record(serial: &str) -> ExpectedRecord {
        ExpectedRecord::Machine {
            bmc_mac_address: format!("02:00:00:00:00:{:02x}", serial.len()),
            chassis_serial_number: serial.to_string(),
            rack_id: None,
            dpu_policy: None,
            interfaces: Vec::new(),
        }
    }

    #[test]
    fn classify_registration_errors() {
        check_values(
            [
                Check {
                    scenario: "AlreadyExists is already present",
                    input: invocation(Status::already_exists("rack exists")),
                    expect: Disposition::AlreadyPresent,
                },
                Check {
                    scenario: "duplicate power shelf BMC MAC is already present despite Internal",
                    input: invocation(Status::internal(
                        "duplicate MAC address for expected host BMC interface: 02:00:00:00:00:01",
                    )),
                    expect: Disposition::AlreadyPresent,
                },
                Check {
                    scenario: "duplicate switch NVOS MAC is already present despite FailedPrecondition",
                    input: invocation(Status::failed_precondition(
                        "NVOS MAC address is already claimed by another expected switch: 02:00:00:00:00:02",
                    )),
                    expect: Disposition::AlreadyPresent,
                },
                Check {
                    scenario: "Internal without a duplicate marker is transient",
                    input: invocation(Status::internal("database error")),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "Unavailable is transient",
                    input: invocation(Status::unavailable("connection refused")),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "DeadlineExceeded is transient",
                    input: invocation(Status::deadline_exceeded("timed out")),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "ResourceExhausted is transient",
                    input: invocation(Status::resource_exhausted("rate limited")),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "Unknown transport error is transient",
                    input: invocation(Status::unknown("transport error")),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "connection failure is transient",
                    input: ClientApiError::ConnectFailed("dns".to_string()),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "InvalidArgument is permanent",
                    input: invocation(Status::invalid_argument("bad serial")),
                    expect: Disposition::Fail,
                },
                Check {
                    scenario: "FailedPrecondition without a duplicate marker is permanent",
                    input: invocation(Status::failed_precondition("MaintenanceMode")),
                    expect: Disposition::Fail,
                },
                Check {
                    scenario: "client configuration error is permanent",
                    input: ClientApiError::ConfigError("profile mismatch".to_string()),
                    expect: Disposition::Fail,
                },
            ],
            |error| classify(&error),
        );
    }

    #[test]
    fn backoff_doubles_to_cap_with_equal_jitter() {
        let config = ExpectedInventoryRegistrationConfig::default();
        check_values(
            [
                Check {
                    scenario: "first retry with no jitter waits half the initial backoff",
                    input: (0, 0.0),
                    expect: Duration::from_millis(250),
                },
                Check {
                    scenario: "first retry with full jitter waits the initial backoff",
                    input: (0, 1.0),
                    expect: Duration::from_millis(500),
                },
                Check {
                    scenario: "base doubles per retry",
                    input: (3, 1.0),
                    expect: Duration::from_secs(4),
                },
                Check {
                    scenario: "base is capped at max_backoff",
                    input: (7, 1.0),
                    expect: Duration::from_secs(30),
                },
                Check {
                    scenario: "large retry counts saturate instead of overflowing",
                    input: (40, 0.5),
                    expect: Duration::from_millis(22_500),
                },
            ],
            |(retry, jitter)| backoff_delay(&config, retry, jitter),
        );
    }

    /// Scripted responses for one record; each attempt pops the next one.
    fn scripted(responses: Vec<Result<(), ClientApiError>>) -> (Responses, Arc<Mutex<u32>>) {
        (
            Arc::new(Mutex::new(responses.into_iter().collect())),
            Arc::new(Mutex::new(0)),
        )
    }

    #[tokio::test]
    async fn retry_stops_on_success_duplicate_permanent_failure_or_exhaustion() {
        struct Case {
            scenario: &'static str,
            responses: Vec<Result<(), ClientApiError>>,
            max_attempts: u32,
            expect: Outcome<Registration, ()>,
            expect_attempts: u32,
        }

        let cases = [
            Case {
                scenario: "transient failure then success registers on the second attempt",
                responses: vec![Err(invocation(Status::unavailable("busy"))), Ok(())],
                max_attempts: 5,
                expect: Yields(Registration::Registered),
                expect_attempts: 2,
            },
            Case {
                scenario: "transient failure then duplicate counts as already present",
                responses: vec![
                    Err(invocation(Status::deadline_exceeded("slow"))),
                    Err(invocation(Status::failed_precondition(
                        "duplicate MAC address for expected host BMC interface: 02:00:00:00:00:01",
                    ))),
                ],
                max_attempts: 5,
                expect: Yields(Registration::AlreadyPresent),
                expect_attempts: 2,
            },
            Case {
                scenario: "permanent failure is not retried",
                responses: vec![Err(invocation(Status::invalid_argument("bad")))],
                max_attempts: 5,
                expect: Fails,
                expect_attempts: 1,
            },
            Case {
                scenario: "transient failures stop at max_attempts",
                responses: (0..3)
                    .map(|_| Err(invocation(Status::unavailable("busy"))))
                    .collect(),
                max_attempts: 3,
                expect: Fails,
                expect_attempts: 3,
            },
        ];

        for case in cases {
            let (responses, attempts) = scripted(case.responses);
            let config = fast_config(case.max_attempts, 1);
            let result = register_with_retry("machine test", &config, || {
                let responses = responses.clone();
                let attempts = attempts.clone();
                async move {
                    *attempts.lock().unwrap() += 1;
                    responses
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("more attempts than scripted responses")
                }
            })
            .await;
            assert_outcome(result.map_err(drop), case.expect, case.scenario);
            assert_eq!(
                *attempts.lock().unwrap(),
                case.expect_attempts,
                "{}: attempt count",
                case.scenario
            );
        }
    }

    #[tokio::test]
    async fn register_all_summarizes_outcomes_and_bounds_concurrency() {
        let records = ["ok", "dup", "bad", "late", "gone"]
            .into_iter()
            .map(machine_record)
            .collect::<Vec<_>>();
        let responses: HashMap<&str, VecDeque<Result<(), ClientApiError>>> = HashMap::from([
            ("ok", VecDeque::from([Ok(())])),
            (
                "dup",
                VecDeque::from([Err(invocation(Status::already_exists("present")))]),
            ),
            (
                "bad",
                VecDeque::from([Err(invocation(Status::invalid_argument("serial")))]),
            ),
            (
                "late",
                VecDeque::from([Err(invocation(Status::unavailable("busy"))), Ok(())]),
            ),
            (
                "gone",
                VecDeque::from([
                    Err(invocation(Status::unavailable("busy"))),
                    Err(invocation(Status::unavailable("busy"))),
                ]),
            ),
        ]);
        let responses = Arc::new(Mutex::new(responses));
        let in_flight = Arc::new(Mutex::new((0_usize, 0_usize)));
        let config = fast_config(2, 2);

        let summary = register_all(records, &config, |record| {
            let responses = responses.clone();
            let in_flight = in_flight.clone();
            async move {
                {
                    let mut counters = in_flight.lock().unwrap();
                    counters.0 += 1;
                    counters.1 = counters.1.max(counters.0);
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
                let ExpectedRecord::Machine {
                    chassis_serial_number,
                    ..
                } = &record
                else {
                    unreachable!("only machine records are scripted");
                };
                let response = responses
                    .lock()
                    .unwrap()
                    .get_mut(chassis_serial_number.as_str())
                    .and_then(VecDeque::pop_front)
                    .expect("more attempts than scripted responses");
                in_flight.lock().unwrap().0 -= 1;
                response
            }
        })
        .await;

        assert_eq!(
            summary,
            ExpectedInventorySummary {
                registered: 2,
                already_present: 1,
                failed: 2,
                failed_identifiers: vec![
                    machine_record("bad").identifier(),
                    machine_record("gone").identifier(),
                ],
            }
        );
        assert!(
            in_flight.lock().unwrap().1 <= config.concurrency,
            "in-flight registrations exceeded the configured concurrency"
        );
    }
}
