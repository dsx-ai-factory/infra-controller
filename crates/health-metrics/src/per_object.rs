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

//! Cross-controller registry backing per-object metrics.
//!
//! Per-object series (one or more per object id) are emitted from shared
//! gauges rather than a type-prefixed metric per controller, so metric names
//! stay stable as observability generalizes across object types. Writers
//! obtain a [`PerObjectGauge`] via [`PerObjectMetricsRegistry::gauge`] and
//! replace an object's series with [`PerObjectGauge::set`]/[`set_all`]/
//! [`clear`]; each gauge's series exist only while the fact they state is
//! true, and entries not refreshed within the registry's hold period are
//! evicted lazily on read.
//!
//! Each gauge owns its label schema: writers record only the label *values*
//! (in schema order), and the label names are applied at collection time. At
//! fleet scale this halves the per-series memory next to storing full
//! key/value pairs per entry.
//!
//! The per-object health classification metric predates this generalization
//! and keeps its dedicated [`PerObjectMetricsRegistry::record`]/
//! [`PerObjectMetricsRegistry::register`] API (opt-in per classification to
//! bound cardinality), sharing the same per-object store underneath.
//!
//! [`set_all`]: PerObjectGauge::set_all
//! [`clear`]: PerObjectGauge::clear

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use health_report::HealthAlertClassification;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;
use prometheus::core::{Collector, Desc};
use prometheus::proto;

const UNHEALTHY_BY_CLASSIFICATION_METRIC: &str = "carbide_object_unhealthy_by_classification_count";

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct ObjectKey {
    /// `machine`, `switch`, `rack`, `power_shelf`, ...
    object_type: &'static str,
    object_id: String,
}

impl ObjectKey {
    fn new(object_type: &'static str, object_id: &str) -> Self {
        Self {
            object_type,
            object_id: object_id.to_string(),
        }
    }
}

/// One object's series on a schema'd gauge: `(value, label values in schema
/// order)` pairs.
type ValueSeries = Vec<(f64, Vec<String>)>;

/// One object's series on the classification metric, which predates the
/// schema'd store and carries caller-supplied labels.
type LabeledSeries = Vec<(f64, Vec<KeyValue>)>;

/// The payload one object currently exposes on one store, together with its
/// last refresh time, under one per-object (so effectively uncontended)
/// mutex — a reader can never observe a fresh payload with a stale deadline
/// or vice versa. Every record/keep-alive/snapshot path runs under the map's
/// *read* lock, concurrently with everything else.
#[derive(Debug)]
struct StoreEntry<S> {
    inner: Mutex<(Arc<S>, Instant)>,
}

