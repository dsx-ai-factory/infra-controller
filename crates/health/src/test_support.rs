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

//! `test_support` supplies hardware metric fixtures to catalogue integration tests.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::Arc;

use crate::HealthError;
use crate::collectors::catalogue_samples;
use crate::config::MetricsConfig;
use crate::endpoint::BmcAddr;
use crate::metrics::MetricsManager;
use crate::otlp::convert::build_metrics_export_request;
use crate::otlp::metrics::metric;
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample, PrometheusSink};

/// `hardware_metric_exposition` exports source fixtures through `PrometheusSink`
/// and checks Prometheus and OTLP metadata separately.
/// The returned text feeds the generated catalogue in `test_integration`.
/// Redfish decoding and exporter failures return `HealthError`. Unexpected
/// source families or exporter metadata panic.
pub fn hardware_metric_exposition() -> Result<String, HealthError> {
    let prefix = MetricsConfig::default().prefix;
    let manager = Arc::new(MetricsManager::new(&prefix)?);
    let sink = PrometheusSink::new(manager.clone(), &prefix)?;
    let context = EventContext {
        endpoint_key: "catalogue".to_string(),
        addr: BmcAddr {
            ip: Ipv4Addr::LOCALHOST.into(),
            port: None,
            mac: None,
        },
        collector_type: "metrics_collector",
        metadata: None,
        rack_id: None,
        labels: Default::default(),
    };

    let samples = catalogue_samples()?
        .into_iter()
        .map(|sample| (context.clone(), sample))
        .collect::<Vec<_>>();
    for (context, sample) in &samples {
        sink.try_handle_event(context, &CollectorEvent::Metric(Box::new(sample.clone())))?;
    }
    let name = |sample: &MetricSample| {
        format!(
            "{prefix}_{}_{}_{}",
            sample.name, sample.metric_type, sample.unit
        )
    };
    let source_names = samples
        .iter()
        .map(|(_, sample)| name(sample))
        .collect::<BTreeSet<_>>();
    let expected_names = EXPECTED_SOURCE_NAMES
        .iter()
        .map(|name| format!("{prefix}_{name}"))
        .collect::<BTreeSet<_>>();
    // Catalogue generation retains old rows. Check the fresh source inventory
    // first so an existing row cannot hide a missing projection or sample.
    assert_eq!(source_names, expected_names);
    assert_eq!(samples.len(), EXPECTED_SOURCE_NAMES.len());

    let exposition = manager.export_telemetry()?;
    let request = build_metrics_export_request(&samples, 200_000_000_000, &prefix);
    let otlp_metrics = request
        .resource_metrics
        .iter()
        .flat_map(|resource| &resource.scope_metrics)
        .flat_map(|scope| &scope.metrics)
        .map(|metric| (metric.name.as_str(), metric))
        .collect::<BTreeMap<_, _>>();
    let expected_metric_count = samples.len();

    for (_, sample) in samples.iter() {
        let legacy_name = name(sample);
        assert_prometheus_family(
            &exposition,
            &legacy_name,
            "gauge",
            "Metrics forwarded through sink pipeline",
        );
        let legacy = otlp_metrics[legacy_name.as_str()];
        assert_eq!(legacy.description, "", "{legacy_name}");
        assert_eq!(legacy.unit, sample.unit, "{legacy_name}");
        let Some(metric::Data::Gauge(gauge)) = &legacy.data else {
            panic!("{legacy_name} must remain an OTLP gauge");
        };
        assert_eq!(gauge.data_points.len(), 1, "{legacy_name}");
    }

    assert_eq!(otlp_metrics.len(), expected_metric_count);
    assert_eq!(
        exposition
            .lines()
            .filter(|line| line.starts_with("# TYPE "))
            .count(),
        expected_metric_count,
    );
    Ok(exposition)
}

fn assert_prometheus_family(exposition: &str, name: &str, ty: &str, help: &str) {
    assert!(
        exposition
            .lines()
            .any(|line| line == format!("# HELP {name} {help}")),
        "missing HELP for {name}",
    );
    assert!(
        exposition
            .lines()
            .any(|line| line == format!("# TYPE {name} {ty}")),
        "missing TYPE for {name}",
    );
    assert!(
        exposition
            .lines()
            .any(|line| line.starts_with(&format!("{name}{{"))),
        "missing sample for {name}",
    );
}

const EXPECTED_SOURCE_NAMES: [&str; 14] = [
    "hw_metric_pcie_correctable_errors_count",
    "hw_metric_pcie_non_fatal_errors_count",
    "hw_metric_pcie_fatal_errors_count",
    "hw_metric_pcie_l0_to_recovery_count",
    "hw_metric_pcie_replay_count",
    "hw_metric_pcie_replay_rollover_count",
    "hw_metric_pcie_nak_sent_count",
    "hw_metric_pcie_nak_received_count",
    "hw_metric_pcie_unsupported_request_count",
    "hw_metric_pcie_bad_tlp_count",
    "hw_metric_pcie_bad_dllp_count",
    "hw_metric_pcie_flow_control_timeout_count",
    "hw_metric_power_limit_throttle_seconds",
    "hw_metric_thermal_limit_throttle_seconds",
];
