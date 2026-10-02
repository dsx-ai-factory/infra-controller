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

use std::fmt;
use std::fmt::Display;
use std::time::Duration;

use ::carbide_utils::metrics::SharedMetricsHolder;
use carbide_instrument::Event;
use opentelemetry::metrics::{Counter, Meter};

/// Metrics that are gathered in a single dpa monitor run
#[derive(Clone, Debug)]
pub(super) struct DpaMonitorMetrics {
    /// Start time of metrics gathering
    pub(super) recording_started_at: std::time::Instant,
    pub(super) num_machines_scanned: usize,
    pub(super) num_instances_scanned: usize,
    pub(super) num_dpa_interfaces_scanned: usize,
    pub(super) num_heartbeats_sent: usize,
    pub(super) num_creates: usize,
    pub(super) num_deletes: usize,
}

impl DpaMonitorMetrics {
    pub(super) fn new() -> Self {
        Self {
            recording_started_at: std::time::Instant::now(),
            num_machines_scanned: 0,
            num_instances_scanned: 0,
            num_dpa_interfaces_scanned: 0,
            num_heartbeats_sent: 0,
            num_creates: 0,
            num_deletes: 0,
        }
    }
}

impl Display for DpaMonitorMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{ machines_scanned: {}, instances_scanned: {}, duration: {} }}",
            self.num_machines_scanned,
            self.num_instances_scanned,
            self.recording_started_at.elapsed().as_millis(),
        )
    }
}

/// Stores Metric data shared between the dpa monitor and the OpenTelemetry background task
pub(super) struct MetricHolder {
    instruments: DpaMonitorInstruments,
    last_iteration_metrics: SharedMetricsHolder<DpaMonitorMetrics>,
}

impl MetricHolder {
    pub(super) fn new(meter: Meter, hold_period: Duration) -> Self {
        let last_iteration_metrics = SharedMetricsHolder::with_hold_period(hold_period);
        let instruments = DpaMonitorInstruments::new(meter, last_iteration_metrics.clone());
        instruments.init_counters();
        Self {
            instruments,
            last_iteration_metrics,
        }
    }

    /// Updates the most recent metrics
    pub(super) fn update_metrics(&self, metrics: DpaMonitorMetrics) {
        self.instruments.emit_counters(&metrics);
        self.last_iteration_metrics.update(metrics);
    }
}

/// One DPA monitor pass. Both cases sample the pass duration; only a failure
/// logs, so a clean pass is measured without writing a line.
#[derive(Event)]
#[event(
    event_name = "dpa_monitor_iteration_finished",
    metric_name = "carbide_dpa_monitor_iteration_latency_milliseconds",
    component = "dpa-monitor",
    metric = histogram,
    describe = "Time consumed for one monitor iteration"
)]
pub(crate) enum DpaMonitorIterationFinished {
    /// A clean pass: sampled, never logged.
    #[event(log = off)]
    Succeeded {
        #[observation]
        latency: Duration,
    },

    #[event(log = warn, message = "DPA monitor error")]
    Failed {
        #[observation]
        latency: Duration,
        #[context]
        error: String,
    },
}

/// Instruments that are used by pub struct DpaMonitor
struct DpaMonitorInstruments {
    heartbeats_sent: Counter<u64>,
    creates: Counter<u64>,
    deletes: Counter<u64>,
}

impl DpaMonitorInstruments {
    fn new(meter: Meter, shared_metrics: SharedMetricsHolder<DpaMonitorMetrics>) -> Self {
        let heartbeats_sent = meter
            .u64_counter("carbide_dpa_monitor_heartbeats_sent")
            .with_description("Number of heartbeats sent to DPA interfaces")
            .build();
        let creates = meter
            .u64_counter("carbide_dpa_monitor_creates")
            .with_description("Number of DPA interfaces created")
            .build();
        let deletes = meter
            .u64_counter("carbide_dpa_monitor_deletes")
            .with_description("Number of DPA interfaces deleted")
            .build();

        meter
            .u64_observable_gauge("carbide_dpa_monitor_interfaces_scanned_count")
            .with_description("Number of DPA interfaces scanned in the last monitor iteration")
            .with_callback(move |o| {
                shared_metrics.if_available(|metrics, attrs| {
                    o.observe(metrics.num_dpa_interfaces_scanned as u64, attrs);
                })
            })
            .build();

        Self {
            heartbeats_sent,
            creates,
            deletes,
        }
    }

