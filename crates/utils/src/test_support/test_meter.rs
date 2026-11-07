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

use opentelemetry::metrics::{Meter, MeterProvider};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use prometheus::{Encoder, TextEncoder};

pub struct TestMeter {
    meter: Meter,
    registry: prometheus::Registry,
    // the meter provider can't be dropped or all metrics are lost
    _meter_provider: SdkMeterProvider,
}

impl TestMeter {
    /// Returns the latest accumulated metrics in prometheus format
    pub fn export_metrics(&self) -> String {
        let mut buffer = vec![];
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap()
    }

    pub fn meter(&self) -> Meter {
        self.meter.clone()
    }

    /// Returns the value of a single metric with a given name
    pub fn formatted_metric(&self, metric_name: &str) -> Option<String> {
        let mut metrics = self.formatted_metrics(metric_name);
        match metrics.len() {
            0 => None,
            1 => metrics.pop(),
            n => panic!(
                "Expected to find a single metric with name \"{metric_name}\", but found {n}. Full metrics:\n{metrics:?}"
            ),
        }
    }

    /// Returns the value of multiple metrics with the given name
    /// This can be used if the metric is duplicated due to attributes
    pub fn formatted_metrics(&self, metric_name: &str) -> Vec<String> {
        let formatted = self.export_metrics();
        let mut result = Vec::new();
        for line in formatted.lines() {
            // Metrics look like "metric_name $value" if without attributes
            // and "metric_name{$attrs} value" if with attributes
            if !line.starts_with(metric_name) {
                continue;
            }
            let line = line.trim_start_matches(metric_name);
            if line.starts_with('{') {
                result.push(line.to_string());
            } else {
                result.push(line.strip_prefix(' ').unwrap_or(line).to_string());
            }
        }
        result.sort();
        result
    }

    /// Returns the value of multiple metrics with the given name in a parsed
    /// format. In case the metric is emitted with attributes, the first element in
    /// the tuple contains the attribute map (e.g. `{attribute1="abc"}`).
    /// The 2nd element in the tuple contains the metric value.
    pub fn parsed_metrics(&self, metric_name: &str) -> Vec<(String, String)> {
        let metric_lines = self.formatted_metrics(metric_name);
        let mut parsed = Vec::new();
        for metric_line in metric_lines.into_iter() {
            // Metrics line look like "$value" if without attributes
            // and "{$attrs} value" if with attributes
            if metric_line.starts_with('{') {
                let end_idx = metric_line.find("} ").unwrap_or_else(|| {
                    panic!("Expected to find end of metric line of {metric_line}")
                });
                let (attribute, amount) = metric_line.split_at(end_idx + 1);
                let amount = &amount[1..];
                parsed.push((attribute.to_string(), amount.to_string()));
            } else {
                parsed.push(("".to_string(), metric_line));
            }
        }
        parsed
    }
}

impl Default for TestMeter {
    /// Builds an OpenTelemetry `Meter` for unit-testing purposes
    fn default() -> Self {
        // Note: This configures metrics bucket between 5.0 and 10000.0, which are best suited
        // for tracking milliseconds
        // See https://github.com/open-telemetry/opentelemetry-rust/blob/495330f63576cfaec2d48946928f3dc3332ba058/opentelemetry-sdk/src/metrics/reader.rs#L155-L158
        let prometheus_registry = prometheus::Registry::new();
        let metrics_exporter = opentelemetry_prometheus::exporter()
            .with_registry(prometheus_registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .unwrap();
        let meter_provider = opentelemetry_sdk::metrics::MeterProviderBuilder::default()
            .with_reader(metrics_exporter)
            .build();

        TestMeter {
            meter: meter_provider.meter("carbide-api"),
            registry: prometheus_registry,
            _meter_provider: meter_provider,
        }
    }
}

#[cfg(test)]
mod test {
    use opentelemetry::KeyValue;

    use super::TestMeter;

    #[test]
    fn test_test_meter() {
        let test_meter = TestMeter::default();

        let meter = test_meter.meter();

        let a = meter.u64_counter("a").build();
        let b = meter.u64_counter("b").build();
        let c = meter.u64_histogram("c").with_unit("s").build();

        a.add(123, &[]);
        let attrs = &[KeyValue::new("attr1", "abc"), KeyValue::new("attr2", "def")];
        b.add(543, attrs);
        c.record(20, &[]);

        assert_eq!(test_meter.formatted_metric("a_total").unwrap(), "123");
        assert_eq!(
            test_meter.formatted_metrics("a_total"),
            vec!["123".to_string()]
        );
        assert_eq!(
            test_meter.formatted_metrics("b_total"),
            vec!["{attr1=\"abc\",attr2=\"def\"} 543".to_string()]
        );
        assert_eq!(
            test_meter.formatted_metrics("c_seconds_count"),
            vec!["1".to_string()]
        );
        assert!(
            test_meter
                .formatted_metrics("c_seconds_bucket")
                .contains(&"{le=\"100\"} 1".to_string())
        );

        assert_eq!(
            test_meter.parsed_metrics("a_total"),
            vec![("".to_string(), "123".to_string())]
        );
        assert_eq!(
            test_meter.parsed_metrics("b_total"),
            vec![(
                "{attr1=\"abc\",attr2=\"def\"}".to_string(),
                "543".to_string()
            )]
        );
        assert_eq!(
            test_meter.parsed_metrics("c_seconds_count"),
            vec![("".to_string(), "1".to_string())]
        );
        let c_buckets = test_meter.parsed_metrics("c_seconds_bucket");
        assert!(c_buckets.contains(&("{le=\"+Inf\"}".to_string(), "1".to_string())));
        assert!(c_buckets.contains(&("{le=\"50\"}".to_string(), "1".to_string())));
        assert!(c_buckets.contains(&("{le=\"10\"}".to_string(), "0".to_string())));
        assert!(c_buckets.contains(&("{le=\"0\"}".to_string(), "0".to_string())));
    }
}
