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

//! The log/metric matrix, end to end: every knob combination produces exactly
//! the declared outputs, with the same label values on both sides.

use std::time::Duration;

use carbide_instrument::testing::{
    ApproxHistogramSum, CapturedFieldKind, MetricsCapture, capture_logs,
};
use carbide_instrument::{
    Event, LabelValue, LogAt, MetricFamily, MetricKind, Outcome, emit, initialize_counter_series,
};
use carbide_test_support::value_scenarios;

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum Stage {
    PreFlight,
    Apply,
}

#[test]
fn histogram_sum_comparison_tolerates_only_low_order_drift() {
    struct Comparison {
        actual: f64,
        expected: f64,
    }

    value_scenarios!(run = |comparison: Comparison| {
        ApproxHistogramSum(comparison.actual) == ApproxHistogramSum(comparison.expected)
    };
        "histogram delta comparison" {
            Comparison {
                actual: 12.500000000000002,
                expected: 12.5,
            } => true,
            Comparison {
                actual: 1e-9,
                expected: 0.0,
            } => false,
            Comparison {
                actual: 12.500001,
                expected: 12.5,
            } => false,
        }
    );
}

/// log = warn, metric = counter: one emit writes the log line AND moves the
/// counter, with identical label values.
#[test]
fn both_sides_from_one_emit() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_both_fired",
        metric_name = "carbide_test_matrix_both_total",
        component = "matrix-test",
        log = warn,
        metric = counter,
        describe_unchecked,
        message = "matrix both fired"
    )]
    struct BothSides {
        #[label]
        stage: Stage,
        #[label]
        outcome: Outcome,
        #[context]
        machine: String,
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(BothSides {
            stage: Stage::Apply,
            outcome: Outcome::Error,
            machine: "machine-1".to_string(),
        });
    });

    assert_eq!(logs.len(), 1);
    let log = &logs[0];
    assert_eq!(log.level, tracing::Level::WARN);
    assert_eq!(log.metadata_name, "test_matrix_both_fired");
    assert_eq!(log.message, "matrix both fired");
    assert_eq!(log.field("event_name"), Some("test_matrix_both_fired"));
    assert_eq!(
        log.field("metric_name"),
        Some("carbide_test_matrix_both_total")
    );
    assert_eq!(log.field("stage"), Some("apply"));
    assert_eq!(log.field("outcome"), Some("error"));
    assert_eq!(log.field("machine"), Some("machine-1"));

    assert_eq!(
        metrics.counter_delta(
            "carbide_test_matrix_both_total",
            &[("stage", "apply"), ("outcome", "error")],
        ),
        1.0
    );
}

/// Counter initialization needs to expose the series without pretending the
/// Event happened. The first real `emit` must therefore move the same series
/// from zero to one and write exactly one log line.
#[test]
fn counter_series_initialization_does_not_emit_the_event() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_counter_initialized",
        metric_name = "carbide_test_matrix_initialized_total",
        component = "matrix-test",
        log = warn,
        metric = counter,
        describe = "Number of initialized counter test events",
        message = "initialized counter fired"
    )]
    struct InitializedCounter {
        #[label]
        stage: Stage,
        #[context]
        detail: String,
    }

    let event = InitializedCounter {
        stage: Stage::PreFlight,
        detail: "first real event".to_string(),
    };
    let metrics = MetricsCapture::start();
    let initialization_logs = capture_logs(|| {
        assert!(initialize_counter_series(&event));
    });

    assert!(initialization_logs.is_empty());
    assert_eq!(
        metrics.counter_delta(
            "carbide_test_matrix_initialized_total",
            &[("stage", "pre_flight")],
        ),
        0.0
    );
    assert!(
        metrics
            .render()
            .contains("carbide_test_matrix_initialized_total{stage=\"pre_flight\"} 0")
    );

    let logs = capture_logs(|| emit(event));
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].message, "initialized counter fired");
    assert_eq!(
        metrics.counter_delta(
            "carbide_test_matrix_initialized_total",
            &[("stage", "pre_flight")],
        ),
        1.0
    );
}

#[test]
fn non_counter_event_cannot_initialize_a_counter_series() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_histogram_initialization_rejected",
        metric_name = "carbide_test_matrix_initialization_milliseconds",
        component = "matrix-test",
        log = off,
        metric = histogram,
        describe = "Test initialization duration"
    )]
    struct Histogram {
        #[observation]
        latency: Duration,
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        assert!(!initialize_counter_series(&Histogram {
            latency: Duration::from_millis(10),
        }));
    });

    assert!(logs.is_empty());
    assert!(
        !metrics
            .render()
            .contains("carbide_test_matrix_initialization_milliseconds")
    );
}

