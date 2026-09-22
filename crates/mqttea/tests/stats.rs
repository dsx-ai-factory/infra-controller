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

// tests/stats.rs
// Unit tests for statistics tracking functionality including queue statistics
// for received messages and publish statistics for sent messages.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use mqttea::stats::{ConnectionStateTracker, PublishStatsTracker, QueueStatsTracker};

// Tests for QueueStatsTracker creation and initial state
#[test]
fn test_queue_stats_tracker_creation() {
    let tracker = QueueStatsTracker::new();
    let stats = tracker.to_stats();

    assert_eq!(stats.pending_messages, 0);
    assert_eq!(stats.pending_bytes, 0);
    assert_eq!(stats.total_processed, 0);
    assert_eq!(stats.total_failed, 0);
    assert_eq!(stats.total_bytes_processed, 0);
    assert!(tracker.is_empty());
}

#[test]
fn test_queue_stats_increment_pending() {
    let tracker = QueueStatsTracker::new();

    // Add some pending messages
    tracker.increment_pending(100); // First message: 100 bytes
    tracker.increment_pending(250); // Second message: 250 bytes
    tracker.increment_pending(75); // Third message: 75 bytes

    let stats = tracker.to_stats();
    assert_eq!(stats.pending_messages, 3);
    assert_eq!(stats.pending_bytes, 425);
    assert_eq!(stats.total_processed, 0);
    assert_eq!(stats.total_failed, 0);
    assert!(!tracker.is_empty());
}

#[test]
fn test_queue_stats_process_messages() {
    let tracker = QueueStatsTracker::new();

    // Add pending messages
    tracker.increment_pending(150);
    tracker.increment_pending(200);

    // Process one message successfully
    tracker.decrement_pending_increment_processed(150);

    let stats = tracker.to_stats();
    assert_eq!(stats.pending_messages, 1);
    assert_eq!(stats.pending_bytes, 200);
    assert_eq!(stats.total_processed, 1);
    assert_eq!(stats.total_bytes_processed, 150);
    assert_eq!(stats.total_failed, 0);
}

#[test]
fn test_queue_stats_fail_messages() {
    let tracker = QueueStatsTracker::new();

    // Add pending messages
    tracker.increment_pending(300);
    tracker.increment_pending(400);

    // Fail one message
    tracker.decrement_pending_increment_failed(300);

    let stats = tracker.to_stats();
    assert_eq!(stats.pending_messages, 1);
    assert_eq!(stats.pending_bytes, 400);
    assert_eq!(stats.total_processed, 0);
    assert_eq!(stats.total_failed, 1);
    assert_eq!(stats.total_bytes_processed, 0);
}

#[test]
fn test_queue_stats_mixed_operations() {
    let tracker = QueueStatsTracker::new();

    // Simulate realistic message processing flow
    tracker.increment_pending(100); // Cat photo received
    tracker.increment_pending(250); // Dog video received
    tracker.increment_pending(75); // Bird song received
    tracker.increment_pending(180); // Fish feeding schedule received

    // Process some successfully
    tracker.decrement_pending_increment_processed(100); // Cat photo processed
    tracker.decrement_pending_increment_processed(250); // Dog video processed

    // Fail one
    tracker.decrement_pending_increment_failed(75); // Bird song failed

    let stats = tracker.to_stats();
    assert_eq!(stats.pending_messages, 1); // Only fish schedule pending
    assert_eq!(stats.pending_bytes, 180);
    assert_eq!(stats.total_processed, 2); // Cat and dog
    assert_eq!(stats.total_bytes_processed, 350); // 100 + 250
    assert_eq!(stats.total_failed, 1); // Bird song
}

#[test]
fn test_queue_stats_is_empty() {
    let tracker = QueueStatsTracker::new();

    assert!(tracker.is_empty());

    tracker.increment_pending(50);
    assert!(!tracker.is_empty());

    tracker.decrement_pending_increment_processed(50);
    assert!(tracker.is_empty());
}

