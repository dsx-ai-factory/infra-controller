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

use std::future::Future;
use std::time::Duration;

use futures::{StreamExt, stream};
use rand::RngExt;
use rpc::admission_retry::{
    MAX_ADMISSION_BACKOFF, MIN_ADMISSION_BACKOFF, PushbackAdvice, admission_retry_delay,
};
use serde::Serialize;
use tonic::Code;

use crate::api_client::{ClientApiError, ExpectedRecord};

/// Rounds per registration pass, including the first one.
const MAX_ATTEMPTS: u32 = 30;
/// Backoff before the first retry; doubles on each further retry.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Upper bound on the backoff between attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Records registered concurrently at startup.
pub(crate) const CONCURRENCY: usize = 8;

/// How a failed registration attempt is handled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    /// The API already holds the record.
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
        ClientApiError::InvocationError(status) => match status.code() {
            Code::AlreadyExists => Disposition::AlreadyPresent,
            Code::ResourceExhausted => match admission_retry_delay(status) {
                PushbackAdvice::StopRetrying => Disposition::Fail,
                PushbackAdvice::Absent | PushbackAdvice::Delay(_) => Disposition::Retry,
            },
            Code::Unavailable
            | Code::DeadlineExceeded
            | Code::Internal
            | Code::Unknown
            | Code::Aborted
            | Code::Cancelled => Disposition::Retry,
            _ => Disposition::Fail,
        },
    }
}

/// Delay nico-api asked for on an admission rejection, clamped to the
/// advertised admission backoff range.
fn pushback_delay(error: &ClientApiError) -> Option<Duration> {
    let ClientApiError::InvocationError(status) = error else {
        return None;
    };
    if status.code() != Code::ResourceExhausted {
        return None;
    }
    match admission_retry_delay(status) {
        PushbackAdvice::Delay(delay) => {
            Some(delay.clamp(MIN_ADMISSION_BACKOFF, MAX_ADMISSION_BACKOFF))
        }
        PushbackAdvice::Absent | PushbackAdvice::StopRetrying => None,
    }
}

/// Delay before retry number `retry` (zero-based). `jitter` in `[0.0, 1.0]`
/// places the result between half of the base and the base.
fn backoff_delay(retry: u32, jitter: f64) -> Duration {
    let base = (INITIAL_BACKOFF * 2_u32.pow(retry)).min(MAX_BACKOFF);
    let half = base / 2;
    half + half.mul_f64(jitter.clamp(0.0, 1.0))
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
    /// Identifiers of the records that could not be registered, sorted; a
    /// non-empty list fails startup, so a serving pod always reports none.
    pub failed_identifiers: Vec<String>,
}

impl ExpectedInventorySummary {
    /// Emits one summary line.
    pub(crate) fn log(&self) {
        tracing::info!(
            registered_count = self.registered,
            already_present_count = self.already_present,
            failed_count = self.failed_identifiers.len(),
            "expected inventory registration finished"
        );
    }
}

