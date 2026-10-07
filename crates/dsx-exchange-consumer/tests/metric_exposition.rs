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

//! Exercises DSX message Events end to end: public metric names keep one
//! `_total` suffix, and Events with logs retain their level, message, and
//! diagnostic context.
//!
//! These tests live in their own binary (its own process-global registry) to
//! keep the `counter_delta` measurements deterministic: the crate's other unit
//! tests emit these same events -- the message counters here, and the
//! health-report persist failure below -- but from a different test process, so
//! they cannot advance a shared counter between a test's baseline and delta.

use carbide_dsx_exchange_consumer::messages::LeakPointType;
use carbide_dsx_exchange_consumer::metrics::{
    DroppedMessageType, HealthReportPersistFailed, LeakAlertDetected, MessageDeduplicated,
    MessageDropped, MessageProcessed, MessageReceived,
};
use carbide_instrument::emit;
use carbide_instrument::testing::{CapturedLog, MetricsCapture, capture_logs};
use carbide_test_support::{Check, check_values, value_scenarios};

fn field<'a>(log: &'a CapturedLog, name: &str) -> Option<&'a str> {
    log.fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

#[derive(Debug, PartialEq)]
struct LogObservation<'a> {
    level: tracing::Level,
    metadata_name: &'a str,
    message: &'a str,
    event_name: Option<&'a str>,
    metric_name: Option<&'a str>,
    message_type: Option<&'a str>,
    point_path: Option<&'a str>,
    point_type: Option<&'a str>,
    rack_id: Option<&'a str>,
    rack_name: Option<&'a str>,
    value: Option<&'a str>,
}

fn observe_log(log: &CapturedLog) -> LogObservation<'_> {
    LogObservation {
        level: log.level,
        metadata_name: &log.metadata_name,
        message: &log.message,
        event_name: field(log, "event_name"),
        metric_name: field(log, "metric_name"),
        message_type: field(log, "message_type"),
        point_path: field(log, "point_path"),
        point_type: field(log, "point_type"),
        rack_id: field(log, "rack_id"),
        rack_name: field(log, "rack_name"),
        value: field(log, "value"),
    }
}

fn expected_drop(message_type: &'static str, message: &'static str) -> LogObservation<'static> {
    LogObservation {
        level: tracing::Level::WARN,
        metadata_name: "dsx_exchange_message_dropped",
        message,
        event_name: Some("dsx_exchange_message_dropped"),
        metric_name: Some("carbide_dsx_exchange_consumer_messages_dropped_total"),
        message_type: Some(message_type),
        point_path: None,
        point_type: None,
        rack_id: None,
        rack_name: None,
        value: None,
    }
}

fn expected_dedup() -> LogObservation<'static> {
    LogObservation {
        level: tracing::Level::TRACE,
        metadata_name: "dsx_exchange_message_deduplicated",
        message: "Deduplicating unchanged value",
        event_name: Some("dsx_exchange_message_deduplicated"),
        metric_name: Some("carbide_dsx_exchange_consumer_dedup_skipped_total"),
        message_type: None,
        point_path: Some("site/rack/point"),
        point_type: Some("LeakDetectRack"),
        rack_id: None,
        rack_name: None,
        value: Some("Faulting"),
    }
}

