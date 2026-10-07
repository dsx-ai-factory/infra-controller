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
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use std::sync::Arc;

use carbide_health::endpoint::{BmcAddr, EndpointMetadata, MachineData, SharedSystemUuid};
use carbide_health::metrics::MetricsManager;
use carbide_health::processor::{
    EventProcessingPipeline, EventProcessor, HealthReportProcessor, LeakEventProcessor,
    RackLeakProcessor,
};
use carbide_health::sink::{
    CollectorEvent, CompositeDataSink, DataSink, EventContext, MetricSample, SensorThresholdContext,
};
use carbide_uuid::rack::RackId;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mac_address::MacAddress;
use nv_redfish::resource::Health as BmcHealth;

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
        std::hint::black_box(context);
        std::hint::black_box(event);
        Ok(())
    }
}

struct NoopProcessor;

impl EventProcessor for NoopProcessor {
    fn processor_type(&self) -> &'static str {
        "noop_processor"
    }

    fn process_event(
        &self,
        _context: &EventContext,
        _event: &CollectorEvent,
    ) -> Vec<CollectorEvent> {
        Vec::new()
    }
}

struct ReemitProcessor;

impl EventProcessor for ReemitProcessor {
    fn processor_type(&self) -> &'static str {
        "reemit_processor"
    }

    fn process_event(
        &self,
        _context: &EventContext,
        event: &CollectorEvent,
    ) -> Vec<CollectorEvent> {
        vec![event.clone()]
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

fn make_composite_sink(count: usize, metrics_manager: Arc<MetricsManager>) -> Arc<dyn DataSink> {
    let mut sinks: Vec<Arc<dyn DataSink>> = Vec::with_capacity(count);
    for _ in 0..count {
        sinks.push(Arc::new(CountingSink));
    }
    Arc::new(CompositeDataSink::new(sinks, metrics_manager))
}

fn metric_events(
    batch_size: usize,
    unique_keys: usize,
    with_health_context: bool,
) -> Vec<CollectorEvent> {
    let unique_keys = unique_keys.max(1);

    (0..batch_size)
        .map(|idx| {
            let sensor_idx = idx % unique_keys;
            let sensor_name = format!("sensor-{sensor_idx}");
            let reading = 20.0 + ((idx % 90) as f64);

            let mut metric = MetricSample {
                key: sensor_name.clone(),
                name: "hw_sensor".to_string(),
                metric_type: "temperature".to_string(),
                unit: "celsius".to_string(),
                value: reading,
                labels: vec![
                    (Cow::Borrowed("sensor_name"), sensor_name.clone()),
                    (Cow::Borrowed("physical_context"), "cpu".to_string()),
                ],
                context: None,
            };

            if with_health_context {
                metric.context = Some(SensorThresholdContext {
                    entity_type: "sensor".to_string(),
                    sensor_id: sensor_name,
                    upper_fatal: Some(90.0),
                    lower_fatal: Some(2.0),
                    upper_critical: Some(85.0),
                    lower_critical: Some(5.0),
                    upper_caution: Some(75.0),
                    lower_caution: Some(10.0),
                    range_max: Some(100.0),
                    range_min: Some(0.0),
                    bmc_health: BmcHealth::Warning,
                });
            }
            CollectorEvent::Metric(metric.into())
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

fn bench_pipeline_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("processor_pipeline_baseline");
    let batch_size = 2_000usize;
    group.throughput(Throughput::Elements(batch_size as u64));
    for (scenario, processor_count) in [("no_processors", 0usize), ("two_noops", 2usize)] {
        let metrics_manager: Arc<MetricsManager> =
            Arc::new(MetricsManager::new("bench").expect("metrics manager should initialize"));
        let sink = make_composite_sink(2, metrics_manager.clone());
        let mut processors: Vec<Arc<dyn EventProcessor>> = Vec::with_capacity(processor_count);
        for _ in 0..processor_count {
            processors.push(Arc::new(NoopProcessor));
        }
        let sink: Arc<dyn DataSink> = if processors.is_empty() {
            sink
        } else {
            Arc::new(EventProcessingPipeline::new(
                processors,
                sink,
                metrics_manager.clone(),
            ))
        };
        let context = event_context();
        let events = metric_events(batch_size, 64, false);

        group.bench_with_input(
            BenchmarkId::new("emit_batch", scenario),
            &events,
            |b, events| {
                b.iter(|| emit_metric_batch(sink.as_ref(), &context, events));
            },
        );
    }

    group.finish();
}

fn bench_pipeline_health_processors(c: &mut Criterion) {
    let mut group = c.benchmark_group("processor_pipeline_health");
    let batch_size = 2_000usize;
    group.throughput(Throughput::Elements(batch_size as u64));
    let metrics_manager: Arc<MetricsManager> =
        Arc::new(MetricsManager::new("bench").expect("metrics manager should initialize"));

    let processors: Vec<Arc<dyn EventProcessor>> = vec![
        Arc::new(HealthReportProcessor::default()),
        Arc::new(LeakEventProcessor::new(1)),
    ];
    let pipeline = EventProcessingPipeline::new(
        processors,
        make_composite_sink(2, metrics_manager.clone()),
        metrics_manager,
    );
    let context = event_context();

    for (scenario, unique_keys) in [("low_cardinality", 64usize), ("high_cardinality", 2_000)] {
        let events = metric_events(batch_size, unique_keys, true);
        group.bench_with_input(
            BenchmarkId::new("emit_batch", scenario),
            &events,
            |b, events| {
                b.iter(|| emit_metric_batch(&pipeline, &context, events));
            },
        );
    }

    group.finish();
}

fn bench_pipeline_loop_guard(c: &mut Criterion) {
    let mut group = c.benchmark_group("processor_pipeline_loop_guard");
    let batch_size = 2_000usize;
    group.throughput(Throughput::Elements(batch_size as u64));
    let metrics_manager: Arc<MetricsManager> =
        Arc::new(MetricsManager::new("bench").expect("metrics manager should initialize"));

    let pipeline = EventProcessingPipeline::new(
        vec![Arc::new(ReemitProcessor)],
        make_composite_sink(2, metrics_manager.clone()),
        metrics_manager,
    );
    let context = event_context();
    let events = metric_events(batch_size, 64, false);

    group.bench_function("single_reemit_processor", |b| {
        b.iter(|| emit_metric_batch(&pipeline, &context, &events));
    });

    group.finish();
}

fn rack_event_contexts(rack_id: &str, tray_count: usize) -> Vec<EventContext> {
    (0..tray_count)
        .map(|idx| {
            let mac = format!("42:9e:b1:bd:{:02x}:{:02x}", idx / 256, idx % 256);
            EventContext {
                endpoint_key: mac.clone(),
                addr: BmcAddr {
                    ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, (idx + 1) as u8)),
                    port: Some(443),
                    mac: Some(MacAddress::from_str(&mac).unwrap()),
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
                rack_id: Some(RackId::new(rack_id)),
            }
        })
        .collect()
}

fn bench_pipeline_rack_leak(c: &mut Criterion) {
    let mut group = c.benchmark_group("processor_pipeline_rack_leak");
    let batch_size = 200usize;
    let metrics_manager: Arc<MetricsManager> =
        Arc::new(MetricsManager::new("bench").expect("metrics manager should initialize"));

    let processors: Vec<Arc<dyn EventProcessor>> = vec![
        Arc::new(HealthReportProcessor::default()),
        Arc::new(LeakEventProcessor::new(1)),
        Arc::new(RackLeakProcessor::new(2)),
    ];
    let pipeline = EventProcessingPipeline::new(
        processors,
        make_composite_sink(2, metrics_manager.clone()),
        metrics_manager,
    );

    for (scenario, tray_count) in [("4_trays", 4usize), ("16_trays", 16)] {
        let contexts = rack_event_contexts("rack-bench", tray_count);
        let events = metric_events(batch_size, 64, true);

        group.throughput(Throughput::Elements(
            (batch_size as u64) * (tray_count as u64),
        ));
        group.bench_with_input(
            BenchmarkId::new("emit_batch_all_trays", scenario),
            &(contexts, events),
            |b, (contexts, events)| {
                b.iter(|| {
                    for ctx in contexts {
                        emit_metric_batch(&pipeline, ctx, events);
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_pipeline_baseline,
    bench_pipeline_health_processors,
    bench_pipeline_loop_guard,
    bench_pipeline_rack_leak,
);
criterion_main!(benches);
