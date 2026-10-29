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

//! The site state metrics. Gauges come from a custom collector that loads
//! each family's snapshot once per scrape, so a count and its sweep times in
//! one scrape always come from the same sweep. Counters (sweeps, unknown
//! states) are counted on the OTel meter as snapshots are delivered; calls
//! are counted by the gates.
//!
//! Label rules: no unit IDs, URLs or regexes in labels. A label set missing
//! from a newer snapshot is published as 0 for [`RETIRE_AFTER`], then
//! dropped; a failed read keeps the last snapshot, so it never publishes 0.
//! Host versions are no labels: exact-version counts wait for the firmware
//! tests (#8154).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use carbide_site_health_probe_api::{Family, HostFields, SiteMatch, Stage, Unit, UnitType};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use prometheus::core::{Collector, Desc};
use prometheus::proto::MetricFamily;
use prometheus::{GaugeVec, Opts};
use serde::Serialize;
use tokio::time::Instant;

use crate::config::HostReadMode;
use crate::readers::rest_inventory::{REST_RESOURCES, RestTotalsData, SiteMatchData};
use crate::snapshot::{AnySnapshot, FailureKind, FamilySnapshot, Reader, SnapshotSink, UnitsData};
use crate::state::store::{ALL_FAMILIES, SnapshotStore};

const PREFIX: &str = "carbide_site_health_probe_";

/// How long a vanished label set is published as 0 before it is dropped.
pub(crate) const RETIRE_AFTER: Duration = Duration::from_secs(15 * 60);

/// The value of an absent label.
const UNKNOWN: &str = "unknown";
const NONE: &str = "none";

/// Every gauge: name without prefix, help, labels.
const GAUGES: &[(&str, &str, &[&str])] = &[
    (
        "units",
        "Units per type, stage (registered, ready, error) and state, from the newest complete sweep.",
        &["type", "stage", "state"],
    ),
    (
        "hosts",
        "Hosts per model, instance type, state, allocatability and update completion.",
        &[
            "model",
            "instance_type",
            "state",
            "allocatable",
            "update_complete",
        ],
    ),
    (
        "hosts_update_eligible",
        "Hosts per model and firmware update eligibility (true, false, unknown) with the blocking reason.",
        &["model", "eligible", "reason"],
    ),
    (
        "host_firmware_requests",
        "Open host firmware requests per model and initiator (automatic, manual, other).",
        &["model", "initiator"],
    ),
    (
        "unresolved_units",
        "Units the probe counts but cannot place, per type (DPUs no host lists).",
        &["type"],
    ),
    (
        "expected_records",
        "Expected records per type and whether exactly one device in the census links to them.",
        &["type", "linked"],
    ),
    (
        "identity_mismatches",
        "Expected records linked to several devices and devices without a record, per type and kind.",
        &["type", "kind"],
    ),
    (
        "rest_totals",
        "REST site totals per resource. Absent while the total is unavailable.",
        &["resource"],
    ),
    (
        "rest_totals_available",
        "1 while the REST total of a resource exists, 0 after the site answered 412.",
        &["resource"],
    ),
    (
        "rest_site_match",
        "Whether the REST site's machines are this NICo site's (match, mismatch, unknown), one-hot.",
        &["result"],
    ),
    (
        "sweep_start_timestamp_seconds",
        "Unix time the sweep behind a family's current snapshot started.",
        &["reader", "family"],
    ),
    (
        "sweep_complete_timestamp_seconds",
        "Unix time the sweep behind a family's current snapshot completed.",
        &["reader", "family"],
    ),
    (
        "sweep_duration_seconds",
        "Duration of the sweep behind a family's current snapshot.",
        &["reader", "family"],
    ),
    (
        "last_attempt_failed",
        "Why the newest attempt to read a family failed (none, timeout, pushback, rpc, unresolved, decode), one-hot.",
        &["reader", "family", "kind"],
    ),
    (
        "process_start_timestamp_seconds",
        "Unix time this probe process started.",
        &[],
    ),
    (
        "host_read_mode",
        "How host details are read (full, compact), one-hot.",
        &["mode"],
    ),
];

/// Gauges that are never published as 0 for a vanished label set.
const NOT_RETIRED: &[&str] = &["rest_totals"];

const FAILURE_KINDS: [FailureKind; 5] = [
    FailureKind::Timeout,
    FailureKind::Pushback,
    FailureKind::Rpc,
    FailureKind::Unresolved,
    FailureKind::Decode,
];

/// The wire name of an API enum, which is also its label value.
fn wire<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| UNKNOWN.to_string())
}

