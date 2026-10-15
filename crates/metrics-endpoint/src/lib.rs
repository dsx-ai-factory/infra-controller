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
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

use bytes::Bytes;
use carbide_instrument::{Event, LabelValue, emit};
use carbide_metrics_utils::OtelView;
use http_body_util::Full;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, body};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Meter, MeterProvider};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_semantic_conventions as semconv;
use prometheus::proto::MetricFamily;
use prometheus::{Encoder, TextEncoder};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

mod listener;

pub use listener::bind_tcp_listener;

/// Health and readiness controller
#[derive(Debug, Clone)]
pub struct HealthController {
    ready: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
}

impl Default for HealthController {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthController {
    pub fn new() -> Self {
        // Ready and healthy by default
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            healthy: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Relaxed);
    }

    pub fn set_healthy(&self, healthy: bool) {
        self.healthy.store(healthy, Ordering::Relaxed);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSetup {
    pub registry: prometheus::Registry,
    pub meter: Meter,
    // Need to retain this, if it's dropped, metrics are not held
    pub meter_provider: SdkMeterProvider,
    pub health_controller: HealthController,
}

/// Depth of the bounded queue feeding the dedicated encoder thread.
///
/// The single encoder thread caps total encode CPU at one core no matter how many scrapes
/// arrive at once. This queue is sized to comfortably absorb a burst of concurrent scrapes
/// — roughly half a second of queued encode work on a modern core — so a transient backlog
/// waits behind the in-flight encode instead of being shed. Only a sustained overload that
/// fills all 64 slots sheds the excess with `503`.
const ENCODER_QUEUE_CAPACITY: usize = 64;

/// How long shutdown waits for the encoder thread to drain to its `Stop` and exit before
/// returning anyway. `Stop` cannot be observed while the thread is mid-encode, so a
/// pathologically slow — or hung — collector callback (`catch_unwind` catches panics, not
/// hangs) must not block shutdown forever; the detached thread finishes its encode and exits
/// shortly after, so this is bounded-lifetime lingering rather than a permanent leak.
const ENCODER_SHUTDOWN_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Why the encoder thread could not produce an exposition. Both variants become an HTTP
/// `500` but log distinctly. `Panicked` is isolated per-request via `catch_unwind` so a
/// misbehaving collector or observable callback cannot take down the sole encoder thread.
#[derive(Debug)]
enum EncodeError {
    /// The Prometheus text encoder returned an error.
    Encode(prometheus::Error),
    /// A collector or observable callback panicked during `gather()`/encode; the encoder
    /// thread caught the unwind and stayed alive to serve later scrapes.
    Panicked,
}

/// Why a `/metrics` scrape failed to return an exposition, as a bounded low-cardinality
/// label for [`MetricsScrapeFailed`]. Each variant maps to one failure branch of the
/// handler (and its HTTP status): `EncoderBusy` → 503, all others → 500.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum ScrapeOutcome {
    /// The encoder queue was full; the scrape was shed (503).
    EncoderBusy,
    /// The encoder thread was gone, so nothing could be enqueued (500).
    EncoderGone,
    /// The Prometheus text encoder returned an error (500).
    EncodeFailed,
    /// A collector/observable callback panicked during gather/encode (500).
    EncoderPanicked,
    /// The encoder dropped the reply without answering, e.g. during shutdown (500).
    ReplyDropped,
}

/// A metrics scrape did not produce a response. Each variant is one way that
/// happens, and picks the level and diagnostic that case already had.
#[derive(Event)]
#[event(
    event_name = "metrics_scrape_failed",
    metric_name = "carbide_metrics_scrape_failures_total",
    component = "metrics-endpoint",
    metric = counter,
    describe = "Number of /metrics scrape failures, by outcome.",
    labels(outcome: ScrapeOutcome),
)]
enum MetricsScrapeFailed {
    /// The encoder queue was full; the scrape was shed (503).
    #[event(labels(outcome = EncoderBusy), log = warn, message = "metrics encoder busy; shedding scrape with 503")]
    EncoderBusy {},

    /// The encoder thread was gone, so nothing could be enqueued (500).
    #[event(labels(outcome = EncoderGone), log = error, message = "metrics encoder thread is gone; cannot encode metrics")]
    EncoderGone {},

    /// The Prometheus text encoder returned an error (500).
    #[event(labels(outcome = EncodeFailed), log = error, message = "failed to encode metrics")]
    EncodeFailed {},

    /// A collector/observable callback panicked during gather/encode (500).
    #[event(labels(outcome = EncoderPanicked), log = error, message = "metrics encoder caught a panic while encoding")]
    EncoderPanicked {},

    /// The encoder dropped the reply without answering, e.g. during shutdown (500).
    #[event(labels(outcome = ReplyDropped), log = error, message = "metrics encoder dropped the reply without responding")]
    ReplyDropped {},
}

/// The reply channel a scrape hands to the encoder thread. The thread sends the encoded
/// exposition back through it, or an [`EncodeError`] describing why it could not.
type EncodeReply = oneshot::Sender<Result<Vec<u8>, EncodeError>>;

