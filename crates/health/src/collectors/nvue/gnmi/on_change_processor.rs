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
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use prometheus::{CounterVec, Gauge, Opts};

use super::client::typed_value_to_string;
use super::proto::{self, PathElem};
use super::sample_processor::now_unix_secs;
use super::subscriber::GnmiStreamMetrics;
use crate::HealthError;
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};

type ParsedRow = HashMap<String, String>;
type CachedRows = HashMap<String, ParsedRow>;

enum DeleteTarget {
    Row(String),
    Leaf {
        instance_id: String,
        leaf_name: String,
    },
}

pub(crate) const ON_CHANGE_STREAM_ID_SYSTEM_EVENTS: &str = "nvue_gnmi_events";

pub(crate) struct OnChangeStreamMetrics {
    pub(crate) rows_total: CounterVec,
    pub(crate) last_row_timestamp: Gauge,
}

impl OnChangeStreamMetrics {
    pub(crate) fn new(
        registry: &prometheus::Registry,
        prefix: &str,
        stream_id: &str,
        const_labels: HashMap<String, String>,
    ) -> Result<Self, HealthError> {
        let rows_total = CounterVec::new(
            Opts::new(
                format!("{prefix}_{stream_id}_total"),
                "ON_CHANGE rows received by severity (field 'severity' if present)",
            )
            .const_labels(const_labels.clone()),
            &["severity"],
        )?;
        registry.register(Box::new(rows_total.clone()))?;

        let last_row_timestamp = Gauge::with_opts(
            Opts::new(
                format!("{prefix}_{stream_id}_last_timestamp"),
                "Unix timestamp of most recent ON_CHANGE row",
            )
            .const_labels(const_labels),
        )?;
        registry.register(Box::new(last_row_timestamp.clone()))?;

        Ok(Self {
            rows_total,
            last_row_timestamp,
        })
    }
}

pub(crate) struct GnmiOnChangeProcessor {
    pub(crate) collector_name: String,
    pub(crate) stream_metrics: OnChangeStreamMetrics,
    pub(crate) data_sink: Option<Arc<dyn DataSink>>,
    pub(crate) event_context: EventContext,
    pub(crate) switch_id: String,
    cached_rows: Mutex<CachedRows>,
}

impl GnmiOnChangeProcessor {
    pub(crate) fn new(
        collector_name: String,
        stream_metrics: OnChangeStreamMetrics,
        data_sink: Option<Arc<dyn DataSink>>,
        event_context: EventContext,
        switch_id: String,
    ) -> Self {
        Self {
            collector_name,
            stream_metrics,
            data_sink,
            event_context,
            switch_id,
            cached_rows: Mutex::new(HashMap::new()),
        }
    }

    #[allow(deprecated)]
    pub(crate) fn process_subscribe_response(
        &self,
        resp: &proto::SubscribeResponse,
        stream_metrics: &GnmiStreamMetrics,
    ) {
        let notification = match &resp.response {
            Some(proto::subscribe_response::Response::Update(n)) => n,
            Some(proto::subscribe_response::Response::SyncResponse(_)) => return,
            Some(proto::subscribe_response::Response::Error(e)) => {
                stream_metrics.stream_errors_total.inc();
                tracing::warn!(
                    grpc_status_code = e.code,
                    error = %e.message,
                    stream = %self.collector_name,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "nvue_gnmi ON_CHANGE: server error in stream"
                );
                return;
            }
            None => return,
        };

        stream_metrics.notifications_received_total.inc();
        stream_metrics
            .last_notification_timestamp
            .set(now_unix_secs());

        let start = Instant::now();
        let entity_count = self.process_notification(notification);
        stream_metrics
            .notification_processing_seconds
            .observe(start.elapsed().as_secs_f64());
        stream_metrics.monitored_entities.set(entity_count as f64);
    }

