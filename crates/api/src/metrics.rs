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

use carbide_api_core::bootstrap::ApiMetricsEmitter;
use carbide_metrics_utils::OtelView;
use opentelemetry::metrics::{Meter, MeterProvider};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_semantic_conventions as semcov;
use spancounter::SpanCountReader;

#[derive(Debug, Clone)]
pub(crate) struct Metrics {
    pub registry: prometheus::Registry,
    pub meter: Meter,
    // Need to retain this, if it's dropped, metrics are not held
    pub _meter_provider: SdkMeterProvider,
}

pub(crate) fn setup_metrics(spancount_reader: Option<SpanCountReader>) -> eyre::Result<Metrics> {
    // Histograms without a matching view use OpenTelemetry's default boundaries.
    // Unit-specific and count-oriented histograms register explicit views below.
    // See https://github.com/open-telemetry/opentelemetry-rust/blob/495330f63576cfaec2d48946928f3dc3332ba058/opentelemetry-sdk/src/metrics/reader.rs#L155-L158
    use opentelemetry::KeyValue;

    let service_telemetry_attributes = opentelemetry_sdk::Resource::builder()
        .with_attributes(vec![
            KeyValue::new(semcov::resource::SERVICE_NAME, "carbide-api"),
            KeyValue::new(semcov::resource::SERVICE_NAMESPACE, "forge-system"),
        ])
        .build();
    let registry = prometheus::Registry::new();
    let exporter = opentelemetry_prometheus::exporter()
        .with_registry(registry.clone())
        .without_scope_info()
        .without_target_info()
        .build()?;
    let meter_provider = opentelemetry_sdk::metrics::MeterProviderBuilder::default()
        .with_reader(exporter)
        .with_resource(service_telemetry_attributes)
        .with_view(admission_duration_histogram_view()?)
        .with_view(time_in_state_histogram_view()?)
        .with_view(retry_histogram_view("*_attempts_*")?)
        .with_view(retry_histogram_view("*_retries_*")?)
        .with_view(ApiMetricsEmitter::machine_reboot_duration_view()?)
        .with_view(carbide_site_explorer::site_explorer_latency_histogram_view(
            "carbide_site_explorer_*_latency",
        )?)
        .with_view(carbide_site_explorer::site_explorer_latency_histogram_view(
            "carbide_endpoint_exploration_duration",
        )?)
        .with_view(carbide_site_explorer::site_explorer_latency_histogram_view(
            "carbide_endpoint_exploration_step_latency",
        )?)
        .build();
    // After this call `global::meter()` will be available
    opentelemetry::global::set_meter_provider(meter_provider.clone());
    let meter = meter_provider.meter("carbide-api");

    register_spancount_gauge(&meter, spancount_reader);
    register_tokio_runtime_metrics(&meter, tokio::runtime::Handle::current());
    // Counts are process-global, so this also exposes an embedding host's layer.
    carbide_instrument::log_events::register(&meter);
    forge_http_connector::connector::register_global_metrics(&meter);

    Ok(Metrics {
        registry,
        meter,
        _meter_provider: meter_provider,
    })
}

fn admission_duration_histogram_view() -> carbide_metrics_utils::Result<OtelView> {
    carbide_metrics_utils::new_view(
        "carbide_api_admission_*_duration",
        Some(opentelemetry_sdk::metrics::InstrumentKind::Histogram),
        opentelemetry_sdk::metrics::Aggregation::ExplicitBucketHistogram {
            boundaries: vec![
                0.0, 0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5,
                10.0,
            ],
            record_min_max: true,
        },
    )
}

/// Configures a View for the state controllers' `*_time_in_state` histograms, which
/// record seconds. Objects can wait hours or days in one state, beyond the last default
/// boundary of 10000. The default boundaries are kept so existing `le` series remain,
/// with larger ones appended up to 7 days.
fn time_in_state_histogram_view() -> carbide_metrics_utils::Result<OtelView> {
    carbide_metrics_utils::new_view(
        "carbide_*_time_in_state",
        Some(opentelemetry_sdk::metrics::InstrumentKind::Histogram),
        opentelemetry_sdk::metrics::Aggregation::ExplicitBucketHistogram {
            boundaries: vec![
                0.0, 5.0, 10.0, 25.0, 50.0, 75.0, 100.0, 250.0, 500.0, 750.0, 1000.0, 2500.0,
                5000.0, 7500.0, 10000.0, 14400.0, 28800.0, 86400.0, 259200.0, 604800.0,
            ],
            record_min_max: true,
        },
    )
}

