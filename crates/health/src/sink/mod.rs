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

mod composite;
mod dedup_queue;
#[cfg(not(feature = "bench-hooks"))]
pub(crate) mod event_mapper;
#[cfg(feature = "bench-hooks")]
pub mod event_mapper;
mod events;
mod health_report;
mod log_file;
mod nvlink_domain_health_report;
pub(crate) mod otlp;
mod power_shelf_health_report;
mod prometheus;
mod rack_health_report;
mod switch_health_report;
mod tracing;

pub use composite::CompositeDataSink;
pub use events::{
    Classification, CollectorEvent, DiagnosticLogRecord, EventContext, FirmwareInfo, HealthReport,
    HealthReportAlert, HealthReportSuccess, HealthReportTarget, LogRecord, LogSeverity,
    MetricSample, Probe, ReportSource, SensorAttribution, SensorThresholdContext,
};
pub use health_report::HealthReportSink;
pub use log_file::LogFileSink;
pub use nvlink_domain_health_report::NvLinkDomainHealthReportSink;
pub use power_shelf_health_report::PowerShelfHealthReportSink;
pub use prometheus::PrometheusSink;
pub use rack_health_report::RackHealthReportSink;
pub use switch_health_report::SwitchHealthReportSink;
pub use tracing::TracingSink;

#[cfg(not(feature = "bench-hooks"))]
pub(crate) use self::otlp::OtlpSink;
#[cfg(feature = "bench-hooks")]
pub use self::otlp::OtlpSink;
use crate::HealthError;

pub trait DataSink: Send + Sync {
    fn sink_type(&self) -> &'static str;

    /// Handles one event, surfacing failure to the caller.
    ///
    /// Implementations log their own failure detail at the failure site; the
    /// returned error exists so dispatchers can meter per-sink outcomes (the
    /// composite records it as `component_failures_total{component_kind="sink"}`).
    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError>;

    /// Fire-and-forget entry point for callers that do not track outcomes.
    ///
    /// The result is deliberately dropped here: failures are already logged
    /// by the failing sink, and metered when dispatched through the composite.
    fn handle_event(&self, context: &EventContext, event: &CollectorEvent) {
        if let Err(error) = self.try_handle_event(context, event) {
            // Fire-and-forget by contract: the sink logged its own detail and
            // the composite meters failures; this line is the safety net for a
            // future sink that forgets to do either.
            ::tracing::debug!(%error, "sink dropped an event");
        }
    }
}

/// One attempt to submit a health report upstream to the NICo API, emitted by
/// the report sinks' submission workers for every completed attempt.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "health_report_submission_completed",
    metric_name = "carbide_health_report_submissions_total",
    component = "nico-hardware-health",
    log = dynamic,
    metric = counter,
    message = "Failed to submit health report",
    describe = "Number of health report submissions to the NICo API, by report target and outcome."
)]
pub(crate) struct HealthReportSubmitted {
    #[label]
    pub target: HealthReportTarget,
    #[label]
    pub outcome: carbide_instrument::Outcome,

    /// The machine, NVLink domain, rack, switch, or power shelf the report describes.
    #[context]
    pub id: String,
    #[context]
    pub worker_id: usize,
    /// The submission error's text; empty on success (the line only renders
    /// on failure).
    #[context]
    pub error: String,
}

/// Every submission is counted; only the failures write the WARN line.
impl carbide_instrument::DynamicLog for HealthReportSubmitted {
    fn log_at(&self) -> carbide_instrument::LogAt {
        match self.outcome {
            carbide_instrument::Outcome::Error => {
                carbide_instrument::LogAt::Level(::tracing::Level::WARN)
            }
            carbide_instrument::Outcome::Ok => carbide_instrument::LogAt::Off,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mac_address::MacAddress;

    use super::{
        CollectorEvent, CompositeDataSink, DataSink, DiagnosticLogRecord, EventContext, LogRecord,
        LogSeverity, MetricSample, PrometheusSink,
    };
    use crate::endpoint::{BmcAddr, EndpointMetadata, MachineData, SharedSystemUuid};
    use crate::metrics::MetricsManager;

    struct CountingSink {
        counter: Arc<AtomicUsize>,
    }

    impl DataSink for CountingSink {
        fn sink_type(&self) -> &'static str {
            "counting_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            _event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct NoopSink;

    impl DataSink for NoopSink {
        fn sink_type(&self) -> &'static str {
            "noop_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            _event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            Ok(())
        }
    }

    struct FailingSink;

    impl DataSink for FailingSink {
        fn sink_type(&self) -> &'static str {
            "failing_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            _event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            Err(crate::HealthError::GenericError(
                "sink rejected the event".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn test_composite_sink_fanout_with_noop_sink() {
        let success_counter = Arc::new(AtomicUsize::new(0));
        let metrics_manager =
            Arc::new(MetricsManager::new("test").expect("should create metrics manager"));

        let sink_ok_1 = Arc::new(CountingSink {
            counter: success_counter.clone(),
        });
        let sink_noop = Arc::new(NoopSink);
        let sink_ok_2 = Arc::new(CountingSink {
            counter: success_counter.clone(),
        });

        let composite =
            CompositeDataSink::new(vec![sink_ok_1, sink_noop, sink_ok_2], metrics_manager);

        let context = EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().expect("valid ip"),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").unwrap(),
            },
            collector_type: "test",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        };

        let event = CollectorEvent::Metric(
            MetricSample {
                key: "key".to_string(),
                name: "metric".to_string(),
                metric_type: "gauge".to_string(),
                unit: "count".to_string(),
                value: 1.0,
                labels: Vec::new(),
                context: None,
            }
            .into(),
        );
        composite.handle_event(&context, &event);

        assert_eq!(success_counter.load(Ordering::SeqCst), 2);
    }

    /// A failing sink must move `component_failures_total{component_kind="sink"}`
    /// for its own series, keep the fanout going, and leave the healthy
    /// sinks' failure series untouched.
    #[tokio::test]
    async fn test_composite_sink_meters_per_sink_failures() {
        let handled_counter = Arc::new(AtomicUsize::new(0));
        let metrics_manager =
            Arc::new(MetricsManager::new("test").expect("should create metrics manager"));

        let composite = CompositeDataSink::new(
            vec![
                Arc::new(FailingSink),
                Arc::new(CountingSink {
                    counter: handled_counter.clone(),
                }),
            ],
            metrics_manager.clone(),
        );

        let context = EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().expect("valid ip"),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").unwrap(),
            },
            collector_type: "test",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        };
        let event = CollectorEvent::MetricCollectionStart;