    fn process_notification(&self, notification: &proto::Notification) -> usize {
        let prefix_elems: &[PathElem] = notification
            .prefix
            .as_ref()
            .map(|p| p.elem.as_slice())
            .unwrap_or_default();

        let mut updated_rows: CachedRows = HashMap::new();
        let mut delete_targets = Vec::new();

        for update in &notification.update {
            let val = match update.val.as_ref() {
                Some(v) => v,
                None => continue,
            };

            let update_elems: &[PathElem] = update
                .path
                .as_ref()
                .map(|p| p.elem.as_slice())
                .unwrap_or_default();

            let combined: Vec<&PathElem> = prefix_elems.iter().chain(update_elems.iter()).collect();

            let Some(instance_key) = find_instance_key(&combined) else {
                continue;
            };
            let Some(leaf_elem) = combined.last() else {
                continue;
            };

            let value = typed_value_to_string(val).unwrap_or_default();
            updated_rows
                .entry(instance_key.to_string())
                .or_default()
                .insert(leaf_elem.name.clone(), value);
        }

        for path in &notification.delete {
            let combined: Vec<&PathElem> = prefix_elems.iter().chain(path.elem.iter()).collect();
            if let Some(delete_target) = delete_target_from_path(&combined) {
                delete_targets.push(delete_target);
            }
        }

        let mut cached_rows = match self.cached_rows.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let mut rows_to_emit = CachedRows::new();
        for target in delete_targets {
            match target {
                DeleteTarget::Row(instance_id) => {
                    cached_rows.remove(&instance_id);
                }
                DeleteTarget::Leaf {
                    instance_id,
                    leaf_name,
                } => {
                    if let Some(row) = cached_rows.get_mut(&instance_id)
                        && row.remove(&leaf_name).is_some()
                    {
                        if row.is_empty() {
                            cached_rows.remove(&instance_id);
                        } else {
                            rows_to_emit.insert(instance_id, row.clone());
                        }
                    }
                }
            }
        }

        for (instance_id, updated_row) in updated_rows {
            let row = cached_rows.entry(instance_id.clone()).or_default();
            let mut changed = false;

            for (leaf_name, value) in updated_row {
                let is_changed = row.get(&leaf_name) != Some(&value);
                if is_changed {
                    row.insert(leaf_name, value);
                    changed = true;
                }
            }

            if changed {
                rows_to_emit.insert(instance_id, row.clone());
            }
        }

        let entity_count = cached_rows.len();
        drop(cached_rows);

        for (instance_id, row) in rows_to_emit {
            self.emit_row_as_metric(&instance_id, &row);
        }

        entity_count
    }

    fn emit_row_as_metric(&self, instance_id: &str, row: &ParsedRow) {
        let severity = row.get("severity").map(String::as_str).unwrap_or("unknown");
        let text = row.get("text").map(String::as_str).unwrap_or("");

        self.stream_metrics.last_row_timestamp.set(now_unix_secs());
        self.stream_metrics
            .rows_total
            .with_label_values(&[severity])
            .inc();

        tracing::info!(
            switch_id = %self.switch_id,
            stream = %self.collector_name,
            instance_id,
            severity,
            text,
            rack_id = self.event_context.rack_id().map(tracing::field::display),
            "nvue_gnmi ON_CHANGE: row received"
        );

        let Some(sink) = &self.data_sink else { return };

        let key = format!("{}:{}", self.collector_name, instance_id);
        let mut labels = vec![
            (Cow::Borrowed("instance_id"), instance_id.to_string()),
            (Cow::Borrowed("text"), text.to_string()),
        ];
        for (key, value) in row {
            if key != "text" {
                labels.push((Cow::Owned(key.clone()), value.clone()));
            }
        }

        sink.handle_event(
            &self.event_context,
            &CollectorEvent::Metric(Box::new(MetricSample {
                key,
                name: self.collector_name.clone(),
                metric_type: "on_change_row".to_string(),
                unit: "severity".to_string(),
                value: severity_to_f64(Some(severity)),
                labels,
                context: None,
            })),
        );
    }
}

fn find_instance_key<'a>(elems: &[&'a PathElem]) -> Option<&'a str> {
    find_instance_key_with_index(elems).map(|(_, key)| key)
}