/// An item on the encoder queue: either a scrape to encode (carrying its reply channel) or
/// a control message telling the thread to stop. Using an explicit `Stop` lets the thread
/// block on `recv()` with no polling; shutdown enqueues one and the thread exits regardless
/// of how many connection tasks still hold a queue sender.
enum Item {
    Work(EncodeReply),
    Stop,
}

/// The shared state between HTTP requests
struct MetricsHandlerState {
    /// Bounded queue to the single dedicated encoder thread. Each `/metrics` scrape
    /// enqueues a reply channel and awaits the encoded exposition on it. The thread
    /// owns the registry and prefix, so those are not held here.
    encoder_tx: SyncSender<Item>,
    health_controller: HealthController,
}

/// An old/new prefix pair: metric families whose name starts with `old` are
/// re-exposed under `new` as well, so scrapers on either name keep working
/// through a rename migration.
#[derive(Debug, Clone)]
pub struct PrefixMigration {
    pub old: String,
    pub new: String,
}

/// Configuration for the metrics endpoint
pub struct MetricsEndpointConfig {
    pub address: SocketAddr,
    pub registry: prometheus::Registry,
    pub health_controller: Option<HealthController>,
    /// When set, the `/metrics` exposition additionally emits every metric family
    /// whose name starts with `.old` under a copy renamed to use `.new` in place of
    /// that prefix, so the same series appear under both names. This supports a
    /// gradual metric-rename migration where series are published under both an old
    /// and a new prefix for a time. Defaults to `None`, in which case the exposition
    /// is unchanged.
    pub additional_prefix: Option<PrefixMigration>,
}

pub fn new_metrics_setup(
    service_name: &'static str,
    service_namespace: &'static str,
    set_global_meter: bool,
) -> eyre::Result<MetricsSetup> {
    // This defines attributes that are set on the exported metrics
    let service_telemetry_attributes = opentelemetry_sdk::Resource::builder()
        .with_attributes(vec![
            KeyValue::new(semconv::resource::SERVICE_NAME, service_name),
            KeyValue::new(semconv::resource::SERVICE_NAMESPACE, service_namespace),
        ])
        .build();

    // This sets the global meter provider
    // Note: This configures metrics bucket between 5.0 and 10000.0, which are best suited
    // for tracking milliseconds
    // See https://github.com/open-telemetry/opentelemetry-rust/blob/495330f63576cfaec2d48946928f3dc3332ba058/opentelemetry-sdk/src/metrics/reader.rs#L155-L158
    let prometheus_registry = prometheus::Registry::new();
    let metrics_exporter = opentelemetry_prometheus::exporter()
        .with_registry(prometheus_registry.clone())
        .without_scope_info()
        .without_target_info()
        .build()?;
    let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(metrics_exporter)
        .with_resource(service_telemetry_attributes)
        .with_view(create_metric_view_for_retry_histograms("*_attempts_*")?)
        .with_view(create_metric_view_for_retry_histograms("*_retries_*")?)
        .build();

    if set_global_meter {
        // After this call `global::meter()` will be available
        opentelemetry::global::set_meter_provider(meter_provider.clone());
    }

    Ok(MetricsSetup {
        registry: prometheus_registry,
        meter: meter_provider.meter(service_name),
        meter_provider,
        health_controller: HealthController::new(),
    })
}

/// Configures a View for Histograms that describe retries or attempts for operations
/// The view reconfigures the histogram to use a small set of buckets that track
/// the exact amount of retry attempts up to 3, and 2 additional buckets up to 10.
/// This is more useful than the default histogram range where the lowest sets of
/// buckets are 0, 5, 10, 25
fn create_metric_view_for_retry_histograms(
    name_filter: &'static str,
) -> carbide_metrics_utils::Result<OtelView> {
    carbide_metrics_utils::new_view(
        name_filter,
        Some(opentelemetry_sdk::metrics::InstrumentKind::Histogram),
        opentelemetry_sdk::metrics::Aggregation::ExplicitBucketHistogram {
            boundaries: vec![0.0, 1.0, 2.0, 3.0, 5.0, 10.0],
            record_min_max: true,
        },
    )
}

/// Start a HTTP endpoint which exposes metrics using the provided configuration.
///
/// The endpoint runs until the process exits. Callers that need to stop it (for
/// example on graceful shutdown) should use [`run_metrics_endpoint_with_cancellation`].
pub async fn run_metrics_endpoint(config: &MetricsEndpointConfig) -> Result<(), std::io::Error> {
    run_metrics_endpoint_with_cancellation(config, CancellationToken::new()).await
}

/// Start a HTTP endpoint which exposes metrics and runs until `cancel_token` is
/// cancelled.
///
/// This binds `config.address` using [`bind_tcp_listener`], including its IPv4 fallback.
/// Callers that have already bound a listener can use [`run_metrics_endpoint_with_listener`] directly.
pub async fn run_metrics_endpoint_with_cancellation(
    config: &MetricsEndpointConfig,
    cancel_token: CancellationToken,
) -> Result<(), std::io::Error> {
    let listener = bind_tcp_listener(config.address).await?;

    tracing::info!(
        metrics_address = config.address.to_string(),
        "Starting metrics listener"
    );

    run_metrics_endpoint_with_listener(config, cancel_token, listener).await
}

