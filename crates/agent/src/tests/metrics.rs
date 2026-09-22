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
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use opentelemetry::metrics::MeterProvider;
use prometheus::{Encoder, TextEncoder};

use crate::fmds_client::register_external_connection_metric;
use crate::instrumentation::NetworkMonitorMetricsState;
use crate::network_monitor::NetworkMonitorError;

#[test]
fn test_metrics() {
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
    let meter = meter_provider.meter("agent");

    let metrics = crate::instrumentation::create_metrics(meter.clone());
    let fmds_connection_status = register_external_connection_metric(&meter);
    fmds_connection_status.store(true, Ordering::Relaxed);
    metrics.record_machine_boot_time(1740171762);
    metrics.record_agent_start_time(1740171801);
    metrics.record_client_cert_expiry_time(|| Some(1742763762));

    let network_monitor_metrics = NetworkMonitorMetricsState::initialize(
        meter,
        "fm100ds10jimoops3mvpb4udrtnp9031m8sif0846eqbu4i5o49n74ijnf0"
            .parse()
            .unwrap(),
    );
    network_monitor_metrics.record_network_loss_percent(
        0.5,
        "fm100ds10jimoops3mvpb4udrtnp9031m8sif0846eqbu4i5o49n74ijnf0"
            .parse()
            .unwrap(),
        "fm100dsm61jm8b3ltfj0vh1vnhqff6jak7dhmp429qen6jtr0njjt5iqeq0"
            .parse()
            .unwrap(),
    );
    network_monitor_metrics.record_monitor_error(
        "fm100ds10jimoops3mvpb4udrtnp9031m8sif0846eqbu4i5o49n74ijnf0"
            .parse()
            .unwrap(),
        NetworkMonitorError::PingError.to_string(),
    );
    network_monitor_metrics.record_network_latency(
        Duration::from_millis(2),
        "fm100ds10jimoops3mvpb4udrtnp9031m8sif0846eqbu4i5o49n74ijnf0"
            .parse()
            .unwrap(),
        "fm100dsm61jm8b3ltfj0vh1vnhqff6jak7dhmp429qen6jtr0njjt5iqeq0"
            .parse()
            .unwrap(),
    );
    network_monitor_metrics.record_communication_error(
        "fm100ds10jimoops3mvpb4udrtnp9031m8sif0846eqbu4i5o49n74ijnf0"
            .parse()
            .unwrap(),
        "fm100dsm61jm8b3ltfj0vh1vnhqff6jak7dhmp429qen6jtr0njjt5iqeq0"
            .parse()
            .unwrap(),
        NetworkMonitorError::PingError.to_string(),
    );
    network_monitor_metrics.update_network_reachable_map(HashMap::from([(
        "fm100dsm61jm8b3ltfj0vh1vnhqff6jak7dhmp429qen6jtr0njjt5iqeq0"
            .parse()
            .unwrap(),
        true,
    )]));

    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = prometheus_registry.gather();
    encoder.encode(&metric_families, &mut buffer).unwrap();

    let prom_metrics = String::from_utf8(buffer).unwrap();
    assert_eq!(prom_metrics, include_str!("fixtures/metrics.txt"));
}

/// A metrics collection that finds no readable client certificate exports no
/// expiry series at all, rather than a misleading placeholder value.
#[test]
fn client_cert_expiry_gauge_exports_nothing_without_a_certificate() {
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
    let meter = meter_provider.meter("agent");

    let metrics = crate::instrumentation::create_metrics(meter);
    metrics.record_client_cert_expiry_time(|| None);

    let exported: Vec<String> = prometheus_registry
        .gather()
        .iter()
        .map(|family| family.name().to_string())
        .collect();
    assert!(
        !exported
            .iter()
            .any(|name| name.contains("client_cert_expiry_time_seconds")),
        "an unreadable certificate must export no expiry series: {exported:?}"
    );
}