#[test]
fn test_queue_stats_reset() {
    let tracker = QueueStatsTracker::new();

    // Add some activity
    tracker.increment_pending(100);
    tracker.decrement_pending_increment_processed(100);
    tracker.increment_pending(200);
    tracker.decrement_pending_increment_failed(200);

    let stats_before = tracker.to_stats();
    assert_eq!(stats_before.total_processed, 1);
    assert_eq!(stats_before.total_failed, 1);
    assert_eq!(stats_before.total_bytes_processed, 100);

    // Reset should clear processed/failed but not pending
    tracker.reset_counters();

    let stats_after = tracker.to_stats();
    assert_eq!(stats_after.total_processed, 0);
    assert_eq!(stats_after.total_failed, 0);
    assert_eq!(stats_after.total_bytes_processed, 0);
    // Pending counts should remain unchanged by reset
    assert_eq!(stats_after.pending_messages, 0);
    assert_eq!(stats_after.pending_bytes, 0);
}

// Tests for PublishStatsTracker creation and initial state
#[test]
fn test_publish_stats_tracker_creation() {
    let tracker = PublishStatsTracker::new();
    let stats = tracker.to_stats();

    assert_eq!(stats.total_published, 0);
    assert_eq!(stats.total_failed, 0);
    assert_eq!(stats.total_bytes_published, 0);
}

#[test]
fn test_publish_stats_increment_published() {
    let tracker = PublishStatsTracker::new();

    // Publish some messages
    tracker.increment_published(150); // Turtle update
    tracker.increment_published(300); // Lizard status
    tracker.increment_published(75); // Gecko position

    let stats = tracker.to_stats();
    assert_eq!(stats.total_published, 3);
    assert_eq!(stats.total_bytes_published, 525); // 150 + 300 + 75
    assert_eq!(stats.total_failed, 0);
}

#[test]
fn test_publish_stats_increment_failed() {
    let tracker = PublishStatsTracker::new();

    // Some successful publishes
    tracker.increment_published(200);
    tracker.increment_published(150);

    // Some failures
    tracker.increment_failed();
    tracker.increment_failed();
    tracker.increment_failed();

    let stats = tracker.to_stats();
    assert_eq!(stats.total_published, 2);
    assert_eq!(stats.total_bytes_published, 350);
    assert_eq!(stats.total_failed, 3);
}

#[test]
fn test_publish_stats_mixed_operations() {
    let tracker = PublishStatsTracker::new();

    // Simulate realistic publish pattern
    tracker.increment_published(120); // Horse gallop notification sent
    tracker.increment_failed(); // Pony message failed to send
    tracker.increment_published(280); // Donkey status update sent
    tracker.increment_published(90); // Mule location update sent
    tracker.increment_failed(); // Zebra alert failed to send
    tracker.increment_failed(); // Unicorn message failed (unrealistic but fun)

    let stats = tracker.to_stats();
    assert_eq!(stats.total_published, 3);
    assert_eq!(stats.total_bytes_published, 490); // 120 + 280 + 90
    assert_eq!(stats.total_failed, 3);
}

#[test]
fn test_publish_stats_reset() {
    let tracker = PublishStatsTracker::new();

    // Add some activity
    tracker.increment_published(500);
    tracker.increment_published(250);
    tracker.increment_failed();
    tracker.increment_failed();

    let stats_before = tracker.to_stats();
    assert_eq!(stats_before.total_published, 2);
    assert_eq!(stats_before.total_bytes_published, 750);
    assert_eq!(stats_before.total_failed, 2);

    // Reset should clear all counters
    tracker.reset_counters();

    let stats_after = tracker.to_stats();
    assert_eq!(stats_after.total_published, 0);
    assert_eq!(stats_after.total_bytes_published, 0);
    assert_eq!(stats_after.total_failed, 0);
}