impl<S: PartialEq> StoreEntry<S> {
    fn new(payload: S) -> Self {
        Self {
            inner: Mutex::new((Arc::new(payload), Instant::now())),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (Arc<S>, Instant)> {
        self.inner.lock().expect("per-object entry mutex poisoned")
    }

    fn payload(&self) -> Arc<S> {
        self.lock().0.clone()
    }

    /// Replaces the payload unless unchanged (the common case: controllers
    /// re-record every iteration), and extends the eviction deadline.
    fn record(&self, payload: S) {
        let mut inner = self.lock();
        if *inner.0 != payload {
            inner.0 = Arc::new(payload);
        }
        inner.1 = Instant::now();
    }

    fn touch(&self) {
        self.lock().1 = Instant::now();
    }

    /// Returns the payload while the entry is within `hold_period` of its
    /// last refresh (relative to `now`), `None` once stale.
    fn payload_if_fresh(&self, now: Instant, hold_period: Duration) -> Option<Arc<S>> {
        let inner = self.lock();
        (now.saturating_duration_since(inner.1) <= hold_period).then(|| inner.0.clone())
    }
}

/// Concurrent per-object store shared by the schema'd gauges and the
/// classification metric: an object-keyed map of payloads with hold-period
/// eviction.
#[derive(Debug)]
struct PerObjectStore<S> {
    hold_period: Duration,
    /// Read-locked by every recording, keep-alive, and scrape-snapshot path
    /// (per-entry state is interior-mutable), so they all run concurrently;
    /// the write lock is taken only when the object *set* changes (insert,
    /// remove, eviction).
    entries: RwLock<HashMap<ObjectKey, StoreEntry<S>>>,
}

impl<S: PartialEq> PerObjectStore<S> {
    fn new(hold_period: Duration) -> Self {
        Self {
            hold_period,
            entries: RwLock::new(HashMap::new()),
        }
    }

    fn read_entries(&self) -> std::sync::RwLockReadGuard<'_, HashMap<ObjectKey, StoreEntry<S>>> {
        self.entries.read().expect("per-object store lock poisoned")
    }

    fn write_entries(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<ObjectKey, StoreEntry<S>>> {
        self.entries
            .write()
            .expect("per-object store lock poisoned")
    }

    /// Replaces the object's payload (keeping the entry when unchanged).
    /// Recording into an existing entry needs only the read lock plus the
    /// entry's own (per-object, uncontended) mutex, so writers never
    /// serialize with each other or with scrape snapshots; the write lock is
    /// taken only to admit an object seen for the first time.
    fn record(&self, key: ObjectKey, payload: S) {
        if let Some(entry) = self.read_entries().get(&key) {
            entry.record(payload);
            return;
        }
        let mut entries = self.write_entries();
        // Re-check under the write lock: another writer may have inserted the
        // entry between the read and write acquisitions.
        if let Some(entry) = entries.get(&key) {
            entry.record(payload);
        } else {
            entries.insert(key, StoreEntry::new(payload));
        }
    }

    /// Removes the object's entry. Checks existence under the read lock
    /// first: clearing an absent entry is a steady-state no-op for optional
    /// facts (e.g. a machine without an instance clears every iteration) and
    /// must not take the write lock.
    fn remove(&self, key: &ObjectKey) {
        if !self.read_entries().contains_key(key) {
            return;
        }
        self.write_entries().remove(key);
    }

    /// Refreshes the entry's eviction deadline without changing its payload;
    /// a no-op if absent.
    fn touch(&self, key: &ObjectKey) {
        if let Some(entry) = self.read_entries().get(key) {
            entry.touch();
        }
    }

    /// Refreshes the entry's eviction deadline while `still_current` holds
    /// for its payload; otherwise the entry is removed — the recorded fact no
    /// longer describes the object's current state, so keeping it alive would
    /// publish contradictory labels.
    fn touch_if(&self, key: &ObjectKey, still_current: impl Fn(&S) -> bool) {
        match self.read_entries().get(key) {
            None => return,
            Some(entry) if still_current(&entry.payload()) => {
                entry.touch();
                return;
            }
            Some(_) => {}
        }
        // Re-check under the write lock — a writer may have replaced the
        // payload with a current one in between.
        let mut entries = self.write_entries();
        if let Some(entry) = entries.get(key) {
            if still_current(&entry.payload()) {
                entry.touch();
            } else {
                entries.remove(key);
            }
        }
    }

    /// Snapshots the live entries' payloads as one collection of `Arc`
    /// clones under the read lock — concurrent with writers — so encoding
    /// and observation happen outside any lock. Stale entries are skipped,
    /// and evicted afterwards (the only write-lock the scrape path ever
    /// takes, and only when something actually expired).
    fn snapshot_live(&self) -> Vec<Arc<S>> {
        let now = Instant::now();
        let mut stale = Vec::new();
        let snapshot: Vec<Arc<S>> = {
            let entries = self.read_entries();
            let mut snapshot = Vec::with_capacity(entries.len());
            for (key, entry) in entries.iter() {
                match entry.payload_if_fresh(now, self.hold_period) {
                    Some(payload) => snapshot.push(payload),
                    None => stale.push(key.clone()),
                }
            }
            snapshot
        };
        if !stale.is_empty() {
            let mut entries = self.write_entries();
            for key in stale {
                // Re-check: the entry may have been refreshed or replaced
                // since the read-locked pass (a refresh after `now` reads as
                // fresh via the saturating comparison).
                let still_stale = entries
                    .get(&key)
                    .is_some_and(|entry| entry.payload_if_fresh(now, self.hold_period).is_none());
                if still_stale {
                    entries.remove(&key);
                }
            }
        }
        snapshot
    }

    /// Visits the live entries under the read lock. Test-only introspection;
    /// production readers go through [`Self::snapshot_live`] so no per-entry
    /// work runs under the lock.
    #[cfg(test)]
    fn for_each_live(&self, mut visit: impl FnMut(&ObjectKey, &S)) {
        let now = Instant::now();
        for (key, entry) in self.read_entries().iter() {
            if let Some(payload) = entry.payload_if_fresh(now, self.hold_period) {
                visit(key, &payload);
            }
        }
    }
}

/// Writer handle for one per-object gauge. Cheap to clone; all clones share
/// the same series store. The gauge owns its label schema — writers pass
/// label *values* in schema order and the names are applied at collection
/// time. The `(object_type, object_id)` key controls only series lifecycle
/// (replace/evict) — values are emitted exactly as supplied, so metrics that
/// need `object_type`/`object_id` labels must include them in the schema.
#[derive(Clone, Debug)]
pub struct PerObjectGauge(Arc<GaugeState>);

#[derive(Debug)]
struct GaugeState {
    label_names: &'static [&'static str],
    store: PerObjectStore<ValueSeries>,
}

impl PerObjectGauge {
    fn new(label_names: &'static [&'static str], hold_period: Duration) -> Self {
        Self(Arc::new(GaugeState {
            label_names,
            store: PerObjectStore::new(hold_period),
        }))
    }

