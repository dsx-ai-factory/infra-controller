// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use prometheus::IntGauge;
use prometheus::core::{Collector, Desc};

use super::*;
use crate::metrics::{CollectorRegistry, GaugeReading, MetricLabel, export_registry};

#[test]
fn direct_and_gzip_match_complete_registry_output_across_namespaces() {
    let manager = MetricsManager::new("test").unwrap();
    let mut owners = Vec::new();

    for namespace in 0..2 {
        let owner = manager
            .create_telemetry_collector_registry(format!("owner_{namespace}"), "test")
            .unwrap();

        let gauges = owner
            .create_gauge_metrics(
                "shared_id".into(),
                "Readings\\with\nhelp",
                vec![("endpoint".into(), namespace.to_string())],
            )
            .unwrap();

        for index in 0..10001usize {
            let value = match index % 7 {
                0 => f64::NAN,
                1 => f64::INFINITY,
                2 => f64::NEG_INFINITY,
                3 => -0.0,
                4 => -12.5,
                _ => index as f64 / 17.0,
            };

            let semantics = if index.is_multiple_of(3) {
                MetricSemantics::CumulativeCounter {
                    start_time: None,
                    description: "Counter\\help\ntext",
                }
            } else {
                MetricSemantics::Gauge
            };

            let mut labels = vec![("sensor".into(), format!("{index:08}\"\\\n\t\r\u{00e9}"))];

            if index.is_multiple_of(2) {
                labels.push(("status".into(), "healthy".to_string()));
            }

            gauges.record(
                GaugeReading::new(
                    format!("reading-{index}"),
                    "temperature",
                    "sensor",
                    "celsius",
                    value,
                )
                .with_labels(labels)
                .with_semantics(semantics),
            );
        }

        owners.push(owner);
    }

    let expected = export_registry(&manager.telemetry_registry).unwrap();
    let direct = manager.export_telemetry().unwrap();

    assert_eq!(direct, expected);
    assert!(!lock(&manager.telemetry_state).generic);

    for level in [1, 6, 9] {
        let compressed = export(&manager, Some(level), &CancellationToken::new()).unwrap();
        let mut decoded = String::new();
        flate2::read::GzDecoder::new(compressed.as_slice())
            .read_to_string(&mut decoded)
            .unwrap();

        assert_eq!(decoded, expected, "level {level}");
    }
}

#[test]
fn registered_handles_and_snapshots_obey_removal_lifetimes() {
    let (manager, owner, gauges) = recorded_gauge(Vec::new());

    let original_handle = Arc::downgrade(&gauges);
    drop(gauges);

    assert!(manager.export_telemetry().unwrap().contains(" 1\n"));

    let gauges = original_handle.upgrade().unwrap();

    assert!(
        owner
            .create_gauge_metrics("stream".into(), "Readings", Vec::new())
            .is_err()
    );

    let cancel = CancellationToken::new();
    let captured = Snapshot::capture(&manager, &cancel).unwrap().unwrap();

    let reading = Arc::downgrade(&captured.streams[0].readings[0]);

    for value in [2.0, 3.0] {
        gauges.record(GaugeReading::new(
            "one",
            "temperature",
            "sensor",
            "celsius",
            value,
        ));
    }

    assert!(manager.export_telemetry().unwrap().contains(" 3\n"));

    owner.unregister_gauge_metrics(&gauges).unwrap();
    let mut before_removal = Vec::new();

    encode_families(
        captured.families(&cancel).unwrap().unwrap(),
        &mut before_removal,
        &cancel,
    )
    .unwrap();

    assert!(manager.export_telemetry().unwrap().is_empty());
    assert!(String::from_utf8(before_removal).unwrap().contains(" 1\n"));
    assert!(reading.upgrade().is_some());

    drop(captured);

    assert!(reading.upgrade().is_none());
    assert!(owner.unregister_gauge_metrics(&gauges).is_err());

    let replacement = owner
        .create_gauge_metrics("stream".into(), "Readings", Vec::new())
        .unwrap();

    replacement.record(GaugeReading::new(
        "new",
        "replacement",
        "sensor",
        "celsius",
        3.0,
    ));

    let survivor = manager
        .create_telemetry_collector_registry("survivor".into(), "test")
        .unwrap();

    survivor
        .create_gauge_metrics("stream".into(), "Readings", Vec::new())
        .unwrap()
        .record(GaugeReading::new(
            "other", "survivor", "sensor", "celsius", 4.0,
        ));

    drop(owner);
    let result = manager.export_telemetry().unwrap();

    assert!(!result.contains("replacement"));
    assert!(result.contains("survivor"));
}