/// `#[context(value)]` retains each supported structured type instead of
/// routing it through `Display` formatting.
#[test]
fn native_context_retains_its_type() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_native_context",
        component = "matrix-test",
        message = "native context"
    )]
    struct NativeContext {
        #[context(value)]
        ready: bool,
        #[context(value)]
        attempt: i64,
        #[context(value)]
        retry_interval_seconds: f64,
        #[context(value)]
        phase: String,
    }

    let logs = capture_logs(|| {
        emit(NativeContext {
            ready: true,
            attempt: 3,
            retry_interval_seconds: 30.5,
            phase: "backoff".to_string(),
        });
    });

    assert_eq!(logs.len(), 1);
    value_scenarios!(run = |field| (
        logs[0].field(field).map(str::to_string),
        logs[0].field_kind(field),
    );
        "native context retains its rendered value and tracing type" {
            "ready" => (Some("true".to_string()), Some(CapturedFieldKind::Bool)),
            "attempt" => (Some("3".to_string()), Some(CapturedFieldKind::I64)),
            "retry_interval_seconds" => (
                Some("30.5".to_string()),
                Some(CapturedFieldKind::F64),
            ),
            "phase" => (Some("backoff".to_string()), Some(CapturedFieldKind::String)),
        }
    );
}

/// `#[label(name = "...")]` preserves a frozen metric key without changing
/// the generated log's field name. This test pins both surfaces so the
/// compatibility alias cannot leak into the log schema.
#[test]
fn label_alias_changes_only_the_metric_key() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_label_alias_fired",
        metric_name = "carbide_test_matrix_label_alias_total",
        component = "matrix-test",
        log = info,
        metric = counter,
        describe = "Number of aliased-label test events",
        message = "aliased label fired"
    )]
    struct AliasedLabel {
        #[label(name = "component")]
        publisher: Stage,
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(AliasedLabel {
            publisher: Stage::Apply,
        });
    });

    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].field("publisher"), Some("apply"));
    assert_eq!(logs[0].field("component"), None);
    assert_eq!(
        metrics.counter_delta(
            "carbide_test_matrix_label_alias_total",
            &[("component", "apply")],
        ),
        1.0
    );
    assert_eq!(
        metrics.counter_delta(
            "carbide_test_matrix_label_alias_total",
            &[("publisher", "apply")],
        ),
        0.0
    );
}

/// log = off, metric = counter: the counter moves and no log line is built at
/// all (message is not even required).
#[test]
fn metric_only_writes_no_log() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_quiet_fired",
        metric_name = "carbide_test_matrix_quiet_total",
        component = "matrix-test",
        log = off,
        metric = counter,
        describe_unchecked
    )]
    struct Quiet {
        #[label]
        stage: Stage,
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(Quiet {
            stage: Stage::PreFlight,
        });
        emit(Quiet {
            stage: Stage::PreFlight,
        });
    });

    assert!(logs.is_empty(), "log = off must not construct any log line");
    assert_eq!(
        metrics.counter_delta(
            "carbide_test_matrix_quiet_total",
            &[("stage", "pre_flight")]
        ),
        2.0
    );
}

/// metric = none: a plain structured log, and nothing appears on the registry.
#[test]
fn log_only_registers_no_metric() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_log_only_fired",
        component = "matrix-test",
        log = info,
        metric = none,
        message = "log only fired"
    )]
    struct LogOnly {
        #[context]
        detail: String,
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(LogOnly {
            detail: "just words".to_string(),
        });
    });

    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::INFO);
    assert_eq!(logs[0].metadata_name, "test_matrix_log_only_fired");
    assert_eq!(
        logs[0].field("event_name"),
        Some("test_matrix_log_only_fired")
    );
    assert_eq!(logs[0].field("metric_name"), None);
    assert_eq!(logs[0].field("detail"), Some("just words"));
    assert_eq!(
        metrics.counter_delta("carbide_test_matrix_logonly", &[]),
        0.0
    );
}