/// Run the metrics service on an existing listener.
///
/// Returns an error only if the dedicated encoder thread cannot be spawned at startup
/// (rare — OS resource exhaustion); once running it serves until `cancel_token` fires.
pub async fn run_metrics_endpoint_with_listener(
    config: &MetricsEndpointConfig,
    cancel_token: CancellationToken,
    listener: TcpListener,
) -> Result<(), std::io::Error> {
    // Spawn the single dedicated encoder thread. It owns the registry and prefix and
    // serializes all `/metrics` encoding onto one core. We keep a `shutdown_tx` sender
    // clone so that, once the accept loop ends, we can enqueue an `Item::Stop` and join the
    // thread — stopping it regardless of how many connection tasks still hold a sender.
    let (encoder_tx, encoder_thread) =
        spawn_encoder_thread(config.registry.clone(), config.additional_prefix.clone())?;
    let shutdown_tx = encoder_tx.clone();

    let handler_state = Arc::new(MetricsHandlerState {
        encoder_tx,
        health_controller: config.health_controller.clone().unwrap_or_default(),
    });

    while let Some(result) = cancel_token.run_until_cancelled(listener.accept()).await {
        let stream = match result {
            Ok((stream, _addr)) => stream,
            Err(e) => {
                tracing::error!(error = %e, "error accepting TCP connection");
                continue;
            }
        };
        let io = TokioIo::new(stream);

        let handler_state = handler_state.clone();

        tokio::task::spawn(async move {
            let handler_state = handler_state.clone();
            if let Err(err) = Builder::new(TokioExecutor::new())
                .serve_connection(
                    io,
                    service_fn(move |req: Request<body::Incoming>| {
                        let handler_state = handler_state.clone();
                        async move { handle_metrics_request(req, handler_state).await }
                    }),
                )
                .await
            {
                tracing::warn!(error = err, "Error serving connection for metrics listener");
            }
        });
    }

    // The accept loop only returns once the token is cancelled. Enqueue a `Stop` and join
    // the encoder thread so shutdown is clean and connection-independent: `Stop` ends the
    // thread no matter how many connection tasks still hold a queue sender. Since the accept
    // loop has stopped, no new work is enqueued, so the queue drains to the `Stop` and the
    // thread exits (bounded by queue depth times encode time — typically ~empty). Do the
    // blocking send + join off an async worker, and cap the wait: `Stop` cannot be observed
    // while the thread is mid-encode, so a pathologically slow (or hung) collector must not
    // block shutdown forever.
    let join_task = tokio::task::spawn_blocking(move || {
        // If the thread already exited, the send errors — ignore it.
        shutdown_tx.send(Item::Stop).ok();
        encoder_thread.join()
    });
    match tokio::time::timeout(ENCODER_SHUTDOWN_JOIN_TIMEOUT, join_task).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(_panic))) => tracing::error!("metrics encoder thread panicked"),
        Ok(Err(err)) => tracing::error!(error = %err, "failed to join metrics encoder thread"),
        Err(_elapsed) => tracing::warn!(
            "metrics encoder did not exit within the shutdown timeout (a scrape is likely \
             mid-encode); returning without it — the detached thread exits on its queued Stop"
        ),
    }

    Ok(())
}

/// Spawn the single dedicated OS thread that owns the registry and serializes all
/// `/metrics` encoding onto one core.
///
/// The thread owns `registry` (a cheap `Arc`-backed clone) and `additional_prefix`
/// (moved in), and services items arriving on the returned bounded channel: for each
/// [`Item::Work`] it encodes the current exposition with [`encode_metrics`] and sends the
/// `Result` back through the oneshot; an [`Item::Stop`] — enqueued by the server once its
/// accept loop ends — breaks the loop. A requester that already gave up (its receiver is
/// closed) is skipped without encoding, so an abandoned scrape never burns the sole
/// encoder.
///
/// It blocks on `recv()` with no polling; the loop also ends if every sender is dropped.
/// The explicit `Stop` makes shutdown connection-independent — the thread exits regardless
/// of how many connection tasks still hold a queue sender. Using a plain OS thread (not a
/// tokio worker) keeps the blocking `recv` off the async runtime.
///
/// Returns an error if the OS refuses to create the thread (e.g. resource exhaustion), so
/// startup can surface it rather than panic.
fn spawn_encoder_thread(
    registry: prometheus::Registry,
    additional_prefix: Option<PrefixMigration>,
) -> std::io::Result<(SyncSender<Item>, std::thread::JoinHandle<()>)> {
    let (encoder_tx, encoder_rx) = sync_channel::<Item>(ENCODER_QUEUE_CAPACITY);

    let handle = std::thread::Builder::new()
        .name("metrics-encoder".to_string())
        .spawn(move || {
            // `recv` blocks until an item arrives; `Err` means every sender was dropped.
            while let Ok(item) = encoder_rx.recv() {
                match item {
                    Item::Stop => break,
                    Item::Work(reply_tx) => {
                        // A scrape that disconnected or timed out while queued has already
                        // dropped its receiver; skip the expensive gather+encode so we do
                        // not burn the sole encoder (and risk 503-ing live scrapes) on
                        // work nobody is waiting for.
                        if reply_tx.is_closed() {
                            continue;
                        }
                        // Isolate a panicking collector/observable callback: a panic during
                        // `gather()` must fail only this one scrape (500), not unwind and
                        // kill the sole encoder thread — which would then 500 every later
                        // scrape until the process restarts. This restores the per-request
                        // isolation the previous `spawn_blocking` gave for free.
                        let result =
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                encode_metrics(&registry, additional_prefix.as_ref())
                            })) {
                                Ok(Ok(buffer)) => Ok(buffer),
                                Ok(Err(err)) => Err(EncodeError::Encode(err)),
                                Err(_panic) => {
                                    tracing::error!(
                                        "metrics gather/encode panicked; encoder thread surviving"
                                    );
                                    Err(EncodeError::Panicked)
                                }
                            };
                        // The requester may still have gone away between the check and
                        // here; dropping the result in that case is fine.
                        reply_tx.send(result).ok();
                    }
                }
            }
        })?;

    Ok((encoder_tx, handle))
}

