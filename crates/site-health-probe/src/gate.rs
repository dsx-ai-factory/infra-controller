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

//! Per-API call gates. Every call the probe makes to one API (nico-api,
//! nico-rest-api, the metrics store) passes that API's gate, so whatever mix
//! of readers and latency probes is running, the API never sees more than one
//! probe call in flight or calls started faster than the configured rate.
//! Pushback (429, 503, RESOURCE_EXHAUSTED, UNAVAILABLE) widens the spacing
//! until a call succeeds.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::config::Caps;
use crate::framework::ObservationRecorder;

/// Retries a reader spends on one call after its first attempt.
pub(crate) const MAX_RETRIES: usize = 2;

/// The first pushback waits this long; later ones double it.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);

/// Why one API call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallError {
    /// The call ran past the gate's call timeout.
    Timeout,
    /// The API asked the probe to slow down.
    Pushback { retry_after: Option<Duration> },
    /// The API answered with an error.
    Status(String),
    /// The reply did not decode.
    Decode(String),
    /// The call never got an answer.
    Transport(String),
}

impl CallError {
    /// A bounded description, safe to log and to serve: an answer's status
    /// code at most, never upstream message text, reply values or URLs.
    pub(crate) fn category(&self) -> String {
        match self {
            CallError::Timeout => "timed out".to_string(),
            CallError::Pushback { .. } => "pushed back".to_string(),
            // Every Status is built from a status code alone.
            CallError::Status(code) => code.clone(),
            CallError::Decode(_) => "undecodable reply".to_string(),
            CallError::Transport(_) => "transport error".to_string(),
        }
    }

    /// Timeouts, pushback and transport errors may pass on a retry; an error
    /// answer or an undecodable reply will not.
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(
            self,
            CallError::Timeout | CallError::Pushback { .. } | CallError::Transport(_)
        )
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Timeout => write!(f, "call timed out"),
            CallError::Pushback {
                retry_after: Some(after),
            } => write!(f, "pushback, retry after {after:?}"),
            CallError::Pushback { retry_after: None } => write!(f, "pushback"),
            CallError::Status(msg) | CallError::Decode(msg) | CallError::Transport(msg) => {
                write!(f, "{msg}")
            }
        }
    }
}

impl std::error::Error for CallError {}

/// Serializes and spaces the calls to one API.
pub(crate) struct CallGate {
    api: &'static str,
    spacing: Duration,
    call_timeout: Duration,
    max_backoff: Duration,
    /// One permit: at most one call in flight. Tokio's semaphore is fair, so
    /// waiting callers start in arrival order.
    permit: Semaphore,
    state: Mutex<GateState>,
    /// `calls_total`, once metrics are set up.
    calls: OnceLock<Counter<u64>>,
}

struct GateState {
    next_start: Instant,
    backoff: Duration,
}

impl CallGate {
    pub(crate) fn new(
        api: &'static str,
        calls_per_second: f64,
        call_timeout: Duration,
        max_backoff: Duration,
    ) -> Self {
        Self {
            api,
            spacing: Duration::from_secs_f64(1.0 / calls_per_second),
            call_timeout,
            max_backoff,
            permit: Semaphore::new(1),
            state: Mutex::new(GateState {
                next_start: Instant::now(),
                backoff: Duration::ZERO,
            }),
            calls: OnceLock::new(),
        }
    }

    /// Counts every call from now on in `calls`, by API, call and result.
    pub(crate) fn count_calls(&self, calls: Counter<u64>) {
        // A second registration keeps the first counter.
        let _ = self.calls.set(calls);
    }