fn expected_alert(point_type: &'static str) -> LogObservation<'static> {
    LogObservation {
        level: tracing::Level::INFO,
        metadata_name: "dsx_exchange_leak_alert_detected",
        message: "Leak alert detected, inserting health override",
        event_name: Some("dsx_exchange_leak_alert_detected"),
        metric_name: Some("carbide_dsx_exchange_consumer_alerts_detected_total"),
        message_type: None,
        point_path: Some("site/rack/point"),
        point_type: Some(point_type),
        rack_id: Some("rack-42"),
        rack_name: Some("Rack-42"),
        value: Some("Faulting"),
    }
}

#[derive(Clone, Copy)]
struct MetricSeries {
    name: &'static str,
    labels: &'static [(&'static str, &'static str)],
}

#[test]
fn leak_point_type_metric_labels_keep_the_bms_names() {
    use carbide_instrument::LabelValue;

    value_scenarios!(
        run = |point_type: LeakPointType| point_type.label_value().to_string();
        "each leak point uses the canonical BMS metric label" {
            LeakPointType::LeakDetectRack => "LeakDetectRack".to_string(),
            LeakPointType::LeakSensorFaultRack => "LeakSensorFaultRack".to_string(),
            LeakPointType::LeakDetectRackTray => "LeakDetectRackTray".to_string(),
        }
    );
}

/// `message_events_preserve_metrics_and_logs` pins each Event's public metric
/// series and, when logging is enabled, its level, message, and context. The
/// alert rows also cover every bounded BMS CamelCase `point_type` label.
#[test]
fn message_events_preserve_metrics_and_logs() {
    let alert_cases = [
        (LeakPointType::LeakDetectRack, "LeakDetectRack"),
        (LeakPointType::LeakSensorFaultRack, "LeakSensorFaultRack"),
        (LeakPointType::LeakDetectRackTray, "LeakDetectRackTray"),
    ];

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(MessageReceived);
        emit(MessageProcessed);
        emit(MessageDropped {
            message_type: DroppedMessageType::Metadata,
        });
        emit(MessageDropped {
            message_type: DroppedMessageType::Value,
        });
        emit(MessageDeduplicated {
            point_path: "site/rack/point".to_string(),
            point_type: LeakPointType::LeakDetectRack,
            value: "Faulting".to_string(),
        });
        for &(point_type, _) in &alert_cases {
            emit(LeakAlertDetected {
                point_type,
                point_path: "site/rack/point".to_string(),
                rack_id: "rack-42".to_string(),
                rack_name: "Rack-42".to_string(),
                value: "Faulting".to_string(),
            });
        }
    });

    check_values(
        [
            Check {
                scenario: "received message increments its counter",
                input: MetricSeries {
                    name: "carbide_dsx_exchange_consumer_messages_received_total",
                    labels: &[],
                },
                expect: 1.0,
            },
            Check {
                scenario: "processed message increments its counter",
                input: MetricSeries {
                    name: "carbide_dsx_exchange_consumer_messages_processed_total",
                    labels: &[],
                },
                expect: 1.0,
            },
            Check {
                scenario: "both queue drop variants share the existing counter",
                input: MetricSeries {
                    name: "carbide_dsx_exchange_consumer_messages_dropped_total",
                    labels: &[],
                },
                expect: 2.0,
            },
            Check {
                scenario: "deduplicated message increments its counter",
                input: MetricSeries {
                    name: "carbide_dsx_exchange_consumer_dedup_skipped_total",
                    labels: &[],
                },
                expect: 1.0,
            },
            Check {
                scenario: "rack leak alert keeps its label series",
                input: MetricSeries {
                    name: "carbide_dsx_exchange_consumer_alerts_detected_total",
                    labels: &[("point_type", "LeakDetectRack")],
                },
                expect: 1.0,
            },
            Check {
                scenario: "rack sensor fault keeps its label series",
                input: MetricSeries {
                    name: "carbide_dsx_exchange_consumer_alerts_detected_total",
                    labels: &[("point_type", "LeakSensorFaultRack")],
                },
                expect: 1.0,
            },
            Check {
                scenario: "rack tray leak alert keeps its label series",
                input: MetricSeries {
                    name: "carbide_dsx_exchange_consumer_alerts_detected_total",
                    labels: &[("point_type", "LeakDetectRackTray")],
                },
                expect: 1.0,
            },
        ],
        |series| metrics.counter_delta(series.name, series.labels),
    );

    let exposition = metrics.render();
    value_scenarios!(
        run = |name: &str| exposition.contains(name);
        "counter names are not doubled by the exporter" {
            "carbide_dsx_exchange_consumer_messages_received_total_total" => false,
            "carbide_dsx_exchange_consumer_messages_processed_total_total" => false,
            "carbide_dsx_exchange_consumer_messages_dropped_total_total" => false,
            "carbide_dsx_exchange_consumer_dedup_skipped_total_total" => false,
            "carbide_dsx_exchange_consumer_alerts_detected_total_total" => false,
        }
    );
    assert!(exposition.contains(
        "# HELP carbide_dsx_exchange_consumer_alerts_detected_total Number of leak alerts detected"
    ));

    assert_eq!(logs.len(), 6, "received and processed must remain silent");
    check_values(
        [
            Check {
                scenario: "metadata queue drop keeps its warning",
                input: 0,
                expect: expected_drop("metadata", "Message queue full, dropping metadata message"),
            },
            Check {
                scenario: "value queue drop keeps its warning",
                input: 1,
                expect: expected_drop("value", "Message queue full, dropping value message"),
            },
            Check {
                scenario: "deduplicated value keeps its trace context",
                input: 2,
                expect: expected_dedup(),
            },
            Check {
                scenario: "rack leak alert keeps its diagnostic context",
                input: 3,
                expect: expected_alert("LeakDetectRack"),
            },
            Check {
                scenario: "rack sensor fault keeps its diagnostic context",
                input: 4,
                expect: expected_alert("LeakSensorFaultRack"),
            },
            Check {
                scenario: "rack tray leak keeps its diagnostic context",
                input: 5,
                expect: expected_alert("LeakDetectRackTray"),
            },
        ],
        |index| observe_log(&logs[index]),
    );
}

/// `HealthReportPersistFailed` exposes the failed rack and moves its zero-label
/// counter from the same `emit`. Keep this check in its own integration-test
/// process: unit tests emit the same Event without `MetricsCapture`, so a
/// shared registry would make the counter delta depend on test order.
#[test]
fn health_report_persist_failed_logs_warn_and_counts() {
    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(HealthReportPersistFailed {
            rack_id: "rack-42".to_string(),
            error: "API call failed: deadline exceeded".to_string(),
        });
    });

    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_eq!(logs[0].message, "Failed to persist rack health report");
    assert_eq!(field(&logs[0], "rack_id"), Some("rack-42"));
    assert_eq!(
        field(&logs[0], "error"),
        Some("API call failed: deadline exceeded")
    );

    assert_eq!(
        metrics.counter_delta(
            "carbide_dsx_exchange_consumer_health_report_persist_failures_total",
            &[],
        ),
        1.0
    );
}
