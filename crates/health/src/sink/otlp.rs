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

use std::num::NonZeroUsize;
use std::sync::Arc;

use carbide_instrument::emit;
use opentelemetry::KeyValue;
use prometheus::{Counter, CounterVec, Opts};

use super::dedup_queue::{DedupQueue, SaveOutcome};
use super::event_mapper::RedfishEventMapper;
use super::{CollectorEvent, DataSink, EventContext, LogRecord, MetricSample};
use crate::HealthError;
use crate::config::OtlpTargetConfig;
use crate::metrics::MetricsManager;
use crate::otlp::drain::OtlpDrainTask;
use crate::otlp::metrics_drain::OtlpMetricsDrainTask;
use crate::otlp::{ConfiguredOtlpTarget, OtlpQueueEntryDropped, OtlpSignal};

pub(crate) type OtlpQueue = DedupQueue<String, (EventContext, CollectorEvent)>;
pub(crate) type OtlpMetricsQueue = DedupQueue<OtlpMetricQueueKey, (EventContext, MetricSample)>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct OtlpMetricQueueKey {
    endpoint_key: String,
    collector_type: &'static str,
    sample_name: String,
    sample_key: String,
    metric_type: String,
    unit: String,
}

#[cfg(not(feature = "bench-hooks"))]
pub(crate) struct OtlpSink {
    queue: Arc<OtlpQueue>,
    metrics_queue: Arc<OtlpMetricsQueue>,
    replaced_total: Counter,
    metrics_replaced_total: Counter,
    target: ConfiguredOtlpTarget,
    mapper: Arc<dyn RedfishEventMapper>,
    include_diagnostics: bool,
}

#[cfg(feature = "bench-hooks")]
pub struct OtlpSink {
    queue: Arc<OtlpQueue>,
    metrics_queue: Arc<OtlpMetricsQueue>,
    replaced_total: Counter,
    metrics_replaced_total: Counter,
    target: ConfiguredOtlpTarget,
    mapper: Arc<dyn RedfishEventMapper>,
    include_diagnostics: bool,
}

/// Returns whether an event belongs in the logs drain.
pub(crate) fn is_otlp_log_relevant(event: &CollectorEvent) -> bool {
    !matches!(
        event,
        CollectorEvent::Metric(_)
            | CollectorEvent::MetricCollectionStart
            | CollectorEvent::MetricCollectionEnd
            | CollectorEvent::CollectorRemoved
    )
}

fn metric_queue_key(context: &EventContext, sample: &MetricSample) -> OtlpMetricQueueKey {
    OtlpMetricQueueKey {
        endpoint_key: context.endpoint_key.clone(),
        collector_type: context.collector_type,
        sample_name: sample.name.clone(),
        sample_key: sample.key.clone(),
        metric_type: sample.metric_type.clone(),
        unit: sample.unit.clone(),
    }
}

