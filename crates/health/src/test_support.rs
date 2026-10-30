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
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::HealthError;
use crate::collectors::{catalogue_samples, emit_catalogue_samples};
use crate::config::MetricsConfig;
use crate::endpoint::BmcAddr;
use crate::metrics::MetricsManager;
use crate::otlp::convert::build_metrics_export_request;
use crate::otlp::metrics::{AggregationTemporality, metric, number_data_point};
use crate::sink::{
    CollectorEvent, DataSink, EventContext, MetricSample, MetricSemantics, PrometheusSink,
};

/// `hardware_metric_exposition` exports source fixtures through `PrometheusSink`
/// and checks metadata and readings in both Prometheus and OTLP exports.
/// The returned text feeds the generated catalogue in `test_integration`.
/// Metrics setup, Redfish decoding and forwarding, and text export failures
/// return `HealthError`. Inventory or exported metric mismatches panic,
/// including missing gNMI samples after logged sink failures.
pub fn hardware_metric_exposition() -> Result<String, HealthError> {
    let prefix = MetricsConfig::default().prefix;
    let manager = Arc::new(MetricsManager::new(&prefix)?);
    let sink = Arc::new(CatalogueSink {
        prometheus: PrometheusSink::new(manager.clone(), &prefix)?,
        samples: Mutex::default(),
    });
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

    for sample in catalogue_samples()? {
        sink.try_handle_event(&context, &CollectorEvent::Metric(Box::new(sample)))?;
    }
    emit_catalogue_samples(
        sink.clone(),
        EventContext {
            collector_type: "nvue_gnmi",
            ..context
        },
    );

    let samples = sink.samples.lock().expect("catalogue samples mutex");
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
    let mut expected_metric_count = samples.len();

    for (_, sample) in samples.iter() {
        let legacy_name = name(sample);
        assert_prometheus_family(
            &exposition,
            &legacy_name,
            "gauge",
            "Metrics forwarded through sink pipeline",
            sample.value,
        );
        let legacy = otlp_metrics[legacy_name.as_str()];
        assert_eq!(legacy.description, "", "{legacy_name}");
        assert_eq!(legacy.unit, sample.unit, "{legacy_name}");
        let Some(metric::Data::Gauge(gauge)) = &legacy.data else {
            panic!("{legacy_name} must remain an OTLP gauge");
        };
        assert_eq!(gauge.data_points.len(), 1, "{legacy_name}");
        let expected_value = Some(number_data_point::Value::AsDouble(sample.value));
        assert_eq!(gauge.data_points[0].value, expected_value, "{legacy_name}");

        if let MetricSemantics::CumulativeCounter {
            description,
            start_time,
        } = sample.semantics
        {
            assert!(!description.trim().is_empty(), "{legacy_name}");
            let alias_name = format!("{legacy_name}_total");
            assert_prometheus_family(
                &exposition,
                &alias_name,
                "counter",
                description,
                sample.value,
            );
            let counter = otlp_metrics[alias_name.as_str()];
            assert_eq!(counter.description, description, "{alias_name}");
            assert_eq!(counter.unit, sample.unit, "{alias_name}");
            let Some(metric::Data::Sum(sum)) = &counter.data else {
                panic!("{alias_name} must be an OTLP sum");
            };
            assert_eq!(sum.data_points.len(), 1, "{alias_name}");
            assert_eq!(sum.data_points[0].value, expected_value, "{alias_name}");
            let expected_start = start_time.map_or(0, |start| {
                start
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("fixture start follows Unix epoch")
                    .as_nanos()
                    .try_into()
                    .expect("fixture start fits OTLP timestamp")
            });
            assert_eq!(
                sum.data_points[0].start_time_unix_nano, expected_start,
                "{alias_name}",
            );
            assert_eq!(
                sum.data_points[0].attributes,
                gauge.data_points[0].attributes
            );
            assert!(sum.is_monotonic, "{alias_name}");
            assert_eq!(
                sum.aggregation_temporality,
                AggregationTemporality::Cumulative as i32,
                "{alias_name}",
            );
            expected_metric_count += 1;
        }
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

/// `CatalogueSink` forwards samples to Prometheus and retains accepted samples
/// for source inventory and exporter checks.
struct CatalogueSink {
    prometheus: PrometheusSink,
    samples: Mutex<Vec<(EventContext, MetricSample)>>,
}

impl DataSink for CatalogueSink {
    fn sink_type(&self) -> &'static str {
        "catalogue"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        self.prometheus.try_handle_event(context, event)?;
        if let CollectorEvent::Metric(sample) = event {
            self.samples
                .lock()
                .expect("catalogue samples mutex")
                .push((context.clone(), sample.as_ref().clone()));
        }
        Ok(())
    }
}

fn assert_prometheus_family(
    exposition: &str,
    name: &str,
    ty: &str,
    help: &str,
    expected_value: f64,
) {
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
    let row = exposition
        .lines()
        .find(|line| line.starts_with(&format!("{name}{{")))
        .unwrap_or_else(|| panic!("missing sample for {name}"));
    let value = row
        .rsplit_once(' ')
        .expect("metric sample value")
        .1
        .parse::<f64>()
        .expect("numeric metric sample");
    assert_eq!(value, expected_value, "{name}");
}

const EXPECTED_SOURCE_NAMES: [&str; 56] = [
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
    "hw_metric_current_correctable_ecc_errors_count",
    "hw_metric_current_uncorrectable_ecc_errors_count",
    "hw_metric_current_indeterminate_correctable_errors_count",
    "hw_metric_current_indeterminate_uncorrectable_errors_count",
    "hw_metric_lifetime_correctable_ecc_errors_count",
    "hw_metric_lifetime_uncorrectable_ecc_errors_count",
    "hw_metric_lifetime_indeterminate_correctable_errors_count",
    "hw_metric_lifetime_indeterminate_uncorrectable_errors_count",
    "hw_metric_correctable_io_read_errors_count",
    "hw_metric_correctable_io_write_errors_count",
    "hw_metric_uncorrectable_io_read_errors_count",
    "hw_metric_uncorrectable_io_write_errors_count",
    "hw_metric_power_on_hours_hours",
    "hw_metric_read_io_kibibytes",
    "hw_metric_write_io_kibibytes",
    "hw_metric_energy_kilowatt_hours",
    "hw_metric_nvidia_pcie_tx_bytes",
    "hw_metric_nvidia_pcie_rx_bytes",
    "hw_metric_nvidia_hardware_violation_throttle_seconds",
    "hw_metric_nvidia_global_software_violation_throttle_seconds",
    "hw_metric_nvidia_accumulated_gpu_context_utilization_seconds",
    "hw_metric_nvidia_accumulated_sm_utilization_seconds",
    "hw_metric_nvidia_power_brake_assertion_seconds",
    "hw_metric_nvidia_correctable_row_remapping_count",
    "hw_metric_nvidia_uncorrectable_row_remapping_count",
    "nvue_gnmi_interface_in_errors_count",
    "nvue_gnmi_interface_out_errors_count",
    "nvue_gnmi_interface_out_discards_count",
    "nvue_gnmi_interface_in_octets_bytes",
    "nvue_gnmi_interface_out_octets_bytes",
    "nvue_gnmi_interface_in_packets_count",
    "nvue_gnmi_interface_out_packets_count",
    "nvue_gnmi_interface_link_downed_count",
    "nvue_gnmi_interface_link_error_recovery_count",
    "nvue_gnmi_interface_rcv_remote_physical_errors_count",
    "nvue_gnmi_interface_rcv_switch_relay_errors_count",
    "nvue_gnmi_interface_rcv_constraint_errors_count",
    "nvue_gnmi_interface_local_link_integrity_errors_count",
    "nvue_gnmi_interface_port_buffer_overrun_errors_count",
    "nvue_gnmi_interface_qp1_dropped_count",
    "nvue_gnmi_interface_vl15_dropped_count",
    "nvue_gnmi_interface_port_xmit_wait_count",
];