/// metric = histogram: the observation records in the unit `metric_name` declares
/// (a Duration converts), and the log still fires independently.
#[test]
fn histogram_records_the_observation_in_declared_units() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_copy_finished",
        metric_name = "carbide_test_matrix_copy_duration_seconds",
        component = "matrix-test",
        log = info,
        metric = histogram,
        message = "copy finished"
    )]
    struct CopyFinished {
        #[label]
        outcome: Outcome,
        #[observation]
        took: Duration,
        #[context]
        host: String,
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(CopyFinished {
            outcome: Outcome::Ok,
            took: Duration::from_millis(1500),
            host: "10.0.0.5".to_string(),
        });
    });

    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].metadata_name, "test_matrix_copy_finished");
    assert_eq!(
        logs[0].field("event_name"),
        Some("test_matrix_copy_finished")
    );
    assert_eq!(
        logs[0].field("metric_name"),
        Some("carbide_test_matrix_copy_duration_seconds")
    );
    assert_eq!(
        metrics.histogram_count_delta(
            "carbide_test_matrix_copy_duration_seconds",
            &[("outcome", "ok")],
        ),
        1
    );
    let sum = metrics.histogram_sum_delta(
        "carbide_test_matrix_copy_duration_seconds",
        &[("outcome", "ok")],
    );
    assert_eq!(sum, ApproxHistogramSum(1.5), "1500ms records as 1.5s");
}

/// A unit struct works (zero labels), and the declared knob constants are
/// what the derive wrote.
#[test]
fn unit_struct_and_declared_knobs() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_tick_recorded",
        metric_name = "carbide_test_matrix_unit_total",
        component = "matrix-test",
        log = off,
        metric = counter,
        describe_unchecked
    )]
    struct Tick;

    assert_eq!(<Tick as Event>::LOG, LogAt::Off);
    assert_eq!(<Tick as Event>::METRIC, MetricKind::Counter);
    assert_eq!(<Tick as Event>::EVENT_NAME, "test_matrix_tick_recorded");
    assert_eq!(
        <Tick as Event>::METRIC_NAME,
        Some("carbide_test_matrix_unit_total")
    );
    assert_eq!(<Tick as Event>::COMPONENT, "matrix-test");

    let metrics = MetricsCapture::start();
    emit(Tick);
    assert_eq!(
        metrics.counter_delta("carbide_test_matrix_unit_total", &[]),
        1.0
    );
}

/// A hand-written log_at override: failures log, successes are counted
/// silently -- the count-everything-log-only-failures idiom.
#[test]
fn per_instance_log_at_override() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_call_finished",
        metric_name = "carbide_test_matrix_calls_total",
        component = "matrix-test",
        log = dynamic,
        metric = counter,
        describe_unchecked,
        message = "call finished"
    )]
    struct CallFinished {
        #[label]
        outcome: Outcome,
    }

    impl carbide_instrument::DynamicLog for CallFinished {
        fn log_at(&self) -> LogAt {
            match self.outcome {
                Outcome::Error => LogAt::Level(tracing::Level::WARN),
                Outcome::Ok => LogAt::Off,
            }
        }
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(CallFinished {
            outcome: Outcome::Ok,
        });
        emit(CallFinished {
            outcome: Outcome::Error,
        });
    });

    assert_eq!(logs.len(), 1, "only the failure logs");
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_eq!(logs[0].metadata_name, "test_matrix_call_finished");
    assert_eq!(
        logs[0].field("event_name"),
        Some("test_matrix_call_finished")
    );
    assert_eq!(
        logs[0].field("metric_name"),
        Some("carbide_test_matrix_calls_total")
    );
    assert_eq!(
        metrics.counter_delta("carbide_test_matrix_calls_total", &[("outcome", "ok")]),
        1.0
    );
    assert_eq!(
        metrics.counter_delta("carbide_test_matrix_calls_total", &[("outcome", "error")]),
        1.0
    );
}

/// Every supported histogram unit round-trips: `metric_name` in the attribute is
/// the exposed name, and the observation records in that unit.
#[test]
fn histogram_units_round_trip() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_lag_sampled",
        metric_name = "carbide_test_matrix_lag_duration_milliseconds",
        component = "matrix-test",
        log = off,
        metric = histogram
    )]
    struct LagSampled {
        #[observation]
        took: Duration,
    }

    #[derive(Event)]
    #[event(
        event_name = "test_matrix_poll_sampled",
        metric_name = "carbide_test_matrix_poll_duration_microseconds",
        component = "matrix-test",
        log = off,
        metric = histogram
    )]
    struct PollSampled {
        #[observation]
        took: Duration,
    }

    #[derive(Event)]
    #[event(
        event_name = "test_matrix_payload_sized",
        metric_name = "carbide_test_matrix_payload_bytes",
        component = "matrix-test",
        log = off,
        metric = histogram
    )]
    struct PayloadSized {
        #[observation]
        size: u64,
    }

    let metrics = MetricsCapture::start();
    emit(LagSampled {
        took: Duration::from_millis(250),
    });
    emit(PollSampled {
        took: Duration::from_micros(1500),
    });
    emit(PayloadSized { size: 4096 });

    for (name, expected_sum) in [
        ("carbide_test_matrix_lag_duration_milliseconds", 250.0),
        ("carbide_test_matrix_poll_duration_microseconds", 1500.0),
        ("carbide_test_matrix_payload_bytes", 4096.0),
    ] {
        assert_eq!(metrics.histogram_count_delta(name, &[]), 1, "{name}");
        let sum = metrics.histogram_sum_delta(name, &[]);
        assert_eq!(sum, ApproxHistogramSum(expected_sum), "{name}");
    }
}

