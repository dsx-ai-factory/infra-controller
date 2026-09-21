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

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use nv_redfish::ServiceRoot;
use nv_redfish::core::Bmc;
use nv_redfish::event_service::EventStreamPayload;
use prometheus::{Counter, Gauge, Histogram, HistogramOpts, IntCounter, IntGauge, Opts};
use rand::RngExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::HealthError;
use crate::bmc::BmcClient;
use crate::endpoint::BmcEndpoint;
use crate::limiter::RateLimiter;
use crate::metrics::{
    CollectorRegistry, ComponentKind, MetricsManager, operation_duration_buckets_seconds,
};
use crate::sink::{CollectorEvent, DataSink, EventContext};

/// Result of a collector iteration
#[derive(Debug, Clone)]
pub struct IterationResult {
    /// Whether a refresh was triggered (data was fetched vs cached)
    pub refresh_triggered: bool,
    /// Number of entities collected, if applicable
    pub entity_count: Option<usize>,
    /// Number of partial fetch failures tolerated during the iteration
    pub fetch_failures: usize,
}

pub trait PeriodicCollector<B: Bmc>: Send + 'static {
    type Config: Send + 'static;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError>
    where
        Self: Sized;

    fn run_iteration(
        &mut self,
    ) -> impl std::future::Future<Output = Result<IterationResult, HealthError>> + Send;

    /// Returns the type identifier for this collector
    fn collector_type(&self) -> &'static str;

    fn stop(&mut self) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }
}

pub type EventStream<'a> = BoxStream<'a, Result<CollectorEvent, HealthError>>;

/// Result of opening a streaming collector connection.
pub enum StreamingConnectResult<'a> {
    /// The stream is accepted and should be treated as connected.
    Connected(EventStream<'a>),

    /// The connection failed before it should be marked connected, but the
    /// collector produced events that still need to reach sinks.
    Failed {
        /// Events to emit before surfacing the connection failure.
        events: Vec<CollectorEvent>,

        /// Error that should drive reconnect/backoff behavior.
        error: HealthError,
    },
}

/// Trait for collectors that maintain a long-lived stream (SSE, gRPC, etc.)
/// runtime.rs creates the BMC client and injects it, the collector opens the stream and maps payloads to events
#[async_trait]
pub trait StreamingCollector<B: Bmc>: Send + 'static {
    type Config: Send + 'static;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError>
    where
        Self: Sized;

    /// Open or reopen the streaming connection using the injected BMC.
    async fn connect(&mut self) -> Result<StreamingConnectResult<'_>, HealthError>;

    fn collector_type(&self) -> &'static str;
}