    /// The API this gate guards ("nico-api", "nico-rest-api", "metrics-store").
    pub(crate) fn api(&self) -> &'static str {
        self.api
    }

    /// Runs one call: waits for the permit and the next start time, then runs
    /// `f` under the call timeout. Only the call itself is timed and recorded
    /// on `rec` as operation `call`, never the wait, and only when it
    /// succeeds. Dropping the returned future mid-call releases the permit and
    /// keeps the spacing already reserved for the next call.
    pub(crate) async fn call<T, F, Fut>(
        &self,
        rec: &ObservationRecorder,
        call: &'static str,
        f: F,
    ) -> Result<T, CallError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, CallError>>,
    {
        let _permit = self
            .permit
            .acquire()
            .await
            .map_err(|_| CallError::Transport(format!("{} gate closed", self.api)))?;
        let next_start = self.lock().next_start;
        tokio::time::sleep_until(next_start).await;

        let started = Instant::now();
        {
            let mut state = self.lock();
            state.next_start = started + self.spacing + state.backoff;
        }
        let result = match tokio::time::timeout(self.call_timeout, f()).await {
            Ok(result) => result,
            Err(_elapsed) => Err(CallError::Timeout),
        };
        let took = started.elapsed();
        if let Some(calls) = self.calls.get() {
            let outcome = match &result {
                Ok(_) => "ok",
                Err(CallError::Timeout) => "timeout",
                Err(CallError::Pushback { .. }) => "pushback",
                Err(_) => "error",
            };
            calls.add(
                1,
                &[
                    KeyValue::new("api", self.api),
                    KeyValue::new("call", call),
                    KeyValue::new("result", outcome),
                ],
            );
        }

        let mut state = self.lock();
        match &result {
            Ok(_) => {
                state.backoff = Duration::ZERO;
                state.next_start = started + self.spacing;
                rec.record(call, took);
            }
            Err(CallError::Pushback { retry_after }) => {
                state.backoff = match retry_after {
                    // A server's own hint wins, within the configured cap.
                    Some(after) => (*after).min(self.max_backoff),
                    None if state.backoff.is_zero() => FIRST_BACKOFF.min(self.max_backoff),
                    None => (state.backoff * 2).min(self.max_backoff),
                };
                // The back-off runs from the pushback reply, not the start.
                state.next_start = (started + self.spacing).max(Instant::now() + state.backoff);
                tracing::debug!(api = self.api, call, backoff = ?state.backoff, "pushback");
            }
            Err(_) => {}
        }
        result
    }

    /// Runs `f` through [`CallGate::call`], retrying at most [`MAX_RETRIES`]
    /// times while the error is retryable. Each attempt waits its turn at the
    /// gate, so retries never jump the spacing or the back-off.
    pub(crate) async fn call_with_retries<T, F, Fut>(
        &self,
        rec: &ObservationRecorder,
        call: &'static str,
        mut f: F,
    ) -> Result<T, CallError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, CallError>>,
    {
        let mut attempt = 0;
        loop {
            match self.call(rec, call, &mut f).await {
                Err(err) if err.is_retryable() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    tracing::debug!(
                        api = self.api,
                        call,
                        attempt,
                        error = %err.category(),
                        "retrying call"
                    );
                }
                result => return result,
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One gate per API, shared by every reader and probe that calls it.
pub(crate) struct Gates {
    pub nico_api: Arc<CallGate>,
    pub rest: Arc<CallGate>,
    pub metrics_store: Arc<CallGate>,
}

impl Gates {
    pub(crate) fn new(caps: &Caps) -> Self {
        let gate = |api| {
            Arc::new(CallGate::new(
                api,
                caps.calls_per_second,
                caps.call_timeout,
                caps.max_backoff,
            ))
        };
        Self {
            nico_api: gate("nico-api"),
            rest: gate("nico-rest-api"),
            metrics_store: gate("metrics-store"),
        }
    }

    /// Counts the calls of every gate in `calls`.
    pub(crate) fn count_calls(&self, calls: &Counter<u64>) {
        for gate in [&self.nico_api, &self.rest, &self.metrics_store] {
            gate.count_calls(calls.clone());
        }
    }
}

/// Maps a gRPC error. RESOURCE_EXHAUSTED and UNAVAILABLE are pushback,
/// DEADLINE_EXCEEDED is a timeout; any other code is an error answer. Only
/// the code is kept: a status message can carry anything the server put in
/// it.
pub(crate) fn classify_grpc(status: &tonic::Status) -> CallError {
    match status.code() {
        tonic::Code::ResourceExhausted | tonic::Code::Unavailable => {
            CallError::Pushback { retry_after: None }
        }
        tonic::Code::DeadlineExceeded => CallError::Timeout,
        code => CallError::Status(format!("gRPC {code:?}")),
    }
}

/// Maps an HTTP reply status. `None` for a success; 429 and 503 are pushback,
/// honoring a `Retry-After` in seconds; any other status is an error answer.
pub(crate) fn classify_http(status: StatusCode, headers: &HeaderMap) -> Option<CallError> {
    if status.is_success() {
        return None;
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        let retry_after = headers
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        return Some(CallError::Pushback { retry_after });
    }
    Some(CallError::Status(format!("HTTP {}", status.as_u16())))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn gate() -> Arc<CallGate> {
        Arc::new(CallGate::new(
            "nico-api",
            2.0,
            Duration::from_secs(5),
            Duration::from_secs(30),
        ))
    }

    /// Records each call's start and the most calls ever in flight at once.
    #[derive(Default)]
    struct Server {
        starts: Mutex<Vec<Instant>>,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
    }

    impl Server {
        async fn serve(&self, took: Duration) -> Result<(), CallError> {
            self.starts.lock().unwrap().push(Instant::now());
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(took).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }

        fn gaps(&self) -> Vec<Duration> {
            let mut starts = self.starts.lock().unwrap().clone();
            starts.sort();
            starts.windows(2).map(|w| w[1] - w[0]).collect()
        }
    }

    async fn hammer(gate: Arc<CallGate>, server: Arc<Server>, took: Duration) {
        let mut tasks = tokio::task::JoinSet::new();
        for task in 0..3 {
            let (gate, server) = (Arc::clone(&gate), Arc::clone(&server));
            tasks.spawn(async move {
                let rec = ObservationRecorder::default();
                // 20 calls across 3 tasks: 7, 7 and 6.
                for _ in 0..(if task == 2 { 6 } else { 7 }) {
                    gate.call(&rec, "find_machine_ids", || server.serve(took))
                        .await
                        .expect("call succeeds");
                }
            });
        }
        while let Some(joined) = tasks.join_next().await {
            joined.expect("task ends");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn gate_caps_hold_against_slow_server() {
        for took in [Duration::from_secs(3), Duration::ZERO] {
            let server = Arc::new(Server::default());
            hammer(gate(), Arc::clone(&server), took).await;
            assert_eq!(server.starts.lock().unwrap().len(), 20);
            assert_eq!(
                server.max_in_flight.load(Ordering::SeqCst),
                1,
                "took {took:?}"
            );
            for gap in server.gaps() {
                assert!(
                    gap >= Duration::from_millis(500) && gap >= took,
                    "took {took:?}: calls started {gap:?} apart"
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn gate_backs_off_on_pushback_and_resets() {
        let gate = gate();
        let server = Server::default();
        let rec = ObservationRecorder::default();
        for _ in 0..3 {
            let err = gate
                .call(&rec, "list", || async {
                    server.serve(Duration::ZERO).await?;
                    Err::<(), _>(CallError::Pushback { retry_after: None })
                })
                .await
                .expect_err("pushback");
            assert_eq!(err, CallError::Pushback { retry_after: None });
        }
        for _ in 0..2 {
            gate.call(&rec, "list", || server.serve(Duration::ZERO))
                .await
                .expect("success");
        }
        // Back-off 1, 2 and 4 s after the pushbacks. The first success resets
        // it, so the call after that waits only the 500 ms spacing.
        assert_eq!(
            server.gaps(),
            [1000, 2000, 4000, 500].map(Duration::from_millis)
        );
        assert_eq!(
            rec.take().len(),
            2,
            "only the successful calls are recorded"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn gate_honors_retry_after() {
        let gate = gate();
        let server = Server::default();
        let rec = ObservationRecorder::default();
        let hinted = |secs| {
            let server = &server;
            move || async move {
                server.serve(Duration::ZERO).await?;
                Err::<(), _>(CallError::Pushback {
                    retry_after: Some(Duration::from_secs(secs)),
                })
            }
        };
        gate.call(&rec, "list", hinted(7))
            .await
            .expect_err("pushback");
        // A hint above the cap is cut to the cap.
        gate.call(&rec, "list", hinted(3600))
            .await
            .expect_err("pushback");
        gate.call(&rec, "list", || server.serve(Duration::ZERO))
            .await
            .expect("success");
        assert_eq!(
            server.gaps(),
            vec![Duration::from_secs(7), Duration::from_secs(30)]
        );

        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(
            classify_http(StatusCode::TOO_MANY_REQUESTS, &headers),
            Some(CallError::Pushback {
                retry_after: Some(Duration::from_secs(7))
            })
        );
        assert_eq!(
            classify_http(StatusCode::SERVICE_UNAVAILABLE, &HeaderMap::new()),
            Some(CallError::Pushback { retry_after: None })
        );
        assert_eq!(
            classify_http(StatusCode::PRECONDITION_FAILED, &HeaderMap::new()),
            Some(CallError::Status("HTTP 412".to_string()))
        );
        assert_eq!(classify_http(StatusCode::OK, &headers), None);
        assert_eq!(
            classify_grpc(&tonic::Status::resource_exhausted("slow down")),
            CallError::Pushback { retry_after: None }
        );
        assert_eq!(
            classify_grpc(&tonic::Status::unavailable("restarting")),
            CallError::Pushback { retry_after: None }
        );
        // Only the code is kept: a status message can carry a token.
        let denied = classify_grpc(&tonic::Status::permission_denied("token=SENTINEL-c0ffee"));
        assert_eq!(
            denied,
            CallError::Status("gRPC PermissionDenied".to_string())
        );
        assert!(!format!("{denied} {}", denied.category()).contains("SENTINEL"));
    }

    #[tokio::test(start_paused = true)]
    async fn gate_deadline_is_timeout() {
        let gate = gate();
        let server = Arc::new(Server::default());
        let rec = ObservationRecorder::default();
        let started = Instant::now();
        let err = gate
            .call(&rec, "find_machines_by_ids", || {
                server.serve(Duration::from_secs(10))
            })
            .await
            .expect_err("deadline");
        assert_eq!(err, CallError::Timeout);
        assert_eq!(started.elapsed(), Duration::from_secs(5));
        assert!(rec.take().is_empty(), "a timed-out call is not recorded");

        // A timeout is retried, at most twice.
        let attempts = AtomicUsize::new(0);
        let err = gate
            .call_with_retries(&rec, "find_machines_by_ids", || {
                attempts.fetch_add(1, Ordering::SeqCst);
                server.serve(Duration::from_secs(10))
            })
            .await
            .expect_err("deadline every time");
        assert_eq!(err, CallError::Timeout);
        assert_eq!(attempts.load(Ordering::SeqCst), 1 + MAX_RETRIES);

        // An error answer is not retried.
        attempts.store(0, Ordering::SeqCst);
        gate.call_with_retries(&rec, "find_machines_by_ids", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(CallError::Status("denied".to_string())) }
        })
        .await
        .expect_err("error answer");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(!CallError::Decode("bad reply".to_string()).is_retryable());
    }
}