/// The outbound-call helper: every completion records the RED histogram,
/// only failures write the WARN.
#[test]
fn red_helper_counts_everything_and_logs_only_failures() {
    use futures_util::FutureExt as _;

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        let ok: Result<u32, String> = carbide_instrument::red::instrumented(
            "matrix_backend",
            "matrix_op",
            std::future::ready(Ok(7)),
        )
        .now_or_never()
        .expect("ready future");
        assert_eq!(ok, Ok(7));

        let failed: Result<u32, String> = carbide_instrument::red::instrumented(
            "matrix_backend",
            "matrix_op",
            std::future::ready(Err("boom".to_string())),
        )
        .now_or_never()
        .expect("ready future");
        assert_eq!(failed, Err("boom".to_string()));
    });

    assert_eq!(logs.len(), 1, "successes are counted silently");
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_eq!(logs[0].field("backend"), Some("matrix_backend"));
    assert_eq!(logs[0].field("operation"), Some("matrix_op"));
    assert_eq!(logs[0].field("error"), Some("boom"));

    for outcome in ["ok", "error"] {
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "matrix_backend"),
                    ("operation", "matrix_op"),
                    ("outcome", outcome),
                ],
            ),
            1,
            "{outcome}"
        );
    }
}

/// Event identity fields survive the real formatter boundary as searchable
/// logfmt key/value pairs, without changing the human-readable message.
#[test]
fn event_identity_renders_through_logfmt() {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::prelude::*;

    #[derive(Clone)]
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for TestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut buffer = self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            buffer.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Event)]
    #[event(
        event_name = "test_matrix_logfmt_rendered",
        metric_name = "carbide_test_matrix_logfmt_total",
        component = "matrix-test",
        log = info,
        metric = counter,
        message = "logfmt identity rendered",
        describe = "Number of logfmt-rendering test events",
    )]
    struct LogfmtRendered {
        #[label]
        outcome: Outcome,
        #[context]
        machine_id: String,
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = TestWriter(buffer.clone());
    let layer = logfmt::layer().with_writer(Arc::new(move || Box::new(writer.clone())));
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit(LogfmtRendered {
            outcome: Outcome::Ok,
            machine_id: "machine-1".to_string(),
        });
    });

    let rendered = String::from_utf8(
        buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
    )
    .expect("logfmt output is UTF-8");
    assert!(
        rendered.contains("event_name=test_matrix_logfmt_rendered"),
        "{rendered}"
    );
    assert!(
        rendered.contains("metric_name=carbide_test_matrix_logfmt_total"),
        "{rendered}"
    );
    assert!(
        rendered.contains("msg=\"logfmt identity rendered\""),
        "{rendered}"
    );
    assert_eq!(rendered.matches("event_name=").count(), 1, "{rendered}");
    assert_eq!(rendered.matches("metric_name=").count(), 1, "{rendered}");
}