        composite.handle_event(&context, &event);
        composite.handle_event(&context, &event);

        assert_eq!(
            handled_counter.load(Ordering::SeqCst),
            2,
            "a failing sink must not block the sinks after it"
        );

        let export = metrics_manager
            .export_metrics()
            .expect("service metrics export should work");
        let failure_lines: Vec<&str> = export
            .lines()
            .filter(|line| line.starts_with("test_component_failures_total{"))
            .collect();

        assert_eq!(
            failure_lines,
            vec![
                r#"test_component_failures_total{component_kind="sink",component_name="failing_sink"} 2"#
            ],
            "only the failing sink's series may move"
        );
    }

    /// A failed submission writes one WARN line and ticks the counter's
    /// error series.
    #[test]
    fn health_report_submission_failure_logs_warn_and_ticks_counter() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};

        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            carbide_instrument::emit(super::HealthReportSubmitted {
                target: super::HealthReportTarget::Rack,
                outcome: carbide_instrument::Outcome::Error,
                id: "RACK_1".to_string(),
                worker_id: 3,
                error: "connection refused".to_string(),
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::WARN);
        assert_eq!(logs[0].message, "Failed to submit health report");
        assert_eq!(
            metrics.counter_delta(
                "carbide_health_report_submissions_total",
                &[("target", "rack"), ("outcome", "error")],
            ),
            1.0
        );
    }

    /// A successful submission is counted but never logged.
    #[test]
    fn health_report_submission_success_counts_without_logging() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};

        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            carbide_instrument::emit(super::HealthReportSubmitted {
                target: super::HealthReportTarget::Machine,
                outcome: carbide_instrument::Outcome::Ok,
                id: "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0".to_string(),
                worker_id: 0,
                error: String::new(),
            });
        });

        assert!(logs.is_empty(), "successful submissions must not log");
        assert_eq!(
            metrics.counter_delta(
                "carbide_health_report_submissions_total",
                &[("target", "machine"), ("outcome", "ok")],
            ),
            1.0
        );
    }

    #[tokio::test]
    async fn test_prometheus_sink_only_records_metric_events() {
        let metrics_manager =
            Arc::new(MetricsManager::new("test").expect("should create metrics manager"));
        let sink = PrometheusSink::new(metrics_manager.clone(), "test_sink")
            .expect("sink should initialize");

        let context = EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().expect("valid ip"),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").unwrap(),
            },
            collector_type: "test",
            labels: std::collections::BTreeMap::from([(
                "site".to_string(),
                "rno-dev7".to_string(),
            )]),
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: Some(
                    "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                        .parse()
                        .expect("valid machine id"),
                ),
                machine_serial: None,
                system_uuid: SharedSystemUuid::default(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                driver_version: None,
            })),
            rack_id: None,
        };

        let log_event = CollectorEvent::Log(
            LogRecord {
                body: "ignored by prometheus sink".to_string(),
                severity: LogSeverity::Info,
                attributes: Vec::new(),
                diagnostic_record: Some(DiagnosticLogRecord {
                    body: "also ignored by prometheus sink".to_string(),
                    attributes: Vec::new(),
                }),
            }
            .into(),
        );
        sink.handle_event(&context, &log_event);

        let export_after_log = metrics_manager
            .export_telemetry()
            .expect("telemetry export should work");
        assert!(!export_after_log.contains("test_sink_hw_sensor"));

        let metric_event = CollectorEvent::Metric(
            MetricSample {
                key: "metric_key".to_string(),
                name: "hw_sensor".to_string(),
                metric_type: "temperature".to_string(),
                unit: "celsius".to_string(),
                value: 42.0,
                labels: vec![(Cow::Borrowed("sensor"), "temp1".to_string())],
                context: None,
            }
            .into(),
        );

        sink.handle_event(&context, &metric_event);

        let export_after_metric = metrics_manager
            .export_telemetry()
            .expect("telemetry export should work");
        assert!(export_after_metric.contains("test_sink_hw_sensor_temperature_celsius"));
        assert!(export_after_metric.contains("site=\"rno-dev7\""));

        let service_metrics = metrics_manager
            .export_metrics()
            .expect("service metrics export should work");
        assert!(!service_metrics.contains("test_sink_hw_sensor_temperature_celsius"));
    }

    #[tokio::test]
    async fn test_prometheus_sink_removes_collector_metrics() {
        let metrics_manager =
            Arc::new(MetricsManager::new("test").expect("should create metrics manager"));
        let sink = PrometheusSink::new(metrics_manager.clone(), "test_sink")
            .expect("sink should initialize");

        let context = EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().expect("valid ip"),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").unwrap(),
            },
            collector_type: "sensor_collector",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: Some(
                    "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                        .parse()
                        .expect("valid machine id"),
                ),
                machine_serial: None,
                system_uuid: SharedSystemUuid::default(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                driver_version: None,
            })),
            rack_id: None,
        };

        let metric_event = CollectorEvent::Metric(
            MetricSample {
                key: "metric_key".to_string(),
                name: "hw_sensor".to_string(),
                metric_type: "temperature".to_string(),
                unit: "celsius".to_string(),
                value: 42.0,
                labels: vec![(Cow::Borrowed("sensor"), "temp1".to_string())],
                context: None,
            }
            .into(),
        );

        sink.handle_event(&context, &metric_event);
        let export_before_remove = metrics_manager
            .export_telemetry()
            .expect("telemetry export should work");
        assert!(export_before_remove.contains("test_sink_hw_sensor_temperature_celsius"));

        sink.handle_event(&context, &CollectorEvent::CollectorRemoved);

        let export_after_remove = metrics_manager
            .export_telemetry()
            .expect("telemetry export should work");
        assert!(!export_after_remove.contains("test_sink_hw_sensor_temperature_celsius"));
        assert!(!export_after_remove.contains("endpoint_key=\"42:9e:b1:bd:9d:dd\""));
    }

    #[tokio::test]
    async fn test_prometheus_sink_sweeps_stale_metrics_per_collection_window() {
        let metrics_manager =
            Arc::new(MetricsManager::new("test").expect("should create metrics manager"));
        let sink = PrometheusSink::new(metrics_manager.clone(), "test_sink")
            .expect("sink should initialize");

        let context = EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().expect("valid ip"),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").unwrap(),
            },
            collector_type: "sensor_collector",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: Some(
                    "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                        .parse()
                        .expect("valid machine id"),
                ),
                machine_serial: None,
                system_uuid: SharedSystemUuid::default(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                driver_version: None,
            })),
            rack_id: None,
        };

        let start_event = CollectorEvent::MetricCollectionStart;
        sink.handle_event(&context, &start_event);
        let s1_event = CollectorEvent::Metric(
            MetricSample {
                key: "s1".to_string(),
                name: "hw_sensor".to_string(),
                metric_type: "temperature".to_string(),
                unit: "celsius".to_string(),
                value: 10.0,
                labels: vec![(Cow::Borrowed("sensor"), "temp1".to_string())],
                context: None,
            }
            .into(),
        );
        sink.handle_event(&context, &s1_event);
        let end_event = CollectorEvent::MetricCollectionEnd;
        sink.handle_event(&context, &end_event);

        let first_export = metrics_manager
            .export_telemetry()
            .expect("telemetry export should work");
        assert!(first_export.contains("sensor=\"temp1\""));

        let start_event = CollectorEvent::MetricCollectionStart;
        sink.handle_event(&context, &start_event);
        let s2_event = CollectorEvent::Metric(
            MetricSample {
                key: "s2".to_string(),
                name: "hw_sensor".to_string(),
                metric_type: "temperature".to_string(),
                unit: "celsius".to_string(),
                value: 20.0,
                labels: vec![(Cow::Borrowed("sensor"), "temp2".to_string())],
                context: None,
            }
            .into(),
        );
        sink.handle_event(&context, &s2_event);
        let end_event = CollectorEvent::MetricCollectionEnd;
        sink.handle_event(&context, &end_event);

        let second_export = metrics_manager
            .export_telemetry()
            .expect("telemetry export should work");
        assert!(!second_export.contains("sensor=\"temp1\""));
        assert!(second_export.contains("sensor=\"temp2\""));
    }
}