    /// Replaces the object's series with a single one. `label_values` must
    /// match the gauge's label schema in length and order.
    pub fn set(
        &self,
        object_type: &'static str,
        object_id: &str,
        value: f64,
        label_values: Vec<String>,
    ) {
        self.set_all(object_type, object_id, vec![(value, label_values)]);
    }

    /// Replaces all of the object's series on this gauge; an empty set
    /// removes the object so its series stop being emitted. Each series'
    /// label values must match the gauge's label schema in length and order.
    pub fn set_all(&self, object_type: &'static str, object_id: &str, series: ValueSeries) {
        for (_, label_values) in &series {
            assert_eq!(
                label_values.len(),
                self.0.label_names.len(),
                "series has {} label values but the gauge schema {:?} has {} labels",
                label_values.len(),
                self.0.label_names,
                self.0.label_names.len(),
            );
        }
        let key = ObjectKey::new(object_type, object_id);
        if series.is_empty() {
            self.0.store.remove(&key);
        } else {
            self.0.store.record(key, series);
        }
    }

    /// Removes all of the object's series on this gauge.
    pub fn clear(&self, object_type: &'static str, object_id: &str) {
        self.0.store.remove(&ObjectKey::new(object_type, object_id));
    }

    /// Refreshes the entry's eviction deadline, but only while every series'
    /// leading label values still match `prefix` (in schema order, like
    /// [`Self::set`]); otherwise the entry is removed — the recorded fact no
    /// longer describes the object's current state, so keeping it alive would
    /// publish contradictory labels.
    pub fn touch_if_values(&self, object_type: &'static str, object_id: &str, prefix: &[String]) {
        let key = ObjectKey::new(object_type, object_id);
        self.0.store.touch_if(&key, |series| {
            series
                .iter()
                .all(|(_, label_values)| label_values.starts_with(prefix))
        });
    }
}

/// Exports one [`PerObjectGauge`] as a native Prometheus metric family. The
/// family name and help are served from `desc`, the same source the registry
/// checks for collisions; label names come from the gauge's schema and are
/// applied to the stored values at collection time.
#[derive(Debug)]
struct GaugeCollector {
    gauge: PerObjectGauge,
    desc: Desc,
}

impl GaugeCollector {
    fn new(
        gauge: PerObjectGauge,
        name: &'static str,
        help: &'static str,
    ) -> prometheus::Result<Self> {
        let desc = Desc::new(
            name.to_string(),
            help.to_string(),
            gauge
                .0
                .label_names
                .iter()
                .map(|label| label.to_string())
                .collect(),
            HashMap::new(),
        )?;
        Ok(Self { gauge, desc })
    }
}

impl Collector for GaugeCollector {
    fn desc(&self) -> Vec<&Desc> {
        vec![&self.desc]
    }