pub struct BackoffConfig {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

pub struct ExponentialBackoff {
    initial: Duration,
    max: Duration,
    current: Duration,
}

impl ExponentialBackoff {
    pub fn new(config: &BackoffConfig) -> Self {
        Self {
            initial: config.initial,
            max: config.max,
            current: config.initial,
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        let base = self.current;
        self.current = (self.current * 2).min(self.max);
        let jitter_ms = rand::rng().random_range(0..base.as_millis().max(1) as u64);
        base + Duration::from_millis(jitter_ms)
    }

    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

type SseStream = Pin<
    Box<
        dyn futures::TryStream<
                Ok = EventStreamPayload,
                Error = HealthError,
                Item = Result<EventStreamPayload, HealthError>,
            > + Send,
    >,
>;

/// Open a Redfish SSE event stream from a BMC.
pub(crate) async fn open_sse_stream<B: Bmc + 'static>(
    bmc: Arc<B>,
) -> Result<SseStream, HealthError> {
    let root = ServiceRoot::new(bmc)
        .await
        .map_err(|e| HealthError::BmcError(Box::new(e)))?;

    let event_service = root
        .event_service()
        .await
        .map_err(|e| HealthError::BmcError(Box::new(e)))?
        .ok_or_else(|| {
            HealthError::SseNotAvailable("BMC does not expose an EventService".to_string())
        })?;

    let stream = event_service
        .events()
        .await
        .map_err(|e| HealthError::BmcError(Box::new(e)))?;

    Ok(Box::pin(
        stream.map_err(|e| HealthError::BmcError(Box::new(e))),
    ))
}

pub struct StreamMetrics {
    connected: IntGauge,
    reconnections_total: Counter,
    items_processed_total: Counter,
    stream_errors_total: Counter,
}

impl StreamMetrics {
    fn new(
        registry: &prometheus::Registry,
        prefix: &str,
        const_labels: HashMap<String, String>,
    ) -> Result<Self, HealthError> {
        let connected = IntGauge::with_opts(
            Opts::new(
                format!("{prefix}_stream_connected"),
                "1 while the stream is connected, 0 otherwise",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(connected.clone()))?;

        let reconnections_total = Counter::with_opts(
            Opts::new(
                format!("{prefix}_stream_reconnections_total"),
                "Total reconnection attempts",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(reconnections_total.clone()))?;

        let items_processed_total = Counter::with_opts(
            Opts::new(
                format!("{prefix}_stream_items_processed_total"),
                "Total stream items processed",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(items_processed_total.clone()))?;

        let stream_errors_total = Counter::with_opts(
            Opts::new(
                format!("{prefix}_stream_errors_total"),
                "Total stream errors",
            )
            .const_labels(const_labels),
        )?;
        registry.register(Box::new(stream_errors_total.clone()))?;

        Ok(Self {
            connected,
            reconnections_total,
            items_processed_total,
            stream_errors_total,
        })
    }
}

/// Builds collector metric labels and includes `rack_id` only when available.
pub(crate) fn collector_metric_labels(
    collector_type: &str,
    endpoint_key: String,
    endpoint: &BmcEndpoint,
) -> HashMap<String, String> {
    let mut labels = HashMap::from([
        ("collector_type".to_string(), collector_type.to_string()),
        ("endpoint_key".to_string(), endpoint_key),
    ]);

    if let Some(rack_id) = endpoint.rack_id.as_ref() {
        labels.insert("rack_id".to_string(), rack_id.to_string());
    }

    labels
}

/// RAII guard: increments the passed IntGauge on construction, decrements on drop.
/// Ensures every exit path from a connected stream (cancel, error, end, reconnect) dec's.
pub(crate) struct StreamingConnectionGuard(IntGauge);

impl StreamingConnectionGuard {
    pub(crate) fn inc(gauge: IntGauge) -> Self {
        gauge.inc();
        Self(gauge)
    }
}

impl Drop for StreamingConnectionGuard {
    fn drop(&mut self) {
        self.0.dec();
    }
}

pub struct Collector {
    handle: JoinHandle<()>,
    cancel_token: CancellationToken,
}

pub struct CollectorStartContext {
    pub limiter: Arc<dyn RateLimiter>,
    pub iteration_interval: Duration,
    pub collector_registry: Arc<CollectorRegistry>,
    pub metrics_manager: Arc<MetricsManager>,
}

pub struct StreamingCollectorStartContext {
    pub backoff_config: BackoffConfig,
    pub collector_registry: Arc<CollectorRegistry>,
}

impl Collector {
    pub fn start<C: PeriodicCollector<BmcClient>>(
        endpoint: Arc<BmcEndpoint>,
        bmc: Arc<BmcClient>,
        config: C::Config,
        start_context: CollectorStartContext,
    ) -> Result<Self, HealthError> {
        let CollectorStartContext {
            limiter,
            iteration_interval,
            collector_registry,
            metrics_manager,
        } = start_context;

        let cancel_token = CancellationToken::new();
        let cancel_token_clone = cancel_token.clone();

        let mut runner = C::new_runner(bmc, endpoint.clone(), config)?;

        let const_labels =
            collector_metric_labels(runner.collector_type(), endpoint.key(), &endpoint);

        let registry = collector_registry.registry();

        let iteration_histogram = Histogram::with_opts(
            HistogramOpts::new(
                format!(
                    "{}_collector_iteration_seconds",
                    collector_registry.prefix()
                ),
                "Duration of collector iterations",
            )
            .const_labels(const_labels.clone())
            .buckets(operation_duration_buckets_seconds()),
        )?;
        registry.register(Box::new(iteration_histogram.clone()))?;

        let refresh_counter = Counter::with_opts(
            Opts::new(
                format!("{}_collector_refresh_total", collector_registry.prefix()),
                "Number of collector refreshes",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(refresh_counter.clone()))?;

        let entities_gauge = Gauge::with_opts(
            Opts::new(
                format!("{}_monitored_entities", collector_registry.prefix()),
                "Number of entities being monitored",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(entities_gauge.clone()))?;

        let fetch_failures_counter = IntCounter::with_opts(
            Opts::new(
                format!(
                    "{}_collector_fetch_failures_total",
                    collector_registry.prefix()
                ),
                "Number of partial collector fetch failures",
            )
            .const_labels(const_labels),
        )?;
        registry.register(Box::new(fetch_failures_counter.clone()))?;

        let component_metrics = metrics_manager.component_metrics();

        let handle = tokio::spawn(async move {
            let collector_type = runner.collector_type();
            let _collector_registry = collector_registry;
            loop {
                tokio::select! {
                    _ = cancel_token_clone.cancelled() => {
                        tracing::info!(
                            endpoint = ?endpoint.addr,
                            rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                            "collector cancelled"
                        );

                        runner.stop().await;
                        break;
                    }
                    _ = async {
                        limiter.acquire().await;

                        let start = Instant::now();
                        let iteration_result = runner.run_iteration().await;
                        let duration = start.elapsed();

                        iteration_histogram.observe(duration.as_secs_f64());
                        component_metrics.record_operation(
                            ComponentKind::Collector,
                            collector_type,
                            duration,
                            iteration_result.is_ok(),
                        );

                        match iteration_result {
                            Ok(result) => {
                                if result.refresh_triggered {
                                    refresh_counter.inc();
                                }

                                if let Some(entity_count) = result.entity_count {
                                    entities_gauge.set(entity_count as f64);
                                }

                                if result.fetch_failures > 0 {
                                    let fetch_failures = result.fetch_failures as u64;
                                    fetch_failures_counter.inc_by(fetch_failures);
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    error = ?e,
                                    endpoint = ?endpoint.addr,
                                    collector_type = collector_type,
                                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                                    "Error during collector iteration"
                                );
                            }
                        }

                        tokio::time::sleep(iteration_interval).await;
                    } => {
                    }
                }
            }
        });

        Ok(Self {
            handle,
            cancel_token,
        })
    }

    /// Starts a streaming collector and reports failures to the caller.
    ///
    /// The callback receives `Err` when connection establishment fails and
    /// `Ok(connected_for)` when an accepted stream ends or returns an error.
    /// Cancellation does not invoke the callback. Returning `false` stops the
    /// collector.
    pub fn start_streaming<S, F>(
        endpoint: Arc<BmcEndpoint>,
        bmc: Arc<BmcClient>,
        config: S::Config,
        data_sink: Arc<dyn DataSink>,
        start_context: StreamingCollectorStartContext,
        mut on_stream_failure: F,
    ) -> Result<Self, HealthError>
    where
        S: StreamingCollector<BmcClient>,
        F: FnMut(Result<Duration, &HealthError>) -> bool + Send + 'static,
    {
        let StreamingCollectorStartContext {
            backoff_config,
            collector_registry,
        } = start_context;

        let cancel_token = CancellationToken::new();
        let cancel_clone = cancel_token.clone();

        let mut collector = S::new_runner(Arc::clone(&bmc), endpoint.clone(), config)?;
        let event_context = EventContext::from_endpoint(&endpoint, collector.collector_type());

        let const_labels =
            collector_metric_labels(collector.collector_type(), endpoint.key(), &endpoint);

        let registry = collector_registry.registry();
        let metrics = StreamMetrics::new(registry, collector_registry.prefix(), const_labels)?;

        let handle = tokio::spawn(async move {
            let collector_type = collector.collector_type();
            let _collector_registry = collector_registry;
            let mut backoff = ExponentialBackoff::new(&backoff_config);

            loop {
                tracing::info!(
                    collector_type,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "streaming collector connecting"
                );

                let Some(stream_result) =
                    cancel_clone.run_until_cancelled(collector.connect()).await
                else {
                    return;
                };

                match stream_result {
                    Err(e) => {
                        metrics.reconnections_total.inc();
                        tracing::error!(
                            error = ?e,
                            collector_type,
                            endpoint = ?endpoint.addr,
                            rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                            "streaming collector connection failed"
                        );

                        if !on_stream_failure(Err(&e)) {
                            return;
                        }
                    }
                    Ok(StreamingConnectResult::Failed { events, error }) => {
                        metrics.reconnections_total.inc();

                        for event in events {
                            metrics.items_processed_total.inc();
                            data_sink.handle_event(&event_context, &event);
                        }

                        tracing::error!(
                            error = ?error,
                            collector_type,
                            endpoint = ?endpoint.addr,
                            rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                            "streaming collector connection failed"
                        );

                        if !on_stream_failure(Err(&error)) {
                            return;
                        }
                    }
                    Ok(StreamingConnectResult::Connected(mut stream)) => {
                        // the guard lives exactly as long as we hold an open stream; Drop
                        // handles dec for every exit path (shutdown, error, stream end).
                        let _conn_guard = StreamingConnectionGuard::inc(metrics.connected.clone());
                        let connected_at = Instant::now();

                        backoff.reset();
                        tracing::info!(
                            collector_type,
                            endpoint = ?endpoint.addr,
                            rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                            "streaming collector connected"
                        );

                        loop {
                            let Some(item) = cancel_clone.run_until_cancelled(stream.next()).await
                            else {
                                tracing::info!(
                                    collector_type,
                                    endpoint = ?endpoint.addr,
                                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                                    "streaming collector shutting down"
                                );
                                return;
                            };

                            match item {
                                Some(Ok(event)) => {
                                    metrics.items_processed_total.inc();
                                    data_sink.handle_event(&event_context, &event);
                                }
                                Some(Err(e)) => {
                                    metrics.stream_errors_total.inc();
                                    metrics.reconnections_total.inc();
                                    tracing::error!(
                                        error = ?e,
                                        collector_type,
                                        endpoint = ?endpoint.addr,
                                        rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                                        "streaming collector stream error, reconnecting"
                                    );

                                    if !on_stream_failure(Ok(connected_at.elapsed())) {
                                        return;
                                    }

                                    break;
                                }
                                None => {
                                    tracing::info!(
                                        collector_type,
                                        endpoint = ?endpoint.addr,
                                        rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                                        "streaming collector stream ended, reconnecting"
                                    );

                                    if !on_stream_failure(Ok(connected_at.elapsed())) {
                                        return;
                                    }

                                    break;
                                }
                            }
                        }
                    }
                }

                let delay = backoff.next_delay();
                if cancel_clone
                    .run_until_cancelled(tokio::time::sleep(delay))
                    .await
                    .is_none()
                {
                    return;
                }
            }
        });

        Ok(Self {
            handle,
            cancel_token,
        })
    }

    /// spawn helper for streaming collectors that don't fit `StreamingCollector`
    /// (e.g. gNMI bidi subscribe with in-loop multiplexing). The closure gets a
    /// CancellationToken and should return once it's cancelled.
    pub fn spawn_task<F, Fut>(task_fn: F) -> Self
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let cancel_token = CancellationToken::new();
        let cancel_clone = cancel_token.clone();
        let handle = tokio::spawn(task_fn(cancel_clone));
        Self {
            handle,
            cancel_token,
        }
    }

    pub(crate) async fn finished(&mut self) {
        let _ = (&mut self.handle).await;
    }

    pub async fn stop(self) {
        self.cancel_token.cancel();
        let _ = self.handle.await;
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use carbide_uuid::rack::RackId;

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::metrics::MetricsManager;
    use crate::sink::{LogRecord, LogSeverity};

    #[derive(Default)]
    struct CountingSink(AtomicUsize);

    impl CountingSink {
        fn log_count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl DataSink for CountingSink {
        fn sink_type(&self) -> &'static str {
            "counting_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            if matches!(event, CollectorEvent::Log(_)) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    #[test]
    fn collector_metric_labels_include_only_available_rack_id() {
        struct TestCase {
            name: &'static str,
            rack_id: Option<RackId>,
            expected_rack_id: Option<&'static str>,
        }

        let cases = [
            TestCase {
                name: "rack identity is available",
                rack_id: Some(RackId::new("RACK_1")),
                expected_rack_id: Some("RACK_1"),
            },
            TestCase {
                name: "rack identity is unavailable",
                rack_id: None,
                expected_rack_id: None,
            },
        ];

        for case in cases {
            let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
            endpoint.rack_id = case.rack_id;

            let labels = collector_metric_labels("test_collector", endpoint.key(), &endpoint);

            assert_eq!(
                labels.get("rack_id").map(String::as_str),
                case.expected_rack_id,
                "{}",
                case.name
            );
        }
    }

    struct TestStreamingCollector;

    #[async_trait]
    impl StreamingCollector<BmcClient> for TestStreamingCollector {
        type Config = ();

        fn new_runner(
            _bmc: Arc<BmcClient>,
            _endpoint: Arc<BmcEndpoint>,
            _config: Self::Config,
        ) -> Result<Self, HealthError> {
            Ok(Self)
        }

        async fn connect(&mut self) -> Result<StreamingConnectResult<'_>, HealthError> {
            let event = CollectorEvent::Log(Box::new(LogRecord {
                body: "pre-connected rejection".to_string(),
                severity: LogSeverity::Error,
                attributes: Vec::new(),
                diagnostic_record: None,
            }));

            Ok(StreamingConnectResult::Failed {
                events: vec![event],
                error: HealthError::GenericError("pre-connected failure".to_string()),
            })
        }

        fn collector_type(&self) -> &'static str {
            "test_streaming_collector"
        }
    }

    #[derive(Clone, Copy)]
    enum StreamBehavior {
        End,
        Error,
        Pending,
    }

    struct SessionStreamingCollector {
        behaviors: VecDeque<StreamBehavior>,
    }

    #[async_trait]
    impl StreamingCollector<BmcClient> for SessionStreamingCollector {
        type Config = Vec<StreamBehavior>;

        fn new_runner(
            _bmc: Arc<BmcClient>,
            _endpoint: Arc<BmcEndpoint>,
            config: Self::Config,
        ) -> Result<Self, HealthError> {
            Ok(Self {
                behaviors: config.into(),
            })
        }

        async fn connect(&mut self) -> Result<StreamingConnectResult<'_>, HealthError> {
            let behavior = self
                .behaviors
                .pop_front()
                .unwrap_or(StreamBehavior::Pending);

            let stream: EventStream<'_> = match behavior {
                StreamBehavior::End => Box::pin(futures::stream::empty()),
                StreamBehavior::Error => Box::pin(futures::stream::once(async {
                    Err(HealthError::GenericError("stream failed".to_string()))
                })),
                StreamBehavior::Pending => Box::pin(futures::stream::pending()),
            };

            Ok(StreamingConnectResult::Connected(stream))
        }

        fn collector_type(&self) -> &'static str {
            "session_streaming_collector"
        }
    }

    #[tokio::test]
    async fn streaming_collector_emits_pre_connected_failure_events_without_connected_callback()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = Arc::new(test_endpoint(mac("00:11:22:33:44:66")));
        let bmc = Arc::clone(endpoint.bmc());
        let metrics_manager = MetricsManager::new("test_streaming_runtime_preconnect_failure")?;

        let collector_registry = Arc::new(metrics_manager.create_collector_registry(
            "streaming_collector_preconnect_failure_test".to_string(),
            "test_streaming_runtime_preconnect_failure",
        )?);

        let sink = Arc::new(CountingSink::default());
        let data_sink: Arc<dyn DataSink> = sink.clone();
        let (callback_tx, callback_rx) = tokio::sync::oneshot::channel();
        let mut callback_tx = Some(callback_tx);

        let collector = Collector::start_streaming::<TestStreamingCollector, _>(
            endpoint,
            bmc,
            (),
            data_sink,
            StreamingCollectorStartContext {
                backoff_config: BackoffConfig::default(),
                collector_registry,
            },
            move |result| {
                if let Some(tx) = callback_tx.take() {
                    let _ = tx.send(result.is_ok());
                }

                false
            },
        )?;

        let connected_callback =
            tokio::time::timeout(Duration::from_secs(1), callback_rx).await??;

        collector.stop().await;

        assert!(!connected_callback);
        assert_eq!(sink.log_count(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn streaming_collector_reports_stream_end_and_error_but_not_cancellation()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = Arc::new(test_endpoint(mac("00:11:22:33:44:77")));
        let bmc = Arc::clone(endpoint.bmc());
        let metrics_manager = MetricsManager::new("test_streaming_runtime_stream_lifetime")?;

        let collector_registry = Arc::new(metrics_manager.create_collector_registry(
            "streaming_collector_stream_lifetime_test".to_string(),
            "test_streaming_runtime_stream_lifetime",
        )?);

        let data_sink: Arc<dyn DataSink> = Arc::new(CountingSink::default());
        let (callback_tx, mut callback_rx) = tokio::sync::mpsc::unbounded_channel();

        let collector = Collector::start_streaming::<SessionStreamingCollector, _>(
            endpoint,
            bmc,
            vec![
                StreamBehavior::End,
                StreamBehavior::Error,
                StreamBehavior::Pending,
            ],
            data_sink,
            StreamingCollectorStartContext {
                backoff_config: BackoffConfig {
                    initial: Duration::ZERO,
                    max: Duration::ZERO,
                },
                collector_registry,
            },
            move |result| callback_tx.send(result.is_ok()).is_ok(),
        )?;

        let callbacks = tokio::time::timeout(Duration::from_secs(1), async {
            [callback_rx.recv().await, callback_rx.recv().await]
        })
        .await?;

        collector.stop().await;

        assert_eq!(callbacks, [Some(true), Some(true)]);
        assert!(callback_rx.try_recv().is_err());

        Ok(())
    }
}
