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

use std::sync::Barrier;
use std::time::Duration;

use carbide_instrument::testing::{MetricsCapture, capture_logs};
use carbide_instrument::{DynamicLog, Event, Instrumentation, LogAt, MetricFamily, Outcome};
use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;

struct Metrics {
    instrumentation: Instrumentation,
    registry: prometheus::Registry,
    _provider: SdkMeterProvider,
}

impl Metrics {
    fn new() -> Self {
        let registry = prometheus::Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .unwrap();
        let provider = SdkMeterProvider::builder().with_reader(exporter).build();
        Self {
            instrumentation: Instrumentation::new(provider.meter("carbide-instrument")),
            registry,
            _provider: provider,
        }
    }

    fn metric(&self, name: &str, outcome: Outcome) -> Option<prometheus::proto::Metric> {
        self.registry
            .gather()
            .into_iter()
            .find(|family| family.name() == name)?
            .get_metric()
            .iter()
            .find(|metric| {
                metric.get_label().len() == 1
                    && metric.get_label()[0].name() == "outcome"
                    && metric.get_label()[0].value()
                        == carbide_instrument::LabelValue::label_value(&outcome).as_str()
            })
            .cloned()
    }
}

#[derive(Event)]
#[event(
    event_name = "test_owned_counter",
    metric_name = "carbide_test_owned_total",
    component = "instrument-test",
    metric = counter,
    describe = "Number of owned instrumentation test events",
    log = dynamic,
    message = "owned counter event",
)]
struct Count {
    #[label]
    outcome: Outcome,
}

impl DynamicLog for Count {
    fn log_at(&self) -> LogAt {
        match self.outcome {
            Outcome::Ok => LogAt::Off,
            Outcome::Error => LogAt::Level(tracing::Level::WARN),
        }
    }
}

#[derive(Event)]
#[event(
    event_name = "test_owned_duration",
    metric_name = "carbide_test_owned_milliseconds",
    component = "instrument-test",
    metric = histogram,
    describe = "Duration of owned instrumentation test operations",
    labels(outcome: Outcome),
)]
enum Latency {
    #[event(labels(outcome = Ok), log = warn, message = "test operation finished")]
    Finished {
        #[observation]
        elapsed: Duration,
    },
}

#[derive(Event)]
#[event(
    event_name = "test_owned_level",
    metric_name = "carbide_test_owned_bytes",
    component = "instrument-test",
    metric = gauge,
    describe = "Size observed by owned instrumentation tests",
    log = off,
)]
struct Level {
    #[label]
    outcome: Outcome,
    #[observation]
    bytes: u64,
}

#[derive(MetricFamily)]
#[metric(
    name = "carbide_test_owned_family_total",
    kind = counter,
    component = "instrument-test",
    describe = "Number of shared owned instrumentation test events",
)]
struct Family {
    outcome: Outcome,
}

#[derive(Event)]
#[event(event_name = "test_owned_family_struct", metric_family = Family, log = off)]
struct FamilyStruct {
    #[label]
    outcome: Outcome,
}

#[derive(Event)]
#[event(
    event_name = "test_owned_family_enum",
    metric_family = Family,
)]
enum FamilyEnum {
    #[event(labels(outcome = Outcome::Ok), log = off)]
    Finished {},
}

#[derive(Event, MetricFamily)]
#[event(
    event_name = "test_owned_dual_inline",
    metric_name = "carbide_test_owned_dual_inline_total",
    component = "instrument-test",
    metric = counter,
    describe = "Number of inline events on a dual declaration",
    log = off,
)]
#[metric(
    name = "carbide_test_owned_dual_family_total",
    kind = counter,
    component = "instrument-test",
    describe = "Number of family events on a dual declaration",
)]
struct DualDeclaration;

#[derive(Event)]
#[event(event_name = "test_owned_dual_family", metric_family = DualDeclaration, log = off)]
struct UsesDualFamily;

#[test]
fn inline_and_family_declarations_on_one_type_keep_their_own_identity() {
    let metrics = Metrics::new();
    metrics.instrumentation.emit(DualDeclaration);
    metrics.instrumentation.emit_count(UsesDualFamily, 3);
    let families = metrics.registry.gather();
    for (name, expected) in [
        ("carbide_test_owned_dual_inline_total", 1.0),
        ("carbide_test_owned_dual_family_total", 3.0),
    ] {
        let family = families
            .iter()
            .find(|family| family.name() == name)
            .unwrap();
        assert_eq!(family.get_metric().len(), 1);
        assert_eq!(family.get_metric()[0].get_counter().value(), expected);
    }
}

