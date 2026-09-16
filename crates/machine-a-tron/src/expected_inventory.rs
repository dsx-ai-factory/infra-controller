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

/// Attempts per record, including the first one.
const MAX_ATTEMPTS: u32 = 10;
/// Backoff before the first retry; doubles on each further retry.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Upper bound on the backoff between attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Records registered concurrently at startup.
pub(crate) const CONCURRENCY: usize = 8;

/// Fragment of the message nico-api returns for an expected record whose BMC
/// MAC address is already registered. The database unique constraint raises
/// it for machines, switches, and power shelves alike, and it reaches the
/// client as `Internal` rather than `AlreadyExists`, so the message is matched
/// instead of the status code. An NVOS MAC claimed by another expected switch
/// is not matched: nico-api reports it only for a switch whose own BMC MAC is
/// absent, so that record is missing rather than present.
const DUPLICATE_RECORD_MARKER: &str = "duplicate MAC address for expected host BMC interface";

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
                || status.message().contains(DUPLICATE_RECORD_MARKER)
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
/// from `INITIAL_BACKOFF` and is capped at `MAX_BACKOFF`; `jitter` in
/// `[0.0, 1.0]` places the result between half of the base and the base.
fn backoff_delay(retry: u32, jitter: f64) -> Duration {
    let base = (INITIAL_BACKOFF * 2_u32.pow(retry)).min(MAX_BACKOFF);
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
/// fails permanently, or exhausts `MAX_ATTEMPTS`.
pub(crate) async fn register_with_retry<F, Fut>(
    identifier: &str,
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
        if attempts >= MAX_ATTEMPTS {
            return Err(error);
        }
        let delay = backoff_delay(attempts - 1, rand::rng().random::<f64>());
        tracing::warn!(
            identifier,
            attempt = attempts,
            max_attempts = MAX_ATTEMPTS,
            retry_delay_milliseconds = delay.as_millis(),
            error = %error,
            "transient error registering expected inventory record; retrying"
        );
        tokio::time::sleep(delay).await;
    }
}

/// Counts of the device records from one startup registration pass, exposed
/// on `/expected-inventory/status`. Racks are not counted: a rack that cannot
/// be registered aborts startup before any device record is attempted.
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

    /// Emits one summary line.
    pub(crate) fn log(&self) {
        tracing::info!(
            registered_count = self.registered,
            already_present_count = self.already_present,
            failed_count = self.failed,
            "expected inventory registration finished"
        );
    }
}

/// Registers every record with at most `concurrency` in flight and returns
/// the aggregate outcome.
pub(crate) async fn register_all<F, Fut>(
    records: Vec<ExpectedRecord>,
    concurrency: usize,
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
            let result = register_with_retry(&identifier, || register(record.clone())).await;
            (identifier, result)
        })
        .buffer_unordered(concurrency)
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
                    scenario: "NVOS MAC claimed by another expected switch is a conflict",
                    input: invocation(Status::failed_precondition(
                        "NVOS MAC address is already claimed by another expected switch: 02:00:00:00:00:02",
                    )),
                    expect: Disposition::Fail,
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
                    scenario: "base is capped at MAX_BACKOFF",
                    input: (7, 1.0),
                    expect: Duration::from_secs(30),
                },
            ],
            |(retry, jitter)| backoff_delay(retry, jitter),
        );
    }

    /// Scripted responses for one record; each attempt pops the next one.
    fn scripted(responses: Vec<Result<(), ClientApiError>>) -> (Responses, Arc<Mutex<u32>>) {
        (
            Arc::new(Mutex::new(responses.into_iter().collect())),
            Arc::new(Mutex::new(0)),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn retry_stops_on_success_duplicate_permanent_failure_or_exhaustion() {
        struct Case {
            scenario: &'static str,
            responses: Vec<Result<(), ClientApiError>>,
            expect: Outcome<Registration, ()>,
            expect_attempts: u32,
        }

        let cases = [
            Case {
                scenario: "transient failure then success registers on the second attempt",
                responses: vec![Err(invocation(Status::unavailable("busy"))), Ok(())],
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
                expect: Yields(Registration::AlreadyPresent),
                expect_attempts: 2,
            },
            Case {
                scenario: "permanent failure is not retried",
                responses: vec![Err(invocation(Status::invalid_argument("bad")))],
                expect: Fails,
                expect_attempts: 1,
            },
            Case {
                scenario: "transient failures stop at MAX_ATTEMPTS",
                responses: (0..MAX_ATTEMPTS)
                    .map(|_| Err(invocation(Status::unavailable("busy"))))
                    .collect(),
                expect: Fails,
                expect_attempts: MAX_ATTEMPTS,
            },
        ];

        for case in cases {
            let (responses, attempts) = scripted(case.responses);
            let result = register_with_retry("machine test", || {
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

    #[tokio::test(start_paused = true)]
    async fn register_all_summarizes_outcomes_and_bounds_concurrency() {
        let concurrency = 2;
        let records = ["ok", "dup", "bad", "worse"]
            .into_iter()
            .map(machine_record)
            .collect::<Vec<_>>();
        let responses: HashMap<&str, Result<(), ClientApiError>> = HashMap::from([
            ("ok", Ok(())),
            ("dup", Err(invocation(Status::already_exists("present")))),
            ("bad", Err(invocation(Status::invalid_argument("serial")))),
            ("worse", Err(invocation(Status::invalid_argument("mac")))),
        ]);
        let responses = Arc::new(Mutex::new(responses));
        let in_flight = Arc::new(Mutex::new((0_usize, 0_usize)));

        let summary = register_all(records, concurrency, |record| {
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
                    .remove(chassis_serial_number.as_str())
                    .expect("each record is attempted once");
                in_flight.lock().unwrap().0 -= 1;
                response
            }
        })
        .await;

        assert_eq!(
            summary,
            ExpectedInventorySummary {
                registered: 1,
                already_present: 1,
                failed: 2,
                failed_identifiers: vec![
                    machine_record("bad").identifier(),
                    machine_record("worse").identifier(),
                ],
            }
        );
        assert!(
            in_flight.lock().unwrap().1 <= concurrency,
            "in-flight registrations exceeded the concurrency bound"
        );
    }
}
