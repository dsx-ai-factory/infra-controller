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

pub mod convert;
pub mod drain;
pub mod metrics_drain;

use std::hash::Hash;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use carbide_instrument::{LabelValue, emit};
use opentelemetry::StringValue;
use tokio::task::JoinSet;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::HealthError;
use crate::collectors::{BackoffConfig, ExponentialBackoff};
use crate::config::{OtlpTargetConfig, OtlpTlsConfig};
use crate::sink::DedupQueue;

/// Maximum time allowed to establish a replacement OTLP channel.
const OTLP_RELOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Which OTLP signal a drain exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum OtlpSignal {
    Logs,
    Metrics,
}

/// An OTLP endpoint selected from the finite target list loaded at startup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfiguredOtlpTarget(pub(crate) String);

impl LabelValue for ConfiguredOtlpTarget {
    fn label_value(&self) -> StringValue {
        self.0.clone().into()
    }
}

/// An OTLP queue dropped its oldest entry to admit a new identity.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "otlp_queue_entry_dropped",
    metric_name = "carbide_health_otlp_queue_dropped_total",
    component = "nico-hardware-health",
    log = off,
    metric = counter,
    message = "otlp queue dropped its oldest entry",
    describe = "Number of OTLP queue entries dropped because a per-target queue reached capacity, by target and signal."
)]
pub(crate) struct OtlpQueueEntryDropped {
    #[label]
    pub(crate) target: ConfiguredOtlpTarget,
    #[label]
    pub(crate) signal: OtlpSignal,
}

/// Builds an OTLP endpoint with the target's current TLS or mTLS policy.
///
/// HTTPS targets without an explicit TLS profile use platform trust roots. An
/// explicit profile is reread on every call so reconnects can adopt rotated
/// certificate files.
///
/// # Errors
///
/// Returns an error when the endpoint URI is invalid or the configured TLS
/// material cannot be loaded, validated, or applied.
pub(crate) async fn target_endpoint(target: &OtlpTargetConfig) -> Result<Endpoint, HealthError> {
    let endpoint = Channel::from_shared(target.endpoint.clone()).map_err(|error| {
        HealthError::GenericError(format!(
            "invalid OTLP target endpoint {}: {error}",
            target.endpoint
        ))
    })?;

    let tls_config = match &target.tls {
        Some(tls) => crate::tls::otlp_tonic_tls_config(tls).await?,
        None if endpoint.uri().scheme_str() == Some("https") => {
            ClientTlsConfig::new().with_enabled_roots()
        }
        None => return Ok(endpoint),
    };

    endpoint.tls_config(tls_config).map_err(|error| {
        HealthError::GenericError(format!(
            "invalid TLS configuration for OTLP target {}: {error}",
            target.endpoint
        ))
    })
}

/// Establishes a replacement channel before a drain adopts refreshed TLS material.
///
/// The function returns only after the TCP and TLS handshakes succeed. Reload
/// failures and attempts exceeding ten seconds return an error, allowing the
/// caller to retain its current channel.
///
/// # Errors
///
/// Returns an error when endpoint construction fails, the connection cannot be
/// established, or the connection attempt exceeds its deadline.
pub(crate) async fn connect_replacement_target(
    target: &OtlpTargetConfig,
) -> Result<Channel, HealthError> {
    connect_replacement_target_with_timeout(target, OTLP_RELOAD_CONNECT_TIMEOUT).await
}

async fn connect_replacement_target_with_timeout(
    target: &OtlpTargetConfig,
    connect_timeout: Duration,
) -> Result<Channel, HealthError> {
    let endpoint = target_endpoint(target).await?;

    let channel = tokio::time::timeout(connect_timeout, endpoint.connect())
        .await
        .map_err(|_| {
            HealthError::GenericError(format!(
                "timed out connecting replacement channel for OTLP target {} after {:?}",
                target.endpoint, connect_timeout
            ))
        })?;

    channel.map_err(|error| {
        HealthError::GenericError(format!(
            "failed to connect replacement channel for OTLP target {}: {error}",
            target.endpoint
        ))
    })
}