/// Encode the registry's metric families in the Prometheus text exposition format.
///
/// When `additional_prefix` is `Some(PrefixMigration { old, new })`, every gathered
/// family whose name starts with `old` is additionally emitted under a copy whose name
/// has that prefix replaced by `new`, so the same series appear under both names. When
/// `None`, the output is exactly `TextEncoder` over `registry.gather()`.
///
/// Returns the `prometheus::Error` from the text encoder rather than panicking, so the
/// handler can surface an encode failure as `500` instead of tearing down the thread.
fn encode_metrics(
    registry: &prometheus::Registry,
    additional_prefix: Option<&PrefixMigration>,
) -> Result<Vec<u8>, prometheus::Error> {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let mut metric_families = registry.gather();

    if let Some(PrefixMigration { old, new }) = additional_prefix {
        let alt_name_families: Vec<MetricFamily> = metric_families
            .iter()
            .filter_map(|family| {
                if !family.name().starts_with(old) {
                    return None;
                }

                let mut alt_name_family = family.clone();
                alt_name_family.set_name(family.name().replacen(old, new, 1));
                Some(alt_name_family)
            })
            .collect();

        if !alt_name_families.is_empty() {
            metric_families.extend(alt_name_families);
        }
    }

    encoder.encode(&metric_families, &mut buffer)?;
    Ok(buffer)
}

