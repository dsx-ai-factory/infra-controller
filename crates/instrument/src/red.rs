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

//! The outbound-call RED triad: wrap a call in [`instrumented`] and every
//! completion records
//! `carbide_external_call_duration_milliseconds{backend, operation, outcome}`
//! -- the histogram's `_count`, split by `outcome`, is the request and error
//! rate, so one instrument covers the triad. Successes are counted silently;
//! a failure also writes one WARN with the error as log-only context.
//!
//! The `backend` and `operation` labels are `&'static str` **on purpose**:
//! label values must be compile-time literals (a generated method name, a
//! fixed backend tag), never data from the wire -- the type is the
//! cardinality guard. For a streaming call, the recorded duration covers
//! connection and response headers (time to the stream handle), not the
//! stream's lifetime.

use std::fmt::Display;
use std::sync::OnceLock;
use std::time::Instant;

use opentelemetry::KeyValue;
use opentelemetry::metrics::Histogram;

/// The exposed name is `carbide_external_call_duration_milliseconds`; the
/// exporter appends the unit suffix (the `carbide-instrument` convention).
const INSTRUMENT_NAME: &str = "carbide_external_call_duration";

fn histogram() -> &'static Histogram<f64> {
    static HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
    HISTOGRAM.get_or_init(|| {
        opentelemetry::global::meter("carbide-instrument")
            .f64_histogram(INSTRUMENT_NAME)
            .with_unit("ms")
            .with_description(
                "Duration of outbound calls by backend, operation, and outcome; the _count \
                 series, split by outcome, gives the request and error rates.",
            )
            .build()
    })
}

/// Times `call`, records the RED histogram on every completion, and logs a
/// WARN only on failure. Returns the call's result untouched.
pub async fn instrumented<T, E: Display>(
    backend: &'static str,
    operation: &'static str,
    call: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let started = Instant::now();
    let result = call.await;
    let outcome = if result.is_ok() { "ok" } else { "error" };
    record(
        backend,
        operation,
        outcome,
        started.elapsed().as_secs_f64() * 1_000.0,
    );
    if let Err(error) = &result {
        tracing::warn!(backend, operation, error = %error, "external call failed");
    }
    result
}

/// Records one completed outbound call on the shared RED histogram. This is
/// the low-level half of [`instrumented`] for callers whose backend needs its
/// own outcome vocabulary or logging policy (a refusal that is an answer, not
/// a failure); `outcome` is a label value and must be a compile-time literal.
pub fn record(
    backend: &'static str,
    operation: &'static str,
    outcome: &'static str,
    elapsed_ms: f64,
) {
    histogram().record(
        elapsed_ms,
        &[
            KeyValue::new("backend", backend),
            KeyValue::new("operation", operation),
            KeyValue::new("outcome", outcome),
        ],
    );
}