/// Two Events sharing one `MetricFamily` move a single instrument with a
/// single label set, and each keeps its own level, message, and context.
#[test]
fn one_family_backs_two_events() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
    enum FamilyStage {
        Fetch,
        Persist,
    }

    #[derive(MetricFamily)]
    #[metric(
        name = "carbide_test_matrix_family_total",
        kind = counter,
        component = "matrix-test",
        describe = "Number of matrix family test failures, by stage and outcome."
    )]
    struct MatrixFamily {
        stage: FamilyStage,
        outcome: Outcome,
    }

    #[derive(Event)]
    #[event(
        event_name = "test_matrix_family_fetch_failed",
        metric_family = MatrixFamily,
        log = warn,
        message = "matrix family fetch failed"
    )]
    struct FetchFailed {
        #[label]
        stage: FamilyStage,
        #[label]
        outcome: Outcome,
        #[context]
        url: String,
    }

    #[derive(Event)]
    #[event(
        event_name = "test_matrix_family_persist_failed",
        metric_family = MatrixFamily,
        log = error,
        message = "matrix family persist failed"
    )]
    struct PersistFailed {
        #[label]
        stage: FamilyStage,
        #[label]
        outcome: Outcome,
        #[context]
        machine: String,
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(FetchFailed {
            stage: FamilyStage::Fetch,
            outcome: Outcome::Error,
            url: "https://example.invalid".to_string(),
        });
        emit(PersistFailed {
            stage: FamilyStage::Persist,
            outcome: Outcome::Error,
            machine: "machine-7".to_string(),
        });
    });

    // The family supplies the metric identity to both Events.
    assert_eq!(
        <FetchFailed as Event>::METRIC_NAME,
        Some("carbide_test_matrix_family_total")
    );
    assert_eq!(
        <PersistFailed as Event>::METRIC_NAME,
        <FetchFailed as Event>::METRIC_NAME
    );
    assert_eq!(<PersistFailed as Event>::COMPONENT, "matrix-test");
    assert_eq!(
        <PersistFailed as Event>::DESCRIBE,
        <FetchFailed as Event>::DESCRIBE
    );

    // Each Event keeps its own log surface.
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_eq!(logs[0].message, "matrix family fetch failed");
    assert_eq!(logs[0].field("stage"), Some("fetch"));
    assert_eq!(logs[0].field("url"), Some("https://example.invalid"));
    assert_eq!(
        logs[0].field("metric_name"),
        Some("carbide_test_matrix_family_total")
    );
    assert_eq!(logs[1].level, tracing::Level::ERROR);
    assert_eq!(logs[1].field("machine"), Some("machine-7"));

    // One instrument, two label sets on the same family.
    assert_eq!(
        metrics.counter_delta(
            "carbide_test_matrix_family_total",
            &[("stage", "fetch"), ("outcome", "error")],
        ),
        1.0
    );
    assert_eq!(
        metrics.counter_delta(
            "carbide_test_matrix_family_total",
            &[("stage", "persist"), ("outcome", "error")],
        ),
        1.0
    );
}

/// A histogram family converts its `#[observation]` through the unit the
/// family's name declares, not one restated on the Event.
#[test]
fn a_histogram_family_converts_the_observation() {
    #[derive(MetricFamily)]
    #[metric(
        name = "carbide_test_matrix_family_duration_milliseconds",
        kind = histogram,
        component = "matrix-test",
        describe = "Duration of matrix family test work, by stage."
    )]
    struct WorkDuration {
        stage: Stage,
    }

    #[derive(Event)]
    #[event(
        event_name = "test_matrix_family_work_finished",
        metric_family = WorkDuration,
        log = off
    )]
    struct WorkFinished {
        #[label]
        stage: Stage,
        #[observation]
        took: Duration,
    }

    assert_eq!(
        <WorkFinished as Event>::METRIC,
        MetricKind::Histogram { unit: "ms" }
    );

    let event = WorkFinished {
        stage: Stage::Apply,
        took: Duration::from_millis(250),
    };
    // 250ms recorded in the family's declared milliseconds, not seconds.
    assert!((Event::observation(&event) - 250.0).abs() < f64::EPSILON);

    let metrics = MetricsCapture::start();
    emit(event);
    assert!(
        metrics
            .render()
            .contains("carbide_test_matrix_family_duration_milliseconds"),
        "the family's histogram is exported under its declared name"
    );
}