impl OtlpSink {
    /// Creates one independently queued sink for each configured OTLP target.
    ///
    /// The returned order matches `configs`. Each sink starts separate log and
    /// metric drain tasks. Queue metrics identify the configured endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if no Tokio runtime is active, or if Prometheus metrics
    /// cannot be created, registered, or initialized for a target.
    pub(crate) fn new_many(
        configs: &[OtlpTargetConfig],
        mapper: Arc<dyn RedfishEventMapper>,
        metrics_manager: &MetricsManager,
        prefix: &str,
    ) -> Result<Vec<Self>, HealthError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|e| {
            HealthError::GenericError(format!("otlp sink requires active tokio runtime: {e}"))
        })?;

        let replaced_total = CounterVec::new(
            Opts::new(
                format!("{prefix}_otlp_sink_replaced_total"),
                "total log events replaced in the otlp queue before drain could process them, labeled by target",
            ),
            &["target"],
        )?;

        metrics_manager
            .global_registry()
            .register(Box::new(replaced_total.clone()))?;

        let metrics_replaced_total = CounterVec::new(
            Opts::new(
                format!("{prefix}_otlp_sink_metrics_replaced_total"),
                "total metric samples replaced in the otlp queue before drain could process them, labeled by target",
            ),
            &["target"],
        )?;

        metrics_manager
            .global_registry()
            .register(Box::new(metrics_replaced_total.clone()))?;

        let mut sinks = Vec::with_capacity(configs.len());

        for config in configs {
            let capacity = NonZeroUsize::new(config.queue_capacity).ok_or_else(|| {
                HealthError::GenericError("otlp queue capacity must be greater than 0".to_string())
            })?;

            let queue: Arc<OtlpQueue> = Arc::new(DedupQueue::bounded(capacity));
            let metrics_queue: Arc<OtlpMetricsQueue> = Arc::new(DedupQueue::bounded(capacity));

            let target = ConfiguredOtlpTarget(config.endpoint.clone());

            let replaced_total =
                replaced_total.get_metric_with_label_values(&[&config.endpoint])?;

            let metrics_replaced_total =
                metrics_replaced_total.get_metric_with_label_values(&[&config.endpoint])?;

            let drain = OtlpDrainTask::new(queue.clone(), config.clone());
            handle.spawn(drain.run());

            // Each target and signal owns a drain so a slow collector cannot
            // block delivery to another target or signal.
            let metrics_drain = OtlpMetricsDrainTask::new(
                metrics_queue.clone(),
                config.clone(),
                prefix.to_string(),
            );

            handle.spawn(metrics_drain.run());

            sinks.push(Self {
                queue,
                metrics_queue,
                replaced_total,
                metrics_replaced_total,
                target,
                mapper: mapper.clone(),
                include_diagnostics: config.include_diagnostics,
            });
        }

        register_queue_depth_metric(&sinks);

        Ok(sinks)
    }

    /// Records the observable result of one log or metric queue insertion.
    fn record_save_outcome(&self, outcome: SaveOutcome, signal: OtlpSignal) {
        match outcome {
            SaveOutcome::Inserted => {}
            SaveOutcome::Replaced => match signal {
                OtlpSignal::Logs => self.replaced_total.inc(),
                OtlpSignal::Metrics => self.metrics_replaced_total.inc(),
            },
            SaveOutcome::DroppedOldest => emit(OtlpQueueEntryDropped {
                target: self.target.clone(),
                signal,
            }),
        }
    }

    /// Enqueues the emitted log record using the parent event identity.
    fn enqueue_log_event(&self, context: &EventContext, record: &LogRecord) {
        let record = record.emitted_log_record(self.include_diagnostics);

        let key = self
            .mapper
            .queue_key(&context.endpoint_key, &record.attributes);

        let event = CollectorEvent::Log(Box::new(record.into_owned()));

        let outcome = self.queue.save_latest(key, (context.clone(), event));
        self.record_save_outcome(outcome, OtlpSignal::Logs);
    }
}

#[cfg(feature = "bench-hooks")]
impl OtlpSink {
    pub fn new_for_bench(mapper: Arc<dyn RedfishEventMapper>) -> Self {
        Self::new_for_bench_with_diagnostics(mapper, false)
    }
}

#[cfg(any(test, feature = "bench-hooks"))]
impl OtlpSink {
    /// Builds a bench sink with diagnostic emission explicitly configured.
    fn new_for_bench_with_diagnostics(
        mapper: Arc<dyn RedfishEventMapper>,
        include_diagnostics: bool,
    ) -> Self {
        let capacity = match NonZeroUsize::new(OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY) {
            Some(capacity) => capacity,
            None => NonZeroUsize::MIN,
        };

        Self {
            queue: Arc::new(DedupQueue::bounded(capacity)),
            metrics_queue: Arc::new(DedupQueue::bounded(capacity)),
            replaced_total: Counter::new("bench_replaced", "bench").unwrap(),
            metrics_replaced_total: Counter::new("bench_metrics_replaced", "bench").unwrap(),
            target: ConfiguredOtlpTarget("bench".to_string()),
            mapper,
            include_diagnostics,
        }
    }
}

#[cfg(feature = "bench-hooks")]
impl OtlpSink {
    pub fn pop_for_bench(&self) -> Option<(EventContext, CollectorEvent)> {
        self.queue.pop().map(|(_key, value)| value)
    }

    pub fn pop_metric_for_bench(&self) -> Option<(EventContext, MetricSample)> {
        self.metrics_queue.pop().map(|(_key, value)| value)
    }
}

impl DataSink for OtlpSink {
    fn sink_type(&self) -> &'static str {
        "otlp_sink"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        if let CollectorEvent::Metric(sample) = event {
            let key = metric_queue_key(context, sample);

            let outcome = self
                .metrics_queue
                .save_latest(key, (context.clone(), (**sample).clone()));

            self.record_save_outcome(outcome, OtlpSignal::Metrics);

            return Ok(());
        }