/// A gRPC status code as a bounded metric label: one variant per
/// [`tonic::Code`], a set closed by the gRPC protocol itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum GrpcCode {
    Ok,
    Cancelled,
    Unknown,
    InvalidArgument,
    DeadlineExceeded,
    NotFound,
    AlreadyExists,
    PermissionDenied,
    ResourceExhausted,
    FailedPrecondition,
    Aborted,
    OutOfRange,
    Unimplemented,
    Internal,
    Unavailable,
    DataLoss,
    Unauthenticated,
}

impl From<tonic::Code> for GrpcCode {
    fn from(code: tonic::Code) -> Self {
        match code {
            tonic::Code::Ok => Self::Ok,
            tonic::Code::Cancelled => Self::Cancelled,
            tonic::Code::Unknown => Self::Unknown,
            tonic::Code::InvalidArgument => Self::InvalidArgument,
            tonic::Code::DeadlineExceeded => Self::DeadlineExceeded,
            tonic::Code::NotFound => Self::NotFound,
            tonic::Code::AlreadyExists => Self::AlreadyExists,
            tonic::Code::PermissionDenied => Self::PermissionDenied,
            tonic::Code::ResourceExhausted => Self::ResourceExhausted,
            tonic::Code::FailedPrecondition => Self::FailedPrecondition,
            tonic::Code::Aborted => Self::Aborted,
            tonic::Code::OutOfRange => Self::OutOfRange,
            tonic::Code::Unimplemented => Self::Unimplemented,
            tonic::Code::Internal => Self::Internal,
            tonic::Code::Unavailable => Self::Unavailable,
            tonic::Code::DataLoss => Self::DataLoss,
            tonic::Code::Unauthenticated => Self::Unauthenticated,
        }
    }
}

/// A drain dropped a whole export batch: the collector rejected it with a
/// non-retryable status, or the retry budget ran out.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "otlp_export_failed",
    metric_name = "carbide_health_otlp_export_failures_total",
    component = "nico-hardware-health",
    log = error,
    metric = counter,
    message = "otlp export failed, dropping batch",
    describe = "Number of OTLP export batches dropped after a send failure, by signal and gRPC status code."
)]
pub(crate) struct OtlpExportFailed {
    #[label]
    pub signal: OtlpSignal,
    #[label]
    pub code: GrpcCode,
    /// The status message the collector returned.
    #[context]
    pub error: String,
    /// How many log records or metric points the dropped batch held.
    #[context]
    pub record_count: usize,
    /// The attempt index the drop happened on (the retry cap for retryable
    /// statuses, earlier for non-retryable ones).
    #[context]
    pub attempt: usize,

    /// Configured endpoint that rejected or failed to accept the batch.
    #[context]
    pub endpoint: String,
}

/// One OTLP signal's export path to a target: how a drain turns queued items
/// into a request and sends it.
pub(crate) trait OtlpExport {
    type Item;
    type Request: Send;

    /// Builds the request for `items`, stamped with the batch's export time.
    fn build(&self, items: &[Self::Item], observed_nanos: u64) -> Self::Request;

    /// Number of log records or metric points in `request`.
    fn record_count(request: &Self::Request) -> usize;

    /// Encoded size of `request` in bytes.
    fn encoded_len(request: &Self::Request) -> usize;

    fn send(
        &mut self,
        request: Self::Request,
    ) -> impl Future<Output = Result<(), tonic::Status>> + Send;
}

const MAX_EXPORT_RETRIES: usize = 5;