#[test]
fn raw_registry_handles_keep_future_custom_registrations_visible() {
    let (manager, owner, gauges) = recorded_gauge(Vec::new());
    drop(gauges);

    let raw = owner.registry().clone();
    let before = manager.export_telemetry().unwrap();
    let custom = IntGauge::new("custom_gauge", "Custom collector").unwrap();

    custom.set(23);
    raw.register(Box::new(custom)).unwrap();
    let expected = export_registry(&manager.telemetry_registry).unwrap();

    assert!(lock(&manager.telemetry_state).generic);
    assert!(!before.contains("custom_gauge"));
    assert_eq!(manager.export_telemetry().unwrap(), expected);
    assert!(expected.contains("custom_gauge 23\n"));
    assert!(expected.contains("test_temperature_sensor_celsius 1\n"));
}

#[test]
fn conflicting_family_metadata_uses_generic_export_before_writing() {
    for alias_collision in [false, true] {
        let manager = MetricsManager::new("test").unwrap();

        let [(_first, a), (_second, b)] =
            [("first", "First help"), ("second", "Second help")].map(|(id, help)| {
                let owner = manager
                    .create_telemetry_collector_registry(id.into(), "test")
                    .unwrap();

                let gauge = owner
                    .create_gauge_metrics(
                        "stream".into(),
                        help,
                        vec![("endpoint".into(), id.into())],
                    )
                    .unwrap();

                (owner, gauge)
            });

        if alias_collision {
            a.record(
                GaugeReading::new("one", "requests", "count", "units", 1.0).with_semantics(
                    MetricSemantics::CumulativeCounter {
                        start_time: None,
                        description: "Counter help",
                    },
                ),
            );

            b.record(GaugeReading::new(
                "two",
                "requests",
                "count",
                "units_total",
                2.0,
            ));
        } else {
            a.record(GaugeReading::new("one", "a_b", "c", "units", 1.0));
            b.record(GaugeReading::new("two", "a", "b_c", "units", 2.0));
        }

        let cancel = CancellationToken::new();
        let snapshot = Snapshot::capture(&manager, &cancel).unwrap().unwrap();

        assert!(snapshot.families(&cancel).unwrap().is_none());

        assert_eq!(
            manager.export_telemetry().unwrap(),
            export_registry(&manager.telemetry_registry).unwrap()
        );
    }
}

#[test]
fn cancelled_exports_and_failed_writes_preserve_the_error() {
    struct FailedWriter;

    impl Write for FailedWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected write failure",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let (manager, _owner, gauges) = recorded_gauge(Vec::new());
    drop(gauges);

    let cancel = CancellationToken::new();
    cancel.cancel();

    assert!(
        matches!(export(&manager, None, &cancel), Err(Error::Io(error)) if error.kind() == io::ErrorKind::Interrupted)
    );

    assert!(
        matches!(encode(&manager, &mut FailedWriter, &CancellationToken::new()), Err(Error::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe)
    );
}

#[test]
fn encoding_observes_cancellation_within_an_output_batch() {
    struct CancellingWriter {
        cancel: CancellationToken,
        bytes: usize,
    }

    impl Write for CancellingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes += bytes.len();
            self.cancel.cancel();

            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let (manager, _owner, _gauges) =
        recorded_gauge(vec![("sensor".into(), "x".repeat(ENCODE_BATCH * 3))]);

    let cancel = CancellationToken::new();

    let mut writer = CancellingWriter {
        cancel: cancel.clone(),
        bytes: 0,
    };

    let result = encode(&manager, &mut writer, &cancel);

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == io::ErrorKind::ConnectionAborted)
    );

    assert_eq!(writer.bytes, ENCODE_BATCH);
}

#[test]
fn raw_unregister_releases_readings_without_waiting_for_namespace_drop() {
    let (manager, owner, gauges) = recorded_gauge(Vec::new());

    let reading = {
        let entry = gauges.gauges.iter().next().unwrap();
        Arc::downgrade(entry.value())
    };

    let raw = owner.registry().clone();

    raw.unregister(Box::new((*gauges).clone())).unwrap();
    drop(gauges);

    assert!(manager.export_telemetry().unwrap().is_empty());
    assert!(reading.upgrade().is_none());

    let next = owner
        .create_gauge_metrics("second".into(), "Readings", Vec::new())
        .unwrap();

    let handle = Arc::downgrade(&next);

    drop(next);

    assert!(handle.upgrade().is_none());
    assert!(lock(&manager.telemetry_state).gauges.is_empty());
}

struct RegistryReadingString {
    owner: Arc<CollectorRegistry>,
    worker: mpsc::Sender<(JoinHandle<()>, bool)>,
}

impl From<RegistryReadingString> for String {
    fn from(input: RegistryReadingString) -> Self {
        let (completed_tx, completed_rx) = mpsc::channel();
        let owner = input.owner;

        let worker = thread::spawn(move || {
            let gauge = IntGauge::new("from_conversion", "Conversion callback").unwrap();
            gauge.set(7);
            owner.registry().register(Box::new(gauge)).unwrap();
            let _ = completed_tx.send(());
        });

        let completed = completed_rx.recv_timeout(Duration::from_secs(5)).is_ok();
        input.worker.send((worker, completed)).unwrap();

        "callback".into()
    }
}

