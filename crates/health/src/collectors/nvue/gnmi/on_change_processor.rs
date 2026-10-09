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
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use prometheus::{CounterVec, Gauge, Opts};

use super::client::typed_value_to_string;
use super::proto::{self, PathElem};
use super::reconciliation::{scalar_value, snapshot_origin_matches};
use super::sample_processor::now_unix_secs;
use super::subscriber::GnmiStreamMetrics;
use crate::HealthError;
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};

type ParsedRow = HashMap<String, String>;
type CachedRows = HashMap<String, ParsedRow>;

enum DeleteTarget {
    All,
    Row(String),
    Leaf {
        instance_id: String,
        leaf_name: String,
    },
}

/// Presence is retained only for rows cached before this snapshot started.
/// Snapshot responses do not emit events or update cached row values.
pub(super) struct EventSnapshot {
    candidates: HashSet<String>,
    present: HashSet<String>,
}

impl EventSnapshot {
    #[allow(deprecated)]
    pub(super) fn process_response(
        &mut self,
        response: &proto::SubscribeResponse,
    ) -> Result<bool, tonic::Status> {
        let notification = match &response.response {
            Some(proto::subscribe_response::Response::SyncResponse(complete)) => {
                return Ok(*complete);
            }
            Some(proto::subscribe_response::Response::Update(notification)) => notification,
            _ => {
                return Err(tonic::Status::invalid_argument(
                    "unexpected event snapshot response",
                ));
            }
        };

        let prefix = notification.prefix.as_ref();

        let mut paths = prefix
            .into_iter()
            .chain(
                notification
                    .update
                    .iter()
                    .filter_map(|update| update.path.as_ref()),
            )
            .chain(notification.delete.iter());

        if paths.any(|path| {
            !path.element.is_empty()
                || (!path.target.is_empty() && path.target != "nvos")
                || !snapshot_origin_matches("", &path.origin)
                || path.elem.iter().any(|elem| {
                    if elem.name == "system-event" {
                        elem.key
                            .iter()
                            .any(|(key, id)| key != "event-id" || id.is_empty())
                    } else {
                        !elem.key.is_empty()
                    }
                })
        }) {
            return Err(tonic::Status::invalid_argument(
                "unsupported event snapshot path",
            ));
        }

        let prefix_elems = prefix.map(|path| path.elem.as_slice()).unwrap_or_default();

        if prefix_elems
            .first()
            .is_some_and(|root| root.name != "system-events")
        {
            return Err(tonic::Status::invalid_argument(
                "unexpected event snapshot root",
            ));
        }

        for path in &notification.delete {
            let combined = prefix_elems
                .iter()
                .chain(path.elem.iter())
                .collect::<Vec<_>>();

            match delete_target_from_path(&combined) {
                Some(DeleteTarget::All) => self.present.clear(),
                Some(DeleteTarget::Row(id)) => {
                    self.present.remove(&id);
                }
                _ => {
                    return Err(tonic::Status::invalid_argument(
                        "unsupported event snapshot delete path",
                    ));
                }
            }
        }

        for update in &notification.update {
            let path = update
                .path
                .as_ref()
                .ok_or_else(|| tonic::Status::invalid_argument("missing event snapshot path"))?;

            let combined = prefix_elems
                .iter()
                .chain(path.elem.iter())
                .collect::<Vec<_>>();

            let [root, event, tail @ ..] = combined.as_slice() else {
                return Err(tonic::Status::invalid_argument(
                    "unsupported event snapshot update path",
                ));
            };

            let leaf_path = match tail {
                [leaf] => leaf.name == "event-id",
                [state, leaf] => state.name == "state" && !leaf.name.is_empty(),
                _ => false,
            };

            if root.name != "system-events" || event.name != "system-event" || !leaf_path {
                return Err(tonic::Status::invalid_argument(
                    "unsupported event snapshot update path",
                ));
            }

            let id = event
                .key
                .get("event-id")
                .filter(|id| !id.is_empty())
                .ok_or_else(|| tonic::Status::invalid_argument("missing event snapshot ID"))?;

            if !scalar_value(update.val.as_ref()) {
                return Err(tonic::Status::invalid_argument(
                    "unsupported event snapshot value",
                ));
            }

            if self.candidates.contains(id) {
                self.present.insert(id.clone());
            }
        }

        Ok(false)
    }
}