    fn collect(&self) -> Vec<proto::MetricFamily> {
        // Snapshot under the gauge read lock (one exact-size Vec of Arc
        // clones), then encode just-in-time outside it so a large scrape
        // never stalls writers.
        let label_names = self.gauge.0.label_names;
        let metrics: Vec<proto::Metric> = self
            .gauge
            .0
            .store
            .snapshot_live()
            .iter()
            .flat_map(|series| {
                series.iter().map(|(value, label_values)| {
                    prometheus_metric(*value, label_names, label_values)
                })
            })
            .collect();
        if metrics.is_empty() {
            return Vec::new();
        }
        let mut family = proto::MetricFamily::default();
        family.set_name(self.desc.fq_name.clone());
        family.set_help(self.desc.help.clone());
        family.set_field_type(proto::MetricType::GAUGE);
        family.set_metric(metrics);
        vec![family]
    }
}

fn prometheus_metric(value: f64, label_names: &[&str], label_values: &[String]) -> proto::Metric {
    let mut labels: Vec<proto::LabelPair> = label_names
        .iter()
        .zip(label_values)
        .map(|(name, value)| {
            let mut label = proto::LabelPair::default();
            label.set_name(name.to_string());
            label.set_value(value.clone());
            label
        })
        .collect();
    labels.sort_by(|left, right| left.name().cmp(right.name()));

    let mut gauge = proto::Gauge::default();
    gauge.set_value(value);
    let mut metric = proto::Metric::from_gauge(gauge);
    metric.set_label(labels);
    metric
}

/// Shared factory for per-object gauges, plus the per-object health
/// classification metric. Stale entries (not refreshed within `hold_period`)
/// are evicted lazily on read, mirroring the controllers' `metric_hold_time`.
#[derive(Debug)]
pub struct PerObjectMetricsRegistry {
    emit_for_classifications: HashSet<HealthAlertClassification>,
    classification: Arc<PerObjectStore<LabeledSeries>>,
    /// The state/info gauges created via [`Self::gauge`], swept by
    /// [`Self::clear_object`] and [`Self::touch_object`]. Deliberately
    /// excludes the classification store: deletion clears it too (explicitly,
    /// in `clear_object`), but a keep-alive touch must not extend the
    /// pre-existing main-endpoint metric's eviction beyond its own hold.
    /// Write-locked only by [`Self::gauge`] (setup time); sweeps read-lock.
    gauges: RwLock<Vec<PerObjectGauge>>,
}