fn flag(value: bool) -> String {
    value.to_string()
}

fn unix_seconds(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub(crate) fn units_of(snap: &dyn AnySnapshot) -> Option<&UnitsData> {
    snap.as_any()
        .downcast_ref::<FamilySnapshot<UnitsData>>()
        .map(|s| &s.data)
}

pub(crate) fn data_of<T: 'static>(snap: &dyn AnySnapshot) -> Option<&T> {
    snap.as_any()
        .downcast_ref::<FamilySnapshot<T>>()
        .map(|s| &s.data)
}

/// Series per gauge name: label values in [`GAUGES`] order, and value.
pub(crate) type Series = BTreeMap<&'static str, BTreeMap<Vec<String>, f64>>;

/// The label names of the gauge `name`.
pub(crate) fn gauge_labels(name: &str) -> &'static [&'static str] {
    GAUGES
        .iter()
        .find(|(gauge, _, _)| *gauge == name)
        .map_or(&[], |(_, _, labels)| *labels)
}

/// Adds `value` to the series `name{labels}`.
fn add(series: &mut Series, name: &'static str, labels: Vec<String>, value: f64) {
    *series.entry(name).or_default().entry(labels).or_insert(0.0) += value;
}

/// Sets the series `name{labels}` to `value`.
fn set(series: &mut Series, name: &'static str, labels: Vec<String>, value: f64) {
    series.entry(name).or_default().insert(labels, value);
}