/// Exports `items` in order, in requests no larger than the target's
/// `max_request_bytes`.
///
/// A range whose request is too large, or that the target rejects with
/// `RESOURCE_EXHAUSTED`, is split in half at once until each part fits; a
/// single item is always sent. Other retryable failures, including
/// `RESOURCE_EXHAUSTED` for a single item, resend the same range, rebuilt with
/// the same export time, up to five times with a backoff that starts over for
/// each range. A range that still fails is dropped and reported with
/// [`OtlpExportFailed`].
pub(crate) async fn export_items<E: OtlpExport>(
    export: &mut E,
    items: &[E::Item],
    target: &OtlpTargetConfig,
    signal: OtlpSignal,
) {
    let observed_nanos = convert::export_time_nanos();
    // Ranges waiting to be sent, the next one last.
    let mut pending: Vec<Range<usize>> = std::iter::once(0..items.len()).collect();

    while let Some(range) = pending.pop() {
        let request = export.build(&items[range.clone()], observed_nanos);
        let record_count = E::record_count(&request);
        if record_count == 0 {
            continue;
        }
        if range.len() > 1 && E::encoded_len(&request) > target.max_request_bytes {
            push_halves(&mut pending, range);
            continue;
        }

        let mut backoff = ExponentialBackoff::new(&BackoffConfig {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(10),
        });
        let mut request = Some(request);
        for attempt in 0..=MAX_EXPORT_RETRIES {
            // `send` consumes the request, so only a retry pays for another
            // one, rebuilt with the same export time.
            let request = request
                .take()
                .unwrap_or_else(|| export.build(&items[range.clone()], observed_nanos));
            match export.send(request).await {
                Ok(()) => {
                    tracing::debug!(
                        endpoint = %target.endpoint,
                        ?signal,
                        record_count,
                        "exported to otlp target"
                    );
                    break;
                }
                Err(status)
                    if status.code() == tonic::Code::ResourceExhausted && range.len() > 1 =>
                {
                    tracing::warn!(
                        error = status.message(),
                        endpoint = %target.endpoint,
                        ?signal,
                        record_count,
                        "otlp target rejected export as resource exhausted, splitting it"
                    );
                    push_halves(&mut pending, range.clone());
                    break;
                }
                Err(status) if is_retryable(&status) && attempt < MAX_EXPORT_RETRIES => {
                    let delay = backoff.next_delay();
                    tracing::warn!(
                        grpc_status_code = ?status.code(),
                        error = status.message(),
                        endpoint = %target.endpoint,
                        ?signal,
                        attempt,
                        retry_in = ?delay,
                        "retryable otlp export error"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(status) => {
                    emit(OtlpExportFailed {
                        signal,
                        code: status.code().into(),
                        error: status.message().to_string(),
                        record_count,
                        attempt,
                        endpoint: target.endpoint.clone(),
                    });
                    break;
                }
            }
        }
    }
}

/// Drains `queue` to `target` for one signal for as long as the task runs.
///
/// Whenever the queue is notified, an export finishes, or `flush_interval`
/// elapses, the drain starts an export for every full batch in the queue while
/// fewer than `max_concurrent_exports` are in flight. Each export runs as its
/// own task, so building and encoding requests uses several worker threads. A
/// partial batch is sent when `flush_interval` elapses without a full one.
/// Exports share one connection, which is replaced when the target's TLS
/// material is reloaded.
pub(crate) async fn run_drain<K, E>(
    queue: Arc<DedupQueue<K, E::Item>>,
    target: OtlpTargetConfig,
    signal: OtlpSignal,
    make_export: impl Fn(Channel) -> E,
) where
    K: Eq + Hash + Clone,
    E: OtlpExport + Clone + Send + 'static,
    E::Item: Send + Sync + 'static,
{
    let target = Arc::new(target);
    let mut export = make_export(connect(&target, signal).await);
    let mut batch = Vec::with_capacity(target.batch_size);
    let mut interval = tokio::time::interval(target.flush_interval);

    // Non-TLS targets use the default only to construct a dormant interval;
    // the select guard below disables reloads for them. Start after one full
    // period and delay missed ticks so stalled drains do not initiate a
    // burst of replacement connections when they resume.
    let tls_reload_period = target
        .tls
        .as_ref()
        .map_or(OtlpTlsConfig::DEFAULT_RELOAD_INTERVAL, |tls| {
            tls.reload_interval
        });
    let mut tls_reload_interval = tokio::time::interval_at(
        tokio::time::Instant::now() + tls_reload_period,
        tls_reload_period,
    );
    tls_reload_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut in_flight = JoinSet::new();
    loop {
        let mut flush_partial = false;
        tokio::select! {
            Some(result) = in_flight.join_next(), if !in_flight.is_empty() => {
                if let Err(error) = result {
                    tracing::error!(%error, endpoint = %target.endpoint, ?signal, "otlp export task failed");
                }
            }
            () = queue.notified(), if in_flight.len() < target.max_concurrent_exports => {}
            _ = interval.tick() => flush_partial = true,
            _ = tls_reload_interval.tick(), if target.tls.is_some() => {
                match connect_replacement_target(&target).await {
                    Ok(channel) => {
                        export = make_export(channel);
                        tracing::debug!(
                            endpoint = %target.endpoint,
                            ?signal,
                            "refreshed otlp target TLS material"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            ?error,
                            endpoint = %target.endpoint,
                            ?signal,
                            "failed to reload otlp target TLS material, keeping current client"
                        );
                    }
                }
            }
        }

        while in_flight.len() < target.max_concurrent_exports {
            drain_batch(&queue, &mut batch, target.batch_size);
            let full = batch.len() >= target.batch_size;
            let send_partial = flush_partial && !batch.is_empty();
            if !full && !send_partial {
                break;
            }
            let ready = std::mem::replace(&mut batch, Vec::with_capacity(target.batch_size));
            in_flight.spawn(export_batch(export.clone(), ready, target.clone(), signal));
            if !full {
                break;
            }
            interval.reset();
        }
    }
}

async fn export_batch<E: OtlpExport>(
    mut export: E,
    batch: Vec<E::Item>,
    target: Arc<OtlpTargetConfig>,
    signal: OtlpSignal,
) {
    export_items(&mut export, &batch, &target, signal).await;
}

/// Tops `batch` up to `batch_size` from the queue.
fn drain_batch<K: Eq + Hash + Clone, V>(
    queue: &DedupQueue<K, V>,
    batch: &mut Vec<V>,
    batch_size: usize,
) {
    while batch.len() < batch_size {
        match queue.pop() {
            Some((_key, value)) => batch.push(value),
            None => break,
        }
    }
}

/// Connects to `target`, retrying with backoff until it succeeds.
async fn connect(target: &OtlpTargetConfig, signal: OtlpSignal) -> Channel {
    let mut backoff = ExponentialBackoff::new(&BackoffConfig {
        initial: Duration::from_secs(1),
        max: Duration::from_secs(30),
    });

    loop {
        let endpoint = match target_endpoint(target).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let delay = backoff.next_delay();
                tracing::warn!(
                    ?error,
                    endpoint = %target.endpoint,
                    ?signal,
                    retry_in = ?delay,
                    "failed to configure otlp target connection"
                );
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        match endpoint.connect().await {
            Ok(channel) => {
                tracing::info!(endpoint = %target.endpoint, ?signal, "connected to otlp target");
                return channel;
            }
            Err(error) => {
                let delay = backoff.next_delay();
                tracing::warn!(
                    ?error,
                    endpoint = %target.endpoint,
                    ?signal,
                    retry_in = ?delay,
                    "failed to connect to otlp target"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Queues both halves of `range` so its first half is sent next.
fn push_halves(pending: &mut Vec<Range<usize>>, range: Range<usize>) {
    let middle = range.start + range.len() / 2;
    pending.push(middle..range.end);
    pending.push(range.start..middle);
}

fn is_retryable(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        tonic::Code::Unavailable
            | tonic::Code::DeadlineExceeded
            | tonic::Code::ResourceExhausted
            | tonic::Code::Aborted
            | tonic::Code::Internal
    )
}

pub use opentelemetry_proto::tonic::collector::logs::v1 as collector_logs;
pub use opentelemetry_proto::tonic::collector::metrics::v1 as collector_metrics;
pub use opentelemetry_proto::tonic::common::v1 as common;
pub use opentelemetry_proto::tonic::logs::v1 as logs;
pub use opentelemetry_proto::tonic::metrics::v1 as metrics;
pub use opentelemetry_proto::tonic::resource::v1 as resource;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_instrument::emit;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::{
        OtlpExport, OtlpExportFailed, OtlpSignal, connect_replacement_target_with_timeout,
        export_items, target_endpoint,
    };
    use crate::HealthError;
    use crate::config::OtlpTargetConfig;

    #[tokio::test]
    async fn https_target_without_tls_profile_starts_tls_handshake()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;

        let target = OtlpTargetConfig {
            endpoint: format!("https://{address}"),
            tls: None,
            batch_size: 1,
            queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
            max_request_bytes: OtlpTargetConfig::DEFAULT_MAX_REQUEST_BYTES,
            max_concurrent_exports: OtlpTargetConfig::DEFAULT_MAX_CONCURRENT_EXPORTS,
            flush_interval: Duration::from_secs(1),
            include_diagnostics: false,
            include_alert_details: false,
        };

        let endpoint = target_endpoint(&target).await?;

        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut content_type = [0_u8; 1];

            stream.read_exact(&mut content_type).await?;

            Ok::<_, std::io::Error>(content_type[0])
        });

        let _connect_result = timeout(Duration::from_secs(1), endpoint.connect()).await?;

        let content_type = timeout(Duration::from_secs(1), peer).await??;

        assert_eq!(
            content_type?, 0x16,
            "connection must start with a TLS record"
        );

        Ok(())
    }

    #[tokio::test]
    async fn replacement_connection_times_out_without_tls_handshake()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();

        let target = OtlpTargetConfig {
            endpoint: format!("https://{address}"),
            tls: None,
            batch_size: 1,
            queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
            max_request_bytes: OtlpTargetConfig::DEFAULT_MAX_REQUEST_BYTES,
            max_concurrent_exports: OtlpTargetConfig::DEFAULT_MAX_CONCURRENT_EXPORTS,
            flush_interval: Duration::from_secs(1),
            include_diagnostics: false,
            include_alert_details: false,
        };

        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;

            accepted_tx
                .send(())
                .map_err(|_| std::io::Error::other("replacement connection task stopped"))?;

            let _ = release_rx.await;

            drop(stream);

            Ok::<_, std::io::Error>(())
        });

        let replacement = tokio::spawn(async move {
            connect_replacement_target_with_timeout(&target, Duration::from_millis(100)).await
        });

        accepted_rx.await?;

        let result = replacement.await?;

        drop(release_tx);
        peer.await??;

        assert!(
            matches!(result, Err(HealthError::GenericError(message)) if message.contains("timed out connecting replacement channel"))
        );

        Ok(())
    }

    /// A dropped logs batch writes one ERROR line and ticks the counter's
    /// logs-signal series, labelled with the gRPC status code.
    #[test]
    fn otlp_export_failure_logs_error_and_ticks_counter() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(OtlpExportFailed {
                signal: OtlpSignal::Logs,
                code: tonic::Code::Unavailable.into(),
                error: "connection refused".to_string(),
                record_count: 17,
                attempt: 5,
                endpoint: "http://localhost:4317".to_string(),
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::ERROR);
        assert_eq!(logs[0].message, "otlp export failed, dropping batch");
        assert_eq!(
            metrics.counter_delta(
                "carbide_health_otlp_export_failures_total",
                &[("signal", "logs"), ("code", "unavailable")],
            ),
            1.0
        );
    }

    /// The metrics drain counts on its own signal series, and multi-word
    /// gRPC codes render as snake_case label values.
    #[test]
    fn otlp_metrics_export_failure_counts_on_the_metrics_signal_series() {
        let metrics = MetricsCapture::start();
        emit(OtlpExportFailed {
            signal: OtlpSignal::Metrics,
            code: tonic::Code::DeadlineExceeded.into(),
            error: "deadline exceeded".to_string(),
            record_count: 3,
            attempt: 0,
            endpoint: "http://localhost:4317".to_string(),
        });

        assert_eq!(
            metrics.counter_delta(
                "carbide_health_otlp_export_failures_total",
                &[("signal", "metrics"), ("code", "deadline_exceeded")],
            ),
            1.0
        );
    }

    /// Items are their encoded sizes in bytes; every request the target sees
    /// is recorded.
    struct FakeExport {
        reject_multi_item_requests: bool,
        /// Answer the first attempt of every distinct request `UNAVAILABLE`.
        fail_first_attempt: bool,
        sent: Vec<Vec<usize>>,
    }

    impl OtlpExport for FakeExport {
        type Item = usize;
        type Request = Vec<usize>;

        fn build(&self, items: &[usize], _observed_nanos: u64) -> Vec<usize> {
            items.to_vec()
        }

        fn record_count(request: &Vec<usize>) -> usize {
            request.len()
        }

        fn encoded_len(request: &Vec<usize>) -> usize {
            request.iter().sum()
        }

        async fn send(&mut self, request: Vec<usize>) -> Result<(), tonic::Status> {
            let rejected = self.reject_multi_item_requests && request.len() > 1;
            let first_attempt = !self.sent.contains(&request);
            self.sent.push(request);
            if rejected {
                return Err(tonic::Status::resource_exhausted("message too large"));
            }
            if self.fail_first_attempt && first_attempt {
                return Err(tonic::Status::unavailable("collector restarting"));
            }
            Ok(())
        }
    }

    struct ExportCase {
        items: Vec<usize>,
        reject_multi_item_requests: bool,
        fail_first_attempt: bool,
    }

    async fn requests_sent(case: ExportCase) -> Result<Vec<Vec<usize>>, std::convert::Infallible> {
        Ok(run_export(case).await.0)
    }

    /// Runs `export_items` against a fake target that accepts at most 10
    /// bytes, returning every request it saw and the (virtual) time it took.
    async fn run_export(case: ExportCase) -> (Vec<Vec<usize>>, Duration) {
        let mut export = FakeExport {
            reject_multi_item_requests: case.reject_multi_item_requests,
            fail_first_attempt: case.fail_first_attempt,
            sent: Vec::new(),
        };
        let target = OtlpTargetConfig {
            endpoint: "http://collector.example:4317".to_string(),
            tls: None,
            batch_size: 512,
            queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
            max_request_bytes: 10,
            max_concurrent_exports: 1,
            flush_interval: Duration::from_secs(2),
            include_diagnostics: false,
            include_alert_details: false,
        };
        let started = tokio::time::Instant::now();
        export_items(&mut export, &case.items, &target, OtlpSignal::Logs).await;
        (export.sent, started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn export_items_bounds_request_size_cases() {
        check_cases_async(
            [
                Case {
                    scenario: "batch within the limit is sent whole",
                    input: ExportCase {
                        items: vec![3, 3, 4],
                        reject_multi_item_requests: false,
                        fail_first_attempt: false,
                    },
                    expect: Yields(vec![vec![3, 3, 4]]),
                },
                Case {
                    scenario: "oversized batch is halved until each part fits, in order",
                    input: ExportCase {
                        items: vec![6, 7, 8],
                        reject_multi_item_requests: false,
                        fail_first_attempt: false,
                    },
                    expect: Yields(vec![vec![6], vec![7], vec![8]]),
                },
                Case {
                    scenario: "single item over the limit is still sent",
                    input: ExportCase {
                        items: vec![20],
                        reject_multi_item_requests: false,
                        fail_first_attempt: false,
                    },
                    expect: Yields(vec![vec![20]]),
                },
                Case {
                    scenario: "batch rejected as resource exhausted is halved",
                    input: ExportCase {
                        items: vec![1, 2, 3],
                        reject_multi_item_requests: true,
                        fail_first_attempt: false,
                    },
                    expect: Yields(vec![vec![1, 2, 3], vec![1], vec![2, 3], vec![2], vec![3]]),
                },
            ],
            requests_sent,
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn export_items_retry_timing() {
        struct TimingCase {
            scenario: &'static str,
            input: ExportCase,
            expect: std::ops::Range<Duration>,
        }

        for case in [
            TimingCase {
                scenario: "a batch rejected as resource exhausted is split without waiting",
                input: ExportCase {
                    items: vec![1, 2, 3],
                    reject_multi_item_requests: true,
                    fail_first_attempt: false,
                },
                expect: Duration::ZERO..Duration::from_nanos(1),
            },
            TimingCase {
                // Each range waits one fresh 100-200 ms first delay; a backoff
                // shared across ranges would wait at least 700 ms.
                scenario: "each range starts its own backoff",
                input: ExportCase {
                    items: vec![6, 7, 8],
                    reject_multi_item_requests: false,
                    fail_first_attempt: true,
                },
                expect: Duration::from_millis(300)..Duration::from_millis(600),
            },
        ] {
            let (_sent, elapsed) = run_export(case.input).await;
            assert!(
                case.expect.contains(&elapsed),
                "{}: took {elapsed:?}, expected {:?}",
                case.scenario,
                case.expect
            );
        }
    }
}
