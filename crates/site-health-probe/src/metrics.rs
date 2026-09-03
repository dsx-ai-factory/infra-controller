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

//! Implements the framework's [`Sink`] over the shared metrics setup,
//! following the repo's carbide_* conventions: histograms in _milliseconds,
//! counters ending _total, low-cardinality labels. The metric set is
//! documented in the nico-site-health-probe chart README (not in
//! docs/observability/core_metrics.md, which is generated from the
//! Event-derived catalogue this standalone binary is not part of).

use std::time::UNIX_EPOCH;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};

use crate::framework::{Outcome, ProbeResult, Sink};

/// Records probe results as Prometheus series.
pub(crate) struct Metrics {
    duration: Histogram<f64>,
    requests: Counter<u64>,
    up: Gauge<f64>,
    last_run: Gauge<f64>,
}

impl Metrics {
    /// Builds the instrument set on the given meter. Instruments register
    /// under their exposed name minus the suffix the Prometheus exporter
    /// appends itself (`_total` for counters, the unit for unit-bearing
    /// instruments); what lands on `/metrics` is asserted by test.
    pub(crate) fn new(meter: &Meter) -> Self {
        Self {
            duration: meter
                .f64_histogram("carbide_site_health_probe_request_duration")
                .with_unit("ms")
                .with_description(
                    "Duration of synthetic probe requests against NICo APIs, by API surface, \
                     probe, and operation.",
                )
                // Millisecond buckets matching the repo's metrics-endpoint
                // defaults (5ms..10s), so probe latencies read alongside
                // service latencies.
                .with_boundaries(vec![
                    5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0,
                ])
                .build(),
            requests: meter
                .u64_counter("carbide_site_health_probe_requests")
                .with_description(
                    "Synthetic probe runs by API surface, probe, and outcome (success, failure, \
                     timeout).",
                )
                .build(),
            up: meter
                .f64_gauge("carbide_site_health_probe_up")
                .with_description(
                    "1 if the probe's most recent run succeeded, 0 on failure, timeout, or panic \
                     — the gauge operators alert on.",
                )
                .build(),
            last_run: meter
                .f64_gauge("carbide_site_health_probe_last_run_timestamp")
                .with_unit("s")
                .with_description(
                    "Unix time of the probe's most recent completed run; a stale value means the \
                     probe is wedged or stopped.",
                )
                .build(),
        }
    }
}

