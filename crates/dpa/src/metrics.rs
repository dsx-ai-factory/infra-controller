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

use std::time::Duration;

use carbide_instrument::{Event, MetricFamily};

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_dpa_monitor_operations_latency_milliseconds",
    kind = histogram,
    component = "dpa-monitor",
    describe = "Time consumed for one operation"
)]
pub(crate) struct DpaMonitorOperationsLatency {}

// The accepted and failed paths keep separate records because they carry
// different context. Both feed the same label-free histogram, so a command
// still contributes exactly one latency sample whichever path it takes.
/// `DpaCommandSent` means `MqtteaClient::send_message` accepted the command.
/// That call queues the MQTT publish; it does not promise broker delivery or
/// a later acknowledgement from the DPA.
#[derive(Event)]
#[event(
    event_name = "dpa_command_sent",
    metric_family = DpaMonitorOperationsLatency,
    log = info,
    message = "sent DPA command"
)]
pub(crate) struct DpaCommandSent {
    #[observation]
    pub latency: Duration,
    #[context(value)]
    pub revision: String,
    #[context(value)]
    pub vni: i64,
}

/// `DpaCommandSendFailed` retains the error, payload, and topic from the
/// existing `ERROR` record when `MqtteaClient::send_message` rejects a
/// command.
#[derive(Event)]
#[event(
    event_name = "dpa_command_send_failed",
    metric_family = DpaMonitorOperationsLatency,
    log = error,
    message = "failed to send DPA command"
)]
pub(crate) struct DpaCommandSendFailed {
    #[observation]
    pub latency: Duration,
    #[context]
    pub error: String,
    #[context]
    pub payload: String,
    #[context]
    pub topic: String,
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{
        ApproxHistogramSum, CapturedFieldKind, MetricsCapture, capture_logs,
    };
    use carbide_test_support::value_scenarios;

    use super::*;

    const EXPOSED_METRIC: &str = "carbide_dpa_monitor_operations_latency_milliseconds";

    enum CommandCase {
        Sent,
        Failed,
    }

    #[derive(Debug, PartialEq)]
    struct LogObservation {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        revision: Option<String>,
        revision_kind: Option<CapturedFieldKind>,
        vni: Option<String>,
        vni_kind: Option<CapturedFieldKind>,
        error: Option<String>,
        error_kind: Option<CapturedFieldKind>,
        payload: Option<String>,
        payload_kind: Option<CapturedFieldKind>,
        topic: Option<String>,
        topic_kind: Option<CapturedFieldKind>,
    }

    #[derive(Debug, PartialEq)]
    struct Observation {
        log_count: usize,
        log: Option<LogObservation>,
        histogram_count_delta: u64,
        histogram_sum_delta: ApproxHistogramSum,
    }

    #[test]
    fn dpa_command_results_pair_their_log_with_latency() {
        value_scenarios!(
            run = |case| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| match case {
                    CommandCase::Sent => emit(DpaCommandSent {
                        latency: Duration::from_micros(12_500),
                        revision: "revision-42".to_string(),
                        vni: 901,
                    }),
                    CommandCase::Failed => emit(DpaCommandSendFailed {
                        latency: Duration::from_millis(375),
                        error: "UnregisteredType(\"SetVni\")".to_string(),
                        payload: "SetVni { metadata: Some(...) }".to_string(),
                        topic: "dpa/command/aabbccddeeff/SetVni".to_string(),
                    }),
                });
                let log = logs.first().map(|log| LogObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    revision: log.field("revision").map(str::to_string),
                    revision_kind: log.field_kind("revision"),
                    vni: log.field("vni").map(str::to_string),
                    vni_kind: log.field_kind("vni"),
                    error: log.field("error").map(str::to_string),
                    error_kind: log.field_kind("error"),
                    payload: log.field("payload").map(str::to_string),
                    payload_kind: log.field_kind("payload"),
                    topic: log.field("topic").map(str::to_string),
                    topic_kind: log.field_kind("topic"),
                });

                Observation {
                    log_count: logs.len(),
                    log,
                    histogram_count_delta: metrics.histogram_count_delta(EXPOSED_METRIC, &[]),
                    histogram_sum_delta: metrics.histogram_sum_delta(EXPOSED_METRIC, &[]),
                }
            };
            "MQTT queue accepted the command" {
                CommandCase::Sent => Observation {
                    log_count: 1,
                    log: Some(LogObservation {
                        level: tracing::Level::INFO,
                        metadata_name: "dpa_command_sent".to_string(),
                        message: "sent DPA command".to_string(),
                        event_name: Some("dpa_command_sent".to_string()),
                        metric_name: Some(EXPOSED_METRIC.to_string()),
                        revision: Some("revision-42".to_string()),
                        revision_kind: Some(CapturedFieldKind::String),
                        vni: Some("901".to_string()),
                        vni_kind: Some(CapturedFieldKind::I64),
                        error: None,
                        error_kind: None,
                        payload: None,
                        payload_kind: None,
                        topic: None,
                        topic_kind: None,
                    }),
                    histogram_count_delta: 1,
                    histogram_sum_delta: ApproxHistogramSum(12.5),
                },
            }
            "MQTT queue rejected the command" {
                CommandCase::Failed => Observation {
                    log_count: 1,
                    log: Some(LogObservation {
                        level: tracing::Level::ERROR,
                        metadata_name: "dpa_command_send_failed".to_string(),
                        message: "failed to send DPA command".to_string(),
                        event_name: Some("dpa_command_send_failed".to_string()),
                        metric_name: Some(EXPOSED_METRIC.to_string()),
                        revision: None,
                        revision_kind: None,
                        vni: None,
                        vni_kind: None,
                        error: Some("UnregisteredType(\"SetVni\")".to_string()),
                        error_kind: Some(CapturedFieldKind::Debug),
                        payload: Some("SetVni { metadata: Some(...) }".to_string()),
                        payload_kind: Some(CapturedFieldKind::Debug),
                        topic: Some("dpa/command/aabbccddeeff/SetVni".to_string()),
                        topic_kind: Some(CapturedFieldKind::Debug),
                    }),
                    histogram_count_delta: 1,
                    histogram_sum_delta: ApproxHistogramSum(375.0),
                },
            }
        );
    }

    /// The Event pair exposes one shared, label-free millisecond histogram for
    /// both command results.
    #[test]
    fn dpa_command_histogram_exposition_stays_stable() {
        let metrics = MetricsCapture::start();
        emit(DpaCommandSent {
            latency: Duration::from_millis(125),
            revision: "revision-42".to_string(),
            vni: 901,
        });

        let encoded = metrics.render();
        assert!(
            encoded.contains(&format!(
                "# HELP {EXPOSED_METRIC} Time consumed for one operation\n"
            )),
            "description or exposed family changed:\n{encoded}"
        );
        assert!(
            encoded.contains(&format!("# TYPE {EXPOSED_METRIC} histogram\n")),
            "expected the millisecond family to remain a histogram:\n{encoded}"
        );
        assert!(
            !encoded.contains("carbide_dpa_monitor_operations_latency_milliseconds_milliseconds"),
            "the unit suffix must be applied exactly once:\n{encoded}"
        );
        for suffix in ["count", "sum"] {
            let prefix = format!("{EXPOSED_METRIC}_{suffix} ");
            let sample = encoded
                .lines()
                .find(|line| line.starts_with(&prefix))
                .unwrap_or_else(|| panic!("missing {prefix} sample:\n{encoded}"));
            assert!(
                !sample.contains('{'),
                "DPA command latency must remain label-free: {sample}"
            );
        }
    }
}