/// Publishes the site state gauges from the snapshot store.
pub(crate) struct StateCollector {
    store: Arc<SnapshotStore>,
    process_start: f64,
    host_read_mode: HostReadMode,
    /// Prototypes for `desc`; every scrape builds fresh vectors.
    prototypes: Vec<GaugeVec>,
    /// Label sets published so far and when they were last present.
    seen: Mutex<HashMap<(&'static str, Vec<String>), Instant>>,
}

fn gauge_vec(name: &str, help: &str, labels: &[&str]) -> GaugeVec {
    GaugeVec::new(Opts::new(format!("{PREFIX}{name}"), help), labels)
        .expect("valid static gauge definition")
}

impl StateCollector {
    pub(crate) fn new(
        store: Arc<SnapshotStore>,
        process_start: SystemTime,
        host_read_mode: HostReadMode,
    ) -> Self {
        Self {
            store,
            process_start: unix_seconds(process_start),
            host_read_mode,
            prototypes: GAUGES
                .iter()
                .map(|(name, help, labels)| gauge_vec(name, help, labels))
                .collect(),
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Every series of one scrape. Each family's snapshot is loaded once.
    fn series(&self) -> Series {
        let mut series = Series::new();
        set(
            &mut series,
            "process_start_timestamp_seconds",
            vec![],
            self.process_start,
        );
        let compact = self.host_read_mode == HostReadMode::Compact;
        set(
            &mut series,
            "host_read_mode",
            vec!["full".into()],
            f64::from(u8::from(!compact)),
        );
        set(
            &mut series,
            "host_read_mode",
            vec!["compact".into()],
            f64::from(u8::from(compact)),
        );

        for family in ALL_FAMILIES {
            if let Some(attempt) = self.store.last_attempt(family) {
                let failed = attempt.failure.as_ref().map(|(kind, _)| *kind);
                let labels = |kind: &str| {
                    vec![
                        attempt.reader.as_str().to_string(),
                        wire(family),
                        kind.to_string(),
                    ]
                };
                set(
                    &mut series,
                    "last_attempt_failed",
                    labels("none"),
                    f64::from(u8::from(failed.is_none())),
                );
                for kind in FAILURE_KINDS {
                    set(
                        &mut series,
                        "last_attempt_failed",
                        labels(kind.as_str()),
                        f64::from(u8::from(failed == Some(kind))),
                    );
                }
            }
            let Some(snap) = self.store.current(family) else {
                continue;
            };
            let meta = snap.meta();
            let times = vec![meta.reader.as_str().to_string(), wire(family)];
            set(
                &mut series,
                "sweep_start_timestamp_seconds",
                times.clone(),
                unix_seconds(meta.start),
            );
            set(
                &mut series,
                "sweep_complete_timestamp_seconds",
                times.clone(),
                unix_seconds(meta.end),
            );
            set(
                &mut series,
                "sweep_duration_seconds",
                times,
                meta.end
                    .duration_since(meta.start)
                    .unwrap_or_default()
                    .as_secs_f64(),
            );
            family_series(family, snap.as_ref(), &mut series);
        }
        series
    }

    /// How many label sets the retirement rule tracks.
    #[cfg(test)]
    pub(crate) fn tracked_label_sets(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// Applies the retirement rule: a label set gone from the current
    /// series is 0 until [`RETIRE_AFTER`] after it was last present.
    fn retire(&self, series: &mut Series) {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        for (name, sets) in series.iter() {
            if NOT_RETIRED.contains(name) {
                continue;
            }
            for labels in sets.keys() {
                seen.insert((*name, labels.clone()), now);
            }
        }
        seen.retain(|(name, labels), last| {
            if now.duration_since(*last) >= RETIRE_AFTER {
                return false;
            }
            series
                .entry(*name)
                .or_default()
                .entry(labels.clone())
                .or_insert(0.0);
            true
        });
    }
}

/// The gauges a family's snapshot contributes. `/v1/count` sums these same
/// series, so a count always equals the published metric.
pub(crate) fn family_series(family: Family, snap: &dyn AnySnapshot, series: &mut Series) {
    match family {
        Family::Host => {
            if let Some(data) = units_of(snap) {
                unit_series(&data.units, series);
                host_series(&data.units, series);
            }
        }
        Family::Dpu => {
            if let Some(data) = units_of(snap) {
                unit_series(&data.units, series);
                let unattached = data
                    .units
                    .iter()
                    .filter(|u| u.dpu.as_ref().is_some_and(|d| d.host_id.is_none()))
                    .count();
                set(
                    series,
                    "unresolved_units",
                    vec![wire(UnitType::Dpu)],
                    unattached as f64,
                );
            }
        }
        Family::Switch | Family::PowerShelf | Family::Rack | Family::Instance => {
            if let Some(data) = units_of(snap) {
                unit_series(&data.units, series);
            }
        }
        Family::Expected => {
            if let Some(data) = units_of(snap) {
                expected_series(&data.units, series);
            }
        }
        Family::HostFirmwareRequests => {
            if let Some(data) = units_of(snap) {
                for unit in &data.units {
                    let Some(host) = &unit.host else { continue };
                    let initiator = host
                        .open_firmware_request
                        .as_ref()
                        .map_or_else(|| UNKNOWN.to_string(), |r| wire(r.initiator));
                    add(
                        series,
                        "host_firmware_requests",
                        vec![model_label(host), initiator],
                        1.0,
                    );
                }
            }
        }
        Family::RestTotals => {
            if let Some(data) = data_of::<RestTotalsData>(snap) {
                for resource in REST_RESOURCES {
                    let total = data.totals.get(&resource).copied().flatten();
                    set(
                        series,
                        "rest_totals_available",
                        vec![wire(resource)],
                        f64::from(u8::from(total.is_some())),
                    );
                    if let Some(total) = total {
                        set(series, "rest_totals", vec![wire(resource)], total as f64);
                    }
                }
            }
        }
        Family::RestSiteMatch => {
            if let Some(data) = data_of::<SiteMatchData>(snap) {
                for result in [SiteMatch::Match, SiteMatch::Mismatch, SiteMatch::Unknown] {
                    set(
                        series,
                        "rest_site_match",
                        vec![wire(result)],
                        f64::from(u8::from(data.result == result)),
                    );
                }
            }
        }
        // Read by no reader until the firmware and instance tests (#8154,
        // #8152).
        Family::RestInstances
        | Family::RestTenantInstances
        | Family::ConfigNicoApi
        | Family::ConfigRest => {}
    }
}

/// `units{type, stage, state}`: every unit is registered; ready and error
/// units also count under their own stage.
fn unit_series(units: &[Unit], series: &mut Series) {
    for unit in units {
        let labels = |stage: Stage| vec![wire(unit.unit_type), wire(stage), unit.state.clone()];
        add(series, "units", labels(Stage::Registered), 1.0);
        if matches!(unit.stage, Stage::Ready | Stage::Error) {
            add(series, "units", labels(unit.stage), 1.0);
        }
    }
}

/// The `model` label of a host. `/v1/units` filters match this value too, so a
/// count and a listing of the same filter cannot disagree.
pub(crate) fn model_label(host: &HostFields) -> String {
    host.model.clone().unwrap_or_else(|| UNKNOWN.to_string())
}

/// The `instance_type` label of a host, shared with `/v1/units` like
/// [`model_label`].
pub(crate) fn instance_type_label(host: &HostFields) -> String {
    host.instance_type
        .clone()
        .unwrap_or_else(|| NONE.to_string())
}

fn host_series(units: &[Unit], series: &mut Series) {
    for (unit, host) in units.iter().filter_map(|u| u.host.as_ref().map(|h| (u, h))) {
        let allocatable = host.allocatable.map_or_else(|| UNKNOWN.to_string(), flag);
        add(
            series,
            "hosts",
            vec![
                model_label(host),
                instance_type_label(host),
                unit.state.clone(),
                allocatable,
                flag(host.update_complete),
            ],
            1.0,
        );
        let eligible = host
            .update_eligible
            .map_or_else(|| UNKNOWN.to_string(), flag);
        let reason = host
            .update_blocking_reason
            .clone()
            .unwrap_or_else(|| NONE.to_string());
        add(
            series,
            "hosts_update_eligible",
            vec![model_label(host), eligible, reason],
            1.0,
        );
    }
}

fn expected_series(units: &[Unit], series: &mut Series) {
    for unit in units {
        let Some(expected) = &unit.expected else {
            continue;
        };
        match unit.unit_type {
            UnitType::ExpectedRecord => add(
                series,
                "expected_records",
                vec![wire(expected.record_type), flag(expected.linked)],
                1.0,
            ),
            UnitType::IdentityMismatch => add(
                series,
                "identity_mismatches",
                vec![wire(expected.record_type), unit.state.clone()],
                1.0,
            ),
            _ => {}
        }
    }
}

impl Collector for StateCollector {
    fn desc(&self) -> Vec<&Desc> {
        self.prototypes.iter().flat_map(|g| g.desc()).collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let mut series = self.series();
        self.retire(&mut series);
        let mut families = Vec::new();
        for (name, help, labels) in GAUGES {
            let Some(sets) = series.get(name) else {
                continue;
            };
            let gauges = gauge_vec(name, help, labels);
            for (values, value) in sets {
                let values: Vec<&str> = values.iter().map(String::as_str).collect();
                gauges.with_label_values(&values).set(*value);
            }
            families.extend(gauges.collect());
        }
        families
    }
}

/// The readers' sink: counts each delivery on the OTel meter, then stores
/// it.
pub(crate) struct CountingSink {
    store: Arc<SnapshotStore>,
    sweeps: Counter<u64>,
    unknown_states: Counter<u64>,
}

impl CountingSink {
    pub(crate) fn new(store: Arc<SnapshotStore>, meter: &Meter) -> Self {
        Self {
            store,
            sweeps: meter
                .u64_counter(format!("{PREFIX}sweeps"))
                .with_description(
                    "Reader sweeps per family by result: complete (published) or incomplete \
                     (the family kept its last snapshot).",
                )
                .build(),
            unknown_states: meter
                .u64_counter(format!("{PREFIX}unknown_states"))
                .with_description(
                    "Units published with state other because their state is not in the \
                     pinned table, per metric and type.",
                )
                .build(),
        }
    }

    /// Publishes both sweep results of each enabled family at 0 before the
    /// first sweep, so a summary sees the first failure as a rise from a
    /// scraped 0, not as a new series it cannot count.
    pub(crate) fn start_counters(&self, families: &[Family]) {
        for family in families {
            let Some(reader) = reader_of(*family) else {
                continue;
            };
            for result in ["complete", "incomplete"] {
                self.add_sweeps(0, reader, *family, result);
            }
        }
    }

    fn count_sweep(&self, reader: Reader, family: Family, result: &'static str) {
        self.add_sweeps(1, reader, family, result);
    }

    fn add_sweeps(&self, n: u64, reader: Reader, family: Family, result: &'static str) {
        self.sweeps.add(
            n,
            &[
                KeyValue::new("reader", reader.as_str()),
                KeyValue::new("family", wire(family)),
                KeyValue::new("result", result),
            ],
        );
    }
}

/// The reader that sweeps `family`; `None` for families no reader sweeps.
fn reader_of(family: Family) -> Option<Reader> {
    match family {
        Family::Host
        | Family::Dpu
        | Family::Switch
        | Family::PowerShelf
        | Family::Rack
        | Family::Instance
        | Family::Expected
        | Family::HostFirmwareRequests => Some(Reader::Devices),
        Family::RestTotals | Family::RestSiteMatch => Some(Reader::RestInventory),
        Family::RestInstances
        | Family::RestTenantInstances
        | Family::ConfigNicoApi
        | Family::ConfigRest => None,
    }
}

impl SnapshotSink for CountingSink {
    fn publish(&self, family: Family, snap: Arc<dyn AnySnapshot>) {
        self.count_sweep(snap.meta().reader, family, "complete");
        if let Some(data) = units_of(snap.as_ref())
            && data.unknown_states > 0
            && let Some(unit) = data.units.first()
        {
            self.unknown_states.add(
                data.unknown_states,
                &[
                    KeyValue::new("metric", "units"),
                    KeyValue::new("type", wire(unit.unit_type)),
                ],
            );
        }
        self.store.publish(family, snap);
    }

    fn failed(&self, reader: Reader, family: Family, kind: FailureKind, error: &str) {
        self.count_sweep(reader, family, "incomplete");
        self.store.failed(reader, family, kind, error);
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use carbide_site_health_probe_api::{
        FirmwareRequest, Initiator, RecordType, RestResource, Source, VersionSource,
    };

    use super::*;
    use crate::config::Paging;
    use crate::snapshot::SweepMeta;

    struct Harness {
        store: Arc<SnapshotStore>,
        registry: prometheus::Registry,
        sink: CountingSink,
        _setup: metrics_endpoint::MetricsSetup,
    }

    fn harness() -> Harness {
        let setup = metrics_endpoint::new_metrics_setup("site-health-probe", "test", false)
            .expect("metrics setup");
        let store = Arc::new(SnapshotStore::new(&Paging::default()));
        setup
            .registry
            .register(Box::new(StateCollector::new(
                Arc::clone(&store),
                SystemTime::now(),
                HostReadMode::Full,
            )))
            .expect("register");
        Harness {
            sink: CountingSink::new(Arc::clone(&store), &setup.meter),
            registry: setup.registry.clone(),
            store,
            _setup: setup,
        }
    }

    fn exposition(registry: &prometheus::Registry) -> String {
        use prometheus::Encoder;
        let mut buffer = Vec::new();
        prometheus::TextEncoder::new()
            .encode(&registry.gather(), &mut buffer)
            .expect("encode");
        String::from_utf8(buffer).expect("utf-8")
    }

    /// Samples of `name` (exact) whose labels include `labels`.
    fn samples(out: &str, name: &str, labels: &[(&str, &str)]) -> Vec<f64> {
        out.lines()
            .filter(|line| !line.starts_with('#'))
            .filter(|line| line.split(['{', ' ']).next() == Some(&format!("{PREFIX}{name}")))
            .filter(|line| {
                labels
                    .iter()
                    .all(|(k, v)| line.contains(&format!("{k}=\"{v}\"")))
            })
            .filter_map(|line| line.rsplit(' ').next()?.parse().ok())
            .collect()
    }

    fn sum(out: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
        samples(out, name, labels).iter().sum()
    }

    fn meta(seq: u64, reader: Reader, end_secs: u64) -> SweepMeta {
        SweepMeta {
            process: 1,
            seq,
            reader,
            start: UNIX_EPOCH + Duration::from_secs(end_secs.saturating_sub(10)),
            end: UNIX_EPOCH + Duration::from_secs(end_secs),
        }
    }

    fn host_fields(model: &str, bmc: &str) -> HostFields {
        HostFields {
            vendor: Some("NVIDIA".to_string()),
            model: Some(model.to_string()),
            instance_type: None,
            bmc_version: Some(bmc.to_string()),
            uefi_version: Some("u1".to_string()),
            version_source: VersionSource::MachineRecord,
            report_at: None,
            report_error: None,
            allocatable: None,
            blocking_reason: Some("compact_host_read_missing".to_string()),
            firmware_autoupdate: None,
            update_eligible: None,
            update_blocking_reason: Some("compact_host_read_missing".to_string()),
            update_complete: false,
            open_firmware_request: None,
            nvlink_domain: None,
            rack_id: None,
            dpu_ids: vec![],
        }
    }

    fn unit(unit_type: UnitType, id: &str, state: &str, stage: Stage) -> Unit {
        Unit {
            id: id.to_string(),
            unit_type,
            source: Source::NicoApi,
            stage,
            done: stage == Stage::Ready,
            state: state.to_string(),
            reason: None,
            bmc_ip: None,
            host: None,
            dpu: None,
            instance: None,
            expected: None,
        }
    }

    fn host(i: usize, state: &str, model: &str, bmc: &str) -> Unit {
        let stage = match state {
            "Ready" => Stage::Ready,
            "Failed" => Stage::Error,
            _ => Stage::Registered,
        };
        Unit {
            host: Some(host_fields(model, bmc)),
            ..unit(UnitType::Host, &format!("fm100h{i:05}"), state, stage)
        }
    }

    fn publish_units(
        sink: &dyn SnapshotSink,
        family: Family,
        seq: u64,
        end: u64,
        units: Vec<Unit>,
    ) {
        sink.publish(
            family,
            Arc::new(FamilySnapshot {
                meta: meta(seq, Reader::Devices, end),
                data: UnitsData::new(units, 0),
            }),
        );
    }

    #[test]
    fn scrape_count_and_sweep_end_from_one_snapshot() {
        let h = harness();
        let store = Arc::clone(&h.store);
        // Snapshot n has n hosts and ends at second n.
        let publisher = thread::spawn(move || {
            for n in 1..=300u64 {
                let hosts = (0..n as usize)
                    .map(|i| host(i, "Ready", "m", "b1"))
                    .collect();
                publish_units(store.as_ref(), Family::Host, n, n, hosts);
            }
        });
        let mut checked = 0;
        while !publisher.is_finished() || checked == 0 {
            let out = exposition(&h.registry);
            let count = sum(&out, "units", &[("type", "host"), ("stage", "registered")]);
            let end = sum(
                &out,
                "sweep_complete_timestamp_seconds",
                &[("family", "host")],
            );
            assert_eq!(count, end, "count and sweep end come from one snapshot");
            checked += 1;
        }
        publisher.join().expect("publisher");
    }

    #[tokio::test(start_paused = true)]
    async fn disappearing_label_zero_then_retired() {
        let h = harness();
        publish_units(
            &h.sink,
            Family::Host,
            1,
            100,
            vec![
                host(1, "Ready", "m", "b1"),
                host(2, "HostInitializing", "m", "b1"),
            ],
        );
        let out = exposition(&h.registry);
        let initializing = [("type", "host"), ("state", "HostInitializing")];
        assert_eq!(samples(&out, "units", &initializing), vec![1.0]);

        publish_units(
            &h.sink,
            Family::Host,
            2,
            200,
            vec![host(1, "Ready", "m", "b1")],
        );
        let out = exposition(&h.registry);
        assert_eq!(
            samples(&out, "units", &initializing),
            vec![0.0],
            "published as 0"
        );

        tokio::time::sleep(RETIRE_AFTER).await;
        let out = exposition(&h.registry);
        assert!(
            samples(&out, "units", &initializing).is_empty(),
            "then dropped"
        );
        assert_eq!(
            samples(&out, "units", &[("state", "Ready"), ("stage", "ready")]),
            vec![1.0]
        );
    }

    fn publish_totals(sink: &dyn SnapshotSink, seq: u64, rack: Option<u64>) {
        sink.publish(
            Family::RestTotals,
            Arc::new(FamilySnapshot {
                meta: meta(seq, Reader::RestInventory, 100 * seq),
                data: RestTotalsData {
                    totals: BTreeMap::from([
                        (RestResource::Machine, Some(10)),
                        (RestResource::Instance, Some(4)),
                        (RestResource::Rack, rack),
                        (RestResource::Tray, None),
                    ]),
                },
            }),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unavailable_total_not_retired_to_zero() {
        let h = harness();
        publish_totals(&h.sink, 1, Some(5));
        let out = exposition(&h.registry);
        assert_eq!(
            samples(&out, "rest_totals", &[("resource", "rack")]),
            vec![5.0]
        );
        publish_totals(&h.sink, 2, None);
        let out = exposition(&h.registry);
        assert!(
            samples(&out, "rest_totals", &[("resource", "rack")]).is_empty(),
            "an unavailable total is absent, never 0"
        );
        assert_eq!(
            samples(&out, "rest_totals_available", &[("resource", "rack")]),
            vec![0.0]
        );
        assert!(samples(&out, "rest_totals", &[("resource", "tray")]).is_empty());
    }

    #[test]
    fn capability_flags_published_zero_and_one() {
        let h = harness();
        publish_totals(&h.sink, 1, Some(5));
        let out = exposition(&h.registry);
        for (resource, available) in [
            ("machine", 1.0),
            ("instance", 1.0),
            ("rack", 1.0),
            ("tray", 0.0),
        ] {
            assert_eq!(
                samples(&out, "rest_totals_available", &[("resource", resource)]),
                vec![available],
                "{resource}"
            );
        }
    }

    #[test]
    fn failed_read_never_zero() {
        let h = harness();
        publish_units(
            &h.sink,
            Family::Host,
            1,
            100,
            vec![host(1, "Ready", "m", "b1"), host(2, "Ready", "m", "b1")],
        );
        h.sink.failed(
            Reader::Devices,
            Family::Host,
            FailureKind::Rpc,
            "Unavailable",
        );
        let out = exposition(&h.registry);
        assert_eq!(
            sum(&out, "units", &[("type", "host"), ("stage", "registered")]),
            2.0
        );
        let failed = |kind| {
            samples(
                &out,
                "last_attempt_failed",
                &[("family", "host"), ("kind", kind)],
            )
        };
        assert_eq!(failed("rpc"), vec![1.0]);
        assert_eq!(failed("none"), vec![0.0]);
        assert_eq!(failed("timeout"), vec![0.0]);
        assert_eq!(
            sum(
                &out,
                "sweeps_total",
                &[("family", "host"), ("result", "incomplete")]
            ),
            1.0
        );
        assert_eq!(
            sum(
                &out,
                "sweeps_total",
                &[("family", "host"), ("result", "complete")]
            ),
            1.0
        );
    }

    #[test]
    fn provider_instance_total_unavailable_not_zero() {
        let h = harness();
        let mut ready = Box::pin(
            h.store
                .wait_all_ready(&[Family::RestTotals, Family::RestSiteMatch]),
        );
        let mut pending = || {
            ready
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        };
        // What the REST reader publishes: the instance total is never read.
        h.sink.publish(
            Family::RestTotals,
            Arc::new(FamilySnapshot {
                meta: meta(1, Reader::RestInventory, 100),
                data: RestTotalsData {
                    totals: BTreeMap::from([
                        (RestResource::Machine, Some(10)),
                        (RestResource::Instance, None),
                        (RestResource::Rack, Some(2)),
                        (RestResource::Tray, Some(8)),
                    ]),
                },
            }),
        );
        assert!(pending(), "the site check has not published");
        h.sink.publish(
            Family::RestSiteMatch,
            Arc::new(FamilySnapshot {
                meta: meta(1, Reader::RestInventory, 100),
                data: SiteMatchData {
                    result: SiteMatch::Match,
                },
            }),
        );
        assert!(!pending(), "ready once both REST families publish");

        let out = exposition(&h.registry);
        assert!(
            samples(&out, "rest_totals", &[("resource", "instance")]).is_empty(),
            "no instance total, not a 0 one"
        );
        assert_eq!(
            samples(&out, "rest_totals_available", &[("resource", "instance")]),
            vec![0.0]
        );
        for (resource, total) in [("machine", 10.0), ("rack", 2.0), ("tray", 8.0)] {
            assert_eq!(
                samples(&out, "rest_totals", &[("resource", resource)]),
                vec![total],
                "{resource}"
            );
        }
    }

    #[test]
    fn sweep_counters_start_at_zero() {
        let h = harness();
        h.sink
            .start_counters(&[Family::Host, Family::RestTotals, Family::ConfigRest]);
        let sweeps = |out: &str, family, result| {
            samples(
                out,
                "sweeps_total",
                &[("family", family), ("result", result)],
            )
        };
        let out = exposition(&h.registry);
        assert_eq!(sweeps(&out, "host", "incomplete"), vec![0.0], "before");
        assert_eq!(sweeps(&out, "host", "complete"), vec![0.0]);
        assert_eq!(
            samples(
                &out,
                "sweeps_total",
                &[("reader", "rest_inventory"), ("family", "rest_totals")]
            ),
            vec![0.0, 0.0]
        );
        assert_eq!(
            samples(&out, "sweeps_total", &[]).len(),
            4,
            "enabled swept families times 2 results"
        );

        h.sink
            .failed(Reader::Devices, Family::Host, FailureKind::Timeout, "slow");
        let out = exposition(&h.registry);
        assert_eq!(sweeps(&out, "host", "incomplete"), vec![1.0], "after");
        assert_eq!(sweeps(&out, "host", "complete"), vec![0.0]);
    }

    #[test]
    fn series_count_within_budget() {
        let h = harness();
        let host_states = crate::census::HOST_STATE_CATEGORIES.map(|(_, c)| c);
        let models = ["gb200", "gb300", "r750"];
        let versions = ["v1", "v2", "v3"];
        let hosts: Vec<Unit> = (0..4500)
            .map(|i| {
                let mut unit = host(
                    i,
                    host_states[i % host_states.len()],
                    models[i % 3],
                    versions[(i / 3) % 3],
                );
                if let Some(fields) = unit.host.as_mut() {
                    fields.uefi_version = Some(versions[(i / 9) % 3].to_string());
                    fields.instance_type = Some(format!("it-{}", i % 3));
                    fields.open_firmware_request = Some(FirmwareRequest {
                        initiator: Initiator::Automatic,
                        requested_at: None,
                        initiated_at: None,
                    });
                }
                unit
            })
            .collect();
        let firmware: Vec<Unit> = hosts
            .iter()
            .take(500)
            .map(|u| Unit {
                unit_type: UnitType::HostFirmwareRequest,
                ..u.clone()
            })
            .collect();
        let dpus: Vec<Unit> = (0..9000)
            .map(|i| {
                unit(
                    UnitType::Dpu,
                    &format!("fm100d{i:05}"),
                    host_states[i % host_states.len()],
                    Stage::Registered,
                )
            })
            .collect();
        // `n` registered units named `{prefix}{i}`, cycling through `states`.
        let fleet = |unit_type, prefix: &str, n: usize, states: &[&str]| -> Vec<Unit> {
            (0..n)
                .map(|i| {
                    let id = format!("{prefix}{i}");
                    unit(unit_type, &id, states[i % states.len()], Stage::Registered)
                })
                .collect()
        };
        let lifecycle = ["created", "initializing", "ready", "error", "maintenance"];
        let switches = fleet(UnitType::Switch, "sw", 2250, &lifecycle);
        let shelves = fleet(UnitType::PowerShelf, "ps", 2000, &lifecycle);
        let racks = fleet(UnitType::Rack, "rack-", 250, &lifecycle);
        let tenant = [
            "provisioning",
            "ready",
            "configuring",
            "terminating",
            "terminated",
            "failed",
            "updating",
            "invalid",
            "repairing",
        ];
        let instances = fleet(UnitType::Instance, "i-", 4500, &tenant);
        let expected: Vec<Unit> = (0..9000)
            .map(|i| {
                let record_type =
                    [RecordType::Host, RecordType::Switch, RecordType::PowerShelf][i % 3];
                Unit {
                    expected: Some(carbide_site_health_probe_api::ExpectedFields {
                        record_type,
                        linked: i % 2 == 0,
                        device_id: None,
                        bmc_mac: None,
                        explored_address: None,
                    }),
                    ..unit(
                        UnitType::ExpectedRecord,
                        &format!("e-{i}"),
                        "linked",
                        Stage::Expected,
                    )
                }
            })
            .collect();
        for (family, units) in [
            (Family::Host, hosts),
            (Family::Dpu, dpus),
            (Family::Switch, switches),
            (Family::PowerShelf, shelves),
            (Family::Rack, racks),
            (Family::Instance, instances),
            (Family::Expected, expected),
            (Family::HostFirmwareRequests, firmware),
        ] {
            publish_units(&h.sink, family, 1, 100, units);
            h.sink
                .failed(Reader::Devices, family, FailureKind::Timeout, "slow");
        }
        let out = exposition(&h.registry);
        let series = out.lines().filter(|line| line.starts_with(PREFIX)).count();
        assert!(series > 200, "the fleet shows up: {series} series");
        assert!(series < 2000, "{series} series");
    }

    #[test]
    fn no_unit_ids_in_label_values() {
        let h = harness();
        let hosts: Vec<Unit> = (0..20).map(|i| host(i, "Ready", "m", "b1")).collect();
        let ids: std::collections::BTreeSet<String> = hosts.iter().map(|u| u.id.clone()).collect();
        publish_units(&h.sink, Family::Host, 1, 100, hosts);
        let out = exposition(&h.registry);
        for id in &ids {
            assert!(!out.contains(id.as_str()), "{id} leaked into a label");
        }
    }

    #[test]
    fn latency_metrics_unchanged() {
        let h = harness();
        let latency = crate::metrics::Metrics::new(&h._setup.meter);
        crate::framework::Sink::record(
            &latency,
            &crate::framework::ProbeResult {
                probe: "grpc_machines",
                api: "nico-api",
                outcome: crate::framework::Outcome::Success,
                observations: vec![crate::framework::Observation {
                    operation: "find_machine_ids",
                    duration: Duration::from_millis(5),
                }],
                error: None,
                panicked: false,
                at: Some(SystemTime::now()),
            },
        );
        publish_units(
            &h.sink,
            Family::Host,
            1,
            100,
            vec![host(1, "Ready", "m", "b1")],
        );
        let out = exposition(&h.registry);
        for name in [
            "carbide_site_health_probe_requests_total{",
            "carbide_site_health_probe_up{",
            "carbide_site_health_probe_last_run_timestamp_seconds{",
            "carbide_site_health_probe_request_duration_milliseconds_bucket{",
        ] {
            assert!(out.contains(name), "{name} missing");
        }
        assert!(out.contains("carbide_site_health_probe_units{"));
    }
}