    fn init_counters(&self) {
        self.heartbeats_sent.add(0, &[]);
        self.creates.add(0, &[]);
        self.deletes.add(0, &[]);
    }

    fn emit_counters(&self, metrics: &DpaMonitorMetrics) {
        self.heartbeats_sent
            .add(metrics.num_heartbeats_sent as u64, &[]);
        self.creates.add(metrics.num_creates as u64, &[]);
        self.deletes.add(metrics.num_deletes as u64, &[]);
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;

    use super::*;

    const EXPOSED_METRIC: &str = "carbide_dpa_monitor_iteration_latency_milliseconds";

    #[test]
    fn dpa_monitor_iteration_results_pair_latency_with_failure_log() {
        struct IterationCase {
            latency: Duration,
            error: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct LogObservation {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            log: Option<LogObservation>,
            histogram_count_delta: u64,
            histogram_sum_delta: ApproxHistogramSum,
        }

        value_scenarios!(
            run = |IterationCase { latency, error }| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    emit(if error.is_empty() {
                        DpaMonitorIterationFinished::Succeeded { latency }
                    } else {
                        DpaMonitorIterationFinished::Failed {
                            latency,
                            error: error.to_string(),
                        }
                    });
                });
                let log = logs.first().map(|log| LogObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                });

                Observation {
                    log_count: logs.len(),
                    log,
                    histogram_count_delta: metrics.histogram_count_delta(EXPOSED_METRIC, &[]),
                    histogram_sum_delta: metrics.histogram_sum_delta(EXPOSED_METRIC, &[]),
                }
            };
            "successful iteration stays silent" {
                IterationCase {
                    latency: Duration::from_millis(125),
                    error: "",
                } => Observation {
                    log_count: 0,
                    log: None,
                    histogram_count_delta: 1,
                    histogram_sum_delta: ApproxHistogramSum(125.0),
                },
            }
            "fractional milliseconds remain precise" {
                IterationCase {
                    latency: Duration::from_micros(125_500),
                    error: "",
                } => Observation {
                    log_count: 0,
                    log: None,
                    histogram_count_delta: 1,
                    histogram_sum_delta: ApproxHistogramSum(125.5),
                },
            }
            "failed iteration retains the warning" {
                IterationCase {
                    latency: Duration::from_millis(375),
                    error: "simulated iteration failure",
                } => Observation {
                    log_count: 1,
                    log: Some(LogObservation {
                        level: tracing::Level::WARN,
                        metadata_name: "dpa_monitor_iteration_finished".to_string(),
                        message: "DPA monitor error".to_string(),
                        event_name: Some("dpa_monitor_iteration_finished".to_string()),
                        metric_name: Some(EXPOSED_METRIC.to_string()),
                        error: Some("simulated iteration failure".to_string()),
                    }),
                    histogram_count_delta: 1,
                    histogram_sum_delta: ApproxHistogramSum(375.0),
                },
            }
        );
    }

    /// The Event replaces the manual histogram without changing its exposed
    /// family name, HELP text, unit suffix, or label-free samples.
    #[test]
    fn dpa_monitor_iteration_histogram_exposition_stays_stable() {
        let metrics = MetricsCapture::start();
        emit(DpaMonitorIterationFinished::Succeeded {
            latency: Duration::from_millis(125),
        });

        let encoded = metrics.render();
        assert!(
            encoded.contains(&format!(
                "# HELP {EXPOSED_METRIC} Time consumed for one monitor iteration\n"
            )),
            "description or exposed family changed:\n{encoded}"
        );
        assert!(
            encoded.contains(&format!("# TYPE {EXPOSED_METRIC} histogram\n")),
            "expected the millisecond family to remain a histogram:\n{encoded}"
        );
        assert!(
            !encoded.contains("carbide_dpa_monitor_iteration_latency_milliseconds_milliseconds"),
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
                "iteration latency must remain label-free: {sample}"
            );
        }
    }
}
