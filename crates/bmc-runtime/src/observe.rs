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

//! What every BMC operation reports: a span naming the BMC, capability,
//! driver, and operation; the outbound-call RED histogram; and a log line for
//! each failure. An HTTP error response is logged once, with its sanitized
//! body, by the Redfish transport that received it.

use std::net::SocketAddr;
use std::time::Instant;

use bmc_drivers::CatalogError;
use bmc_platform::{Capability, PlatformError};
use carbide_instrument::{Event, emit, red};
use tracing::Instrument;

use crate::BmcRef;

/// The `backend` label operations record under. It matches the label
/// `carbide-redfish` records libredfish calls under, so both backends share
/// one series set.
const BACKEND: &str = "redfish";

#[derive(Event)]
#[event(
    event_name = "bmc_operation_failed",
    component = "bmc-runtime",
    log = warn,
    metric = none,
    message = "BMC operation failed"
)]
struct BmcOperationFailed {
    #[context]
    bmc: SocketAddr,
    #[context]
    operation: &'static str,
    #[context]
    error: String,
}

/// The BMC's saved selection names a driver this binary cannot serve.
#[derive(Event)]
#[event(
    event_name = "bmc_driver_unservable",
    component = "bmc-runtime",
    log = warn,
    metric = none,
    message = "no compiled driver serves the BMC's selection"
)]
struct BmcDriverUnservable {
    #[context]
    bmc: SocketAddr,
    #[context]
    capability: Capability,
    #[context]
    error: String,
}

/// Runs `call` as `operation` on `endpoint`, recording the RED histogram with
/// an unsupported answer as its own outcome, and logging any other failure
/// the transport has not already logged. Only the call is timed. `capability`
/// is `None` for an operation no capability driver runs.
pub(crate) async fn observe<T>(
    endpoint: &BmcRef,
    capability: Option<Capability>,
    operation: &'static str,
    call: impl Future<Output = Result<T, PlatformError>>,
) -> Result<T, PlatformError> {
    let bmc = endpoint.address();
    let driver = capability.map(|capability| endpoint.driver_map().get(capability));
    let span = tracing::info_span!(
        "bmc_operation",
        %bmc,
        capability = capability.map(Capability::as_str),
        driver = driver.map(tracing::field::display),
        rules_hash = %endpoint.selection_hash(),
        operation,
    );
    async {
        let started = Instant::now();
        let result = call.await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(PlatformError::Unsupported) => "unsupported",
            Err(_) => "error",
        };
        red::record(BACKEND, operation, outcome, elapsed_ms);
        if let Err(error) = &result
            && needs_failure_log(error)
        {
            emit(BmcOperationFailed {
                bmc,
                operation,
                error: error.to_string(),
            });
        }
        result
    }
    .instrument(span)
    .await
}

/// Whether a failed operation still needs its own log line. An unsupported
/// answer is not a failure, and the transport has already logged the HTTP
/// response behind a BMC or authentication error, with its sanitized body.
fn needs_failure_log(error: &PlatformError) -> bool {
    !matches!(
        error,
        PlatformError::Unsupported | PlatformError::Bmc { .. } | PlatformError::Auth(_)
    )
}

/// Reports why `endpoint` has no driver for `capability`. A capability the
/// selection marks unsupported is an answer rather than a fault.
pub(crate) fn driver_unavailable(endpoint: &BmcRef, capability: Capability, error: &CatalogError) {
    let bmc = endpoint.address();
    match error {
        CatalogError::Unsupported { .. } => tracing::debug!(
            %bmc,
            capability = capability.as_str(),
            "BMC capability is unsupported"
        ),
        error => emit(BmcDriverUnservable {
            bmc,
            capability,
            error: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use bmc_drivers::Driver;
    use carbide_instrument::testing::{MetricsCapture, capture_logs, capture_logs_async};

    use super::*;

    fn endpoint() -> BmcRef {
        crate::test_support::endpoint(Default::default())
    }

    /// Every completion is timed under its outcome, as libredfish's are. A
    /// failure is logged unless it is an unsupported answer or an HTTP error
    /// response, which the transport logs with its body.
    #[tokio::test]
    async fn operations_record_their_outcome_and_log_unlogged_failures() {
        let endpoint = endpoint();
        let cases = [
            ("ok", Ok(()), false),
            ("unsupported", Err(PlatformError::Unsupported), false),
            ("error", Err(PlatformError::Unreachable), true),
            (
                "error",
                Err(PlatformError::from_http_response(500, "internal error")),
                false,
            ),
        ];
        for (outcome, result, logged) in cases {
            let case = format!("{result:?}");
            let metrics = MetricsCapture::start();
            let (_, logs) = capture_logs_async(observe(
                &endpoint,
                Some(Capability::Power),
                "power.set",
                async { result },
            ))
            .await;
            assert_eq!(
                metrics.histogram_count_delta(
                    "carbide_external_call_duration_milliseconds",
                    &[
                        ("backend", "redfish"),
                        ("operation", "power.set"),
                        ("outcome", outcome),
                    ],
                ),
                1,
                "{case}"
            );
            let failures: Vec<_> = logs
                .iter()
                .filter(|log| log.field("event_name") == Some("bmc_operation_failed"))
                .collect();
            assert_eq!(failures.len(), usize::from(logged), "{case}");
            if let Some(failure) = failures.first() {
                assert_eq!(failure.field("operation"), Some("power.set"));
                assert_eq!(failure.field("bmc"), Some("192.0.2.10:443"));
                assert_eq!(failure.field("error"), Some("BMC is unreachable"));
            }
        }
    }

    /// An unsupported capability is an answer, logged at debug; a selection
    /// this binary cannot serve warns.
    #[test]
    fn unavailable_drivers_are_reported_by_cause() {
        let endpoint = endpoint();
        let cases = [
            (
                CatalogError::Unsupported {
                    capability: Capability::Lockdown,
                },
                None,
                tracing::Level::DEBUG,
            ),
            (
                CatalogError::WrongCapability {
                    capability: Capability::Lockdown,
                    driver: Driver::DellIdracBios,
                },
                Some("bmc_driver_unservable"),
                tracing::Level::WARN,
            ),
        ];
        for (error, event_name, level) in cases {
            let logs = capture_logs(|| driver_unavailable(&endpoint, Capability::Lockdown, &error));
            assert_eq!(logs.len(), 1, "{error}");
            assert_eq!(logs[0].field("event_name"), event_name, "{error}");
            assert_eq!(logs[0].level, level, "{error}");
            assert_eq!(logs[0].field("capability"), Some("lockdown"));
        }
    }
}