impl PerObjectMetricsRegistry {
    /// Emits per-object classification series only for
    /// `emit_for_classifications`; an empty set disables that metric entirely
    /// (gauges created via [`Self::gauge`] are unaffected). `hold_period`
    /// governs the classification metric and should match (or slightly
    /// exceed) the feeding controllers' `metric_hold_time`.
    pub fn new(
        emit_for_classifications: impl IntoIterator<Item = HealthAlertClassification>,
        hold_period: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            emit_for_classifications: emit_for_classifications.into_iter().collect(),
            classification: Arc::new(PerObjectStore::new(hold_period)),
            gauges: RwLock::new(Vec::new()),
        })
    }

    /// Creates a per-object gauge with its own label schema and `hold_period`
    /// (an object's series is only refreshed when its controller processes
    /// it, so the hold must cover the slowest feeder's refresh interval),
    /// exported as a native Prometheus collector on `registry` (typically the
    /// dedicated per-object endpoint's registry). Native collection
    /// deliberately bypasses OpenTelemetry instruments, whose per-stream
    /// cardinality limit (2000 series by default) a per-object fleet vastly
    /// exceeds.
    pub fn gauge(
        &self,
        registry: &prometheus::Registry,
        name: &'static str,
        help: &'static str,
        label_names: &'static [&'static str],
        hold_period: Duration,
    ) -> prometheus::Result<PerObjectGauge> {
        let gauge = PerObjectGauge::new(label_names, hold_period);
        registry.register(Box::new(GaugeCollector::new(gauge.clone(), name, help)?))?;
        self.gauges
            .write()
            .expect("per-object registry lock poisoned")
            .push(gauge.clone());
        Ok(gauge)
    }

    /// Removes every series of the object — state/info gauges plus the
    /// classification metric — e.g. when the object was deleted. (Store locks
    /// nest inside the registry lock; no path takes them in the other order.)
    pub fn clear_object(&self, object_type: &'static str, object_id: &str) {
        let key = ObjectKey::new(object_type, object_id);
        self.classification.remove(&key);
        for gauge in self
            .gauges
            .read()
            .expect("per-object registry lock poisoned")
            .iter()
        {
            gauge.0.store.remove(&key);
        }
    }

    /// Refreshes the eviction deadline of the object's state/info series
    /// without changing them. For iterations that could not determine the
    /// object's state at all (e.g. a load failure): neither the state series
    /// nor the info/association series recorded by handlers that never ran
    /// may evict mid-incident. The classification metric is deliberately not
    /// touched — its eviction semantics predate this endpoint and stay
    /// governed solely by its own recording cadence and hold.
    pub fn touch_object(&self, object_type: &'static str, object_id: &str) {
        let key = ObjectKey::new(object_type, object_id);
        for gauge in self
            .gauges
            .read()
            .expect("per-object registry lock poisoned")
            .iter()
        {
            gauge.0.store.touch(&key);
        }
    }

    /// Records the object's current classifications, retaining only those opted
    /// in for emission. An object left with no opted-in classification (e.g. it
    /// became healthy) is removed so its series stop being emitted.
    pub fn record<'a>(
        &self,
        object_type: &'static str,
        object_id: &str,
        classifications: impl IntoIterator<Item = &'a HealthAlertClassification>,
        extra_labels: Vec<KeyValue>,
    ) {
        // When disabled the map is always empty, so skip the key alloc and lock.
        if self.emit_for_classifications.is_empty() {
            return;
        }

        let mut classifications: Vec<String> = classifications
            .into_iter()
            .filter(|c| self.emit_for_classifications.contains(*c))
            .map(ToString::to_string)
            .collect();
        // Deterministic order (the input is set-like), so an unchanged
        // classification set keeps its entry in record instead of being
        // replaced every iteration.
        classifications.sort_unstable();
        let key = ObjectKey::new(object_type, object_id);
        if classifications.is_empty() {
            self.classification.remove(&key);
            return;
        }
        let series: LabeledSeries = classifications
            .into_iter()
            .map(|classification| {
                let mut labels = vec![
                    KeyValue::new("object_type", object_type),
                    KeyValue::new("object_id", object_id.to_string()),
                    KeyValue::new("classification", classification),
                ];
                labels.extend(extra_labels.iter().cloned());
                (1.0, labels)
            })
            .collect();
        self.classification.record(key, series);
    }

    /// Registers the per-object classification metric as a u64 OpenTelemetry
    /// instrument (its pre-existing exposition type; changing it would fork
    /// the series for typed OTLP consumers). Call once per process; with no
    /// opted-in classifications nothing is registered.
    pub fn register(self: &Arc<Self>, meter: &Meter) {
        if self.emit_for_classifications.is_empty() {
            return;
        }
        let store = self.classification.clone();
        meter
            .u64_observable_gauge(UNHEALTHY_BY_CLASSIFICATION_METRIC)
            .with_description(
                "Per-object indication that an object (host, switch, rack, ...) is marked with a \
                 health alert classification due to being unhealthy. Labeled with object_type and \
                 object_id. Only classifications configured via \
                 observability.per_object_metrics_for_classifications are emitted, bounding \
                 metric cardinality.",
            )
            .with_callback(move |observer| {
                // Snapshot under the lock (one exact-size Vec of Arc clones),
                // observe outside it, so a large scrape doesn't stall writers.
                for series in store.snapshot_live() {
                    for (value, labels) in series.iter() {
                        observer.observe(*value as u64, labels);
                    }
                }
            })
            .build();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifications(values: &[&str]) -> Vec<HealthAlertClassification> {
        values.iter().map(|v| v.parse().unwrap()).collect()
    }

    /// `(object_type, object_id, sorted (value, label values) per series)`.
    type GaugeRow = (String, String, Vec<(f64, Vec<String>)>);

    fn gauge_snapshot(gauge: &PerObjectGauge) -> Vec<GaugeRow> {
        let mut rows = Vec::new();
        gauge.0.store.for_each_live(|key, series| {
            let mut series: Vec<(f64, Vec<String>)> = series.clone();
            series.sort_by(|a, b| a.partial_cmp(b).unwrap());
            rows.push((key.object_type.to_string(), key.object_id.clone(), series));
        });
        rows.sort_by(|a, b| a.partial_cmp(b).unwrap());
        rows
    }

    /// `(object_type, object_id, sorted classifications, sorted labels)`.
    type SnapshotRow = (String, String, Vec<String>, Vec<(String, String)>);

    fn snapshot(registry: &PerObjectMetricsRegistry) -> Vec<SnapshotRow> {
        let mut rows = Vec::new();
        registry.classification.for_each_live(|key, series| {
            let mut classifications = Vec::new();
            let mut labels = Vec::new();
            for (value, series_labels) in series.iter() {
                assert!((*value - 1.0).abs() < f64::EPSILON);
                for kv in series_labels {
                    match kv.key.as_str() {
                        "object_type" | "object_id" => {}
                        "classification" => classifications.push(kv.value.to_string()),
                        _ => labels.push((kv.key.to_string(), kv.value.to_string())),
                    }
                }
            }
            classifications.sort();
            labels.sort();
            labels.dedup();
            rows.push((
                key.object_type.to_string(),
                key.object_id.clone(),
                classifications,
                labels,
            ));
        });
        rows.sort();
        rows
    }

    fn values(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn disabled_registry_records_nothing() {
        let registry = PerObjectMetricsRegistry::new(Vec::new(), Duration::from_secs(60));
        registry.record(
            "machine",
            "machine-a",
            &classifications(&["Hardware"]),
            vec![],
        );
        assert!(snapshot(&registry).is_empty());
    }

    #[test]
    fn record_retains_only_opted_in_classifications_and_labels() {
        let registry =
            PerObjectMetricsRegistry::new(classifications(&["Hardware"]), Duration::from_secs(60));

        registry.record(
            "machine",
            "machine-a",
            &classifications(&["Hardware", "PreventAllocations"]),
            vec![KeyValue::new("in_use", "true")],
        );

        assert_eq!(
            snapshot(&registry),
            vec![(
                "machine".to_string(),
                "machine-a".to_string(),
                vec!["Hardware".to_string()],
                vec![("in_use".to_string(), "true".to_string())],
            )]
        );
    }

    #[test]
    fn record_without_opted_in_classification_removes_existing_entry() {
        let registry =
            PerObjectMetricsRegistry::new(classifications(&["Hardware"]), Duration::from_secs(60));

        registry.record(
            "machine",
            "machine-a",
            &classifications(&["Hardware"]),
            vec![],
        );
        assert_eq!(snapshot(&registry).len(), 1);

        // The object now carries only non-opted-in classifications: its series
        // must stop being emitted, and extra labels alone must not keep it alive.
        registry.record(
            "machine",
            "machine-a",
            &classifications(&["PreventAllocations"]),
            vec![KeyValue::new("in_use", "false")],
        );
        assert!(snapshot(&registry).is_empty());
    }

    #[test]
    fn distinct_object_types_and_ids_are_independent() {
        let registry =
            PerObjectMetricsRegistry::new(classifications(&["Hardware"]), Duration::from_secs(60));

        registry.record(
            "machine",
            "shared-id",
            &classifications(&["Hardware"]),
            vec![],
        );
        registry.record(
            "switch",
            "shared-id",
            &classifications(&["Hardware"]),
            vec![],
        );

        assert_eq!(snapshot(&registry).len(), 2);
    }

    #[test]
    fn stale_entries_are_evicted_on_read() {
        let registry =
            PerObjectMetricsRegistry::new(classifications(&["Hardware"]), Duration::from_millis(0));

        registry.record(
            "machine",
            "machine-a",
            &classifications(&["Hardware"]),
            vec![],
        );

        // With a zero hold period the entry is immediately stale on the next read.
        std::thread::sleep(Duration::from_millis(5));
        assert!(snapshot(&registry).is_empty());
    }

    #[test]
    fn gauge_set_replaces_the_objects_series() {
        let gauge = PerObjectGauge::new(&["state"], Duration::from_secs(60));

        gauge.set("machine", "machine-a", 1.0, values(&["provisioning"]));
        gauge.set("machine", "machine-a", 2.0, values(&["ready"]));

        assert_eq!(
            gauge_snapshot(&gauge),
            vec![(
                "machine".to_string(),
                "machine-a".to_string(),
                vec![(2.0, values(&["ready"]))],
            )]
        );
    }

    #[test]
    fn gauge_set_all_emits_one_series_per_entry_and_empty_removes() {
        let gauge = PerObjectGauge::new(&["dpu_id"], Duration::from_secs(60));

        gauge.set_all(
            "machine",
            "machine-a",
            vec![(1.0, values(&["dpu-1"])), (1.0, values(&["dpu-2"]))],
        );
        assert_eq!(gauge_snapshot(&gauge)[0].2.len(), 2);

        gauge.set_all("machine", "machine-a", vec![]);
        assert!(gauge_snapshot(&gauge).is_empty());
    }

    #[test]
    #[should_panic(expected = "label values")]
    fn gauge_set_rejects_values_not_matching_the_schema() {
        let gauge = PerObjectGauge::new(&["state", "substate"], Duration::from_secs(60));
        gauge.set("machine", "machine-a", 1.0, values(&["provisioning"]));
    }

    #[test]
    fn gauge_clear_removes_only_the_given_object() {
        let gauge = PerObjectGauge::new(&[], Duration::from_secs(60));

        gauge.set("machine", "machine-a", 1.0, vec![]);
        gauge.set("switch", "switch-a", 1.0, vec![]);

        gauge.clear("machine", "machine-a");

        let rows = gauge_snapshot(&gauge);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "switch");
    }

    #[test]
    fn prometheus_collector_exports_more_than_otel_default_cardinality() {
        // Native collection must not inherit OpenTelemetry's default
        // per-stream cardinality limit (2000 series per instrument).
        let registry = PerObjectMetricsRegistry::new(Vec::new(), Duration::from_secs(60));
        let prometheus_registry = prometheus::Registry::new();
        let gauge = registry
            .gauge(
                &prometheus_registry,
                "test_per_object_gauge",
                "test gauge",
                &["object_type", "object_id"],
                Duration::from_secs(60),
            )
            .unwrap();

        for i in 0..2100 {
            gauge.set(
                "machine",
                &format!("machine-{i}"),
                1.0,
                values(&["machine", &format!("machine-{i}")]),
            );
        }

        let families = prometheus_registry.gather();
        assert_eq!(families.len(), 1);
        assert_eq!(families[0].get_metric().len(), 2100);
    }

    #[test]
    fn clear_object_sweeps_all_registry_gauges() {
        let registry =
            PerObjectMetricsRegistry::new(classifications(&["Hardware"]), Duration::from_secs(60));
        let prometheus_registry = prometheus::Registry::new();
        let gauge_a = registry
            .gauge(
                &prometheus_registry,
                "test_gauge_a",
                "a",
                &[],
                Duration::from_secs(60),
            )
            .unwrap();
        let gauge_b = registry
            .gauge(
                &prometheus_registry,
                "test_gauge_b",
                "b",
                &[],
                Duration::from_secs(60),
            )
            .unwrap();

        registry.record(
            "machine",
            "machine-a",
            &classifications(&["Hardware"]),
            vec![],
        );
        gauge_a.set("machine", "machine-a", 1.0, vec![]);
        gauge_b.set("machine", "machine-a", 1.0, vec![]);
        gauge_a.set("machine", "machine-b", 1.0, vec![]);

        registry.clear_object("machine", "machine-a");

        assert!(snapshot(&registry).is_empty());
        let rows = gauge_snapshot(&gauge_a);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "machine-b");
        assert!(gauge_snapshot(&gauge_b).is_empty());
    }

    #[test]
    fn touch_if_values_clears_series_with_stale_values() {
        let gauge = PerObjectGauge::new(&["state", "reason"], Duration::from_secs(60));
        gauge.set("machine", "machine-a", 1.0, values(&["failed", "hardware"]));

        // The fact still matches the object's state (prefix compare, so the
        // trailing reason label is not required): kept alive.
        gauge.touch_if_values("machine", "machine-a", &values(&["failed"]));
        assert_eq!(gauge_snapshot(&gauge).len(), 1);

        // The object's state moved on: the stale fact is removed rather than
        // kept publishing contradictory labels.
        gauge.touch_if_values("machine", "machine-a", &values(&["provisioning"]));
        assert!(gauge_snapshot(&gauge).is_empty());
    }

    #[test]
    fn gauge_stale_entries_are_evicted_on_read() {
        let gauge = PerObjectGauge::new(&[], Duration::from_millis(0));

        gauge.set("machine", "machine-a", 1.0, vec![]);

        std::thread::sleep(Duration::from_millis(5));
        assert!(gauge_snapshot(&gauge).is_empty());
    }
}