// Tests for thread safety (important for real-world usage)
#[test]
fn test_queue_stats_thread_safety() {
    let tracker = Arc::new(QueueStatsTracker::new());
    let mut handles = vec![];

    // Spawn multiple threads that operate on the tracker
    for i in 0..5 {
        let tracker_clone = Arc::clone(&tracker);
        let handle = thread::spawn(move || {
            for j in 0..10 {
                let message_size = (i * 10 + j + 1) * 10; // Varying message sizes
                tracker_clone.increment_pending(message_size);

                // Sometimes process, sometimes fail
                if (i + j) % 3 == 0 {
                    tracker_clone.decrement_pending_increment_processed(message_size);
                } else if (i + j) % 7 == 0 {
                    tracker_clone.decrement_pending_increment_failed(message_size);
                }

                // Small delay to increase chance of race conditions
                thread::sleep(Duration::from_millis(1));
            }
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    let stats = tracker.to_stats();

    // Verify that the operations completed without panicking
    // Exact values depend on timing, but should be reasonable
    assert!(stats.pending_messages + stats.total_processed + stats.total_failed > 0);
    assert!(stats.pending_bytes + stats.total_bytes_processed > 0);
}

#[test]
fn test_publish_stats_thread_safety() {
    let tracker = Arc::new(PublishStatsTracker::new());
    let mut handles = vec![];

    // Spawn multiple threads that operate on the tracker
    for i in 0..5 {
        let tracker_clone = Arc::clone(&tracker);
        let handle = thread::spawn(move || {
            for j in 0..10 {
                let message_size = (i * 10 + j + 1) * 15;

                // Sometimes succeed, sometimes fail
                if (i + j) % 4 == 0 {
                    tracker_clone.increment_failed();
                } else {
                    tracker_clone.increment_published(message_size);
                }

                thread::sleep(Duration::from_millis(1));
            }
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    let stats = tracker.to_stats();

    // Verify operations completed without corruption
    assert_eq!(stats.total_published + stats.total_failed, 50); // 5 threads * 10 operations each
    assert!(stats.total_bytes_published > 0);
}

// Edge case and stress tests
#[test]
fn test_queue_stats_large_numbers() {
    let tracker = QueueStatsTracker::new();

    // Test with large message sizes
    tracker.increment_pending(1_000_000); // 1MB message
    tracker.increment_pending(5_000_000); // 5MB message
    tracker.increment_pending(10_000_000); // 10MB message

    let stats = tracker.to_stats();
    assert_eq!(stats.pending_messages, 3);
    assert_eq!(stats.pending_bytes, 16_000_000);

    // Process the large messages
    tracker.decrement_pending_increment_processed(1_000_000);
    tracker.decrement_pending_increment_processed(5_000_000);
    tracker.decrement_pending_increment_processed(10_000_000);

    let final_stats = tracker.to_stats();
    assert_eq!(final_stats.pending_messages, 0);
    assert_eq!(final_stats.total_processed, 3);
    assert_eq!(final_stats.total_bytes_processed, 16_000_000);
    assert!(tracker.is_empty());
}

#[test]
fn test_publish_stats_large_numbers() {
    let tracker = PublishStatsTracker::new();

    // Test with large publish volumes
    for i in 0..1000 {
        let size = if i % 10 == 0 { 100_000 } else { 1_000 }; // Mostly small, some large
        tracker.increment_published(size);
    }

    let stats = tracker.to_stats();
    assert_eq!(stats.total_published, 1000);

    // Calculate expected bytes: 100 large messages (100,000 bytes) + 900 small (1,000 bytes)
    let expected_bytes = (100 * 100_000) + (900 * 1_000);
    assert_eq!(stats.total_bytes_published, expected_bytes);
}

#[test]
fn test_queue_stats_zero_byte_messages() {
    let tracker = QueueStatsTracker::new();

    // Test with zero-byte messages (edge case)
    tracker.increment_pending(0);
    tracker.increment_pending(0);
    tracker.increment_pending(0);

    let stats = tracker.to_stats();
    assert_eq!(stats.pending_messages, 3);
    assert_eq!(stats.pending_bytes, 0);

    tracker.decrement_pending_increment_processed(0);
    tracker.decrement_pending_increment_failed(0);

    let final_stats = tracker.to_stats();
    assert_eq!(final_stats.pending_messages, 1);
    assert_eq!(final_stats.total_processed, 1);
    assert_eq!(final_stats.total_failed, 1);
    assert_eq!(final_stats.pending_bytes, 0);
    assert_eq!(final_stats.total_bytes_processed, 0);
}

#[test]
fn test_publish_stats_zero_byte_messages() {
    let tracker = PublishStatsTracker::new();

    // Test with zero-byte publishes
    tracker.increment_published(0);
    tracker.increment_published(0);
    tracker.increment_published(0);

    let stats = tracker.to_stats();
    assert_eq!(stats.total_published, 3);
    assert_eq!(stats.total_bytes_published, 0);
}

// Tests for register_metrics: the observable instruments expose the
// trackers' atomics through a local meter provider backed by a plain
// prometheus registry (gathering the registry runs the callbacks).

// test_meter builds a meter whose instruments export into the returned
// prometheus registry. The provider must stay alive for the registry to
// keep collecting.
fn test_meter() -> (
    opentelemetry_sdk::metrics::SdkMeterProvider,
    prometheus::Registry,
    opentelemetry::metrics::Meter,
) {
    let registry = prometheus::Registry::new();
    let exporter = opentelemetry_prometheus::exporter()
        .with_registry(registry.clone())
        .without_scope_info()
        .without_target_info()
        .build()
        .expect("build prometheus exporter");
    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(exporter)
        .build();
    let meter = opentelemetry::metrics::MeterProvider::meter(&provider, "mqttea-test");
    (provider, registry, meter)
}

// metric_value reads a counter or gauge value (with exactly the given
// label pairs) out of a gathered prometheus registry.
fn metric_value(registry: &prometheus::Registry, name: &str, labels: &[(&str, &str)]) -> f64 {
    for family in registry.gather() {
        if family.name() != name {
            continue;
        }
        for metric in family.get_metric() {
            let metric_labels: Vec<(String, String)> = metric
                .get_label()
                .iter()
                .map(|pair| (pair.name().to_string(), pair.value().to_string()))
                .collect();
            let expected: Vec<(String, String)> = labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            if metric_labels == expected {
                return match family.get_field_type() {
                    prometheus::proto::MetricType::COUNTER => metric.get_counter().value(),
                    prometheus::proto::MetricType::GAUGE => metric.get_gauge().value(),
                    other => panic!("unexpected metric type {other:?} for {name}"),
                };
            }
        }
        panic!("metric {name} has no series with labels {labels:?}");
    }
    panic!("metric {name} not found in registry");
}

#[test]
fn test_queue_stats_register_metrics_exposes_atomics() {
    let (_provider, registry, meter) = test_meter();
    let tracker = QueueStatsTracker::new();
    tracker.register_metrics(&meter, "test_client");

    // Three messages in, one processed, one failed, plus a queue-full
    // drop, an event loop error, and an unmatched topic.
    tracker.increment_pending(100);
    tracker.increment_pending(250);
    tracker.increment_pending(75);
    tracker.decrement_pending_increment_processed(100);
    tracker.decrement_pending_increment_failed(250);
    tracker.increment_dropped(500);
    tracker.increment_event_loop_errors();
    tracker.increment_unmatched_topics();

    let labels = [("client", "test_client")];
    struct Expect {
        metric: &'static str,
        value: f64,
    }
    let expectations = [
        Expect {
            metric: "carbide_mqtt_queue_pending_messages",
            value: 1.0,
        },
        Expect {
            metric: "carbide_mqtt_queue_pending_bytes",
            value: 75.0,
        },
        Expect {
            metric: "carbide_mqtt_messages_processed_total",
            value: 1.0,
        },
        Expect {
            metric: "carbide_mqtt_processed_bytes_total",
            value: 100.0,
        },
        Expect {
            metric: "carbide_mqtt_messages_failed_total",
            value: 1.0,
        },
        Expect {
            metric: "carbide_mqtt_messages_dropped_total",
            value: 1.0,
        },
        Expect {
            metric: "carbide_mqtt_dropped_bytes_total",
            value: 500.0,
        },
        Expect {
            metric: "carbide_mqtt_event_loop_errors_total",
            value: 1.0,
        },
        Expect {
            metric: "carbide_mqtt_unmatched_topics_total",
            value: 1.0,
        },
    ];
    for expect in expectations {
        assert_eq!(
            metric_value(&registry, expect.metric, &labels),
            expect.value,
            "unexpected value for {}",
            expect.metric
        );
    }
}

#[test]
fn test_publish_stats_register_metrics_exposes_atomics() {
    let (_provider, registry, meter) = test_meter();
    let tracker = PublishStatsTracker::new();
    tracker.register_metrics(&meter, "test_client");

    tracker.increment_published(512);
    tracker.increment_published(256);
    tracker.increment_failed();

    let labels = [("client", "test_client")];
    assert_eq!(
        metric_value(&registry, "carbide_mqtt_messages_published_total", &labels),
        2.0
    );
    assert_eq!(
        metric_value(&registry, "carbide_mqtt_publish_failures_total", &labels),
        1.0
    );
    assert_eq!(
        metric_value(&registry, "carbide_mqtt_published_bytes_total", &labels),
        768.0
    );
}

#[test]
fn test_connection_state_register_metrics_tracks_transitions() {
    let (_provider, registry, meter) = test_meter();
    let tracker = ConnectionStateTracker::new();
    tracker.register_metrics(&meter, "test_client");

    let labels = [("client", "test_client")];

    // Created disconnected.
    assert!(!tracker.is_connected());
    assert_eq!(
        metric_value(&registry, "carbide_mqtt_connected", &labels),
        0.0
    );

    // ConnAck arrives.
    tracker.set_connected(true);
    assert!(tracker.is_connected());
    assert_eq!(
        metric_value(&registry, "carbide_mqtt_connected", &labels),
        1.0
    );

    // Connection error drops the flag again.
    tracker.set_connected(false);
    assert_eq!(
        metric_value(&registry, "carbide_mqtt_connected", &labels),
        0.0
    );
}

// A client-level registration covers all three trackers: every instrument
// appears in the exposition, and a client that never connected reports
// zeros with the connected gauge down.
#[tokio::test]
async fn test_client_register_metrics_registers_all_instruments() {
    let (_provider, registry, meter) = test_meter();
    let client = mqttea::MqtteaClient::new("localhost", 1883, "test-metrics-client", None)
        .await
        .expect("create client");
    client.register_metrics(&meter, "test_client");

    let labels = [("client", "test_client")];
    let all_instruments = [
        "carbide_mqtt_queue_pending_messages",
        "carbide_mqtt_queue_pending_bytes",
        "carbide_mqtt_connected",
        "carbide_mqtt_messages_processed_total",
        "carbide_mqtt_processed_bytes_total",
        "carbide_mqtt_messages_failed_total",
        "carbide_mqtt_messages_dropped_total",
        "carbide_mqtt_dropped_bytes_total",
        "carbide_mqtt_event_loop_errors_total",
        "carbide_mqtt_unmatched_topics_total",
        "carbide_mqtt_messages_published_total",
        "carbide_mqtt_publish_failures_total",
        "carbide_mqtt_published_bytes_total",
    ];
    for name in all_instruments {
        assert_eq!(
            metric_value(&registry, name, &labels),
            0.0,
            "expected {name} to be registered at zero"
        );
    }
    assert!(!client.is_connected());
}