/// Metrics request handler
async fn handle_metrics_request<B>(
    req: Request<B>,
    state: Arc<MetricsHandlerState>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let response = match (req.method(), req.uri().path()) {
        (&Method::GET, "/metrics") => {
            // `gather()` + text encoding can briefly stall a tokio worker on a large
            // registry, so it runs on the single dedicated encoder thread. Hand it a
            // reply channel through the bounded queue and await the encoded exposition.
            let (reply_tx, reply_rx) = oneshot::channel();
            match state.encoder_tx.try_send(Item::Work(reply_tx)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // The single encoder is already busy with a backlog; shed this
                    // scrape rather than pile on more work.
                    emit(MetricsScrapeFailed::EncoderBusy {});
                    return Ok(Response::builder()
                        .status(503)
                        .body(Full::new(Bytes::from("metrics encoder busy")))
                        .unwrap());
                }
                Err(TrySendError::Disconnected(_)) => {
                    emit(MetricsScrapeFailed::EncoderGone {});
                    return Ok(Response::builder()
                        .status(500)
                        .body(Full::new(Bytes::from("Failed to encode metrics")))
                        .unwrap());
                }
            }

            match reply_rx.await {
                Ok(Ok(buffer)) => Response::builder()
                    .status(200)
                    .header(CONTENT_TYPE, TextEncoder::new().format_type())
                    .header(CONTENT_LENGTH, buffer.len())
                    .body(Full::new(Bytes::from(buffer)))
                    .unwrap(),
                Ok(Err(EncodeError::Encode(_err))) => {
                    emit(MetricsScrapeFailed::EncodeFailed {});
                    Response::builder()
                        .status(500)
                        .body(Full::new(Bytes::from("Failed to encode metrics")))
                        .unwrap()
                }
                Ok(Err(EncodeError::Panicked)) => {
                    emit(MetricsScrapeFailed::EncoderPanicked {});
                    Response::builder()
                        .status(500)
                        .body(Full::new(Bytes::from("Failed to encode metrics")))
                        .unwrap()
                }
                Err(_) => {
                    // The encoder thread dropped the reply without answering — it was told
                    // to stop (shutdown) or otherwise went away. Distinct from a busy or
                    // already-gone encoder above.
                    emit(MetricsScrapeFailed::ReplyDropped {});
                    Response::builder()
                        .status(500)
                        .body(Full::new(Bytes::from("Failed to encode metrics")))
                        .unwrap()
                }
            }
        }
        (&Method::GET, "/health") if state.health_controller.is_healthy() => Response::builder()
            .status(200)
            .body(Full::new(Bytes::from("Healthy")))
            .unwrap(),
        (&Method::GET, "/health") => Response::builder()
            .status(503)
            .body(Full::new(Bytes::from("Unhealthy")))
            .unwrap(),
        (&Method::GET, "/ready") if state.health_controller.is_ready() => Response::builder()
            .status(200)
            .body(Full::new(Bytes::from("Ready")))
            .unwrap(),
        (&Method::GET, "/ready") => Response::builder()
            .status(503)
            .body(Full::new(Bytes::from("Unavailable")))
            .unwrap(),
        (&Method::GET, "/") => Response::builder()
            .status(200)
            .body(Full::new(Bytes::from(
                "Metrics are exposed via /metrics. There is nothing else to see here",
            )))
            .unwrap(),
        _ => Response::builder()
            .status(404)
            .body(Full::new(Bytes::from("Invalid URL")))
            .unwrap(),
    };

    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_instrument::testing::MetricsCapture;
    use opentelemetry::KeyValue;
    use opentelemetry_sdk::metrics;
    use prometheus::{Encoder, TextEncoder};

    use super::*;

    /// The `EncodeFailed` outcome (a `TextEncoder` error) cannot be triggered
    /// deterministically through the handler — the text encoder does not error on a valid
    /// registry, and there is no seam to inject a failure — so its emit wiring is covered
    /// here directly. The other four outcomes are driven through `handle_metrics_request`
    /// in the tests below.
    #[test]
    fn encode_failed_outcome_emits_counter() {
        let metrics = MetricsCapture::start();

        emit(MetricsScrapeFailed::EncodeFailed {});

        assert_eq!(
            metrics.counter_delta(
                "carbide_metrics_scrape_failures_total",
                &[("outcome", "encode_failed")]
            ),
            1.0,
        );
    }

    /// This test mostly mimics the test setup above and checks whether
    /// the prometheus opentelemetry stack will only report the most recent
    /// values for gauges and not cached values that are not important anymore
    #[test]
    fn test_gauge_aggregation() {
        let prometheus_registry = prometheus::Registry::new();
        let metrics_exporter = opentelemetry_prometheus::exporter()
            .with_registry(prometheus_registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .unwrap();

        let meter_provider = metrics::MeterProviderBuilder::default()
            .with_reader(metrics_exporter)
            .with_view(create_metric_view_for_retry_histograms("*_attempts_*").unwrap())
            .with_view(create_metric_view_for_retry_histograms("*_retries_*").unwrap())
            .build();

        let meter = meter_provider.meter("myservice");

        let state = KeyValue::new("state", "mystate");
        let p1 = vec![state.clone(), KeyValue::new("error", "ErrA")];
        let p2 = vec![state.clone(), KeyValue::new("error", "ErrB")];
        let p3 = vec![state, KeyValue::new("error", "ErrC")];

        let counter = Arc::new(AtomicUsize::new(0));

        meter
            .u64_observable_gauge("mygauge")
            .with_callback(move |observer| {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                println!("Collection {count}");
                if count.is_multiple_of(2) {
                    observer.observe(1, &p1);
                } else {
                    observer.observe(1, &p2);
                }
                if count % 3 == 1 {
                    observer.observe(1, &p3);
                }
            })
            .build();

        for i in 0..10 {
            let mut buffer = vec![];
            let encoder = TextEncoder::new();
            let metric_families = prometheus_registry.gather();
            encoder.encode(&metric_families, &mut buffer).unwrap();
            let encoded = String::from_utf8(buffer).unwrap();

            if i % 2 == 0 {
                assert!(encoded.contains(r#"mygauge{error="ErrA",state="mystate"} 1"#));
                assert!(!encoded.contains(r#"mygauge{error="ErrB",state="mystate"} 1"#));
            } else {
                assert!(encoded.contains(r#"mygauge{error="ErrB",state="mystate"} 1"#));
                assert!(!encoded.contains(r#"mygauge{error="ErrA",state="mystate"} 1"#));
            }
            if i % 3 == 1 {
                assert!(encoded.contains(r#"mygauge{error="ErrC",state="mystate"} 1"#));
            } else {
                assert!(!encoded.contains(r#"mygauge{error="ErrC",state="mystate"} 1"#));
            }
        }
    }

    #[test]
    fn test_health_controller_state_changes() {
        let controller = HealthController::new();

        // Defaults are true
        assert!(controller.is_ready());
        assert!(controller.is_healthy());

        controller.set_ready(false);
        assert!(!controller.is_ready());

        controller.set_healthy(false);
        assert!(!controller.is_healthy());
        assert!(!controller.is_ready());

        controller.set_ready(true);
        controller.set_healthy(true);
        assert!(controller.is_ready());
        assert!(controller.is_healthy());
    }

    /// Builds a deterministic registry with two `carbide_`-prefixed families plus
    /// one unprefixed family, for exercising the `/metrics` exposition.
    fn sample_registry() -> prometheus::Registry {
        let registry = prometheus::Registry::new();

        let requests = prometheus::Counter::with_opts(prometheus::Opts::new(
            "carbide_requests_total",
            "Total number of requests",
        ))
        .unwrap();
        requests.inc_by(3.0);
        registry.register(Box::new(requests)).unwrap();

        let queue_depth = prometheus::Gauge::with_opts(prometheus::Opts::new(
            "carbide_queue_depth",
            "Current queue depth",
        ))
        .unwrap();
        queue_depth.set(7.0);
        registry.register(Box::new(queue_depth)).unwrap();

        let other = prometheus::Counter::with_opts(prometheus::Opts::new(
            "other_events_total",
            "Other events",
        ))
        .unwrap();
        other.inc();
        registry.register(Box::new(other)).unwrap();

        registry
    }

    /// With `additional_prefix` unset, the exposition must be byte-for-byte what
    /// the pre-change handler produced: a plain `TextEncoder` over `gather()`.
    #[test]
    fn test_additional_prefix_none_is_byte_identical() {
        let registry = sample_registry();

        let mut expected = vec![];
        TextEncoder::new()
            .encode(&registry.gather(), &mut expected)
            .unwrap();

        let actual = encode_metrics(&registry, None).expect("encode succeeds");

        assert_eq!(
            actual, expected,
            "None must reproduce the pre-change exposition exactly"
        );
    }

    /// With `additional_prefix` set, each matching family is emitted under both the
    /// original and the alternate prefix; non-matching families are left untouched.
    #[test]
    fn test_additional_prefix_duplicates_matching_families() {
        let registry = sample_registry();
        let prefixes = PrefixMigration {
            old: "carbide_".to_string(),
            new: "nico_".to_string(),
        };

        let out =
            String::from_utf8(encode_metrics(&registry, Some(&prefixes)).expect("encode succeeds"))
                .unwrap();

        // Original families remain, unchanged.
        assert!(out.contains("# HELP carbide_requests_total Total number of requests"));
        assert!(out.contains("# TYPE carbide_requests_total counter"));
        assert!(out.contains("\ncarbide_requests_total 3"));
        assert!(out.contains("# TYPE carbide_queue_depth gauge"));
        assert!(out.contains("\ncarbide_queue_depth 7"));

        // Alternate-prefixed copies carry identical HELP/TYPE/value.
        assert!(out.contains("# HELP nico_requests_total Total number of requests"));
        assert!(out.contains("# TYPE nico_requests_total counter"));
        assert!(out.contains("\nnico_requests_total 3"));
        assert!(out.contains("# TYPE nico_queue_depth gauge"));
        assert!(out.contains("\nnico_queue_depth 7"));

        // A family that does not match the old prefix is not duplicated.
        assert!(!out.contains("nico_other_events_total"));
        assert_eq!(out.matches("other_events_total 1").count(), 1);

        // Two matching families -> two extra HELP lines (3 original + 2 alternate).
        assert_eq!(out.matches("# HELP ").count(), 5);

        // The alternate copies are appended after the originals.
        assert!(
            out.find("carbide_requests_total 3").unwrap()
                < out.find("nico_requests_total 3").unwrap()
        );
    }

    /// The cancel-token entry point binds, serves, and then returns promptly once
    /// the token is cancelled.
    #[tokio::test]
    async fn test_cancellation_shuts_down_server() {
        let config = MetricsEndpointConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            registry: prometheus::Registry::new(),
            health_controller: None,
            additional_prefix: None,
        };

        let cancel_token = CancellationToken::new();
        let server_token = cancel_token.clone();
        let server = tokio::spawn(async move {
            run_metrics_endpoint_with_cancellation(&config, server_token).await
        });

        // Let the server bind and start accepting before we cancel it.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel_token.cancel();

        let joined = tokio::time::timeout(std::time::Duration::from_secs(5), server).await;
        let result = joined.expect("server did not shut down within timeout");
        assert!(
            result.expect("server task panicked").is_ok(),
            "cancelled server should return Ok"
        );
    }

    /// A live `GET /metrics` round-trip returns `200` with a body byte-for-byte equal to
    /// [`encode_metrics`], proving the dedicated encoder thread produces the same
    /// exposition the handler used to build inline.
    #[tokio::test]
    async fn test_metrics_endpoint_serves_encoded_body() {
        let registry = sample_registry();
        let expected = encode_metrics(&registry, None).expect("encode succeeds");
        let expected_content_type = TextEncoder::new().format_type().to_ascii_lowercase();

        // Bind here so we know the address and there is no bind race with the client.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let config = MetricsEndpointConfig {
            address: addr,
            registry,
            health_controller: None,
            additional_prefix: None,
        };

        let cancel_token = CancellationToken::new();
        let server_token = cancel_token.clone();
        let server = tokio::spawn(async move {
            run_metrics_endpoint_with_listener(&config, server_token, listener).await
        });

        let (headers, body) = scrape_metrics(addr).await;

        assert!(
            headers.starts_with("http/1.1 200"),
            "expected a 200 status line, got headers:\n{headers}"
        );
        assert!(
            headers.contains(&format!("content-type: {expected_content_type}")),
            "expected content-type {expected_content_type:?} in headers:\n{headers}"
        );
        assert_eq!(
            body, expected,
            "served body must be byte-identical to encode_metrics output"
        );

        cancel_token.cancel();
        server
            .await
            .unwrap()
            .expect("metrics server exited cleanly");
    }

    /// The dedicated encoder thread answers an encode request with the expected bytes and
    /// then exits on an `Item::Stop` — while a sender is still alive — proving the exit is
    /// message-driven and never gated on connection tasks dropping their senders.
    #[test]
    fn test_encoder_thread_answers_then_exits_on_stop_message() {
        let registry = sample_registry();
        let expected = encode_metrics(&registry, None).expect("encode succeeds");

        let (encoder_tx, handle) =
            spawn_encoder_thread(registry, None).expect("spawn encoder thread");

        // A normal request is answered with the same bytes as encode_metrics.
        let (reply_tx, mut reply_rx) = oneshot::channel();
        encoder_tx
            .try_send(Item::Work(reply_tx))
            .expect("queue has room");
        // Bounded wait for the reply so a regressed encoder fails the test rather than
        // hanging it, using the same deadline pattern as the thread-exit assertion below.
        let reply_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let encoded = loop {
            match reply_rx.try_recv() {
                Ok(result) => break result.expect("encode succeeds"),
                Err(oneshot::error::TryRecvError::Empty) => {
                    assert!(
                        std::time::Instant::now() < reply_deadline,
                        "encoder did not reply within the deadline"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    panic!("encoder dropped the reply channel without responding")
                }
            }
        };
        assert_eq!(encoded, expected, "encoder bytes match encode_metrics");

        // Send Stop but keep the sender alive: the thread must still exit.
        encoder_tx.send(Item::Stop).expect("send stop");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !handle.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "encoder thread did not exit after Stop was sent"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        handle.join().expect("encoder thread joined cleanly");

        // The sender was alive the whole time, so the exit came from the Stop message, not
        // from the queue disconnecting.
        drop(encoder_tx);
    }

    /// Cancelling the server while a connection (and therefore a queue-sender clone in its
    /// handler state) is still alive must still exit and join the encoder thread. The
    /// server future only completes after that join, so finishing within the timeout
    /// proves the thread did not linger behind the open connection.
    #[tokio::test]
    async fn test_cancel_exits_encoder_thread_with_live_connection() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let config = MetricsEndpointConfig {
            address: addr,
            registry: sample_registry(),
            health_controller: None,
            additional_prefix: None,
        };

        let cancel_token = CancellationToken::new();
        let server_token = cancel_token.clone();
        let server = tokio::spawn(async move {
            run_metrics_endpoint_with_listener(&config, server_token, listener).await
        });

        // Complete one keep-alive `/health` round-trip BEFORE cancelling, so the connection
        // is deterministically accepted and its task holds an `encoder_tx` clone. Without
        // `Connection: close` the socket stays open across shutdown.
        let mut held = tokio::net::TcpStream::connect(addr).await.unwrap();
        held.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 128];
            loop {
                // Read until we have the full headers plus the Content-Length body.
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
                    let body_len: usize = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .map_or(0, |value| value.trim().parse().unwrap());
                    if raw.len() >= end + 4 + body_len {
                        break;
                    }
                }
                let n = held.read(&mut chunk).await.unwrap();
                assert!(n > 0, "connection closed before a full /health response");
                raw.extend_from_slice(&chunk[..n]);
            }
            String::from_utf8_lossy(&raw).to_ascii_lowercase()
        })
        .await
        .expect("/health round-trip timed out");
        assert!(
            response.starts_with("http/1.1 200"),
            "kept-alive /health should be 200, got:\n{response}"
        );

        // Cancel with the connection still open. The encoder thread must still exit and be
        // joined — the server future only completes after that join.
        cancel_token.cancel();

        let joined = tokio::time::timeout(std::time::Duration::from_secs(5), server).await;
        joined
            .expect("server did not shut down (encoder-thread join hung) within timeout")
            .expect("server task panicked")
            .expect("metrics server exited cleanly");

        drop(held);
    }

    /// Issue one `GET /metrics` over a fresh connection and return the lowercased header
    /// block and the raw body bytes. `Connection: close` lets us read to EOF without
    /// parsing `Content-Length`.
    async fn scrape_metrics(addr: SocketAddr) -> (String, Vec<u8>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let exchange = async {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).await.unwrap();
            raw
        };
        let raw = tokio::time::timeout(std::time::Duration::from_secs(5), exchange)
            .await
            .expect("metrics round-trip timed out");

        let separator = b"\r\n\r\n";
        let split = raw
            .windows(separator.len())
            .position(|window| window == separator)
            .expect("response has a header/body separator");
        let headers = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
        let body = raw[split + separator.len()..].to_vec();
        (headers, body)
    }

    /// A collector that panics the first time it is gathered and behaves thereafter. It
    /// lets a test drive one panicking `/metrics` scrape and confirm the encoder thread
    /// survives to serve the next.
    struct PanicOnceCollector {
        desc: prometheus::core::Desc,
        panicked: AtomicBool,
    }

    impl PanicOnceCollector {
        fn new() -> Self {
            let desc = prometheus::core::Desc::new(
                "panic_probe".to_string(),
                "collector that panics on its first gather".to_string(),
                vec![],
                std::collections::HashMap::new(),
            )
            .unwrap();
            Self {
                desc,
                panicked: AtomicBool::new(false),
            }
        }
    }

    impl prometheus::core::Collector for PanicOnceCollector {
        fn desc(&self) -> Vec<&prometheus::core::Desc> {
            vec![&self.desc]
        }

        fn collect(&self) -> Vec<prometheus::proto::MetricFamily> {
            if !self.panicked.swap(true, Ordering::SeqCst) {
                panic!("collector panic during gather");
            }
            vec![]
        }
    }

    /// A collector that panics during `gather()` must fail only that one scrape (500); the
    /// sole encoder thread has to survive so a subsequent scrape on the same server still
    /// succeeds. This is the per-request panic isolation the previous `spawn_blocking`
    /// provided for free.
    #[tokio::test]
    async fn test_encoder_survives_collector_panic() {
        let registry = prometheus::Registry::new();
        registry
            .register(Box::new(PanicOnceCollector::new()))
            .unwrap();
        let survives = prometheus::Counter::with_opts(prometheus::Opts::new(
            "survives_total",
            "present once the encoder survives a panic",
        ))
        .unwrap();
        survives.inc();
        registry.register(Box::new(survives)).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let config = MetricsEndpointConfig {
            address: addr,
            registry,
            health_controller: None,
            additional_prefix: None,
        };

        let cancel_token = CancellationToken::new();
        let server_token = cancel_token.clone();
        let server = tokio::spawn(async move {
            run_metrics_endpoint_with_listener(&config, server_token, listener).await
        });

        let metrics = MetricsCapture::start();

        // First scrape trips the collector panic. The encoder thread catches the unwind
        // and answers 500 instead of dying.
        let (headers, _body) = scrape_metrics(addr).await;
        assert!(
            headers.starts_with("http/1.1 500"),
            "panicking scrape should be 500, got headers:\n{headers}"
        );
        // The panic branch records the `encoder_panicked` outcome before replying.
        assert_eq!(
            metrics.counter_delta(
                "carbide_metrics_scrape_failures_total",
                &[("outcome", "encoder_panicked")]
            ),
            1.0,
            "panicking scrape should increment the encoder_panicked counter once"
        );

        // The thread survived, so a subsequent scrape on the same server succeeds.
        let (headers, body) = scrape_metrics(addr).await;
        assert!(
            headers.starts_with("http/1.1 200"),
            "post-panic scrape should be 200, got headers:\n{headers}"
        );
        assert!(
            String::from_utf8_lossy(&body).contains("survives_total"),
            "post-panic body should contain the registry's metrics"
        );

        cancel_token.cancel();
        server
            .await
            .unwrap()
            .expect("metrics server exited cleanly");
    }

    /// Drive `handle_metrics_request` through its channel-controllable failure branches with
    /// a queue we manipulate — no live server or encoder thread — and assert each maps to
    /// the expected status *and* increments `carbide_metrics_scrape_failures_total` once
    /// under the matching `outcome` label. This guards the handler's emit wiring, not just
    /// that `emit` works. (`EncodeFailed` is covered separately; see
    /// `encode_failed_outcome_emits_counter`.)
    #[tokio::test]
    async fn test_handle_metrics_request_failure_outcomes() {
        // The encoder sender under test, plus any receiver that must stay alive for the
        // call (`None` means it was dropped or handed to a drainer thread).
        type EncoderChannel = (SyncSender<Item>, Option<std::sync::mpsc::Receiver<Item>>);

        struct Case {
            name: &'static str,
            setup: fn() -> EncoderChannel,
            expected_status: u16,
            outcome: &'static str,
        }

        let cases = [
            Case {
                name: "queue full -> 503",
                setup: || {
                    let (tx, rx) = sync_channel::<Item>(1);
                    // Fill the single slot so the handler's try_send sees `Full`.
                    tx.try_send(Item::Stop).expect("prime the queue");
                    (tx, Some(rx))
                },
                expected_status: 503,
                outcome: "encoder_busy",
            },
            Case {
                name: "encoder gone -> 500",
                setup: || {
                    let (tx, rx) = sync_channel::<Item>(1);
                    drop(rx);
                    (tx, None)
                },
                expected_status: 500,
                outcome: "encoder_gone",
            },
            Case {
                name: "reply dropped -> 500",
                setup: || {
                    let (tx, rx) = sync_channel::<Item>(1);
                    // Accept the enqueued Work and drop its reply channel without answering,
                    // so the handler's `reply_rx.await` resolves to `Err` (reply dropped).
                    std::thread::spawn(move || {
                        if let Ok(Item::Work(reply_tx)) = rx.recv() {
                            drop(reply_tx);
                        }
                    });
                    (tx, None)
                },
                expected_status: 500,
                outcome: "reply_dropped",
            },
        ];

        let metrics = MetricsCapture::start();

        for case in cases {
            let (encoder_tx, _rx_guard) = (case.setup)();
            let state = Arc::new(MetricsHandlerState {
                encoder_tx,
                health_controller: HealthController::new(),
            });
            let request = Request::builder()
                .method(Method::GET)
                .uri("/metrics")
                .body(())
                .unwrap();

            let response = handle_metrics_request(request, state)
                .await
                .expect("handler returns a response");

            assert_eq!(
                response.status().as_u16(),
                case.expected_status,
                "case: {}",
                case.name
            );
            assert_eq!(
                metrics.counter_delta(
                    "carbide_metrics_scrape_failures_total",
                    &[("outcome", case.outcome)]
                ),
                1.0,
                "case: {} should increment its outcome counter exactly once",
                case.name
            );
            // `_rx_guard` (kept for the full-queue case) stays alive until here so the
            // channel reads as full rather than disconnected.
        }
    }
}