/// A context field written as `Option<...>` says it does not apply to every
/// case: `None` leaves the key off the log line entirely rather than writing a
/// blank or a sentinel, and `Event::context` agrees with the line.
#[test]
fn an_absent_optional_context_field_leaves_no_key() {
    use std::net::IpAddr;

    #[derive(Event)]
    #[event(
        event_name = "test_matrix_optional_context",
        component = "matrix-test",
        log = warn,
        message = "optional context"
    )]
    struct OptionalContext {
        #[label]
        outcome: Outcome,
        /// A typed field with no blank value: absent is the only honest form.
        #[context]
        peer: Option<IpAddr>,
        #[context]
        error: Option<String>,
        #[context]
        always: String,
    }

    let logs = capture_logs(|| {
        emit(OptionalContext {
            outcome: Outcome::Error,
            peer: Some(IpAddr::from([10, 0, 0, 5])),
            error: Some("boom".to_string()),
            always: "here".to_string(),
        });
        emit(OptionalContext {
            outcome: Outcome::Ok,
            peer: None,
            error: None,
            always: "here".to_string(),
        });
    });

    // Present: rendered through Display exactly as a non-optional field is.
    assert_eq!(logs[0].field("peer"), Some("10.0.0.5"));
    assert_eq!(logs[0].field("error"), Some("boom"));
    assert_eq!(logs[0].field("always"), Some("here"));

    // Absent: the key is not on the line at all -- not blank, not a sentinel.
    assert_eq!(logs[1].field("peer"), None);
    assert_eq!(logs[1].field("error"), None);
    assert_eq!(logs[1].field("always"), Some("here"));
    let keys: Vec<&str> = logs[1].fields.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        !keys.contains(&"peer"),
        "absent field must not appear: {keys:?}"
    );
    assert!(
        !keys.contains(&"error"),
        "absent field must not appear: {keys:?}"
    );

    // Each optional field stands on its own: one being absent says nothing
    // about the other.
    let mixed = capture_logs(|| {
        emit(OptionalContext {
            outcome: Outcome::Error,
            peer: Some(IpAddr::from([10, 0, 0, 9])),
            error: None,
            always: "here".to_string(),
        });
        emit(OptionalContext {
            outcome: Outcome::Error,
            peer: None,
            error: Some("only the error".to_string()),
            always: "here".to_string(),
        });
    });
    assert_eq!(mixed[0].field("peer"), Some("10.0.0.9"));
    assert_eq!(mixed[0].field("error"), None);
    assert_eq!(mixed[1].field("peer"), None);
    assert_eq!(mixed[1].field("error"), Some("only the error"));

    let peer_only = OptionalContext {
        outcome: Outcome::Error,
        peer: Some(IpAddr::from([10, 0, 0, 9])),
        error: None,
        always: "here".to_string(),
    };
    let peer_only_context = Event::context(&peer_only);
    let peer_only_keys: Vec<&str> = peer_only_context.iter().map(|kv| kv.key.as_str()).collect();
    assert_eq!(peer_only_keys, vec!["peer", "always"]);

    // `Event::context` is introspection for tooling, so it agrees with the line.
    let absent = OptionalContext {
        outcome: Outcome::Ok,
        peer: None,
        error: None,
        always: "here".to_string(),
    };
    let context = Event::context(&absent);
    let context_keys: Vec<&str> = context.iter().map(|kv| kv.key.as_str()).collect();
    assert_eq!(context_keys, vec!["always"]);
}

/// An enum Event: one metric, one `event_name`, a case per variant. Each
/// variant renders exactly its own fields, so a field that does not apply to
/// every case is simply absent from the variants that lack it -- no `Option`
/// stands in for it.
#[test]
fn enum_event_renders_each_variant_with_its_own_fields() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_enum_fired",
        metric_name = "carbide_test_matrix_enum_total",
        component = "matrix-test",
        metric = counter,
        log = warn,
        describe = "Number of enum-Event test cases",
        labels(stage: Stage, outcome: Outcome),
    )]
    enum EnumEvent {
        #[event(labels(stage = Apply, outcome = Ok), message = "applied")]
        Applied {
            #[context]
            host: String,
        },

        #[event(labels(stage = Apply), message = "apply failed")]
        ApplyFailed {
            #[label]
            outcome: Outcome,
            #[context]
            host: String,
            #[context]
            error: String,
        },

        /// Counted, never logged.
        #[event(labels(stage = PreFlight, outcome = Ok), log = off)]
        Verified {},
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(EnumEvent::Applied {
            host: "host-1".to_string(),
        });
        emit(EnumEvent::ApplyFailed {
            outcome: Outcome::Error,
            host: "host-2".to_string(),
            error: "boom".to_string(),
        });
        emit(EnumEvent::Verified {});
    });

    // `Verified` is metric-only, so only two records.
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].metadata_name, "test_matrix_enum_fired");
    assert_eq!(logs[0].message, "applied");
    assert_eq!(logs[0].field("stage"), Some("apply"));
    assert_eq!(logs[0].field("outcome"), Some("ok"));
    assert_eq!(logs[0].field("host"), Some("host-1"));
    // The variant has no `error` field at all, so the key is absent.
    assert_eq!(logs[0].field("error"), None);

    assert_eq!(logs[1].message, "apply failed");
    assert_eq!(logs[1].field("outcome"), Some("error"));
    assert_eq!(logs[1].field("error"), Some("boom"));

    for (stage, outcome) in [("apply", "ok"), ("apply", "error"), ("pre_flight", "ok")] {
        assert_eq!(
            metrics.counter_delta(
                "carbide_test_matrix_enum_total",
                &[("stage", stage), ("outcome", outcome)],
            ),
            1.0,
            "{stage}/{outcome}",
        );
    }

    // Zero-series initialization takes any Event, including one variant of an enum.
    assert!(initialize_counter_series(&EnumEvent::Verified {}));
}

