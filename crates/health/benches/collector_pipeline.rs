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

use std::borrow::Cow;
use std::hint::black_box;
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use std::sync::Arc;

use carbide_health::endpoint::{BmcAddr, EndpointMetadata, MachineData, SharedSystemUuid};
use carbide_health::metrics::MetricsManager;
use carbide_health::sink::{
    CollectorEvent, CompositeDataSink, DataSink, EventContext, FirmwareInfo, LogRecord,
    LogSeverity, MetricSample, PrometheusSink,
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mac_address::MacAddress;

const MACHINE_ID: &str = "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";

struct CountingSink;

impl DataSink for CountingSink {
    fn sink_type(&self) -> &'static str {
        "counting_sink"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), carbide_health::HealthError> {
        black_box(context);
        black_box(event);
        Ok(())
    }
}

fn event_context() -> EventContext {
    EventContext {
        endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
        addr: BmcAddr {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            port: Some(443),
            mac: Some(MacAddress::from_str("42:9e:b1:bd:9d:dd").unwrap()),
        },
        collector_type: "sensor_collector",
        labels: Default::default(),
        metadata: Some(EndpointMetadata::Machine(MachineData {
            machine_id: Some(MACHINE_ID.parse().expect("valid machine id")),
            machine_serial: None,
            system_uuid: SharedSystemUuid::default(),
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
            driver_version: None,
        })),
        rack_id: None,
    }
}

fn build_sensor_metric_event(idx: usize, unique_keys: usize) -> CollectorEvent {
    let unique_keys = unique_keys.max(1);
    let sensor_idx = idx % unique_keys;
    let sensor_key = format!("sensor-{sensor_idx}");
    let machine_idx = idx % 16;
    let rack_idx = idx % 4;

    CollectorEvent::Metric(
        MetricSample {
            key: sensor_key.clone(),
            name: "hw_sensor".to_string(),
            metric_type: "temperature".to_string(),
            unit: "celsius".to_string(),
            value: 25.0 + ((idx % 40) as f64),
            labels: vec![
                (Cow::Borrowed("sensor_name"), sensor_key),
                (Cow::Borrowed("physical_context"), "cpu".to_string()),
                (Cow::Borrowed("model"), "x86-test".to_string()),
                (Cow::Borrowed("machine_slot"), format!("slot-{machine_idx}")),
                (Cow::Borrowed("rack"), format!("rack-{rack_idx}")),
            ],
            context: None,
        }
        .into(),
    )
}

fn build_nmxt_metric_event(idx: usize) -> CollectorEvent {
    CollectorEvent::Metric(
        MetricSample {
            key: format!("effective_ber:{}", idx % 64),
            name: "switch_nmxt".to_string(),
            metric_type: "effective_ber".to_string(),
            unit: "count".to_string(),
            value: (idx % 10) as f64,
            labels: vec![
                (Cow::Borrowed("switch_id"), "switch-1".to_string()),
                (Cow::Borrowed("switch_ip"), "10.0.1.1".to_string()),
                (Cow::Borrowed("node_guid"), format!("0x{:x}", idx)),
                (Cow::Borrowed("port_num"), (idx % 64).to_string()),
            ],
            context: None,
        }
        .into(),
    )
}

fn build_log_event(idx: usize) -> CollectorEvent {
    CollectorEvent::Log(
        LogRecord {
            body: format!("BMC event line {idx}"),
            severity: LogSeverity::Info,
            attributes: vec![
                (Cow::Borrowed("machine_id"), MACHINE_ID.to_string()),
                (Cow::Borrowed("entry_id"), idx.to_string()),
                (Cow::Borrowed("service_id"), "logservice-1".to_string()),
            ],
            diagnostic_record: None,
        }
        .into(),
    )
}