#[test]
fn independent_providers_record_the_same_declarations_concurrently() {
    let first = Metrics::new();
    let second = Metrics::new();
    let first_capture = MetricsCapture::for_registry(&first.registry);
    let second_capture = MetricsCapture::for_registry(&second.registry);
    // Initialize the counter in the first provider and the observation
    // instruments in the second: neither initialization order may bind the
    // other owner to its destination.
    first.instrumentation.emit(Count {
        outcome: Outcome::Ok,
    });
    second.instrumentation.emit(Latency::Finished {
        elapsed: Duration::from_millis(2),
    });
    second.instrumentation.emit(Level {
        outcome: Outcome::Ok,
        bytes: 999,
    });

    let start = Barrier::new(2);
    std::thread::scope(|threads| {
        for (metrics, count) in [(&first, 7), (&second, 11)] {
            let start = &start;
            threads.spawn(move || {
                let instrumentation = metrics.instrumentation.clone();
                start.wait();
                for value in 1..=count {
                    instrumentation.emit(Count {
                        outcome: Outcome::Ok,
                    });
                    instrumentation.emit(Latency::Finished {
                        elapsed: Duration::from_millis(2),
                    });
                    instrumentation.emit(Level {
                        outcome: Outcome::Ok,
                        bytes: value,
                    });
                }
            });
        }
    });

    for (capture, counter, observations, level) in [
        (&first_capture, 8.0, 7, 7.0),
        (&second_capture, 11.0, 12, 11.0),
    ] {
        let labels = &[("outcome", "ok")];
        assert_eq!(
            capture.counter_delta("carbide_test_owned_total", labels),
            counter
        );
        assert_eq!(
            capture.histogram_count_delta("carbide_test_owned_milliseconds", labels),
            observations
        );
        assert_eq!(
            capture
                .histogram_sum_delta("carbide_test_owned_milliseconds", labels)
                .0,
            observations as f64 * 2.0
        );
        assert_eq!(
            capture.gauge_value("carbide_test_owned_bytes", labels),
            level
        );
    }
}

#[test]
fn clones_and_family_events_share_one_metric_across_label_values() {
    let metrics = Metrics::new();
    let cloned = metrics.instrumentation.clone();
    metrics.instrumentation.emit(FamilyStruct {
        outcome: Outcome::Ok,
    });
    cloned.emit(FamilyEnum::Finished {});
    cloned.emit_count(
        FamilyStruct {
            outcome: Outcome::Error,
        },
        5,
    );

    assert_eq!(FamilyStruct::__metric_key(), FamilyEnum::__metric_key());
    for (outcome, expected) in [(Outcome::Ok, 2.0), (Outcome::Error, 5.0)] {
        assert_eq!(
            metrics
                .metric("carbide_test_owned_family_total", outcome)
                .unwrap()
                .get_counter()
                .value(),
            expected
        );
    }
}

#[test]
fn owned_recording_preserves_initialization_weighting_and_dynamic_logs() {
    let metrics = Metrics::new();
    let instrumentation = &metrics.instrumentation;
    let logs = capture_logs(|| {
        assert!(instrumentation.initialize_counter_series(&Count {
            outcome: Outcome::Ok
        }));
        assert!(
            !instrumentation.initialize_counter_series(&Latency::Finished {
                elapsed: Duration::ZERO
            })
        );
    });
    assert!(logs.is_empty());
    assert_eq!(
        metrics
            .metric("carbide_test_owned_total", Outcome::Ok)
            .unwrap()
            .get_counter()
            .value(),
        0.0
    );
    assert!(
        metrics
            .metric("carbide_test_owned_milliseconds", Outcome::Ok)
            .is_none()
    );

    let logs = capture_logs(|| {
        instrumentation.emit_count(
            Count {
                outcome: Outcome::Ok,
            },
            7,
        );
        instrumentation.emit(Count {
            outcome: Outcome::Ok,
        });
        instrumentation.emit_count(
            Count {
                outcome: Outcome::Error,
            },
            5,
        );
        instrumentation.emit_count(
            Latency::Finished {
                elapsed: Duration::from_millis(2),
            },
            10,
        );
    });
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_eq!(logs[0].field("event_name"), Some("test_owned_counter"));
    assert_eq!(logs[1].field("event_name"), Some("test_owned_duration"));
    assert_eq!(
        metrics
            .metric("carbide_test_owned_total", Outcome::Ok)
            .unwrap()
            .get_counter()
            .value(),
        8.0
    );
    assert_eq!(
        metrics
            .metric("carbide_test_owned_total", Outcome::Error)
            .unwrap()
            .get_counter()
            .value(),
        5.0
    );
    assert!(
        metrics
            .metric("carbide_test_owned_milliseconds", Outcome::Ok)
            .is_none()
    );
}
