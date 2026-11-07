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

//! Metrics for the DSX Exchange Event Bus MQTT hook.

use carbide_instrument::{Event, LabelValue, MetricFamily};
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;
use tokio::sync::mpsc::WeakSender;

/// The publishing path behind a DSX Exchange Event Bus publish, as the bounded
/// `component` metric label. Each variant renders to the exact value the
/// counter (and the queue-depth gauge) has always reported.
///
/// The set is closed: every construction site in the tree passes one of these
/// three, so the label is a framework `#[label]` rather than a free `&str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub enum PublishComponent {
    /// The BMS DSX Exchange publisher (`carbide-rack`).
    Bms,
    /// The change-driven managed-host state hook (`carbide-api-core`).
    ManagedHost,
    /// The periodic managed-host state republisher (`carbide-api-core`).
    ManagedHostRepublish,
}

/// The outcome of a publish attempt, as the bounded `status` metric label. Each
/// variant renders to the exact value the counter has always reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum PublishStatus {
    /// The message was published within its deadline.
    Ok,
    /// The bounded queue was full, so the message was dropped.
    Overflow,
    /// The publish did not complete before its deadline.
    Timeout,
    /// The broker rejected the publish.
    PublishError,
    /// The message could not be serialized.
    SerializationError,
}

/// The one metric every `DsxEventBus*` Event below records. The family owns
/// the name, kind, description, and label keys, so the eleven Events can no
/// longer drift apart -- an Event that restates any of them is a compile
/// error. `publisher` aliases the metric's frozen `component` label, while the
/// Event-level `component` identifies this crate.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_dsx_event_bus_publish_count_total",
    kind = counter,
    component = "nico-mqtt-common",
    describe = "Number of MQTT publish attempts"
)]
struct DsxEventBusPublishCount {
    #[label(name = "component")]
    publisher: PublishComponent,
    status: PublishStatus,
}

// The Events stay separate rather than merging into one that matches on
// `status`. `publisher` is caller-supplied and `status` is fixed per Event, so
// `(publisher, status)` is all a merged struct could match on -- and
// `DsxEventBusBmsMetadataParseFailed` and
// `DsxEventBusBmsPublicationSerializationFailed` both land on
// `(Bms, SerializationError)` with opposite meanings. Separate types keep each
// path's log level, message, and context.

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_publish_succeeded",
    metric_family = DsxEventBusPublishCount,
    log = debug,
    message = "Published to MQTT"
)]
struct DsxEventBusPublishSucceeded {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    machine_id: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_bms_publish_succeeded",
    metric_family = DsxEventBusPublishCount,
    log = off
)]
struct DsxEventBusBmsPublishSucceeded {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_publish_failed",
    metric_family = DsxEventBusPublishCount,
    log = warn,
    message = "Failed to publish to MQTT"
)]
struct DsxEventBusPublishFailed {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    machine_id: String,
    #[context]
    error: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_bms_publish_failed",
    metric_family = DsxEventBusPublishCount,
    log = warn,
    message = "Failed to publish BMS DSX message"
)]
struct DsxEventBusBmsPublishFailed {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    error: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_publish_timed_out",
    metric_family = DsxEventBusPublishCount,
    log = warn,
    message = "MQTT publish timed out"
)]
struct DsxEventBusPublishTimedOut {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    machine_id: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_bms_publish_timed_out",
    metric_family = DsxEventBusPublishCount,
    log = warn,
    message = "BMS DSX publish timed out"
)]
struct DsxEventBusBmsPublishTimedOut {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_publish_queue_overflowed",
    metric_family = DsxEventBusPublishCount,
    log = warn,
    message = "MQTT state change event dropped (queue full)"
)]
struct DsxEventBusPublishQueueOverflowed {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    machine_id: String,
    #[context]
    error: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_state_change_serialization_failed",
    metric_family = DsxEventBusPublishCount,
    log = error,
    message = "Failed to serialize state change message"
)]
struct DsxEventBusStateChangeSerializationFailed {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    machine_id: String,
    #[context]
    error: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_republish_serialization_failed",
    metric_family = DsxEventBusPublishCount,
    log = error,
    message = "Failed to serialize managed host state for republish"
)]
struct DsxEventBusRepublishSerializationFailed {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    machine_id: String,
    #[context]
    error: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_bms_metadata_parse_failed",
    metric_family = DsxEventBusPublishCount,
    log = warn,
    message = "Failed to parse BMS metadata"
)]
struct DsxEventBusBmsMetadataParseFailed {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    error: String,
}