fn build_firmware_event(idx: usize) -> CollectorEvent {
    let component = format!("component-{idx}");
    CollectorEvent::Firmware(FirmwareInfo {
        component: component.clone(),
        version: format!("1.0.{}", idx % 100),
        attributes: vec![
            (Cow::Borrowed("firmware_name"), component),
            (Cow::Borrowed("version"), format!("1.0.{}", idx % 100)),
        ],
    })
}

fn bench_collector_event_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("collector_event_build");
    let sample_count = 10_000usize;
    group.throughput(Throughput::Elements(sample_count as u64));

    group.bench_function("sensor_metric", |b| {
        b.iter(|| {
            for idx in 0..sample_count {
                black_box(build_sensor_metric_event(idx, 256));
            }
        });
    });

    group.bench_function("nmxt_metric", |b| {
        b.iter(|| {
            for idx in 0..sample_count {
                black_box(build_nmxt_metric_event(idx));
            }
        });
    });

    group.bench_function("log_event", |b| {
        b.iter(|| {
            for idx in 0..sample_count {
                black_box(build_log_event(idx));
            }
        });
    });

    group.bench_function("firmware_event", |b| {
        b.iter(|| {
            for idx in 0..sample_count {
                black_box(build_firmware_event(idx));
            }
        });
    });

    group.finish();
}

fn emit_metric_batch_building(
    sink: &dyn DataSink,
    context: &EventContext,
    batch_size: usize,
    unique_keys: usize,
) {
    let start = CollectorEvent::MetricCollectionStart;
    sink.handle_event(context, &start);

    for idx in 0..batch_size {
        let event = build_sensor_metric_event(idx, unique_keys);
        sink.handle_event(context, &event);
    }

    let end = CollectorEvent::MetricCollectionEnd;
    sink.handle_event(context, &end);
}

fn bench_collector_build_and_emit_prometheus(c: &mut Criterion) {
    let mut group = c.benchmark_group("collector_build_emit_prometheus");
    let batch_size = 2_000usize;
    group.throughput(Throughput::Elements(batch_size as u64));

    for (scenario, unique_keys) in [("low_cardinality", 64usize), ("high_cardinality", 2_000)] {
        let metrics_manager = Arc::new(
            MetricsManager::new("bench_collector").expect("metrics manager should initialize"),
        );
        let sink = PrometheusSink::new(metrics_manager, "bench_collector")
            .expect("prometheus sink should initialize");
        let context = event_context();

        group.bench_with_input(
            BenchmarkId::new("sensor_build_and_emit", scenario),
            &unique_keys,
            |b, unique_keys| {
                b.iter(|| emit_metric_batch_building(&sink, &context, batch_size, *unique_keys));
            },
        );
    }

    group.finish();
}

struct CompositeBuildEmitState {
    sink: CompositeDataSink,
    context: EventContext,
}

impl CompositeBuildEmitState {
    fn new(sink_count: usize) -> Self {
        let mut sinks: Vec<Arc<dyn DataSink>> = Vec::with_capacity(sink_count);
        for _ in 0..sink_count {
            sinks.push(Arc::new(CountingSink));
        }

        let metrics_manager = Arc::new(
            MetricsManager::new("bench_collector").expect("metrics manager should initialize"),
        );
        let sink = CompositeDataSink::new(sinks, metrics_manager);

        Self {
            sink,
            context: event_context(),
        }
    }
}

fn bench_collector_build_and_emit_composite(c: &mut Criterion) {
    let mut group = c.benchmark_group("collector_build_emit_composite");
    let batch_size = 2_000usize;
    group.throughput(Throughput::Elements(batch_size as u64));

    for sink_count in [2usize, 4usize] {
        let state = CompositeBuildEmitState::new(sink_count);
        group.bench_with_input(
            BenchmarkId::new("sensor_build_emit", sink_count),
            &state,
            |b, state| {
                b.iter(|| emit_metric_batch_building(&state.sink, &state.context, batch_size, 64));
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_collector_event_build,
    bench_collector_build_and_emit_prometheus,
    bench_collector_build_and_emit_composite
);
criterion_main!(benches);