/// A family-backed enum Event: the family owns the metric and its label
/// schema, and each variant names its label values in full. Several such enums
/// can share one family, which is how a metric with many cases stays split into
/// readable types instead of one enormous enum.
#[test]
fn family_backed_enum_event_shares_one_metric() {
    #[derive(MetricFamily)]
    #[metric(
        name = "carbide_test_matrix_family_enum_total",
        kind = counter,
        component = "matrix-test",
        describe = "Number of family-backed enum test cases"
    )]
    struct SharedFailures {
        stage: Stage,
        outcome: Outcome,
    }

    #[derive(Event)]
    #[event(event_name = "test_matrix_family_enum_a", metric_family = SharedFailures, log = warn)]
    enum GroupA {
        #[event(labels(stage = Stage::Apply, outcome = Outcome::Ok), message = "a ok")]
        Ok {
            #[context]
            host: String,
        },

        #[event(labels(stage = Stage::Apply, outcome = Outcome::Error), message = "a failed")]
        Failed {
            #[context]
            host: String,
            #[context]
            error: String,
        },
    }

    #[derive(Event)]
    #[event(event_name = "test_matrix_family_enum_b", metric_family = SharedFailures, log = error)]
    enum GroupB {
        #[event(labels(stage = Stage::PreFlight), message = "b failed")]
        Failed {
            #[label]
            outcome: Outcome,
            #[context]
            detail: String,
        },
    }

    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(GroupA::Ok {
            host: "h1".to_string(),
        });
        emit(GroupA::Failed {
            host: "h2".to_string(),
            error: "boom".to_string(),
        });
        emit(GroupB::Failed {
            outcome: Outcome::Error,
            detail: "d".to_string(),
        });
    });

    assert_eq!(logs.len(), 3);
    // Two enums, two event_names, one metric.
    assert_eq!(
        logs[0].field("event_name"),
        Some("test_matrix_family_enum_a")
    );
    assert_eq!(
        logs[2].field("event_name"),
        Some("test_matrix_family_enum_b")
    );
    for log in &logs {
        assert_eq!(
            log.field("metric_name"),
            Some("carbide_test_matrix_family_enum_total")
        );
    }
    // Labels fixed by the variant still render on the log line.
    assert_eq!(logs[0].field("stage"), Some("apply"));
    assert_eq!(logs[0].field("outcome"), Some("ok"));
    assert_eq!(logs[2].field("stage"), Some("pre_flight"));
    assert_eq!(logs[2].field("outcome"), Some("error"));
    // Only the failing variant has an error field at all.
    assert_eq!(logs[0].field("error"), None);
    assert_eq!(logs[1].field("error"), Some("boom"));

    for (stage, outcome) in [("apply", "ok"), ("apply", "error"), ("pre_flight", "error")] {
        assert_eq!(
            metrics.counter_delta(
                "carbide_test_matrix_family_enum_total",
                &[("stage", stage), ("outcome", outcome)],
            ),
            1.0,
            "{stage}/{outcome}",
        );
    }
}

/// An enum Event on a histogram family records each variant's `#[observation]`
/// through the unit the family declares. The family owns the kind, so a variant
/// records the value it was given rather than counting the emit.
#[test]
fn histogram_family_enum_records_each_variants_observation() {
    #[derive(MetricFamily)]
    #[metric(
        name = "carbide_test_matrix_family_enum_duration_milliseconds",
        kind = histogram,
        component = "matrix-test",
        describe = "Duration of family-backed enum test work, by outcome."
    )]
    struct EnumWorkDuration {
        outcome: Outcome,
    }

    #[derive(Event)]
    #[event(
        event_name = "test_matrix_family_enum_work_finished",
        metric_family = EnumWorkDuration
    )]
    enum EnumWorkFinished {
        #[event(labels(outcome = Outcome::Ok), log = off)]
        Succeeded {
            #[observation]
            took: Duration,
        },

        #[event(labels(outcome = Outcome::Error), log = warn, message = "work failed")]
        Failed {
            #[observation]
            took: Duration,
            #[context]
            error: String,
        },
    }

    assert_eq!(
        <EnumWorkFinished as Event>::METRIC,
        MetricKind::Histogram { unit: "ms" }
    );

    // The value recorded is the duration, converted through the family's unit --
    // not the 1.0 a counter would add.
    let failed = EnumWorkFinished::Failed {
        took: Duration::from_millis(250),
        error: "boom".to_string(),
    };
    assert!((Event::observation(&failed) - 250.0).abs() < f64::EPSILON);
    assert!(
        (Event::observation(&EnumWorkFinished::Succeeded {
            took: Duration::from_millis(40),
        }) - 40.0)
            .abs()
            < f64::EPSILON
    );

    let metrics = MetricsCapture::start();
    emit(failed);
    assert!(
        metrics
            .render()
            .contains("carbide_test_matrix_family_enum_duration_milliseconds"),
        "the family's histogram is exported under its declared name"
    );
}

