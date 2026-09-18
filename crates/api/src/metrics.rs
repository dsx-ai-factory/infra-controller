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
        .with_view(retry_histogram_view("*_attempts_*")?)
        .with_view(retry_histogram_view("*_retries_*")?)
        .with_view(ApiMetricsEmitter::machine_reboot_duration_view()?)
        .with_view(carbide_site_explorer::site_explorer_latency_histogram_view(
            "carbide_site_explorer_*_latency",
        )?)
        .with_view(carbide_site_explorer::site_explorer_latency_histogram_view(
            "carbide_endpoint_exploration_duration",
        )?)
        .build();
    // After this call `global::meter()` will be available
    opentelemetry::global::set_meter_provider(meter_provider.clone());
    let meter = meter_provider.meter("carbide-api");

    register_spancount_gauge(&meter, spancount_reader);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use opentelemetry::KeyValue;
    use prometheus::{Encoder, TextEncoder};

    use super::*;

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

    #[test]
    fn admission_duration_histograms_use_seconds_buckets() {
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
            .f64_histogram("carbide_api_admission_handler_execution_duration")
            .with_unit("s")
            .build()
            .record(0.02, &[]);

        let mut buffer = vec![];
        TextEncoder::new()
            .encode(&registry.gather(), &mut buffer)
            .unwrap();
        let encoded = String::from_utf8(buffer).unwrap();

        assert!(encoded.contains(
            "carbide_api_admission_handler_execution_duration_seconds_bucket{le=\"0.005\"} 0"
        ));
        assert!(encoded.contains(
            "carbide_api_admission_handler_execution_duration_seconds_bucket{le=\"0.025\"} 1"
        ));
        assert!(!encoded.contains(
            "carbide_api_admission_handler_execution_duration_seconds_bucket{le=\"25\"}"
        ));
    }
}