/// Configures a View for Histograms that describe retries or attempts for operations
/// The view reconfigures the histogram to use a small set of buckets that track
/// the exact amount of retry attempts up to 3, and 2 additional buckets up to 10.
/// This is more useful than the default histogram range where the lowest sets of
/// buckets are 0, 5, 10, 25
fn retry_histogram_view(name_filter: &'static str) -> carbide_metrics_utils::Result<OtelView> {
    carbide_metrics_utils::new_view(
        name_filter,
        Some(opentelemetry_sdk::metrics::InstrumentKind::Histogram),
        opentelemetry_sdk::metrics::Aggregation::ExplicitBucketHistogram {
            boundaries: vec![0.0, 1.0, 2.0, 3.0, 5.0, 10.0],
            record_min_max: true,
        },
    )
}

fn register_spancount_gauge(meter: &Meter, spancount_reader: Option<SpanCountReader>) {
    meter
        .u64_observable_gauge("carbide_api_tracing_spans_open")
        .with_description("Number of open logging/tracing spans")
        .with_callback(move |observer| {
            let open_spans = spancount_reader
                .as_ref()
                .map_or(0, SpanCountReader::open_spans);
            observer.observe(open_spans as u64, &[]);
        })
        .build();
}

/// Exports the runtime's worker count and each worker's cumulative busy time.
fn register_tokio_runtime_metrics(meter: &Meter, runtime: tokio::runtime::Handle) {
    use opentelemetry::KeyValue;

    let metrics = runtime.metrics();
    meter
        .u64_observable_gauge("carbide_tokio_runtime_workers")
        .with_description("Number of worker threads in the Tokio runtime")
        .with_callback({
            let metrics = metrics.clone();
            move |observer| observer.observe(metrics.num_workers() as u64, &[])
        })
        .build();
    meter
        .f64_observable_counter("carbide_tokio_worker_busy_seconds")
        .with_description(
            "Number of seconds each Tokio runtime worker thread has spent running tasks",
        )
        .with_callback(move |observer| {
            for worker in 0..metrics.num_workers() {
                observer.observe(
                    metrics.worker_total_busy_duration(worker).as_secs_f64(),
                    &[KeyValue::new("worker", worker as i64)],
                );
            }
        })
        .build();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_test_support::{Check, check_values};
    use carbide_utils::test_support::test_meter::TestMeter;
    use opentelemetry::KeyValue;
    use prometheus::{Encoder, TextEncoder};

    use super::*;

    /// The worker count and one busy-time series per worker are exported.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tokio_runtime_metrics_cover_every_worker() {
        let test_meter = TestMeter::default();
        register_tokio_runtime_metrics(&test_meter.meter(), tokio::runtime::Handle::current());

        assert_eq!(
            test_meter
                .formatted_metric("carbide_tokio_runtime_workers")
                .as_deref(),
            Some("2")
        );
        let busy_series: Vec<String> = test_meter
            .parsed_metrics("carbide_tokio_worker_busy_seconds_total")
            .into_iter()
            .map(|(labels, _value)| labels)
            .collect();
        assert_eq!(busy_series, [r#"{worker="0"}"#, r#"{worker="1"}"#]);
    }

    /// This test mostly mimics the test setup above and checks whether
    /// the prometheus opentelemetry stack will only report the most recent
    /// values for gauges and not cached values that are not important anymore
    #[test]
    fn gauge_aggregation_reports_only_current_values() {
        let registry = prometheus::Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .unwrap();
        let provider = opentelemetry_sdk::metrics::MeterProviderBuilder::default()
            .with_reader(exporter)
            .with_view(admission_duration_histogram_view().unwrap())
            .with_view(retry_histogram_view("*_attempts_*").unwrap())
            .with_view(retry_histogram_view("*_retries_*").unwrap())
            .with_view(ApiMetricsEmitter::machine_reboot_duration_view().unwrap())
            .with_view(
                carbide_site_explorer::site_explorer_latency_histogram_view(
                    "carbide_site_explorer_*_latency",
                )
                .unwrap(),
            )
            .with_view(
                carbide_site_explorer::site_explorer_latency_histogram_view(
                    "carbide_endpoint_exploration_duration",
                )
                .unwrap(),
            )
            .with_view(
                carbide_site_explorer::site_explorer_latency_histogram_view(
                    "carbide_endpoint_exploration_step_latency",
                )
                .unwrap(),
            )
            .build();

        let state = KeyValue::new("state", "mystate");
        let even = vec![state.clone(), KeyValue::new("error", "ErrA")];
        let odd = vec![state.clone(), KeyValue::new("error", "ErrB")];
        let every_third = vec![state, KeyValue::new("error", "ErrC")];
        let counter = Arc::new(AtomicUsize::new(0));
        provider
            .meter("myservice")
            .u64_observable_gauge("mygauge")
            .with_callback(move |observer| {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                println!("Collection {count}");
                if count.is_multiple_of(2) {
                    observer.observe(1, &even);
                } else {
                    observer.observe(1, &odd);
                }
                if count % 3 == 1 {
                    observer.observe(1, &every_third);
                }
            })
            .build();

        for index in 0..10 {
            let mut buffer = vec![];
            TextEncoder::new()
                .encode(&registry.gather(), &mut buffer)
                .unwrap();
            let encoded = String::from_utf8(buffer).unwrap();
            if index % 2 == 0 {
                assert!(encoded.contains(r#"mygauge{error="ErrA",state="mystate"} 1"#));
                assert!(!encoded.contains(r#"mygauge{error="ErrB",state="mystate"} 1"#));
            } else {
                assert!(encoded.contains(r#"mygauge{error="ErrB",state="mystate"} 1"#));
                assert!(!encoded.contains(r#"mygauge{error="ErrA",state="mystate"} 1"#));
            }
            if index % 3 == 1 {
                assert!(encoded.contains(r#"mygauge{error="ErrC",state="mystate"} 1"#));
            } else {
                assert!(!encoded.contains(r#"mygauge{error="ErrC",state="mystate"} 1"#));
            }
        }
    }

    /// Exercise production view registration and Prometheus export for slow steps.
    #[tokio::test]
    async fn endpoint_exploration_step_latency_uses_extended_buckets() {
        let metrics = setup_metrics(None).unwrap();
        metrics
            .meter
            .f64_histogram("carbide_endpoint_exploration_step_latency")
            .with_unit("ms")
            .build()
            .record(30_000.0, &[KeyValue::new("step", "redfish_explore")]);

        let mut buffer = vec![];
        TextEncoder::new()
            .encode(&metrics.registry.gather(), &mut buffer)
            .unwrap();
        let encoded = String::from_utf8(buffer).unwrap();
        check_values(
            [
                r#"bucket{step="redfish_explore",le="10000"} 0"#,
                r#"bucket{step="redfish_explore",le="30000"} 1"#,
                r#"bucket{step="redfish_explore",le="3600000"} 1"#,
                r#"bucket{step="redfish_explore",le="+Inf"} 1"#,
                r#"count{step="redfish_explore"} 1"#,
                r#"sum{step="redfish_explore"} 30000"#,
            ]
            .map(|sample| Check {
                scenario: sample,
                input: sample,
                expect: true,
            }),
            |sample| {
                let expected =
                    format!("carbide_endpoint_exploration_step_latency_milliseconds_{sample}");
                encoded.lines().any(|line| line == expected)
            },
        );
    }

    #[test]
    fn admission_duration_histograms_use_seconds_buckets() {
        let expected_buckets = [
            (0.0, 0),
            (0.005, 0),
            (0.01, 0),
            (0.025, 1),
            (0.05, 1),
            (0.075, 1),
            (0.1, 1),
            (0.25, 1),
            (0.5, 1),
            (0.75, 1),
            (1.0, 1),
            (2.5, 1),
            (5.0, 1),
            (7.5, 1),
            (10.0, 1),
            (f64::INFINITY, 1),
        ];
        check_values(
            [
                "carbide_api_admission_handler_execution_duration",
                "carbide_api_admission_pending_wait_duration",
            ]
            .map(|name| Check {
                scenario: name,
                input: name,
                expect: expected_buckets.to_vec(),
            }),
            |name| {
                let registry = prometheus::Registry::new();
                let exporter = opentelemetry_prometheus::exporter()
                    .with_registry(registry.clone())
                    .without_scope_info()
                    .without_target_info()
                    .build()
                    .unwrap();
                let provider = opentelemetry_sdk::metrics::MeterProviderBuilder::default()
                    .with_reader(exporter)
                    .with_view(admission_duration_histogram_view().unwrap())
                    .build();
                provider
                    .meter("test")
                    .f64_histogram(name)
                    .with_unit("s")
                    .build()
                    .record(0.020, &[]);

                let families = registry.gather();
                let exported_name = format!("{name}_seconds");
                let family = families
                    .iter()
                    .find(|family| family.name() == exported_name)
                    .unwrap_or_else(|| panic!("{name}: missing {exported_name} histogram"));
                assert_eq!(
                    family.get_field_type(),
                    prometheus::proto::MetricType::HISTOGRAM,
                    "{name}: metric type"
                );
                assert_eq!(family.get_metric().len(), 1, "{name}: series count");
                let histogram = family.get_metric()[0].get_histogram();
                assert_eq!(histogram.get_sample_count(), 1, "{name}: sample count");
                assert!(
                    (histogram.get_sample_sum() - 0.020).abs() < 1e-12,
                    "{name}: expected sum 0.020 seconds, got {}",
                    histogram.get_sample_sum()
                );
                assert!(
                    histogram
                        .get_bucket()
                        .iter()
                        .all(|bucket| bucket.upper_bound() != 25.0),
                    "{name}: unexpected finite 25-second bucket"
                );

                // The exporter gathers finite buckets; TextEncoder adds +Inf from the count.
                let mut buffer = vec![];
                TextEncoder::new().encode(&families, &mut buffer).unwrap();
                let encoded = String::from_utf8(buffer).unwrap();
                let infinity_sample = format!("{exported_name}_bucket{{le=\"+Inf\"}}");
                let infinity_counts: Vec<u64> = encoded
                    .lines()
                    .filter_map(|line| {
                        let (sample, value) = line.split_once(' ')?;
                        (sample == infinity_sample).then(|| value.parse().unwrap())
                    })
                    .collect();
                assert_eq!(infinity_counts.len(), 1, "{name}: +Inf bucket count");

                histogram
                    .get_bucket()
                    .iter()
                    .map(|bucket| (bucket.upper_bound(), bucket.cumulative_count()))
                    .chain([(f64::INFINITY, infinity_counts[0])])
                    .collect::<Vec<_>>()
            },
        );
    }

    #[test]
    fn machine_reboot_duration_histogram_resolves_long_reboots() {
        let registry = prometheus::Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .unwrap();
        let provider = opentelemetry_sdk::metrics::MeterProviderBuilder::default()
            .with_reader(exporter)
            .with_view(ApiMetricsEmitter::machine_reboot_duration_view().unwrap())
            .build();
        let emitter = ApiMetricsEmitter::new(&provider.meter("test"));
        for duration in [180, 7200, 172800] {
            emitter.record_machine_reboot_duration(
                duration,
                "test-product".to_string(),
                "test-vendor".to_string(),
                "Reboot".to_string(),
            );
        }

        let families = registry.gather();
        let family = families
            .iter()
            .find(|family| family.name() == "carbide_machine_reboot_duration_seconds")
            .unwrap();
        assert_eq!(family.get_metric().len(), 1);
        let metric = &family.get_metric()[0];
        let labels: Vec<_> = metric
            .get_label()
            .iter()
            .map(|label| (label.name(), label.value()))
            .collect();
        assert_eq!(
            labels,
            [
                ("product_name", "test-product"),
                ("reboot_mode", "Reboot"),
                ("vendor", "test-vendor"),
            ]
        );
        let histogram = metric.get_histogram();
        assert_eq!(histogram.get_sample_count(), 3);
        assert_eq!(histogram.get_sample_sum(), 180180.0);
        let buckets: Vec<_> = histogram
            .get_bucket()
            .iter()
            .map(|bucket| (bucket.upper_bound(), bucket.cumulative_count()))
            .collect();
        assert_eq!(
            buckets,
            [
                (180.0, 1),
                (300.0, 1),
                (600.0, 1),
                (900.0, 1),
                (1800.0, 1),
                (3600.0, 1),
                (7200.0, 2),
                (14400.0, 2),
                (28800.0, 2),
                (43200.0, 2),
                (86400.0, 2),
                (172800.0, 3),
            ]
        );
    }

    #[test]
    fn time_in_state_histograms_resolve_multi_day_dwells() {
        let registry = prometheus::Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .unwrap();
        let provider = opentelemetry_sdk::metrics::MeterProviderBuilder::default()
            .with_reader(exporter)
            .with_view(time_in_state_histogram_view().unwrap())
            .build();
        // Two days, past the last default boundary of 10000.
        provider
            .meter("test")
            .f64_histogram("carbide_machines_time_in_state")
            .with_unit("s")
            .build()
            .record(172800.0, &[]);

        let mut buffer = vec![];
        TextEncoder::new()
            .encode(&registry.gather(), &mut buffer)
            .unwrap();
        let encoded = String::from_utf8(buffer).unwrap();

        assert!(encoded.contains("carbide_machines_time_in_state_seconds_bucket{le=\"10000\"} 0"));
        assert!(encoded.contains("carbide_machines_time_in_state_seconds_bucket{le=\"86400\"} 0"));
        assert!(encoded.contains("carbide_machines_time_in_state_seconds_bucket{le=\"259200\"} 1"));
    }
}