/// `Event::context` on an enum answers for the variant in hand: the same fields
/// its log line renders, and nothing belonging to a sibling case.
#[test]
fn enum_event_context_answers_for_the_variant() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_enum_context",
        metric_name = "carbide_test_matrix_enum_context_total",
        component = "matrix-test",
        metric = counter,
        describe = "Number of enum context test cases.",
        labels(outcome: Outcome)
    )]
    enum ContextCase {
        #[event(labels(outcome = Outcome::Ok), log = off)]
        Ok {
            #[context]
            host: String,
        },

        #[event(labels(outcome = Outcome::Error), log = warn, message = "context failed")]
        Failed {
            #[context]
            host: String,
            #[context]
            error: String,
            /// Absent on the cases it does not apply to.
            #[context]
            detail: Option<String>,
        },
    }

    let pairs = |event: &ContextCase| -> Vec<(String, String)> {
        Event::context(event)
            .into_iter()
            .map(|kv| (kv.key.to_string(), kv.value.to_string()))
            .collect()
    };

    // The success case carries only its own field, not the failure's.
    assert_eq!(
        pairs(&ContextCase::Ok {
            host: "h1".to_string(),
        }),
        vec![("host".to_string(), "h1".to_string())]
    );

    assert_eq!(
        pairs(&ContextCase::Failed {
            host: "h2".to_string(),
            error: "boom".to_string(),
            detail: Some("stage 2".to_string()),
        }),
        vec![
            ("host".to_string(), "h2".to_string()),
            ("error".to_string(), "boom".to_string()),
            ("detail".to_string(), "stage 2".to_string()),
        ]
    );

    // An absent optional context field leaves no key, exactly as on the log line.
    assert_eq!(
        pairs(&ContextCase::Failed {
            host: "h3".to_string(),
            error: "boom".to_string(),
            detail: None,
        }),
        vec![
            ("host".to_string(), "h3".to_string()),
            ("error".to_string(), "boom".to_string()),
        ]
    );
}

/// A gauge Event sets its metric to the latest `#[observation]`: it holds a
/// level rather than accumulating, so a second emit replaces the first rather
/// than adding to it.
#[test]
fn a_gauge_event_holds_the_latest_observation() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_pool_size_observed",
        metric_name = "carbide_test_matrix_pool_members",
        component = "matrix-test",
        log = off,
        metric = gauge,
        describe = "Number of members in the matrix test pool"
    )]
    struct PoolSizeObserved {
        #[observation]
        members: u64,
    }

    assert_eq!(
        <PoolSizeObserved as Event>::METRIC,
        MetricKind::Gauge { unit: "" }
    );

    let metrics = MetricsCapture::start();
    emit(PoolSizeObserved { members: 7 });
    assert_eq!(
        metrics.gauge_value("carbide_test_matrix_pool_members", &[]),
        7.0
    );

    // A level, not a total: the second emit replaces the first.
    emit(PoolSizeObserved { members: 3 });
    assert_eq!(
        metrics.gauge_value("carbide_test_matrix_pool_members", &[]),
        3.0
    );
}

/// A gauge whose name ends in a unit suffix records through that unit, the same
/// conversion a histogram gets.
#[test]
fn a_gauge_converts_through_its_unit_suffix() {
    #[derive(Event)]
    #[event(
        event_name = "test_matrix_refresh_window_observed",
        metric_name = "carbide_test_matrix_refresh_window_seconds",
        component = "matrix-test",
        log = off,
        metric = gauge,
        describe = "Time remaining in the matrix test refresh window"
    )]
    struct RefreshWindowObserved {
        #[observation]
        remaining: Duration,
    }

    assert_eq!(
        <RefreshWindowObserved as Event>::METRIC,
        MetricKind::Gauge { unit: "s" }
    );

    let event = RefreshWindowObserved {
        remaining: Duration::from_millis(2500),
    };
    // 2500ms observed as 2.5 seconds, per the name's suffix.
    assert!((Event::observation(&event) - 2.5).abs() < f64::EPSILON);

    let metrics = MetricsCapture::start();
    emit(event);
    assert_eq!(
        metrics.gauge_value("carbide_test_matrix_refresh_window_seconds", &[]),
        2.5
    );
}