fn find_instance_key_with_index<'a>(elems: &[&'a PathElem]) -> Option<(usize, &'a str)> {
    elems
        .iter()
        .enumerate()
        .find_map(|(index, elem)| elem.key.values().next().map(|key| (index, key.as_str())))
}

fn delete_target_from_path(elems: &[&PathElem]) -> Option<DeleteTarget> {
    let (instance_index, instance_id) = find_instance_key_with_index(elems)?;
    if elems.len() == instance_index + 1 {
        return Some(DeleteTarget::Row(instance_id.to_string()));
    }

    elems.last().map(|leaf_elem| DeleteTarget::Leaf {
        instance_id: instance_id.to_string(),
        leaf_name: leaf_elem.name.clone(),
    })
}

fn severity_to_f64(severity: Option<&str>) -> f64 {
    match severity {
        Some(s) if s.eq_ignore_ascii_case("informational") => 1.0,
        Some(s) if s.eq_ignore_ascii_case("warning") || s.eq_ignore_ascii_case("minor") => 2.0,
        Some(s) if s.eq_ignore_ascii_case("error") || s.eq_ignore_ascii_case("major") => 3.0,
        Some(s) if s.eq_ignore_ascii_case("critical") => 4.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::rack::RackId;
    use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::{BmcAddr, EndpointMetadata, SwitchData, SwitchEndpointRole};

    const TEST_COLLECTOR_NAME: &str = "nvue_gnmi_system_events";

    #[derive(Default)]
    struct CapturingSink {
        events: Mutex<Vec<(EventContext, CollectorEvent)>>,
    }

    impl DataSink for CapturingSink {
        fn sink_type(&self) -> &'static str {
            "capturing_sink"
        }

        fn try_handle_event(
            &self,
            context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            self.events
                .lock()
                .expect("lock poisoned")
                .push((context.clone(), event.clone()));
            Ok(())
        }
    }

    fn test_labels() -> HashMap<String, String> {
        HashMap::from([(
            "collector_type".to_string(),
            ON_CHANGE_STREAM_ID_SYSTEM_EVENTS.to_string(),
        )])
    }

    fn test_switch_id(label: &str) -> SwitchId {
        let mut hash = [0u8; 32];
        let bytes = label.as_bytes();
        hash[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
        SwitchId::new(SwitchIdSource::Tpm, hash, SwitchType::NvLink)
    }

    fn test_event_context(collector_type: &'static str) -> EventContext {
        EventContext {
            endpoint_key: "aa:bb:cc:dd:ee:ff".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().unwrap(),
                port: None,
                mac: Some(MacAddress::from_str("AA:BB:CC:DD:EE:FF").unwrap()),
            },
            collector_type,
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn test_processor(data_sink: Option<Arc<dyn DataSink>>) -> GnmiOnChangeProcessor {
        let registry = prometheus::Registry::new();
        let stream_metrics =
            OnChangeStreamMetrics::new(&registry, "test", TEST_COLLECTOR_NAME, test_labels())
                .unwrap();
        GnmiOnChangeProcessor::new(
            TEST_COLLECTOR_NAME.to_string(),
            stream_metrics,
            data_sink,
            test_event_context(TEST_COLLECTOR_NAME),
            "SN1234".to_string(),
        )
    }

    fn make_path_elem(name: &str, keys: &[(&str, &str)]) -> PathElem {
        PathElem {
            name: name.to_string(),
            key: keys
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        }
    }

    fn make_typed_value_string(value: &str) -> proto::TypedValue {
        proto::TypedValue {
            value: Some(proto::typed_value::Value::StringVal(value.to_string())),
        }
    }

    fn make_system_event_update(event_id: &str, leaf_name: &str, value: &str) -> proto::Update {
        proto::Update {
            path: Some(proto::Path {
                elem: vec![
                    make_path_elem("system-event", &[("event-id", event_id)]),
                    make_path_elem("state", &[]),
                    make_path_elem(leaf_name, &[]),
                ],
                ..Default::default()
            }),
            val: Some(make_typed_value_string(value)),
            ..Default::default()
        }
    }

    fn make_system_events_notification(updates: Vec<proto::Update>) -> proto::Notification {
        proto::Notification {
            prefix: Some(proto::Path {
                elem: vec![make_path_elem("system-events", &[])],
                ..Default::default()
            }),
            update: updates,
            ..Default::default()
        }
    }

    fn metric_label<'a>(metric: &'a MetricSample, label: &str) -> Option<&'a str> {
        metric
            .labels
            .iter()
            .find(|(key, _)| key.as_ref() == label)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn test_find_instance_key() {
        let elems = [
            make_path_elem("system-events", &[]),
            make_path_elem("system-event", &[("event-id", "38")]),
            make_path_elem("state", &[]),
            make_path_elem("severity", &[]),
        ];
        let refs: Vec<&PathElem> = elems.iter().collect();
        assert_eq!(find_instance_key(&refs), Some("38"));
    }

    #[test]
    fn test_find_instance_key_missing() {
        let elems = [
            make_path_elem("system-events", &[]),
            make_path_elem("state", &[]),
        ];
        let refs: Vec<&PathElem> = elems.iter().collect();
        assert_eq!(find_instance_key(&refs), None);
    }

    #[test]
    fn test_severity_to_f64() {
        assert_eq!(severity_to_f64(Some("informational")), 1.0);
        assert_eq!(severity_to_f64(Some("warning")), 2.0);
        assert_eq!(severity_to_f64(Some("MINOR")), 2.0);
        assert_eq!(severity_to_f64(Some("error")), 3.0);
        assert_eq!(severity_to_f64(Some("MAJOR")), 3.0);
        assert_eq!(severity_to_f64(Some("critical")), 4.0);
        assert_eq!(severity_to_f64(Some("CRITICAL")), 4.0);
        assert_eq!(severity_to_f64(Some("other")), 0.0);
        assert_eq!(severity_to_f64(None), 0.0);
    }

    #[test]
    fn test_on_change_stream_metrics_duplicate_registration_fails() {
        let registry = prometheus::Registry::new();
        let _ = OnChangeStreamMetrics::new(&registry, "test", "stream_a", test_labels()).unwrap();
        let result = OnChangeStreamMetrics::new(&registry, "test", "stream_a", test_labels());
        assert!(result.is_err());
    }

    #[test]
    fn test_process_notification_severity_and_text() {
        let processor = test_processor(None);
        let notification = proto::Notification {
            prefix: Some(proto::Path {
                elem: vec![make_path_elem("system-events", &[])],
                ..Default::default()
            }),
            update: vec![
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("system-event", &[("event-id", "5")]),
                            make_path_elem("state", &[]),
                            make_path_elem("severity", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("critical")),
                    ..Default::default()
                },
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("system-event", &[("event-id", "5")]),
                            make_path_elem("state", &[]),
                            make_path_elem("text", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("System fatal state detected")),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let count = processor.process_notification(&notification);
        assert_eq!(count, 1);
        assert_eq!(
            processor
                .stream_metrics
                .rows_total
                .with_label_values(&["critical"])
                .get(),
            1.0
        );
        assert!(processor.stream_metrics.last_row_timestamp.get() > 0.0);
    }

    #[test]
    fn test_process_notification_snapshot_diff_no_duplicate_emit() {
        let processor = test_processor(None);
        let notification = proto::Notification {
            prefix: Some(proto::Path {
                elem: vec![make_path_elem("system-events", &[])],
                ..Default::default()
            }),
            update: vec![
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("system-event", &[("event-id", "7")]),
                            make_path_elem("state", &[]),
                            make_path_elem("severity", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("error")),
                    ..Default::default()
                },
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("system-event", &[("event-id", "7")]),
                            make_path_elem("state", &[]),
                            make_path_elem("text", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("same event")),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        processor.process_notification(&notification);
        processor.process_notification(&notification);

        assert_eq!(
            processor
                .stream_metrics
                .rows_total
                .with_label_values(&["error"])
                .get(),
            1.0
        );
    }

    #[test]
    fn test_process_notification_merges_delta_updates_into_cached_row() {
        let sink = Arc::new(CapturingSink::default());
        let processor = test_processor(Some(sink.clone()));

        processor.process_notification(&make_system_events_notification(vec![
            make_system_event_update("9", "severity", "critical"),
        ]));
        processor.process_notification(&make_system_events_notification(vec![
            make_system_event_update("9", "text", "partial event text"),
        ]));

        assert_eq!(
            processor
                .stream_metrics
                .rows_total
                .with_label_values(&["critical"])
                .get(),
            2.0
        );
        assert_eq!(
            processor
                .stream_metrics
                .rows_total
                .with_label_values(&["unknown"])
                .get(),
            0.0
        );

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 2);
        let CollectorEvent::Metric(metric) = &events[1].1 else {
            panic!("expected metric event");
        };
        assert_eq!(metric.value, 4.0);
        assert_eq!(metric_label(metric, "severity"), Some("critical"));
        assert_eq!(metric_label(metric, "text"), Some("partial event text"));
    }

    #[test]
    fn test_process_notification_delete_removes_cached_leaf() {
        let sink = Arc::new(CapturingSink::default());
        let processor = test_processor(Some(sink.clone()));

        processor.process_notification(&make_system_events_notification(vec![
            make_system_event_update("11", "severity", "critical"),
            make_system_event_update("11", "text", "cached event text"),
        ]));

        let delete = proto::Path {
            elem: vec![
                make_path_elem("system-event", &[("event-id", "11")]),
                make_path_elem("state", &[]),
                make_path_elem("severity", &[]),
            ],
            ..Default::default()
        };
        processor.process_notification(&proto::Notification {
            prefix: Some(proto::Path {
                elem: vec![make_path_elem("system-events", &[])],
                ..Default::default()
            }),
            delete: vec![delete],
            ..Default::default()
        });
        processor.process_notification(&make_system_events_notification(vec![
            make_system_event_update("11", "text", "event text after delete"),
        ]));

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 3);
        let CollectorEvent::Metric(metric) = &events[2].1 else {
            panic!("expected metric event");
        };
        assert_eq!(metric.value, 0.0);
        assert_eq!(metric_label(metric, "severity"), None);
        assert_eq!(
            metric_label(metric, "text"),
            Some("event text after delete")
        );
    }

    #[test]
    fn test_process_notification_leaf_delete_emits_updated_cached_row() {
        let sink = Arc::new(CapturingSink::default());
        let processor = test_processor(Some(sink.clone()));

        processor.process_notification(&make_system_events_notification(vec![
            make_system_event_update("13", "severity", "critical"),
            make_system_event_update("13", "text", "cached event text"),
        ]));

        let delete = proto::Path {
            elem: vec![
                make_path_elem("system-event", &[("event-id", "13")]),
                make_path_elem("state", &[]),
                make_path_elem("severity", &[]),
            ],
            ..Default::default()
        };
        processor.process_notification(&proto::Notification {
            prefix: Some(proto::Path {
                elem: vec![make_path_elem("system-events", &[])],
                ..Default::default()
            }),
            delete: vec![delete],
            ..Default::default()
        });

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 2);
        let CollectorEvent::Metric(metric) = &events[1].1 else {
            panic!("expected metric event");
        };
        assert_eq!(metric.value, 0.0);
        assert_eq!(metric_label(metric, "severity"), None);
        assert_eq!(metric_label(metric, "text"), Some("cached event text"));
    }

    #[test]
    fn test_process_notification_row_delete_drops_cached_leaves() {
        let sink = Arc::new(CapturingSink::default());
        let processor = test_processor(Some(sink.clone()));

        assert_eq!(
            processor.process_notification(&make_system_events_notification(vec![
                make_system_event_update("17", "severity", "critical"),
                make_system_event_update("17", "text", "cached event text"),
            ])),
            1
        );

        let delete = proto::Path {
            elem: vec![make_path_elem("system-event", &[("event-id", "17")])],
            ..Default::default()
        };
        assert_eq!(
            processor.process_notification(&proto::Notification {
                prefix: Some(proto::Path {
                    elem: vec![make_path_elem("system-events", &[])],
                    ..Default::default()
                }),
                delete: vec![delete],
                ..Default::default()
            }),
            0
        );

        assert_eq!(
            processor.process_notification(&make_system_events_notification(vec![
                make_system_event_update("17", "text", "event text after row delete"),
            ])),
            1
        );

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 2);
        let CollectorEvent::Metric(metric) = &events[1].1 else {
            panic!("expected metric event");
        };
        assert_eq!(metric.value, 0.0);
        assert_eq!(metric_label(metric, "severity"), None);
        assert_eq!(
            metric_label(metric, "text"),
            Some("event text after row delete")
        );
    }

    #[test]
    fn emitted_metrics_preserve_switch_position_context() {
        let sink = Arc::new(CapturingSink::default());
        let switch_id = test_switch_id("switch-a");
        let registry = prometheus::Registry::new();
        let stream_metrics =
            OnChangeStreamMetrics::new(&registry, "test", TEST_COLLECTOR_NAME, test_labels())
                .unwrap();
        let processor = GnmiOnChangeProcessor::new(
            TEST_COLLECTOR_NAME.to_string(),
            stream_metrics,
            Some(sink.clone()),
            EventContext {
                endpoint_key: "aa:bb:cc:dd:ee:ff".to_string(),
                addr: BmcAddr {
                    ip: "10.0.0.1".parse().unwrap(),
                    port: None,
                    mac: Some(MacAddress::from_str("AA:BB:CC:DD:EE:FF").unwrap()),
                },
                collector_type: ON_CHANGE_STREAM_ID_SYSTEM_EVENTS,
                labels: Default::default(),
                metadata: Some(EndpointMetadata::Switch(SwitchData {
                    id: Some(switch_id),
                    serial: "SN-SWITCH-001".to_string(),
                    slot_number: Some(7),
                    tray_index: Some(3),
                    nvlink_domain_uuid: None,
                    endpoint_role: SwitchEndpointRole::Host,
                    is_primary: false,
                    nmxc_enabled: false,
                    nmxt_enabled: false,
                })),
                rack_id: Some(RackId::new("RACK_2")),
            },
            "SN-SWITCH-001".to_string(),
        );
        let notification = proto::Notification {
            prefix: Some(proto::Path {
                elem: vec![make_path_elem("system-events", &[])],
                ..Default::default()
            }),
            update: vec![
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("system-event", &[("event-id", "42")]),
                            make_path_elem("state", &[]),
                            make_path_elem("severity", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("warning")),
                    ..Default::default()
                },
                proto::Update {
                    path: Some(proto::Path {
                        elem: vec![
                            make_path_elem("system-event", &[("event-id", "42")]),
                            make_path_elem("state", &[]),
                            make_path_elem("text", &[]),
                        ],
                        ..Default::default()
                    }),
                    val: Some(make_typed_value_string("Link down detected on swp1")),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert_eq!(processor.process_notification(&notification), 1);

        let events = sink.events.lock().expect("lock poisoned");
        assert_eq!(events.len(), 1);
        let (context, event) = &events[0];
        assert_eq!(context.switch_id(), Some(switch_id));
        assert_eq!(context.switch_slot_number(), Some(7));
        assert_eq!(context.switch_tray_index(), Some(3));
        assert_eq!(context.rack_id().map(RackId::as_str), Some("RACK_2"));
        let CollectorEvent::Metric(metric) = event else {
            panic!("expected metric event");
        };
        assert_eq!(metric.metric_type, "on_change_row");
        assert_eq!(metric.value, 2.0);
        assert!(
            metric
                .labels
                .iter()
                .any(|(key, value)| key == "instance_id" && value == "42")
        );
    }
}