impl Sink for Metrics {
    fn record(&self, r: &ProbeResult) {
        let api_probe = [KeyValue::new("api", r.api), KeyValue::new("probe", r.probe)];
        for o in &r.observations {
            self.duration.record(
                o.duration.as_secs_f64() * 1000.0,
                &[
                    KeyValue::new("api", r.api),
                    KeyValue::new("probe", r.probe),
                    KeyValue::new("operation", o.operation),
                ],
            );
        }
        self.requests.add(
            1,
            &[
                KeyValue::new("api", r.api),
                KeyValue::new("probe", r.probe),
                KeyValue::new("outcome", r.outcome.as_str()),
            ],
        );
        let up = if r.outcome == Outcome::Success {
            1.0
        } else {
            0.0
        };
        self.up.record(up, &api_probe);
        // Synthetic watchdog results carry no completion time: a wedged run
        // completed nothing, so the timestamp must go stale rather than be
        // refreshed.
        if let Some(at) = r.at {
            let unix = at
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            self.last_run.record(unix, &api_probe);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    struct Harness {
        registry: prometheus::Registry,
        metrics: Metrics,
        // Dropping the provider stops metric collection; hold it.
        _setup: metrics_endpoint::MetricsSetup,
    }

    fn harness() -> Harness {
        let setup = metrics_endpoint::new_metrics_setup("site-health-probe", "test", false)
            .expect("metrics setup");
        Harness {
            registry: setup.registry.clone(),
            metrics: Metrics::new(&setup.meter),
            _setup: setup,
        }
    }

    fn result(outcome: Outcome, at: Option<SystemTime>) -> ProbeResult {
        ProbeResult {
            probe: "grpc_machines",
            api: "nico-api",
            outcome,
            observations: Vec::new(),
            error: None,
            panicked: false,
            at,
        }
    }

    fn exposition(registry: &prometheus::Registry) -> String {
        use prometheus::Encoder;
        let mut buffer = Vec::new();
        prometheus::TextEncoder::new()
            .encode(&registry.gather(), &mut buffer)
            .expect("encode");
        String::from_utf8(buffer).expect("utf-8 exposition")
    }

    /// The value of the series `name{labels...}`, from the text exposition the
    /// scraper actually sees — this pins the exporter-appended name suffixes
    /// as much as the values.
    fn series_value(out: &str, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
        out.lines()
            .filter(|line| line.starts_with(name) && !line.starts_with('#'))
            .find(|line| {
                labels
                    .iter()
                    .all(|(k, v)| line.contains(&format!("{k}=\"{v}\"")))
            })
            .and_then(|line| line.rsplit(' ').next())
            .and_then(|value| value.parse().ok())
    }

    #[test]
    fn record_maps_result_to_exposed_series() {
        let h = harness();
        let mut r = result(Outcome::Success, Some(SystemTime::now()));
        r.observations = vec![
            crate::framework::Observation {
                operation: "find_machine_ids",
                duration: Duration::from_millis(12),
            },
            crate::framework::Observation {
                operation: "find_machines_by_ids",
                duration: Duration::from_millis(30),
            },
        ];
        h.metrics.record(&r);

        let out = exposition(&h.registry);
        let api_probe = [("api", "nico-api"), ("probe", "grpc_machines")];

        // The four EXPOSED names — the exporter appends _total and the unit
        // suffixes; a rename here breaks dashboards, so it fails this test.
        assert_eq!(
            series_value(
                &out,
                "carbide_site_health_probe_requests_total",
                &[api_probe[0], api_probe[1], ("outcome", "success")],
            ),
            Some(1.0),
        );
        assert_eq!(
            series_value(&out, "carbide_site_health_probe_up", &api_probe),
            Some(1.0),
        );
        assert!(
            series_value(
                &out,
                "carbide_site_health_probe_last_run_timestamp_seconds",
                &api_probe,
            )
            .is_some_and(|v| v > 0.0),
        );
        assert_eq!(
            series_value(
                &out,
                "carbide_site_health_probe_request_duration_milliseconds_sum",
                &[
                    api_probe[0],
                    api_probe[1],
                    ("operation", "find_machine_ids")
                ],
            ),
            Some(12.0),
            "durations are recorded in milliseconds, not truncated integers or seconds"
        );
        assert_eq!(
            series_value(
                &out,
                "carbide_site_health_probe_request_duration_milliseconds_count",
                &[
                    api_probe[0],
                    api_probe[1],
                    ("operation", "find_machines_by_ids")
                ],
            ),
            Some(1.0),
        );
        // The explicit boundaries survive the exporter (they are advisory in
        // the OTel API): every configured `le` bucket appears.
        for le in [
            "5", "10", "25", "50", "100", "250", "500", "1000", "2500", "5000", "10000",
        ] {
            assert!(
                series_value(
                    &out,
                    "carbide_site_health_probe_request_duration_milliseconds_bucket",
                    &[("operation", "find_machine_ids"), ("le", le)],
                )
                .is_some(),
                "bucket le={le} is exposed"
            );
        }
    }

    #[test]
    fn up_follows_the_most_recent_outcome() {
        let h = harness();
        let api_probe = [("api", "nico-api"), ("probe", "grpc_machines")];
        let up = |h: &Harness| {
            series_value(
                &exposition(&h.registry),
                "carbide_site_health_probe_up",
                &api_probe,
            )
        };

        h.metrics
            .record(&result(Outcome::Timeout, Some(SystemTime::now())));
        assert_eq!(up(&h), Some(0.0), "timeout marks the probe down");

        let mut panicked = result(Outcome::Failure, Some(SystemTime::now()));
        panicked.panicked = true;
        h.metrics.record(&panicked);
        assert_eq!(up(&h), Some(0.0), "panic marks the probe down");

        h.metrics
            .record(&result(Outcome::Success, Some(SystemTime::now())));
        assert_eq!(up(&h), Some(1.0), "the next success recovers");

        assert_eq!(
            series_value(
                &exposition(&h.registry),
                "carbide_site_health_probe_requests_total",
                &[api_probe[0], api_probe[1], ("outcome", "timeout")],
            ),
            Some(1.0),
            "timeout counts separately from failure"
        );
    }

    #[test]
    fn wedged_result_leaves_last_run_stale() {
        let h = harness();
        let api_probe = [("api", "nico-api"), ("probe", "grpc_machines")];
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        h.metrics.record(&result(Outcome::Success, Some(at)));
        // A synthetic watchdog result: no completion time.
        h.metrics.record(&result(Outcome::Timeout, None));

        assert_eq!(
            series_value(
                &exposition(&h.registry),
                "carbide_site_health_probe_last_run_timestamp_seconds",
                &api_probe,
            ),
            Some(1_700_000_000.0),
            "a run that completed nothing must not refresh the timestamp"
        );
    }
}