#[test]
fn caller_string_conversions_can_access_telemetry_registry_handles() {
    struct RejectedPrefix;

    impl From<RejectedPrefix> for String {
        fn from(_: RejectedPrefix) -> Self {
            panic!("duplicate namespace must fail before prefix conversion");
        }
    }

    for scenario in ["prefix", "help", "duplicate_help"] {
        let manager = MetricsManager::new("test").unwrap();

        let owner = Arc::new(
            manager
                .create_telemetry_collector_registry("owner".into(), "test")
                .unwrap(),
        );

        if scenario == "duplicate_help" {
            owner
                .create_gauge_metrics("stream".into(), "Original help", Vec::new())
                .unwrap();
        }

        let (worker_tx, worker_rx) = mpsc::channel();

        let input = RegistryReadingString {
            owner: owner.clone(),
            worker: worker_tx,
        };

        match scenario {
            "prefix" => {
                let namespace = manager
                    .create_telemetry_collector_registry("second".into(), input)
                    .unwrap();

                assert!(
                    manager
                        .create_telemetry_collector_registry("second".into(), RejectedPrefix)
                        .is_err()
                );

                drop(namespace);
            }
            "help" => {
                owner
                    .create_gauge_metrics("stream".into(), input, Vec::new())
                    .unwrap();
            }
            _ => {
                assert!(matches!(
                    owner.create_gauge_metrics("stream".into(), input, Vec::new()),
                    Err(Error::AlreadyReg)
                ));
            }
        }

        let (worker, completed) = worker_rx.recv().unwrap();
        worker.join().unwrap();

        assert!(completed, "{scenario}");

        assert!(
            manager
                .export_telemetry()
                .unwrap()
                .contains("from_conversion 7\n")
        );
    }
}

#[test]
fn generic_collector_callbacks_can_access_registry_during_managed_mutation() {
    struct ReadingCollector {
        gauge: IntGauge,
        other: Arc<CollectorRegistry>,
        entered: mpsc::Sender<()>,
        mutation: Mutex<mpsc::Receiver<()>>,
        accessed: mpsc::Sender<()>,
    }

    impl Collector for ReadingCollector {
        fn desc(&self) -> Vec<&Desc> {
            self.gauge.desc()
        }

        fn collect(&self) -> Vec<proto::MetricFamily> {
            self.entered.send(()).unwrap();
            self.mutation.lock().unwrap().recv().unwrap();
            self.other.registry();
            self.accessed.send(()).unwrap();
            self.gauge.collect()
        }
    }

    let manager = Arc::new(MetricsManager::new("test").unwrap());

    let [owner, other] = ["owner", "other"].map(|id| {
        Arc::new(
            manager
                .create_telemetry_collector_registry(id.into(), "test")
                .unwrap(),
        )
    });

    let gauge = IntGauge::new("custom_reading", "Custom collector").unwrap();
    gauge.set(17);

    let (entered_tx, entered_rx) = mpsc::channel();
    let (mutation_tx, mutation_rx) = mpsc::channel();
    let (accessed_tx, accessed_rx) = mpsc::channel();
    owner
        .registry()
        .register(Box::new(ReadingCollector {
            gauge,
            other,
            entered: entered_tx,
            mutation: Mutex::new(mutation_rx),
            accessed: accessed_tx,
        }))
        .unwrap();

    let once = AtomicBool::new(false);
    let accessed_rx = Mutex::new(accessed_rx);
    lock(&manager.telemetry_state).before_mutation = Some(Arc::new(move || {
        if once.swap(true, AtomicOrdering::SeqCst) {
            return;
        }

        mutation_tx.send(()).unwrap();
        accessed_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .expect("collector callback must complete before managed mutation");
    }));

    let export_manager = manager.clone();
    let scrape = thread::spawn(move || export_manager.export_telemetry());

    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Keep the namespace alive until both workers drain after a callback failure.
    let mutation_owner = owner.clone();

    let mutation = thread::spawn(move || {
        mutation_owner.create_gauge_metrics("stream".into(), "Readings", Vec::new())
    });

    let mutation_result = mutation.join();
    let exported = scrape.join().unwrap().unwrap();

    lock(&manager.telemetry_state).before_mutation = None;
    drop(owner);

    assert!(mutation_result.is_ok());
    assert!(mutation_result.unwrap().is_ok());
    assert!(exported.contains("custom_reading 17\n"));
}

fn recorded_gauge(
    labels: Vec<MetricLabel>,
) -> (MetricsManager, CollectorRegistry, Arc<GaugeMetrics>) {
    let manager = MetricsManager::new("test").unwrap();

    let owner = manager
        .create_telemetry_collector_registry("owner".into(), "test")
        .unwrap();

    let gauges = owner
        .create_gauge_metrics("stream".into(), "Readings", Vec::new())
        .unwrap();

    gauges.record(
        GaugeReading::new("one", "temperature", "sensor", "celsius", 1.0).with_labels(labels),
    );

    (manager, owner, gauges)
}