#[derive(Event)]
#[event(
    event_name = "dsx_event_bus_bms_publication_serialization_failed",
    metric_family = DsxEventBusPublishCount,
    log = warn,
    message = "Failed to serialize BMS DSX publication"
)]
struct DsxEventBusBmsPublicationSerializationFailed {
    #[label]
    publisher: PublishComponent,
    #[label]
    status: PublishStatus,
    #[context]
    topic: String,
    #[context]
    error: String,
}

/// Metrics for the MQTT state change hook.
#[derive(Clone)]
pub struct MqttHookMetrics {
    component: PublishComponent,
}

impl MqttHookMetrics {
    /// Create new metrics instruments from the given meter.
    ///
    /// Uses a weak reference to the sender to observe queue depth without
    /// preventing shutdown (when the sender is dropped, queue depth reports 0).
    pub fn new<T: Send + 'static>(
        meter: &Meter,
        sender: WeakSender<T>,
        component: PublishComponent,
    ) -> Self {
        // Get max_capacity once at construction (upgrade will succeed since sender still exists)
        let max_capacity = sender.upgrade().map(|s| s.max_capacity()).unwrap_or(0);

        // Register observable gauge for queue depth using sender's capacity
        meter
            .u64_observable_gauge("carbide_dsx_event_bus_queue_depth")
            .with_description(
                "Number of state change messages currently queued for MQTT publishing",
            )
            .with_callback(move |observer| {
                let depth = sender
                    .upgrade()
                    .map(|s| max_capacity - s.capacity())
                    .unwrap_or(0);
                observer.observe(
                    depth as u64,
                    &[KeyValue::new("component", component.label_value())],
                );
            })
            .build();

        Self { component }
    }

    /// Create metrics for a publisher that does not buffer messages in a
    /// bounded queue, so no queue-depth gauge is registered. Used by the
    /// periodic state republisher, which publishes directly from its sweep.
    pub fn without_queue_depth(component: PublishComponent) -> Self {
        Self { component }
    }

    /// `record_managed_success` advances the publish counter and retains the
    /// change hook's `DEBUG` record. `topic` and `machine_id` stay log-only.
    pub fn record_managed_success(&self, topic: String, machine_id: String) {
        carbide_instrument::emit(DsxEventBusPublishSucceeded {
            publisher: self.component,
            status: PublishStatus::Ok,
            topic,
            machine_id,
        });
    }

    /// `record_bms_success` advances the counter without adding a success log;
    /// the BMS publisher has no success record to preserve.
    pub fn record_bms_success(&self) {
        carbide_instrument::emit(DsxEventBusBmsPublishSucceeded {
            publisher: self.component,
            status: PublishStatus::Ok,
        });
    }

    /// `record_overflow` keeps a full-queue drop's counter and `WARN` record
    /// together, with per-message detail left off the metric labels.
    pub fn record_overflow(&self, topic: String, machine_id: String, error: String) {
        carbide_instrument::emit(DsxEventBusPublishQueueOverflowed {
            publisher: self.component,
            status: PublishStatus::Overflow,
            topic,
            machine_id,
            error,
        });
    }

    /// `record_managed_timeout` pairs the managed-host timeout counter with its
    /// `WARN` record and log-only correlation fields.
    pub fn record_managed_timeout(&self, topic: String, machine_id: String) {
        carbide_instrument::emit(DsxEventBusPublishTimedOut {
            publisher: self.component,
            status: PublishStatus::Timeout,
            topic,
            machine_id,
        });
    }

    /// `record_bms_timeout` pairs the BMS timeout counter with its `WARN`
    /// record.
    pub fn record_bms_timeout(&self, topic: String) {
        carbide_instrument::emit(DsxEventBusBmsPublishTimedOut {
            publisher: self.component,
            status: PublishStatus::Timeout,
            topic,
        });
    }

    /// `record_managed_publish_error` pairs a broker failure's counter with its
    /// `WARN` record and log-only error context.
    pub fn record_managed_publish_error(&self, topic: String, machine_id: String, error: String) {
        carbide_instrument::emit(DsxEventBusPublishFailed {
            publisher: self.component,
            status: PublishStatus::PublishError,
            topic,
            machine_id,
            error,
        });
    }

    /// `record_bms_publish_error` pairs a BMS broker failure's counter with its
    /// `WARN` record and log-only error context.
    pub fn record_bms_publish_error(&self, topic: String, error: String) {
        carbide_instrument::emit(DsxEventBusBmsPublishFailed {
            publisher: self.component,
            status: PublishStatus::PublishError,
            topic,
            error,
        });
    }

    /// `record_state_change_serialization_error` pairs a change-driven
    /// serialization failure's counter with its `ERROR` record.
    pub fn record_state_change_serialization_error(
        &self,
        topic: String,
        machine_id: String,
        error: String,
    ) {
        carbide_instrument::emit(DsxEventBusStateChangeSerializationFailed {
            publisher: self.component,
            status: PublishStatus::SerializationError,
            topic,
            machine_id,
            error,
        });
    }

    /// `record_republish_serialization_error` pairs the periodic republisher's
    /// counter with its distinct `ERROR` record and `publisher` label.
    pub fn record_republish_serialization_error(
        &self,
        topic: String,
        machine_id: String,
        error: String,
    ) {
        carbide_instrument::emit(DsxEventBusRepublishSerializationFailed {
            publisher: self.component,
            status: PublishStatus::SerializationError,
            topic,
            machine_id,
            error,
        });
    }

    /// `record_bms_metadata_parse_error` retains the BMS parser's `WARN`
    /// record.
    /// The frozen counter groups this with serialization failures under
    /// `status="serialization_error"`.
    pub fn record_bms_metadata_parse_error(&self, topic: String, error: String) {
        carbide_instrument::emit(DsxEventBusBmsMetadataParseFailed {
            publisher: self.component,
            status: PublishStatus::SerializationError,
            topic,
            error,
        });
    }

    /// `record_bms_publication_serialization_error` retains the BMS encoder's
    /// `WARN` record under the same frozen `serialization_error` status.
    pub fn record_bms_publication_serialization_error(&self, topic: String, error: String) {
        carbide_instrument::emit(DsxEventBusBmsPublicationSerializationFailed {
            publisher: self.component,
            status: PublishStatus::SerializationError,
            topic,
            error,
        });
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::LabelValue;
    use carbide_instrument::testing::{CapturedLog, MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;

    use super::{MqttHookMetrics, PublishComponent, PublishStatus};

    const PUBLISH_METRIC: &str = "carbide_dsx_event_bus_publish_count_total";

    fn assert_event_log(
        log: &CapturedLog,
        event_name: &str,
        level: tracing::Level,
        message: &str,
        publisher: &str,
        status: &str,
    ) {
        assert_eq!(log.metadata_name, event_name);
        assert_eq!(log.level, level);
        assert_eq!(log.message, message);
        assert_eq!(log.field("event_name"), Some(event_name));
        assert_eq!(log.field("metric_name"), Some(PUBLISH_METRIC));
        assert_eq!(log.field("publisher"), Some(publisher));
        assert_eq!(log.field("component"), None);
        assert_eq!(log.field("status"), Some(status));
    }

    /// The `status` label values are the metric's contract: each variant renders
    /// to the exact string the publish counter has always reported.
    #[test]
    fn publish_status_renders_expected_label_values() {
        value_scenarios!(run = |status| status.label_value().to_string();
            "publish status label contract" {
                PublishStatus::Ok => "ok".to_string(),
                PublishStatus::Overflow => "overflow".to_string(),
                PublishStatus::Timeout => "timeout".to_string(),
                PublishStatus::PublishError => "publish_error".to_string(),
                PublishStatus::SerializationError => "serialization_error".to_string(),
            }
        );
    }

    /// The `component` label values are the metric's contract: each variant
    /// renders to the exact string the publish counter and queue-depth gauge
    /// have always reported.
    #[test]
    fn publish_component_renders_expected_label_values() {
        value_scenarios!(run = |component| component.label_value().to_string();
            "publish component label contract" {
                PublishComponent::Bms => "bms".to_string(),
                PublishComponent::ManagedHost => "managed_host".to_string(),
                PublishComponent::ManagedHostRepublish => "managed_host_republish".to_string(),
            }
        );
    }

    #[test]
    fn managed_publish_outcomes_log_and_count() {
        let metrics = MetricsCapture::start();
        let hook = MqttHookMetrics::without_queue_depth(PublishComponent::ManagedHost);
        let logs = capture_logs(|| {
            hook.record_managed_success(
                "NICO/v1/machine/host-1/state".to_string(),
                "host-1".to_string(),
            );
            hook.record_overflow(
                "NICO/v1/machine/host-2/state".to_string(),
                "host-2".to_string(),
                "no available capacity".to_string(),
            );
            hook.record_managed_timeout(
                "NICO/v1/machine/host-3/state".to_string(),
                "host-3".to_string(),
            );
            hook.record_managed_publish_error(
                "NICO/v1/machine/host-4/state".to_string(),
                "host-4".to_string(),
                "broker unavailable".to_string(),
            );
            hook.record_state_change_serialization_error(
                "NICO/v1/machine/host-5/state".to_string(),
                "host-5".to_string(),
                "invalid map key".to_string(),
            );
        });

        assert_eq!(logs.len(), 5);
        assert_event_log(
            &logs[0],
            "dsx_event_bus_publish_succeeded",
            tracing::Level::DEBUG,
            "Published to MQTT",
            "managed_host",
            "ok",
        );
        assert_event_log(
            &logs[1],
            "dsx_event_bus_publish_queue_overflowed",
            tracing::Level::WARN,
            "MQTT state change event dropped (queue full)",
            "managed_host",
            "overflow",
        );
        assert_event_log(
            &logs[2],
            "dsx_event_bus_publish_timed_out",
            tracing::Level::WARN,
            "MQTT publish timed out",
            "managed_host",
            "timeout",
        );
        assert_event_log(
            &logs[3],
            "dsx_event_bus_publish_failed",
            tracing::Level::WARN,
            "Failed to publish to MQTT",
            "managed_host",
            "publish_error",
        );
        assert_event_log(
            &logs[4],
            "dsx_event_bus_state_change_serialization_failed",
            tracing::Level::ERROR,
            "Failed to serialize state change message",
            "managed_host",
            "serialization_error",
        );

        for (index, host) in (1..=5).map(|index| (index - 1, format!("host-{index}"))) {
            assert_eq!(logs[index].field("machine_id"), Some(host.as_str()));
            let topic = format!("NICO/v1/machine/{host}/state");
            assert_eq!(logs[index].field("topic"), Some(topic.as_str()));
        }
        assert_eq!(logs[1].field("error"), Some("no available capacity"));
        assert_eq!(logs[3].field("error"), Some("broker unavailable"));
        assert_eq!(logs[4].field("error"), Some("invalid map key"));

        value_scenarios!(run = |status| {
            metrics.counter_delta(
                PUBLISH_METRIC,
                &[("component", "managed_host"), ("status", status)],
            )
        };
            "each managed publish outcome increments its counter" {
                "ok" => 1.0,
                "overflow" => 1.0,
                "timeout" => 1.0,
                "publish_error" => 1.0,
                "serialization_error" => 1.0,
            }
        );
    }

    #[test]
    fn republish_serialization_failure_logs_and_counts() {
        let metrics = MetricsCapture::start();
        let hook = MqttHookMetrics::without_queue_depth(PublishComponent::ManagedHostRepublish);
        let logs = capture_logs(|| {
            hook.record_republish_serialization_error(
                "NICO/v1/machine/host-6/state".to_string(),
                "host-6".to_string(),
                "invalid state".to_string(),
            );
        });

        assert_eq!(logs.len(), 1);
        assert_event_log(
            &logs[0],
            "dsx_event_bus_republish_serialization_failed",
            tracing::Level::ERROR,
            "Failed to serialize managed host state for republish",
            "managed_host_republish",
            "serialization_error",
        );
        assert_eq!(logs[0].field("topic"), Some("NICO/v1/machine/host-6/state"));
        assert_eq!(logs[0].field("machine_id"), Some("host-6"));
        assert_eq!(logs[0].field("error"), Some("invalid state"));

        assert_eq!(
            metrics.counter_delta(
                PUBLISH_METRIC,
                &[
                    ("component", "managed_host_republish"),
                    ("status", "serialization_error"),
                ],
            ),
            1.0,
        );
    }

    #[test]
    fn bms_publish_siblings_preserve_logs_and_accumulate() {
        let metrics = MetricsCapture::start();
        let hook = MqttHookMetrics::without_queue_depth(PublishComponent::Bms);
        let logs = capture_logs(|| {
            hook.record_bms_success();
            hook.record_bms_publish_error(
                "BMS/v1/PUB/Data/rack-1".to_string(),
                "connection lost".to_string(),
            );
            hook.record_bms_timeout("BMS/v1/PUB/Data/rack-2".to_string());
            hook.record_bms_metadata_parse_error(
                "BMS/v1/PUB/Metadata/rack-3".to_string(),
                "missing pointType".to_string(),
            );
            hook.record_bms_publication_serialization_error(
                "BMS/v1/PUB/Data/rack-4".to_string(),
                "non-finite value".to_string(),
            );
        });

        // `record_bms_success` is the metric-only sibling. The remaining calls
        // preserve the four `WARN` records operators already receive.
        assert_eq!(logs.len(), 4);
        assert_event_log(
            &logs[0],
            "dsx_event_bus_bms_publish_failed",
            tracing::Level::WARN,
            "Failed to publish BMS DSX message",
            "bms",
            "publish_error",
        );
        assert_event_log(
            &logs[1],
            "dsx_event_bus_bms_publish_timed_out",
            tracing::Level::WARN,
            "BMS DSX publish timed out",
            "bms",
            "timeout",
        );
        assert_event_log(
            &logs[2],
            "dsx_event_bus_bms_metadata_parse_failed",
            tracing::Level::WARN,
            "Failed to parse BMS metadata",
            "bms",
            "serialization_error",
        );
        assert_event_log(
            &logs[3],
            "dsx_event_bus_bms_publication_serialization_failed",
            tracing::Level::WARN,
            "Failed to serialize BMS DSX publication",
            "bms",
            "serialization_error",
        );
        assert_eq!(logs[0].field("topic"), Some("BMS/v1/PUB/Data/rack-1"));
        assert_eq!(logs[1].field("topic"), Some("BMS/v1/PUB/Data/rack-2"));
        assert_eq!(logs[2].field("topic"), Some("BMS/v1/PUB/Metadata/rack-3"));
        assert_eq!(logs[3].field("topic"), Some("BMS/v1/PUB/Data/rack-4"));
        assert_eq!(logs[0].field("error"), Some("connection lost"));
        assert_eq!(logs[2].field("error"), Some("missing pointType"));
        assert_eq!(logs[3].field("error"), Some("non-finite value"));

        for (status, expected) in [
            ("ok", 1.0),
            ("publish_error", 1.0),
            ("timeout", 1.0),
            // Metadata parsing and publication encoding are distinct Events,
            // but both feed this frozen `component`/`status` series.
            ("serialization_error", 2.0),
        ] {
            assert_eq!(
                metrics.counter_delta(PUBLISH_METRIC, &[("component", "bms"), ("status", status)],),
                expected,
                "status {status}",
            );
        }
    }
}