pub(crate) const ON_CHANGE_STREAM_ID_SYSTEM_EVENTS: &str = "nvue_gnmi_events";

pub(crate) struct OnChangeStreamMetrics {
    pub(crate) rows_total: CounterVec,
    pub(crate) last_row_timestamp: Gauge,
    pub(crate) last_row_timestamp_seconds: Gauge,
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
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(last_row_timestamp.clone()))?;

        let last_row_timestamp_seconds = Gauge::with_opts(
            Opts::new(
                format!("{prefix}_{stream_id}_last_timestamp_seconds"),
                "Unix timestamp in seconds of most recent ON_CHANGE row",
            )
            .const_labels(const_labels),
        )?;
        registry.register(Box::new(last_row_timestamp_seconds.clone()))?;

        Ok(Self {
            rows_total,
            last_row_timestamp,
            last_row_timestamp_seconds,
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
    reconciliation_candidates: Mutex<HashSet<String>>,
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
            reconciliation_candidates: Mutex::new(HashSet::new()),
        }
    }

    /// Starts a cleanup attempt without retaining IDs outside the current cache.
    pub(super) fn begin_reconciliation(&self) -> Option<EventSnapshot> {
        let candidates: HashSet<_> = self
            .cached_rows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .cloned()
            .collect();

        if candidates.is_empty() {
            return None;
        }

        *self
            .reconciliation_candidates
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = candidates.clone();

        Some(EventSnapshot {
            candidates,
            present: HashSet::new(),
        })
    }

    /// Commits a complete snapshot, or discards eligibility on failure or cancellation.
    pub(super) fn finish_reconciliation(
        &self,
        snapshot: Option<EventSnapshot>,
        stream_metrics: &GnmiStreamMetrics,
    ) -> usize {
        let candidates = std::mem::take(
            &mut *self
                .reconciliation_candidates
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );

        let Some(snapshot) = snapshot else {
            return 0;
        };

        let mut cached_rows = self
            .cached_rows
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let removed: Vec<_> = candidates
            .difference(&snapshot.present)
            .filter(|id| cached_rows.remove(*id).is_some())
            .cloned()
            .collect();

        let entity_count = cached_rows.len();
        drop(cached_rows);

        self.prune_rows(&removed);
        stream_metrics.monitored_entities.set(entity_count as f64);

        removed.len()
    }

    fn prune_rows(&self, instance_ids: &[String]) {
        let Some(sink) = &self.data_sink else {
            return;
        };

        for instance_id in instance_ids {
            sink.prune_metric_key(
                &self.event_context,
                &format!("{}:{}", self.collector_name, instance_id),
                "on_change_row",
                "severity",
            );
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
        stream_metrics.set_last_notification_timestamp(now_unix_secs());

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

        let mut updated_rows = CachedRows::new();
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

            if let Some(target) = delete_target_from_path(&combined) {
                delete_targets.push(target);
            }
        }

        let mut cached_rows = match self.cached_rows.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let mut candidates = self
            .reconciliation_candidates
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let mut rows_to_emit = CachedRows::new();
        let mut rows_to_prune = Vec::new();

        for target in delete_targets {
            match target {
                DeleteTarget::All => {
                    candidates.clear();
                    rows_to_prune.extend(cached_rows.keys().cloned());
                    cached_rows.clear();
                    rows_to_emit.clear();
                }
                DeleteTarget::Row(instance_id) => {
                    candidates.remove(&instance_id);

                    if cached_rows.remove(&instance_id).is_some() {
                        rows_to_emit.remove(&instance_id);
                        rows_to_prune.push(instance_id);
                    }
                }
                DeleteTarget::Leaf {
                    instance_id,
                    leaf_name,
                } => {
                    candidates.remove(&instance_id);
                    if let Some(row) = cached_rows.get_mut(&instance_id)
                        && row.remove(&leaf_name).is_some()
                    {
                        if row.is_empty() {
                            cached_rows.remove(&instance_id);
                            rows_to_emit.remove(&instance_id);
                            rows_to_prune.push(instance_id);
                        } else {
                            rows_to_emit.insert(instance_id, row.clone());
                        }
                    }
                }
            }
        }

        for (instance_id, updated_row) in updated_rows {
            candidates.remove(&instance_id);
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
        drop(candidates);
        drop(cached_rows);

        self.prune_rows(&rows_to_prune);

        for (instance_id, row) in rows_to_emit {
            self.emit_row_as_metric(&instance_id, &row);
        }

        entity_count
    }

    fn emit_row_as_metric(&self, instance_id: &str, row: &ParsedRow) {
        let severity = row.get("severity").map(String::as_str).unwrap_or("unknown");
        let text = row.get("text").map(String::as_str).unwrap_or("");

        let timestamp = now_unix_secs();
        self.stream_metrics.last_row_timestamp.set(timestamp);
        self.stream_metrics
            .last_row_timestamp_seconds
            .set(timestamp);
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
    let [root, rest @ ..] = elems else {
        return Some(DeleteTarget::All);
    };

    if root.name != "system-events" {
        return None;
    }

    let [event, tail @ ..] = rest else {
        return Some(DeleteTarget::All);
    };

    if event.name != "system-event" {
        return None;
    }

    match (event.key.get("event-id"), tail) {
        (None, []) => Some(DeleteTarget::All),
        (None, [state]) if state.name == "state" => Some(DeleteTarget::All),
        (Some(id), []) => Some(DeleteTarget::Row(id.clone())),
        (Some(id), [state]) if state.name == "state" => Some(DeleteTarget::Row(id.clone())),
        (Some(id), [state, leaf]) if state.name == "state" => Some(DeleteTarget::Leaf {
            instance_id: id.clone(),
            leaf_name: leaf.name.clone(),
        }),
        _ => None,
    }
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
    use crate::metrics::MetricsManager;
    use crate::sink::PrometheusSink;

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
    fn on_change_timestamp_seconds_alias_is_registered() {
        let registry = prometheus::Registry::new();
        OnChangeStreamMetrics::new(&registry, "test", "stream_a", test_labels()).unwrap();

        assert!(
            registry
                .gather()
                .iter()
                .any(|family| family.name() == "test_stream_a_last_timestamp_seconds")
        );
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
        assert_eq!(
            processor.stream_metrics.last_row_timestamp.get(),
            processor.stream_metrics.last_row_timestamp_seconds.get()
        );
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

    #[test]
    fn reconciliation_preserves_present_and_concurrently_touched_rows() {
        let manager = Arc::new(MetricsManager::new("test").unwrap());
        let sink = Arc::new(PrometheusSink::new(manager.clone(), "test_sink").unwrap());
        let processor = test_processor(Some(sink));
        let metrics = super::super::subscriber::test_gnmi_stream_metrics();

        for id in ["removed", "present", "unchanged", "recreated"] {
            processor.process_notification(&make_system_events_notification(vec![
                make_system_event_update(id, "severity", "critical"),
            ]));
        }

        let mut snapshot = processor.begin_reconciliation().unwrap();

        let mut notification = make_system_events_notification(vec![make_system_event_update(
            "present", "event-id", "present",
        )]);

        notification.prefix.as_mut().unwrap().origin = "openconfig".into();

        // NVOS places the keyed row in the prefix and also emits a direct event-id leaf.
        let path = notification.update[0].path.as_mut().unwrap();
        notification
            .prefix
            .as_mut()
            .unwrap()
            .elem
            .push(path.elem.remove(0));

        path.elem.remove(0);

        assert!(
            !snapshot
                .process_response(&proto::SubscribeResponse {
                    response: Some(proto::subscribe_response::Response::Update(notification)),
                    ..Default::default()
                })
                .unwrap()
        );

        processor.process_notification(&make_system_events_notification(vec![
            make_system_event_update("unchanged", "severity", "critical"),
            make_system_event_update("new", "severity", "warning"),
        ]));

        let mut replacement = make_system_events_notification(vec![make_system_event_update(
            "recreated",
            "severity",
            "warning",
        )]);

        replacement.delete.push(proto::Path {
            elem: vec![make_path_elem("system-event", &[("event-id", "recreated")])],
            ..Default::default()
        });

        processor.process_notification(&replacement);

        let received = &processor.stream_metrics.rows_total;
        let received_before = received.with_label_values(&["critical"]).get();

        assert_eq!(processor.finish_reconciliation(Some(snapshot), &metrics), 1);
        let export = manager.export_telemetry().unwrap();

        assert!(!export.contains("instance_id=\"removed\""));

        for id in ["present", "unchanged", "recreated", "new"] {
            assert!(export.contains(&format!("instance_id=\"{id}\"")));
        }

        assert_eq!(metrics.monitored_entities.get(), 4.0);

        assert_eq!(
            received.with_label_values(&["critical"]).get(),
            received_before
        );

        assert_eq!(metrics.notifications_received_total.get(), 0.0);
    }

    #[test]
    #[allow(deprecated)]
    fn event_snapshot_rejects_unsupported_data() {
        let valid = make_system_events_notification(vec![make_system_event_update(
            "1", "severity", "critical",
        )]);

        type InvalidateNotification = fn(&mut proto::Notification);

        let cases: &[(&str, InvalidateNotification)] = &[
            ("missing path", |n| n.update[0].path = None),
            ("missing value", |n| n.update[0].val = None),
            ("missing ID", |n| {
                n.update[0].path.as_mut().unwrap().elem[0].key.clear()
            }),
            ("wrong target", |n| {
                n.prefix.as_mut().unwrap().target = "other".into()
            }),
            ("wrong origin", |n| {
                n.prefix.as_mut().unwrap().origin = "other".into()
            }),
            ("wrong Delete origin after presence update", |n| {
                n.update.clear();

                n.delete.push(proto::Path {
                    origin: "other".into(),
                    ..Default::default()
                });
            }),
            ("wrong root", |n| {
                n.prefix.as_mut().unwrap().elem[0].name = "interfaces".into()
            }),
            ("container data", |n| {
                n.update[0].val.as_mut().unwrap().value =
                    Some(proto::typed_value::Value::JsonVal(b"{}".to_vec()));
            }),
            ("leaf delete", |n| {
                n.delete.push(n.update[0].path.clone().unwrap())
            }),
            ("legacy delete after presence update", |n| {
                n.update.clear();

                n.delete.push(proto::Path {
                    element: vec!["system-event[event-id=gone]".into()],
                    ..Default::default()
                });
            }),
            ("unknown Delete key after presence update", |n| {
                n.update.clear();

                n.delete.push(proto::Path {
                    elem: vec![make_path_elem("system-event", &[("unknown-key", "other")])],
                    ..Default::default()
                });
            }),
        ];

        let response = proto::SubscribeResponse {
            response: Some(proto::subscribe_response::Response::Update(valid.clone())),
            ..Default::default()
        };

        for (name, invalidate) in cases {
            let processor = test_processor(None);
            processor.process_notification(&valid);
            let mut snapshot = processor.begin_reconciliation().unwrap();
            snapshot.process_response(&response).unwrap();
            let mut notification = valid.clone();

            invalidate(&mut notification);

            let invalid_response = proto::SubscribeResponse {
                response: Some(proto::subscribe_response::Response::Update(notification)),
                ..Default::default()
            };

            assert!(
                snapshot.process_response(&invalid_response).is_err(),
                "{name}"
            );

            processor
                .finish_reconciliation(None, &super::super::subscriber::test_gnmi_stream_metrics());

            assert_eq!(processor.cached_rows.lock().unwrap().len(), 1, "{name}");
        }
    }

    #[test]
    fn event_snapshot_sync_and_delete_presence() {
        let processor = test_processor(None);
        let metrics = super::super::subscriber::test_gnmi_stream_metrics();

        assert!(processor.begin_reconciliation().is_none());

        let notification = make_system_events_notification(vec![make_system_event_update(
            "1", "severity", "critical",
        )]);

        processor.process_notification(&notification);
        let mut response = proto::SubscribeResponse::default();

        for delete in [
            vec![make_path_elem("system-event", &[("event-id", "1")])],
            Vec::new(),
        ] {
            let mut snapshot = processor.begin_reconciliation().unwrap();
            for notification in [
                notification.clone(),
                proto::Notification {
                    delete: vec![proto::Path {
                        elem: delete,
                        ..Default::default()
                    }],
                    ..make_system_events_notification(Vec::new())
                },
            ] {
                response.response = Some(proto::subscribe_response::Response::Update(notification));
                snapshot.process_response(&response).unwrap();
            }

            assert!(snapshot.present.is_empty());

            for complete in [false, true] {
                response.response =
                    Some(proto::subscribe_response::Response::SyncResponse(complete));

                assert_eq!(snapshot.process_response(&response).unwrap(), complete);
            }

            assert!(
                snapshot
                    .process_response(&proto::SubscribeResponse::default())
                    .is_err()
            );

            processor.finish_reconciliation(None, &metrics);
        }
    }

    #[test]
    fn row_delete_removes_only_the_matching_prometheus_series() {
        let manager = Arc::new(MetricsManager::new("test").unwrap());
        let sink = Arc::new(PrometheusSink::new(manager.clone(), "test_sink").unwrap());
        let processor = test_processor(Some(sink));

        for id in ["1", "2"] {
            processor.process_notification(&make_system_events_notification(vec![
                make_system_event_update(id, "severity", "critical"),
            ]));
        }

        let row_delete = proto::Path {
            elem: vec![make_path_elem("system-event", &[("event-id", "1")])],
            ..Default::default()
        };

        let mut deleted = make_system_events_notification(Vec::new());
        deleted.delete.push(row_delete.clone());
        processor.process_notification(&deleted);

        let export = manager.export_telemetry().unwrap();

        assert!(!export.contains("instance_id=\"1\""));
        assert!(export.contains("instance_id=\"2\""));

        let mut replacement = make_system_events_notification(vec![make_system_event_update(
            "1", "severity", "warning",
        )]);

        replacement.delete.push(row_delete);
        processor.process_notification(&replacement);

        let export = manager.export_telemetry().unwrap();

        assert!(export.contains("instance_id=\"1\""));
        assert!(export.contains("instance_id=\"2\""));
    }

    #[test]
    fn ancestor_deletes_remove_cached_rows_and_prometheus_series() {
        let cases = [
            ("root", Vec::new()),
            ("list", vec![make_path_elem("system-event", &[])]),
            (
                "state",
                vec![
                    make_path_elem("system-event", &[("event-id", "1")]),
                    make_path_elem("state", &[]),
                ],
            ),
        ];

        for (case, path) in cases {
            let manager = Arc::new(MetricsManager::new("test").unwrap());
            let sink = Arc::new(PrometheusSink::new(manager.clone(), "test_sink").unwrap());
            let processor = test_processor(Some(sink));

            for id in ["1", "2"] {
                processor.process_notification(&make_system_events_notification(vec![
                    make_system_event_update(id, "severity", "critical"),
                ]));
            }

            let deleted = proto::Notification {
                delete: vec![proto::Path {
                    elem: path,
                    ..Default::default()
                }],
                ..make_system_events_notification(Vec::new())
            };

            let count = processor.process_notification(&deleted);
            let export = manager.export_telemetry().unwrap();

            assert!(!export.contains("instance_id=\"1\""), "{case}: {export}");

            if case == "state" {
                assert_eq!(count, 1, "{case}");
                assert!(export.contains("instance_id=\"2\""), "{case}: {export}");
            } else {
                assert_eq!(count, 0, "{case}");
                assert!(!export.contains("instance_id=\"2\""), "{case}: {export}");
            }

            let replacement = proto::Notification {
                delete: deleted.delete,
                ..make_system_events_notification(vec![make_system_event_update(
                    "1", "severity", "warning",
                )])
            };

            processor.process_notification(&replacement);

            let export = manager.export_telemetry().unwrap();
            assert!(export.contains("instance_id=\"1\""), "{case}: {export}");
        }
    }

    #[test]
    fn overlapping_deletes_do_not_restore_removed_row() {
        let updates = vec![
            make_system_event_update("17", "severity", "critical"),
            make_system_event_update("17", "text", "cached event text"),
        ];

        let row = proto::Path {
            elem: vec![make_path_elem("system-event", &[("event-id", "17")])],
            ..Default::default()
        };

        let severity = updates[0].path.clone().unwrap();
        let text = updates[1].path.clone().unwrap();

        for (case, deletes) in [
            ("leaf then row", vec![severity.clone(), row]),
            ("all leaves", vec![severity, text]),
        ] {
            let manager = Arc::new(MetricsManager::new("test").unwrap());
            let sink = Arc::new(PrometheusSink::new(manager.clone(), "test_sink").unwrap());
            let processor = test_processor(Some(sink));

            processor.process_notification(&make_system_events_notification(updates.clone()));

            assert!(
                manager
                    .export_telemetry()
                    .unwrap()
                    .contains("instance_id=\"17\"")
            );

            let mut deleted = make_system_events_notification(Vec::new());
            deleted.delete = deletes;
            processor.process_notification(&deleted);

            let export = manager.export_telemetry().unwrap();

            assert!(!export.contains("instance_id=\"17\""), "{case}: {export}");
        }
    }
}