/// Registers every record with at most `concurrency` in flight and returns
/// the aggregate outcome. Records that fail transiently are retried together
/// after one shared backoff for up to `MAX_ATTEMPTS` rounds.
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
    let mut summary = ExpectedInventorySummary::default();
    let mut pending = records;
    for round in 1..=MAX_ATTEMPTS {
        let results = stream::iter(std::mem::take(&mut pending))
            .map(|record| async move {
                let result = register(record.clone()).await;
                (record, result)
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;

        let mut delay = backoff_delay(round - 1, rand::rng().random::<f64>());
        for (record, result) in results {
            let error = match result {
                Ok(()) => {
                    summary.registered += 1;
                    continue;
                }
                Err(error) => error,
            };
            match classify(&error) {
                Disposition::AlreadyPresent => summary.already_present += 1,
                Disposition::Retry if round < MAX_ATTEMPTS => {
                    if let Some(pushback) = pushback_delay(&error) {
                        delay = delay.max(pushback);
                    }
                    pending.push(record);
                }
                Disposition::Retry | Disposition::Fail => {
                    let identifier = record.identifier();
                    tracing::error!(
                        identifier,
                        error = %error,
                        "failed to register expected inventory record"
                    );
                    summary.failed_identifiers.push(identifier);
                }
            }
        }
        if pending.is_empty() {
            break;
        }
        tracing::warn!(
            round,
            max_attempts = MAX_ATTEMPTS,
            pending_count = pending.len(),
            retry_delay_milliseconds = delay.as_millis(),
            "transient errors registering expected inventory records; retrying"
        );
        tokio::time::sleep(delay).await;
    }
    summary.failed_identifiers.sort();
    summary
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use carbide_test_support::{Check, check_values};
    use rpc::admission_retry::ADMISSION_RETRY_PUSHBACK_HEADER;
    use tonic::Status;
    use tonic::metadata::MetadataValue;

    use super::*;

    fn invocation(status: Status) -> ClientApiError {
        ClientApiError::InvocationError(status)
    }

    fn admission_rejected(pushback: &str) -> ClientApiError {
        let mut status = Status::resource_exhausted("admission");
        status.metadata_mut().insert(
            ADMISSION_RETRY_PUSHBACK_HEADER,
            MetadataValue::try_from(pushback).unwrap(),
        );
        invocation(status)
    }

    fn machine_record(serial: &str) -> ExpectedRecord {
        ExpectedRecord::Machine {
            bmc_mac_address: format!("02:00:00:00:00:{:02x}", serial.len()),
            chassis_serial_number: serial.to_string(),
            rack_id: None,
            dpu_policy: None,
            dpf_enabled: true,
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
                    scenario: "NVOS MAC claimed by another expected switch is a conflict",
                    input: invocation(Status::failed_precondition(
                        "NVOS MAC address is already claimed by another expected switch: 02:00:00:00:00:02",
                    )),
                    expect: Disposition::Fail,
                },
                Check {
                    scenario: "Internal is transient",
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
                    scenario: "admission rejection with a pushback delay is transient",
                    input: admission_rejected("5000"),
                    expect: Disposition::Retry,
                },
                Check {
                    scenario: "admission rejection with a negative pushback is permanent",
                    input: admission_rejected("-1"),
                    expect: Disposition::Fail,
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

    /// Runs `register_all` over one machine record per scripted serial; each
    /// attempt on a record pops its next response. Returns the summary, the
    /// attempts per serial, and the peak number of in-flight attempts.
    async fn register_scripted(
        script: Vec<(&str, Vec<Result<(), ClientApiError>>)>,
        concurrency: usize,
    ) -> (ExpectedInventorySummary, HashMap<String, u32>, usize) {
        let records = script
            .iter()
            .map(|(serial, _)| machine_record(serial))
            .collect();
        let responses: HashMap<String, VecDeque<_>> = script
            .into_iter()
            .map(|(serial, responses)| (serial.to_string(), responses.into_iter().collect()))
            .collect();
        let responses = Arc::new(Mutex::new(responses));
        let attempts = Arc::new(Mutex::new(HashMap::<String, u32>::new()));
        let in_flight = Arc::new(Mutex::new((0_usize, 0_usize)));

        let summary = register_all(records, concurrency, |record| {
            let responses = responses.clone();
            let attempts = attempts.clone();
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
                } = record
                else {
                    unreachable!("only machine records are scripted");
                };
                *attempts
                    .lock()
                    .unwrap()
                    .entry(chassis_serial_number.clone())
                    .or_default() += 1;
                let response = responses
                    .lock()
                    .unwrap()
                    .get_mut(&chassis_serial_number)
                    .and_then(VecDeque::pop_front)
                    .expect("more attempts than scripted responses");
                in_flight.lock().unwrap().0 -= 1;
                response
            }
        })
        .await;

        let attempts = attempts.lock().unwrap().clone();
        let peak_in_flight = in_flight.lock().unwrap().1;
        (summary, attempts, peak_in_flight)
    }

    #[tokio::test(start_paused = true)]
    async fn register_all_retries_transient_failures_by_round() {
        let concurrency = 2;
        let transient = || Err(invocation(Status::unavailable("busy")));
        let (summary, attempts, peak_in_flight) = register_scripted(
            vec![
                ("ok", vec![Ok(())]),
                (
                    "dup",
                    vec![Err(invocation(Status::already_exists("present")))],
                ),
                (
                    "bad",
                    vec![Err(invocation(Status::invalid_argument("serial")))],
                ),
                ("late", vec![transient(), Ok(())]),
                ("never", (0..MAX_ATTEMPTS).map(|_| transient()).collect()),
            ],
            concurrency,
        )
        .await;

        assert_eq!(
            summary,
            ExpectedInventorySummary {
                registered: 2,
                already_present: 1,
                failed_identifiers: vec![
                    machine_record("bad").identifier(),
                    machine_record("never").identifier(),
                ],
            }
        );
        assert_eq!(
            attempts,
            HashMap::from([
                ("ok".to_string(), 1),
                ("dup".to_string(), 1),
                ("bad".to_string(), 1),
                ("late".to_string(), 2),
                ("never".to_string(), MAX_ATTEMPTS),
            ])
        );
        assert!(
            peak_in_flight <= concurrency,
            "in-flight registrations exceeded the concurrency bound"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn register_all_waits_for_admission_pushback() {
        let start = tokio::time::Instant::now();

        let (summary, ..) = register_scripted(
            vec![("slow", vec![Err(admission_rejected("5000")), Ok(())])],
            1,
        )
        .await;

        assert_eq!(summary.registered, 1);
        assert!(
            start.elapsed() >= Duration::from_secs(5),
            "retried after {:?}, before the 5 s the server asked for",
            start.elapsed()
        );
    }
}
