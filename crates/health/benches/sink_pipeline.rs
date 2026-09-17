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
    Classification, CollectorEvent, CompositeDataSink, DataSink, EventContext, HealthReport,
    HealthReportSink, LogRecord, LogSeverity, MetricSample, PrometheusSink, ReportSource,
};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use health_report::HealthReport as CarbideHealthReport;
use mac_address::MacAddress;

const MACHINE_ID: &str = "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";
const MACHINE_IDS: [&str; 3] = [
    "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
    "fm100htjsaledfasinabqqer70e2ua5ksqj4kfjii0v0a90vulps48c1h7g",
    "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30",
];

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
        std::hint::black_box(context);
        std::hint::black_box(event);
        Ok(())
    }
}

fn event_context() -> EventContext {
    event_context_for_machine(MACHINE_ID)
}

fn event_context_for_machine(machine_id: &str) -> EventContext {
    EventContext {
        endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
        addr: BmcAddr {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            port: Some(443),
            mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").unwrap(),
        },
        collector_type: "sensor_collector",
        labels: Default::default(),
        metadata: Some(EndpointMetadata::Machine(MachineData {
            machine_id: Some(machine_id.parse().expect("valid machine id")),
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

fn metric_events(batch_size: usize, unique_keys: usize) -> Vec<CollectorEvent> {
    let unique_keys = unique_keys.max(1);

    (0..batch_size)
        .map(|idx| {
            let sensor_idx = idx % unique_keys;
            let key = format!("sensor-{sensor_idx}");

            CollectorEvent::Metric(
                MetricSample {
                    key: key.clone(),
                    name: "hw_sensor".to_string(),
                    metric_type: "temperature".to_string(),
                    unit: "celsius".to_string(),
                    value: (idx % 100) as f64,
                    labels: vec![(Cow::Borrowed("sensor"), key)],
                    context: None,
                }
                .into(),
            )
        })
        .collect()
}

fn emit_metric_batch(sink: &dyn DataSink, context: &EventContext, events: &[CollectorEvent]) {
    let start = CollectorEvent::MetricCollectionStart;
    sink.handle_event(context, &start);
    for event in events {
        sink.handle_event(context, event);
    }
    let end = CollectorEvent::MetricCollectionEnd;
    sink.handle_event(context, &end);
}

fn bench_prometheus_sink(c: &mut Criterion) {
    let mut group = c.benchmark_group("sink_prometheus");
    let batch_size = 2_000usize;
    group.throughput(Throughput::Elements(batch_size as u64));

    for (scenario, unique_keys) in [("low_cardinality", 32usize), ("high_cardinality", 2_000)] {
        let metrics_manager =
            Arc::new(MetricsManager::new("bench_sink").expect("metrics manager should initialize"));
        let sink = PrometheusSink::new(metrics_manager, "bench_sink")
            .expect("prometheus sink should initialize");
        let context = event_context();
        let events = metric_events(batch_size, unique_keys);

        group.bench_with_input(
            BenchmarkId::new("emit_batch", scenario),
            &events,
            |b, events| {
                b.iter(|| emit_metric_batch(&sink, &context, events));
            },
        );
    }

    group.finish();
}

struct CompositeBenchState {
    sink: CompositeDataSink,
    context: EventContext,
    events: Vec<CollectorEvent>,
}

impl CompositeBenchState {
    fn new(sink_count: usize, batch_size: usize) -> Self {
        let mut sinks: Vec<Arc<dyn DataSink>> = Vec::with_capacity(sink_count);
        for _ in 0..sink_count {
            sinks.push(Arc::new(CountingSink));
        }

        let metrics_manager =
            Arc::new(MetricsManager::new("bench_sink").expect("metrics manager should initialize"));
        let sink = CompositeDataSink::new(sinks, metrics_manager);

        Self {
            sink,
            context: event_context(),
            events: metric_events(batch_size, 64),
        }
    }
}

fn bench_composite_sink(c: &mut Criterion) {
    let mut group = c.benchmark_group("sink_composite");
    let batch_size = 2_000usize;

    for sink_count in [2usize, 4usize] {
        let state = CompositeBenchState::new(sink_count, batch_size);
        group.throughput(Throughput::Elements(batch_size as u64));

        group.bench_with_input(
            BenchmarkId::new("emit_only", sink_count),
            &state,
            |b, state| {
                b.iter(|| emit_metric_batch(&state.sink, &state.context, &state.events));
            },
        );
    }

    group.finish();
}

fn health_report_with_alerts(alert_count: usize) -> HealthReport {
    let mut report = HealthReport {
        source: carbide_health::sink::ReportSource::BmcSensors,
        target: Some(carbide_health::sink::HealthReportTarget::Machine),
        observed_at: Some(chrono::Utc::now()),
        successes: Vec::new(),
        alerts: Vec::new(),
    };
    for idx in 0..alert_count {
        report.alerts.push(carbide_health::sink::HealthReportAlert {
            probe_id: carbide_health::sink::Probe::Sensor,
            attribution: None,
            target: Some(format!("target-{idx}")),
            message: format!("alert message #{idx}"),
            classifications: vec![Classification::SensorCritical],
        });
    }
    report
}

struct HealthReportBenchState {
    sink: HealthReportSink,
    context: EventContext,
    distinct_contexts: Vec<EventContext>,
    sensor_event: CollectorEvent,
    leak_event: CollectorEvent,
}

impl HealthReportBenchState {
    fn new() -> Self {
        let sink = HealthReportSink::new_for_bench().expect("bench sink should initialize");
        let context = event_context();
        let distinct_contexts = MACHINE_IDS
            .into_iter()
            .map(event_context_for_machine)
            .collect();
        let sensor_event = CollectorEvent::HealthReport(Arc::new(health_report_with_alerts(256)));
        let leak_event = CollectorEvent::HealthReport(Arc::new(HealthReport {
            source: ReportSource::TrayLeakDetection,
            target: Some(carbide_health::sink::HealthReportTarget::Machine),
            observed_at: Some(chrono::Utc::now()),
            successes: Vec::new(),
            alerts: vec![carbide_health::sink::HealthReportAlert {
                probe_id: carbide_health::sink::Probe::LeakDetection,
                attribution: None,
                target: Some("leak-detector".to_string()),
                message: "leak detected".to_string(),
                classifications: vec![Classification::Leak],
            }],
        }));

        Self {
            sink,
            context,
            distinct_contexts,
            sensor_event,
            leak_event,
        }
    }
}

fn filled_health_report_sink(
    contexts: &[EventContext],
    event: &CollectorEvent,
    leak_event: &CollectorEvent,
) -> HealthReportSink {
    let sink = HealthReportSink::new_for_bench().expect("bench sink should initialize");
    for context in contexts {
        sink.handle_event(context, event);
        sink.handle_event(context, leak_event);
    }
    sink
}

fn drain_pending(sink: &HealthReportSink) -> usize {
    let mut drained = 0;
    while sink.pop_pending_for_bench().is_some() {
        drained += 1;
    }
    drained
}

fn drain_and_convert_pending(sink: &HealthReportSink) -> usize {
    let mut drained = 0;
    while let Some((_machine_id, report)) = sink.pop_pending_for_bench() {
        let converted: CarbideHealthReport = report
            .as_ref()
            .try_into()
            .expect("bench health report conversion should succeed");
        std::hint::black_box(converted);
        drained += 1;
    }
    drained
}

fn bench_health_report_sink(c: &mut Criterion) {
    let mut group = c.benchmark_group("sink_health_report");
    let batch_size = 20_000usize;
    group.throughput(Throughput::Elements(batch_size as u64));

    let state = HealthReportBenchState::new();

    group.bench_with_input(BenchmarkId::new("enqueue", "report"), &state, |b, state| {
        b.iter(|| {
            for _ in 0..batch_size {
                state.sink.handle_event(&state.context, &state.sensor_event);
            }
        });
    });

    group.throughput(Throughput::Elements(6));

    group.bench_with_input(BenchmarkId::new("drain", "report"), &state, |b, state| {
        b.iter_batched(
            || {
                filled_health_report_sink(
                    &state.distinct_contexts,
                    &state.sensor_event,
                    &state.leak_event,
                )
            },
            |sink| {
                std::hint::black_box(drain_pending(&sink));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_with_input(
        BenchmarkId::new("drain_convert", "report"),
        &state,
        |b, state| {
            b.iter_batched(
                || {
                    filled_health_report_sink(
                        &state.distinct_contexts,
                        &state.sensor_event,
                        &state.leak_event,
                    )
                },
                |sink| {
                    std::hint::black_box(drain_and_convert_pending(&sink));
                },
                BatchSize::SmallInput,
            );
        },
    );

    group.finish();
}

fn log_events_with_attrs(count: usize, unique_sensors: usize) -> Vec<CollectorEvent> {
    (0..count)
        .map(|idx| {
            let sensor = format!("HGX_GPU_{}_Temp_1", idx % unique_sensors);
            CollectorEvent::Log(Box::new(LogRecord {
                body: format!("{sensor} sensor crossed threshold"),
                severity: LogSeverity::Warn,
                attributes: vec![
                    (
                        Cow::Borrowed("message_id"),
                        "OpenBMC.0.1.SensorThresholdWarningLowGoingHigh".to_string(),
                    ),
                    (
                        Cow::Borrowed("message_args"),
                        format!(r#"["{sensor}","3.96","-0.05"]"#),
                    ),
                ],
                diagnostic_record: None,
            }))
        })
        .collect()
}

fn bench_otlp_sink(c: &mut Criterion) {
    use carbide_health::sink::OtlpSink;
    use carbide_health::sink::event_mapper::{OpenBmcEventMapper, RedfishEventMapper};

    let mut group = c.benchmark_group("sink_otlp");
    let mapper: Arc<dyn RedfishEventMapper> = Arc::new(OpenBmcEventMapper);
    let context = event_context();

    for (scenario, event_count, unique_sensors) in [
        ("low_contention", 2_000usize, 64usize),
        ("high_contention_dedup", 2_000, 4),
    ] {
        let events = log_events_with_attrs(event_count, unique_sensors);
        group.throughput(Throughput::Elements(event_count as u64));

        group.bench_with_input(
            BenchmarkId::new("enqueue", scenario),
            &events,
            |b, events| {
                b.iter_batched(
                    || OtlpSink::new_for_bench(mapper.clone()),
                    |sink| {
                        for event in events {
                            sink.handle_event(&context, event);
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.throughput(Throughput::Elements(500));
    group.bench_function("drain_500", |b| {
        b.iter_batched(
            || {
                let sink = OtlpSink::new_for_bench(mapper.clone());
                let events = log_events_with_attrs(500, 500);
                for event in &events {
                    sink.handle_event(&context, event);
                }
                sink
            },
            |sink| {
                let mut drained = 0;
                while sink.pop_for_bench().is_some() {
                    drained += 1;
                }
                black_box(drained);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_queue_key_construction(c: &mut Criterion) {
    use carbide_health::sink::event_mapper::{OpenBmcEventMapper, RedfishEventMapper};

    let mut group = c.benchmark_group("queue_key");
    let mapper = OpenBmcEventMapper;

    let sensor_attrs: Vec<(Cow<'static, str>, String)> = vec![
        (
            Cow::Borrowed("message_id"),
            "OpenBMC.0.1.SensorThresholdWarningLowGoingHigh".to_string(),
        ),
        (
            Cow::Borrowed("message_args"),
            r#"["HGX_GPU_0_Temp_1","3.96","-0.05"]"#.to_string(),
        ),
    ];

    let resource_attrs: Vec<(Cow<'static, str>, String)> = vec![
        (
            Cow::Borrowed("message_id"),
            "ResourceEvent.1.0.ResourceErrorsDetected".to_string(),
        ),
        (
            Cow::Borrowed("message_args"),
            r#"["GPU_1 NVLink_9","NVLink Training Error"]"#.to_string(),
        ),
    ];

    group.bench_function("sensor_threshold", |b| {
        b.iter(|| black_box(mapper.queue_key("10.85.14.144", &sensor_attrs)));
    });

    group.bench_function("resource_event", |b| {
        b.iter(|| black_box(mapper.queue_key("10.85.14.144", &resource_attrs)));
    });

    group.finish();
}

fn make_sink_report(alert_count: usize, success_count: usize) -> HealthReport {
    use carbide_health::sink::{HealthReportAlert, HealthReportSuccess};
    HealthReport {
        source: ReportSource::BmcSensors,
        target: Some(carbide_health::sink::HealthReportTarget::Machine),
        observed_at: Some(chrono::Utc::now()),
        successes: (0..success_count)
            .map(|i| HealthReportSuccess {
                probe_id: carbide_health::sink::Probe::Sensor,
                attribution: None,
                target: Some(format!("target-{i}")),
            })
            .collect(),
        alerts: (0..alert_count)
            .map(|i| HealthReportAlert {
                probe_id: carbide_health::sink::Probe::Sensor,
                attribution: None,
                target: Some(format!("target-{i}")),
                message: format!("alert message for probe {i}"),
                classifications: vec![Classification::SensorCritical],
            })
            .collect(),
    }
}

fn bench_content_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("health_report_content_hash");

    for (label, alerts, successes) in [
        ("empty", 0usize, 0usize),
        ("small_success_only", 0, 8),
        ("large_success_only", 0, 256),
        ("small_with_alerts", 4, 4),
        ("large_with_alerts", 64, 64),
    ] {
        let report = Arc::new(make_sink_report(alerts, successes));

        group.bench_with_input(BenchmarkId::new("hash", label), &report, |b, report| {
            b.iter(|| black_box(HealthReportSink::content_hash_for_bench(report)));
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_prometheus_sink,
    bench_composite_sink,
    bench_health_report_sink,
    bench_otlp_sink,
    bench_queue_key_construction,
    bench_content_hash,
);
criterion_main!(benches);