        if !is_otlp_log_relevant(event) {
            return Ok(());
        }

        let (key, event) = match event {
            CollectorEvent::Log(record) => {
                self.enqueue_log_event(context, record);
                return Ok(());
            }
            CollectorEvent::HealthReport(report) => {
                let key = format!(
                    "{}|health_report|{}",
                    context.endpoint_key,
                    report.source.as_str()
                );

                (key, event.clone())
            }
            CollectorEvent::Firmware(info) => {
                let key = format!("{}|firmware|{}", context.endpoint_key, info.component);
                (key, event.clone())
            }
            _ => return Ok(()),
        };

        let outcome = self.queue.save_latest(key, (context.clone(), event));
        self.record_save_outcome(outcome, OtlpSignal::Logs);

        Ok(())
    }
}

/// Registers queue-depth observations for every configured OTLP target.
///
/// The callback runs only when OpenTelemetry collects metrics. It reads an
/// atomic depth snapshot without locking or scanning either queue. Weak
/// references avoid extending a sink or queue lifetime solely for metrics.
fn register_queue_depth_metric(sinks: &[OtlpSink]) {
    let queues = sinks
        .iter()
        .map(|sink| {
            (
                sink.target.0.clone(),
                Arc::downgrade(&sink.queue),
                Arc::downgrade(&sink.metrics_queue),
            )
        })
        .collect::<Vec<_>>();

    opentelemetry::global::meter("carbide-health")
        .u64_observable_gauge("carbide_health_otlp_queue_depth")
        .with_description("Number of entries waiting in an OTLP queue, by target and signal.")
        .with_callback(move |observer| {
            for (target, logs, metrics) in &queues {
                if let Some(logs) = logs.upgrade() {
                    observer.observe(
                        logs.len() as u64,
                        &[
                            KeyValue::new("target", target.clone()),
                            KeyValue::new("signal", "logs"),
                        ],
                    );
                }

                if let Some(metrics) = metrics.upgrade() {
                    observer.observe(
                        metrics.len() as u64,
                        &[
                            KeyValue::new("target", target.clone()),
                            KeyValue::new("signal", "metrics"),
                        ],
                    );
                }
            }
        })
        .build();
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::str::FromStr;

    use carbide_instrument::testing::MetricsCapture;
    use mac_address::MacAddress;

    use super::*;
    use crate::sink::event_mapper::OpenBmcEventMapper;
    use crate::sink::{
        CompositeDataSink, DiagnosticLogRecord, LogRecord, LogSeverity, MetricSample,
    };

    fn test_context() -> EventContext {
        EventContext {
            endpoint_key: "10.85.14.144".to_string(),
            addr: crate::endpoint::BmcAddr {
                ip: "10.85.14.144".parse().unwrap(),
                port: Some(443),
                mac: Some(MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap()),
            },
            collector_type: "test",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn log_event(message_id: &str, message_args: &str) -> CollectorEvent {
        log_event_with_diagnostic_record(message_id, message_args, None)
    }

    /// Builds a log event with an optional diagnostic carrier.
    fn log_event_with_diagnostic_record(
        message_id: &str,
        message_args: &str,
        diagnostic_record: Option<DiagnosticLogRecord>,
    ) -> CollectorEvent {
        CollectorEvent::Log(Box::new(LogRecord {
            body: "test".to_string(),
            severity: LogSeverity::Info,
            attributes: vec![
                (Cow::Borrowed("message_id"), message_id.to_string()),
                (Cow::Borrowed("message_args"), message_args.to_string()),
            ],
            diagnostic_record,
        }))
    }

    /// Builds a diagnostic carrier with stable parent metadata.
    fn diagnostic_log_record(body: &str) -> DiagnosticLogRecord {
        DiagnosticLogRecord {
            body: body.to_string(),
            attributes: vec![(
                Cow::Borrowed("redfish.parent.log_entry_id"),
                "42".to_string(),
            )],
        }
    }

    fn decoded_protobuf_event(test_value: &str) -> CollectorEvent {
        CollectorEvent::Log(Box::new(LogRecord {
            body: "Extended protobuf notification received".to_string(),
            severity: LogSeverity::Info,
            attributes: vec![
                (
                    Cow::Borrowed("notification"),
                    "extended_notification".to_string(),
                ),
                (
                    Cow::Borrowed("message_id"),
                    "Example.1.0.ExtendedNotification".to_string(),
                ),
                (
                    Cow::Borrowed("message_args"),
                    r#"["extended_notification"]"#.to_string(),
                ),
                (
                    Cow::Borrowed(LogRecord::DECODED_PROTOBUF_PAYLOAD_ATTRIBUTE),
                    serde_json::json!({ "testField": test_value }).to_string(),
                ),
                (
                    Cow::Borrowed("protobuf.message_type"),
                    "example.Notification".to_string(),
                ),
            ],
            diagnostic_record: None,
        }))
    }

    fn metric_event() -> CollectorEvent {
        metric_event_with("k", "gauge", "celsius")
    }

    fn metric_event_with(key: &str, metric_type: &str, unit: &str) -> CollectorEvent {
        metric_event_with_name("temp", key, metric_type, unit)
    }

    fn metric_event_with_name(
        name: &str,
        key: &str,
        metric_type: &str,
        unit: &str,
    ) -> CollectorEvent {
        CollectorEvent::Metric(Box::new(MetricSample {
            key: key.to_string(),
            name: name.to_string(),
            metric_type: metric_type.to_string(),
            unit: unit.to_string(),
            value: 42.0,
            labels: vec![(Cow::Borrowed("sensor"), "temp1".to_string())],
            context: None,
        }))
    }

    fn test_sink() -> OtlpSink {
        OtlpSink::new_for_bench_with_diagnostics(Arc::new(OpenBmcEventMapper), false)
    }

    fn bounded_test_sink(capacity: usize) -> OtlpSink {
        let mut sink = test_sink();
        let capacity = NonZeroUsize::new(capacity).expect("test capacity must be nonzero");

        sink.queue = Arc::new(DedupQueue::bounded(capacity));
        sink.metrics_queue = Arc::new(DedupQueue::bounded(capacity));
        sink.target = ConfiguredOtlpTarget("http://bounded.example:4317".to_string());

        sink
    }

    #[test]
    fn is_otlp_log_relevant_excludes_metric_events() {
        assert!(!is_otlp_log_relevant(&metric_event()));
        assert!(!is_otlp_log_relevant(
            &CollectorEvent::MetricCollectionStart
        ));
        assert!(!is_otlp_log_relevant(&CollectorEvent::MetricCollectionEnd));
    }

    #[test]
    fn is_otlp_log_relevant_includes_log_events() {
        assert!(is_otlp_log_relevant(&log_event("OpenBMC.0.1.Test", "[]")));
    }

    #[test]
    fn metric_events_go_to_metrics_queue_not_logs_queue() {
        let sink = test_sink();
        let ctx = test_context();
        sink.handle_event(&ctx, &metric_event());
        assert!(sink.queue.pop().is_none(), "logs queue should be empty");
        assert!(
            sink.metrics_queue.pop().is_some(),
            "metrics queue should have the sample"
        );
    }

    #[test]
    fn metric_collection_sentinels_are_no_op() {
        let sink = test_sink();
        let ctx = test_context();
        sink.handle_event(&ctx, &CollectorEvent::MetricCollectionStart);
        sink.handle_event(&ctx, &CollectorEvent::MetricCollectionEnd);
        assert!(sink.queue.pop().is_none());
        assert!(sink.metrics_queue.pop().is_none());
    }

    #[test]
    fn metric_events_dedup_by_sample_key() {
        let sink = test_sink();
        let ctx = test_context();
        sink.handle_event(&ctx, &metric_event());
        sink.handle_event(&ctx, &metric_event());
        let mut count = 0;
        while sink.metrics_queue.pop().is_some() {
            count += 1;
        }
        assert_eq!(count, 1, "same key should dedup to one entry");
        assert_eq!(sink.metrics_replaced_total.get() as u64, 1);
    }

    #[test]
    fn metric_events_with_same_sample_key_but_different_type_are_separate_entries() {
        let sink = test_sink();
        let ctx = test_context();
        sink.handle_event(&ctx, &metric_event_with("k", "voltage", "volts"));
        sink.handle_event(&ctx, &metric_event_with("k", "current", "volts"));

        let mut count = 0;
        while sink.metrics_queue.pop().is_some() {
            count += 1;
        }

        assert_eq!(count, 2, "metric type is part of metric identity");
        assert_eq!(sink.metrics_replaced_total.get() as u64, 0);
    }

    #[test]
    fn metric_events_with_same_sample_key_but_different_unit_are_separate_entries() {
        let sink = test_sink();
        let ctx = test_context();
        sink.handle_event(&ctx, &metric_event_with("k", "temperature", "celsius"));
        sink.handle_event(&ctx, &metric_event_with("k", "temperature", "fahrenheit"));

        let mut count = 0;
        while sink.metrics_queue.pop().is_some() {
            count += 1;
        }

        assert_eq!(count, 2, "unit is part of metric identity");
        assert_eq!(sink.metrics_replaced_total.get() as u64, 0);
    }

    #[test]
    fn metric_events_with_same_sample_identity_but_different_collector_are_separate_entries() {
        let sink = test_sink();
        let rest_ctx = EventContext {
            collector_type: "nvue_rest",
            ..test_context()
        };
        let gnmi_ctx = EventContext {
            collector_type: "nvue_gnmi",
            ..test_context()
        };
        sink.handle_event(&rest_ctx, &metric_event());
        sink.handle_event(&gnmi_ctx, &metric_event());

        let mut count = 0;
        while sink.metrics_queue.pop().is_some() {
            count += 1;
        }

        assert_eq!(count, 2, "collector type is part of metric identity");
        assert_eq!(sink.metrics_replaced_total.get() as u64, 0);
    }

    #[test]
    fn metric_events_with_same_key_type_and_unit_but_different_name_are_separate_entries() {
        let sink = test_sink();
        let ctx = test_context();
        sink.handle_event(
            &ctx,
            &metric_event_with_name("nvue_rest", "k", "status", "state"),
        );
        sink.handle_event(
            &ctx,
            &metric_event_with_name("nvue_gnmi", "k", "status", "state"),
        );

        let mut count = 0;
        while sink.metrics_queue.pop().is_some() {
            count += 1;
        }

        assert_eq!(count, 2, "metric name is part of metric identity");
        assert_eq!(sink.metrics_replaced_total.get() as u64, 0);
    }

    #[test]
    fn log_events_are_queued() {
        let sink = test_sink();
        let ctx = test_context();
        sink.handle_event(&ctx, &log_event("OpenBMC.0.1.Test", r#"["sensor1"]"#));
        assert!(sink.queue.pop().is_some());
    }

    #[test]
    fn ordinary_log_occurrences_queue_separately_and_replays_replace() {
        let occurrence = |entry_id: &str| {
            let CollectorEvent::Log(mut record) = log_event(
                "ResourceEvent.1.0.ResourceErrorsDetected",
                r#"["resource-1","detail"]"#,
            ) else {
                unreachable!("log_event always returns a log event");
            };

            record
                .attributes
                .push((Cow::Borrowed("service_id"), "EventLog".to_string()));

            record
                .attributes
                .push((Cow::Borrowed("entry_id"), entry_id.to_string()));

            CollectorEvent::Log(record)
        };

        let first = occurrence("41");
        let second = occurrence("42");
        let sink = test_sink();
        let context = test_context();

        sink.handle_event(&context, &first);
        sink.handle_event(&context, &second);

        assert_eq!(sink.queue.len(), 2);
        assert_eq!(sink.replaced_total.get() as u64, 0);

        sink.handle_event(&context, &second);

        assert_eq!(sink.queue.len(), 2);
        assert_eq!(sink.replaced_total.get() as u64, 1);
    }

    #[test]
    fn bounded_signal_queues_drop_oldest_and_report_depth() {
        let metrics = MetricsCapture::start();
        let sink = bounded_test_sink(1);
        let context = test_context();

        register_queue_depth_metric(std::slice::from_ref(&sink));

        sink.handle_event(&context, &log_event("OpenBMC.0.1.First", "[]"));
        sink.handle_event(&context, &log_event("OpenBMC.0.1.Second", "[]"));

        sink.handle_event(
            &context,
            &metric_event_with_name("first", "a", "gauge", "state"),
        );

        sink.handle_event(
            &context,
            &metric_event_with_name("second", "b", "gauge", "state"),
        );

        let target = "http://bounded.example:4317";

        for signal in ["logs", "metrics"] {
            assert_eq!(
                metrics.counter_delta(
                    "carbide_health_otlp_queue_dropped_total",
                    &[("target", target), ("signal", signal)],
                ),
                1.0,
                "{signal} queue should report one eviction",
            );

            assert_eq!(
                metrics.gauge_value(
                    "carbide_health_otlp_queue_depth",
                    &[("target", target), ("signal", signal)],
                ),
                1.0,
                "{signal} queue should remain at capacity",
            );
        }

        assert_eq!(sink.queue.len(), 1);
        assert_eq!(sink.metrics_queue.len(), 1);
    }

    #[test]
    fn composite_fans_log_event_to_each_otlp_target_queue() {
        let first = Arc::new(test_sink());
        let second = Arc::new(test_sink());
        let sinks: Vec<Arc<dyn DataSink>> = vec![first.clone(), second.clone()];

        let metrics_manager = Arc::new(
            MetricsManager::new("otlp_multi_target_test")
                .expect("metrics manager should initialize"),
        );

        let composite = CompositeDataSink::new(sinks, metrics_manager);
        let context = test_context();
        let event = log_event("OpenBMC.0.1.Test", r#"["sensor1"]"#);

        composite.handle_event(&context, &event);

        assert!(first.queue.pop().is_some());
        assert!(second.queue.pop().is_some());
    }

    #[tokio::test]
    async fn new_many_labels_replacement_counters_by_target() {
        let configs = vec![
            OtlpTargetConfig {
                endpoint: "http://first.example:4317".to_string(),
                batch_size: 512,
                queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
                flush_interval: std::time::Duration::from_secs(2),
                include_diagnostics: false,
                include_alert_details: false,
                tls: None,
            },
            OtlpTargetConfig {
                endpoint: "http://second.example:4317".to_string(),
                batch_size: 512,
                queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
                flush_interval: std::time::Duration::from_secs(2),
                include_diagnostics: false,
                include_alert_details: false,
                tls: None,
            },
        ];

        let metrics_manager = MetricsManager::new("otlp_replacement_manager")
            .expect("metrics manager should initialize");

        let sinks = OtlpSink::new_many(
            &configs,
            Arc::new(OpenBmcEventMapper),
            &metrics_manager,
            "otlp_replacement_test",
        )
        .expect("OTLP sinks should initialize");

        sinks[0].replaced_total.inc();
        sinks[1].metrics_replaced_total.inc();

        let metrics = metrics_manager
            .export_metrics()
            .expect("metrics should export");

        assert!(metrics.contains(
            "otlp_replacement_test_otlp_sink_replaced_total{target=\"http://first.example:4317\"} 1"
        ));

        assert!(
            metrics
                .contains("otlp_replacement_test_otlp_sink_replaced_total{target=\"http://second.example:4317\"} 0")
        );

        assert!(metrics.contains(
            "otlp_replacement_test_otlp_sink_metrics_replaced_total{target=\"http://first.example:4317\"} 0"
        ));

        assert!(metrics.contains(
            "otlp_replacement_test_otlp_sink_metrics_replaced_total{target=\"http://second.example:4317\"} 1"
        ));
    }

    /// Verifies OTLP logs omit diagnostic payloads by default.
    #[test]
    fn diagnostic_log_record_is_skipped_by_default() {
        let sink = test_sink();
        let ctx = test_context();

        sink.handle_event(
            &ctx,
            &log_event_with_diagnostic_record(
                "OpenBMC.0.1.Test",
                "[]",
                Some(diagnostic_log_record("payload-a")),
            ),
        );

        let mut bodies = Vec::new();
        while let Some((_key, (_context, CollectorEvent::Log(record)))) = sink.queue.pop() {
            bodies.push(record.body);
        }

        assert_eq!(bodies, vec!["test"]);
        assert_eq!(sink.replaced_total.get() as u64, 0);
    }

    /// Verifies diagnostics are folded into the single latest parent log.
    #[test]
    fn diagnostic_log_record_uses_latest_wins_by_endpoint() {
        let sink = OtlpSink::new_for_bench_with_diagnostics(Arc::new(OpenBmcEventMapper), true);
        let ctx = test_context();

        sink.handle_event(
            &ctx,
            &log_event_with_diagnostic_record(
                "OpenBMC.0.1.Test",
                "[]",
                Some(diagnostic_log_record("payload-a")),
            ),
        );
        sink.handle_event(
            &ctx,
            &log_event_with_diagnostic_record(
                "OpenBMC.0.1.Test",
                "[]",
                Some(diagnostic_log_record("payload-b")),
            ),
        );
        sink.handle_event(
            &ctx,
            &log_event_with_diagnostic_record(
                "OpenBMC.0.1.Test",
                "[]",
                Some(diagnostic_log_record("payload-c")),
            ),
        );

        let mut records = Vec::new();
        while let Some((_key, (_context, CollectorEvent::Log(record)))) = sink.queue.pop() {
            records.push(record);
        }

        assert_eq!(records.len(), 1);

        let diagnostic_body: serde_json::Value =
            serde_json::from_str(&records[0].body).expect("valid diagnostic body");
        assert_eq!(diagnostic_body["message"].as_str(), Some("test"));
        assert_eq!(
            diagnostic_body["diagnostic_data"].as_str(),
            Some("payload-c")
        );
        assert_eq!(sink.replaced_total.get() as u64, 2);

        assert!(
            records[0]
                .attributes
                .iter()
                .any(|(key, _)| key.as_ref() == "redfish.parent.log_entry_id")
        );
    }

    #[test]
    fn decoded_protobuf_is_projected_for_all_otlp_targets() {
        let enabled = OtlpSink::new_for_bench_with_diagnostics(Arc::new(OpenBmcEventMapper), true);
        let context = test_context();

        enabled.handle_event(&context, &decoded_protobuf_event("first"));
        enabled.handle_event(&context, &decoded_protobuf_event("second"));
        enabled.handle_event(&context, &decoded_protobuf_event("second"));

        let mut records = Vec::new();

        while let Some((_key, (_context, CollectorEvent::Log(record)))) = enabled.queue.pop() {
            records.push(record);
        }

        assert_eq!(records.len(), 2, "distinct notifications remain queued");
        assert_eq!(enabled.replaced_total.get() as u64, 1);

        assert!(records.iter().all(|record| {
            record.attributes.iter().any(|(key, value)| {
                key.as_ref() == "protobuf.decoded_payload" && value.contains("testField")
            })
        }));

        assert!(records.iter().any(|record| {
            record
                .attributes
                .iter()
                .any(|(_, value)| value.contains("first"))
        }));

        assert!(records.iter().any(|record| {
            record
                .attributes
                .iter()
                .any(|(_, value)| value.contains("second"))
        }));

        let diagnostics_disabled =
            OtlpSink::new_for_bench_with_diagnostics(Arc::new(OpenBmcEventMapper), false);

        diagnostics_disabled.handle_event(&context, &decoded_protobuf_event("first"));
        diagnostics_disabled.handle_event(&context, &decoded_protobuf_event("second"));

        let mut diagnostics_disabled_records = Vec::new();

        while let Some((_key, (_context, CollectorEvent::Log(record)))) =
            diagnostics_disabled.queue.pop()
        {
            diagnostics_disabled_records.push(record);
        }

        assert_eq!(diagnostics_disabled_records.len(), 2);

        assert!(diagnostics_disabled_records.iter().all(|record| {
            record.attributes.iter().any(|(key, value)| {
                key.as_ref() == "protobuf.decoded_payload" && value.contains("testField")
            })
        }));
    }

    #[test]
    fn replaced_counter_increments_on_dedup() {
        let sink = test_sink();
        let ctx = test_context();

        sink.handle_event(
            &ctx,
            &log_event(
                "OpenBMC.0.1.SensorThresholdWarningLowGoingHigh",
                r#"["HGX_GPU_0_Temp_1","3.96","-0.05"]"#,
            ),
        );
        assert_eq!(sink.replaced_total.get() as u64, 0);

        sink.handle_event(
            &ctx,
            &log_event(
                "OpenBMC.0.1.SensorThresholdWarningHighGoingLow",
                r#"["HGX_GPU_0_Temp_1","3.96","-0.05"]"#,
            ),
        );
        assert_eq!(sink.replaced_total.get() as u64, 1);
    }

    #[test]
    fn different_sensors_are_separate_entries() {
        let sink = test_sink();
        let ctx = test_context();

        sink.handle_event(
            &ctx,
            &log_event(
                "OpenBMC.0.1.SensorThresholdWarningLowGoingHigh",
                r#"["HGX_GPU_0_Temp_1","3.96","-0.05"]"#,
            ),
        );
        sink.handle_event(
            &ctx,
            &log_event(
                "OpenBMC.0.1.SensorThresholdWarningLowGoingHigh",
                r#"["HGX_GPU_1_Temp_1","3.96","-0.05"]"#,
            ),
        );

        let mut count = 0;
        while sink.queue.pop().is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
    }
}
