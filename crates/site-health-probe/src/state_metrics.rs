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
//! Names, labels and label values follow the shared metric contract,
//! `carbide_site_health_probe_api::metrics`. A label set missing from a
//! newer snapshot is published as 0 for [`RETIRE_AFTER`], then dropped; a
//! failed read keeps the last snapshot, so it never publishes 0.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use carbide_site_health_probe_api::metrics::{
    EXPECTED_RECORDS, GAUGES, Gauge, HOST_FIRMWARE_REQUESTS, HOST_PREDICATES_KNOWN, HOST_READ_MODE,
    HOSTS, HOSTS_UPDATE_ELIGIBLE, IDENTITY_MISMATCHES, LAST_ATTEMPT_FAILED, NONE, PROCESS_START,
    REST_SITE_MATCH, REST_TOTALS, REST_TOTALS_AVAILABLE, SWEEP_COMPLETE, SWEEP_DURATION,
    SWEEP_RESULT_COMPLETE, SWEEP_RESULT_INCOMPLETE, SWEEP_START, SWEEPS, UNITS, UNKNOWN,
    UNKNOWN_STATES, UNRESOLVED_UNITS, flag_label, instance_type_label, model_label, wire,
};
use carbide_site_health_probe_api::{Family, SiteMatch, Stage, Unit, UnitType};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use prometheus::core::{Collector, Desc};
use prometheus::proto::MetricFamily;
use prometheus::{GaugeVec, Opts};
use tokio::time::Instant;

use crate::readers::rest_inventory::{REST_RESOURCES, RestTotalsData, SiteMatchData};
use crate::snapshot::{AnySnapshot, FailureKind, FamilySnapshot, Reader, SnapshotSink, UnitsData};
use crate::state::store::{ALL_FAMILIES, SnapshotStore};

/// How long a vanished label set is published as 0 before it is dropped.
pub(crate) const RETIRE_AFTER: Duration = Duration::from_secs(15 * 60);

/// Gauges that are never published as 0 for a vanished label set.
const NOT_RETIRED: &[&str] = &[REST_TOTALS.name];

const FAILURE_KINDS: [FailureKind; 5] = [
    FailureKind::Timeout,
    FailureKind::Pushback,
    FailureKind::Rpc,
    FailureKind::Unresolved,
    FailureKind::Decode,
];

fn flag(value: bool) -> String {
    value.to_string()
}

/// Unix seconds as the collector publishes them.
pub(crate) fn unix_seconds(at: SystemTime) -> f64 {
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
    /// Prototypes for `desc`; every scrape builds fresh vectors.
    prototypes: Vec<GaugeVec>,
    /// Label sets published so far and when they were last present.
    seen: Mutex<HashMap<(&'static str, Vec<String>), Instant>>,
}

fn gauge_vec(gauge: &Gauge) -> GaugeVec {
    GaugeVec::new(Opts::new(gauge.metric_name(), gauge.help), gauge.labels)
        .expect("valid static gauge definition")
}

impl StateCollector {
    pub(crate) fn new(store: Arc<SnapshotStore>, process_start: SystemTime) -> Self {
        Self {
            store,
            process_start: unix_seconds(process_start),
            prototypes: GAUGES.iter().map(gauge_vec).collect(),
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Every series of one scrape. Each family's snapshot is loaded once.
    fn series(&self) -> Series {
        let mut series = Series::new();
        set(&mut series, PROCESS_START.name, vec![], self.process_start);

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
                    LAST_ATTEMPT_FAILED.name,
                    labels("none"),
                    f64::from(u8::from(failed.is_none())),
                );
                for kind in FAILURE_KINDS {
                    set(
                        &mut series,
                        LAST_ATTEMPT_FAILED.name,
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
                SWEEP_START.name,
                times.clone(),
                unix_seconds(meta.start),
            );
            set(
                &mut series,
                SWEEP_COMPLETE.name,
                times.clone(),
                unix_seconds(meta.end),
            );
            set(
                &mut series,
                SWEEP_DURATION.name,
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
                for (mode, on) in [
                    ("full", !data.compact_host_read),
                    ("compact", data.compact_host_read),
                ] {
                    set(
                        series,
                        HOST_READ_MODE.name,
                        vec![mode.to_string()],
                        f64::from(u8::from(on)),
                    );
                }
                // A filter on a predicate is answered, and a summary may
                // count the sweep, only when every host has it.
                for predicate in ["allocatable", "updateEligible"] {
                    set(
                        series,
                        HOST_PREDICATES_KNOWN.name,
                        vec![predicate.to_string()],
                        f64::from(u8::from(data.host_predicate_known(predicate))),
                    );
                }
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
                    UNRESOLVED_UNITS.name,
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
                        HOST_FIRMWARE_REQUESTS.name,
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
                        REST_TOTALS_AVAILABLE.name,
                        vec![wire(resource)],
                        f64::from(u8::from(total.is_some())),
                    );
                    if let Some(total) = total {
                        set(series, REST_TOTALS.name, vec![wire(resource)], total as f64);
                    }
                }
            }
        }
        Family::RestSiteMatch => {
            if let Some(data) = data_of::<SiteMatchData>(snap) {
                for result in [SiteMatch::Match, SiteMatch::Mismatch, SiteMatch::Unknown] {
                    set(
                        series,
                        REST_SITE_MATCH.name,
                        vec![wire(result)],
                        f64::from(u8::from(data.result == result)),
                    );
                }
            }
        }
        // Served by `/v1/config` only.
        Family::ConfigNicoApi => {}
        // Read by no reader until the instance tests (#8152).
        Family::RestInstances | Family::RestTenantInstances | Family::ConfigRest => {}
    }
}

/// `units{type, stage, state}`: every unit is registered; ready and error
/// units also count under their own stage.
fn unit_series(units: &[Arc<Unit>], series: &mut Series) {
    for unit in units {
        let labels = |stage: Stage| vec![wire(unit.unit_type), wire(stage), unit.state.clone()];
        add(series, UNITS.name, labels(Stage::Registered), 1.0);
        if matches!(unit.stage, Stage::Ready | Stage::Error) {
            add(series, UNITS.name, labels(unit.stage), 1.0);
        }
    }
}

fn host_series(units: &[Arc<Unit>], series: &mut Series) {
    for (unit, host) in units.iter().filter_map(|u| u.host.as_ref().map(|h| (u, h))) {
        add(
            series,
            HOSTS.name,
            vec![
                model_label(host),
                instance_type_label(host),
                unit.state.clone(),
                flag_label(host.allocatable),
                flag(host.update_complete),
            ],
            1.0,
        );
        let eligible = flag_label(host.update_eligible);
        let reason = host
            .update_blocking_reason
            .clone()
            .unwrap_or_else(|| NONE.to_string());
        add(
            series,
            HOSTS_UPDATE_ELIGIBLE.name,
            vec![model_label(host), eligible, reason],
            1.0,
        );
    }
}

fn expected_series(units: &[Arc<Unit>], series: &mut Series) {
    for unit in units {
        let Some(expected) = &unit.expected else {
            continue;
        };
        match unit.unit_type {
            UnitType::ExpectedRecord => add(
                series,
                EXPECTED_RECORDS.name,
                vec![wire(expected.record_type), flag(expected.linked)],
                1.0,
            ),
            UnitType::IdentityMismatch => add(
                series,
                IDENTITY_MISMATCHES.name,
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
        for gauge in GAUGES {
            let Some(sets) = series.get(gauge.name) else {
                continue;
            };
            let gauges = gauge_vec(gauge);
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
                .u64_counter(SWEEPS.name)
                .with_description(SWEEPS.help)
                .build(),
            unknown_states: meter
                .u64_counter(UNKNOWN_STATES.name)
                .with_description(UNKNOWN_STATES.help)
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
            for result in [SWEEP_RESULT_COMPLETE, SWEEP_RESULT_INCOMPLETE] {
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
        Family::ConfigNicoApi => Some(Reader::NicoConfig),
        Family::RestInstances | Family::RestTenantInstances | Family::ConfigRest => None,
    }
}

impl SnapshotSink for CountingSink {
    fn publish(&self, family: Family, snap: Arc<dyn AnySnapshot>) {
        self.count_sweep(snap.meta().reader, family, SWEEP_RESULT_COMPLETE);
        if let Some(data) = units_of(snap.as_ref())
            && data.unknown_states > 0
            && let Some(unit) = data.units.first()
        {
            self.unknown_states.add(
                data.unknown_states,
                &[
                    KeyValue::new("metric", UNITS.name),
                    KeyValue::new("type", wire(unit.unit_type)),
                ],
            );
        }
        self.store.publish(family, snap);
    }

    fn failed(&self, reader: Reader, family: Family, kind: FailureKind, error: &str) {
        self.count_sweep(reader, family, SWEEP_RESULT_INCOMPLETE);
        self.store.failed(reader, family, kind, error);
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use carbide_site_health_probe_api::metrics::PREFIX;
    use carbide_site_health_probe_api::{
        FirmwareRequest, HostFields, Initiator, RecordType, RestResource, Source, VersionSource,
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

    /// Every state metric's name, help, labels and value for one fixed
    /// site. The shared metric contract must keep each line.
    const EXPOSITION: &str = r#"
# HELP carbide_site_health_probe_expected_records Expected records per type and whether exactly one device in the census links to them.
# TYPE carbide_site_health_probe_expected_records gauge
carbide_site_health_probe_expected_records{linked="false",type="rack"} 1
carbide_site_health_probe_expected_records{linked="true",type="host"} 1
# HELP carbide_site_health_probe_host_firmware_requests Open host firmware requests per model and initiator (automatic, manual, other).
# TYPE carbide_site_health_probe_host_firmware_requests gauge
carbide_site_health_probe_host_firmware_requests{initiator="manual",model="gb200"} 1
carbide_site_health_probe_host_firmware_requests{initiator="unknown",model="r750"} 1
# HELP carbide_site_health_probe_host_predicates_known 1 when the current host snapshot has the predicate for every host (allocatable, updateEligible), else 0.
# TYPE carbide_site_health_probe_host_predicates_known gauge
carbide_site_health_probe_host_predicates_known{predicate="allocatable"} 0
carbide_site_health_probe_host_predicates_known{predicate="updateEligible"} 0
# HELP carbide_site_health_probe_host_read_mode How the current host snapshot was read (full, compact), one-hot.
# TYPE carbide_site_health_probe_host_read_mode gauge
carbide_site_health_probe_host_read_mode{mode="compact"} 0
carbide_site_health_probe_host_read_mode{mode="full"} 1
# HELP carbide_site_health_probe_hosts Hosts per model, instance type, state, allocatability and update completion.
# TYPE carbide_site_health_probe_hosts gauge
carbide_site_health_probe_hosts{allocatable="true",instance_type="it-1",model="gb200",state="Ready",update_complete="true"} 1
carbide_site_health_probe_hosts{allocatable="unknown",instance_type="none",model="gb200",state="Failed",update_complete="false"} 1
carbide_site_health_probe_hosts{allocatable="unknown",instance_type="none",model="r750",state="HostInitializing",update_complete="false"} 1
carbide_site_health_probe_hosts{allocatable="unknown",instance_type="none",model="unknown",state="Ready",update_complete="false"} 1
# HELP carbide_site_health_probe_hosts_update_eligible Hosts per model and firmware update eligibility (true, false, unknown) with the blocking reason.
# TYPE carbide_site_health_probe_hosts_update_eligible gauge
carbide_site_health_probe_hosts_update_eligible{eligible="false",model="gb200",reason="in_use"} 1
carbide_site_health_probe_hosts_update_eligible{eligible="unknown",model="gb200",reason="compact_host_read_missing"} 1
carbide_site_health_probe_hosts_update_eligible{eligible="unknown",model="r750",reason="compact_host_read_missing"} 1
carbide_site_health_probe_hosts_update_eligible{eligible="unknown",model="unknown",reason="compact_host_read_missing"} 1
# HELP carbide_site_health_probe_identity_mismatches Expected records linked to several devices and devices without a record, per type and kind.
# TYPE carbide_site_health_probe_identity_mismatches gauge
carbide_site_health_probe_identity_mismatches{kind="ambiguous_link",type="switch"} 1
# HELP carbide_site_health_probe_last_attempt_failed Why the newest attempt to read a family failed (none, timeout, pushback, rpc, unresolved, decode), one-hot.
# TYPE carbide_site_health_probe_last_attempt_failed gauge
carbide_site_health_probe_last_attempt_failed{family="dpu",kind="decode",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="dpu",kind="none",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="dpu",kind="pushback",reader="devices"} 1
carbide_site_health_probe_last_attempt_failed{family="dpu",kind="rpc",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="dpu",kind="timeout",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="dpu",kind="unresolved",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="expected",kind="decode",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="expected",kind="none",reader="devices"} 1
carbide_site_health_probe_last_attempt_failed{family="expected",kind="pushback",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="expected",kind="rpc",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="expected",kind="timeout",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="expected",kind="unresolved",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host",kind="decode",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host",kind="none",reader="devices"} 1
carbide_site_health_probe_last_attempt_failed{family="host",kind="pushback",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host",kind="rpc",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host",kind="timeout",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host",kind="unresolved",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host_firmware_requests",kind="decode",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host_firmware_requests",kind="none",reader="devices"} 1
carbide_site_health_probe_last_attempt_failed{family="host_firmware_requests",kind="pushback",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host_firmware_requests",kind="rpc",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host_firmware_requests",kind="timeout",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="host_firmware_requests",kind="unresolved",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="instance",kind="decode",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="instance",kind="none",reader="devices"} 1
carbide_site_health_probe_last_attempt_failed{family="instance",kind="pushback",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="instance",kind="rpc",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="instance",kind="timeout",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="instance",kind="unresolved",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="power_shelf",kind="decode",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="power_shelf",kind="none",reader="devices"} 1
carbide_site_health_probe_last_attempt_failed{family="power_shelf",kind="pushback",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="power_shelf",kind="rpc",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="power_shelf",kind="timeout",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="power_shelf",kind="unresolved",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="rack",kind="decode",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="rack",kind="none",reader="devices"} 1
carbide_site_health_probe_last_attempt_failed{family="rack",kind="pushback",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="rack",kind="rpc",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="rack",kind="timeout",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="rack",kind="unresolved",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_site_match",kind="decode",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_site_match",kind="none",reader="rest_inventory"} 1
carbide_site_health_probe_last_attempt_failed{family="rest_site_match",kind="pushback",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_site_match",kind="rpc",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_site_match",kind="timeout",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_site_match",kind="unresolved",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_totals",kind="decode",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_totals",kind="none",reader="rest_inventory"} 1
carbide_site_health_probe_last_attempt_failed{family="rest_totals",kind="pushback",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_totals",kind="rpc",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_totals",kind="timeout",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="rest_totals",kind="unresolved",reader="rest_inventory"} 0
carbide_site_health_probe_last_attempt_failed{family="switch",kind="decode",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="switch",kind="none",reader="devices"} 1
carbide_site_health_probe_last_attempt_failed{family="switch",kind="pushback",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="switch",kind="rpc",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="switch",kind="timeout",reader="devices"} 0
carbide_site_health_probe_last_attempt_failed{family="switch",kind="unresolved",reader="devices"} 0
# HELP carbide_site_health_probe_process_start_timestamp_seconds Unix time this probe process started.
# TYPE carbide_site_health_probe_process_start_timestamp_seconds gauge
carbide_site_health_probe_process_start_timestamp_seconds 1000000
# HELP carbide_site_health_probe_rest_site_match Whether the REST site's machines are this NICo site's (match, mismatch, unknown), one-hot.
# TYPE carbide_site_health_probe_rest_site_match gauge
carbide_site_health_probe_rest_site_match{result="match"} 0
carbide_site_health_probe_rest_site_match{result="mismatch"} 1
carbide_site_health_probe_rest_site_match{result="unknown"} 0
# HELP carbide_site_health_probe_rest_totals REST site totals per resource. Absent while the total is unavailable.
# TYPE carbide_site_health_probe_rest_totals gauge
carbide_site_health_probe_rest_totals{resource="instance"} 4
carbide_site_health_probe_rest_totals{resource="machine"} 10
# HELP carbide_site_health_probe_rest_totals_available 1 while the REST total of a resource exists, 0 after the site answered 412.
# TYPE carbide_site_health_probe_rest_totals_available gauge
carbide_site_health_probe_rest_totals_available{resource="instance"} 1
carbide_site_health_probe_rest_totals_available{resource="machine"} 1
carbide_site_health_probe_rest_totals_available{resource="rack"} 0
carbide_site_health_probe_rest_totals_available{resource="tray"} 0
# HELP carbide_site_health_probe_sweep_complete_timestamp_seconds Unix time the sweep behind a family's current snapshot completed.
# TYPE carbide_site_health_probe_sweep_complete_timestamp_seconds gauge
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="dpu",reader="devices"} 110
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="expected",reader="devices"} 120
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="host",reader="devices"} 100
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="host_firmware_requests",reader="devices"} 130
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="instance",reader="devices"} 170
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="power_shelf",reader="devices"} 160
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="rack",reader="devices"} 140
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="rest_site_match",reader="rest_inventory"} 100
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="rest_totals",reader="rest_inventory"} 100
carbide_site_health_probe_sweep_complete_timestamp_seconds{family="switch",reader="devices"} 150
# HELP carbide_site_health_probe_sweep_duration_seconds Duration of the sweep behind a family's current snapshot.
# TYPE carbide_site_health_probe_sweep_duration_seconds gauge
carbide_site_health_probe_sweep_duration_seconds{family="dpu",reader="devices"} 10
carbide_site_health_probe_sweep_duration_seconds{family="expected",reader="devices"} 10
carbide_site_health_probe_sweep_duration_seconds{family="host",reader="devices"} 10
carbide_site_health_probe_sweep_duration_seconds{family="host_firmware_requests",reader="devices"} 10
carbide_site_health_probe_sweep_duration_seconds{family="instance",reader="devices"} 10
carbide_site_health_probe_sweep_duration_seconds{family="power_shelf",reader="devices"} 10
carbide_site_health_probe_sweep_duration_seconds{family="rack",reader="devices"} 10
carbide_site_health_probe_sweep_duration_seconds{family="rest_site_match",reader="rest_inventory"} 10
carbide_site_health_probe_sweep_duration_seconds{family="rest_totals",reader="rest_inventory"} 10
carbide_site_health_probe_sweep_duration_seconds{family="switch",reader="devices"} 10
# HELP carbide_site_health_probe_sweep_start_timestamp_seconds Unix time the sweep behind a family's current snapshot started.
# TYPE carbide_site_health_probe_sweep_start_timestamp_seconds gauge
carbide_site_health_probe_sweep_start_timestamp_seconds{family="dpu",reader="devices"} 100
carbide_site_health_probe_sweep_start_timestamp_seconds{family="expected",reader="devices"} 110
carbide_site_health_probe_sweep_start_timestamp_seconds{family="host",reader="devices"} 90
carbide_site_health_probe_sweep_start_timestamp_seconds{family="host_firmware_requests",reader="devices"} 120
carbide_site_health_probe_sweep_start_timestamp_seconds{family="instance",reader="devices"} 160
carbide_site_health_probe_sweep_start_timestamp_seconds{family="power_shelf",reader="devices"} 150
carbide_site_health_probe_sweep_start_timestamp_seconds{family="rack",reader="devices"} 130
carbide_site_health_probe_sweep_start_timestamp_seconds{family="rest_site_match",reader="rest_inventory"} 90
carbide_site_health_probe_sweep_start_timestamp_seconds{family="rest_totals",reader="rest_inventory"} 90
carbide_site_health_probe_sweep_start_timestamp_seconds{family="switch",reader="devices"} 140
# HELP carbide_site_health_probe_sweeps_total Reader sweeps per family by result: complete (published) or incomplete (the family kept its last snapshot).
# TYPE carbide_site_health_probe_sweeps_total counter
carbide_site_health_probe_sweeps_total{family="dpu",reader="devices",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="dpu",reader="devices",result="incomplete"} 1
carbide_site_health_probe_sweeps_total{family="expected",reader="devices",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="host",reader="devices",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="host",reader="devices",result="incomplete"} 0
carbide_site_health_probe_sweeps_total{family="host_firmware_requests",reader="devices",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="instance",reader="devices",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="power_shelf",reader="devices",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="rack",reader="devices",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="rest_site_match",reader="rest_inventory",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="rest_totals",reader="rest_inventory",result="complete"} 1
carbide_site_health_probe_sweeps_total{family="rest_totals",reader="rest_inventory",result="incomplete"} 0
carbide_site_health_probe_sweeps_total{family="switch",reader="devices",result="complete"} 1
# HELP carbide_site_health_probe_units Units per type, stage (registered, ready, error) and state, from the newest complete sweep.
# TYPE carbide_site_health_probe_units gauge
carbide_site_health_probe_units{stage="error",state="Failed",type="dpu"} 1
carbide_site_health_probe_units{stage="error",state="Failed",type="host"} 1
carbide_site_health_probe_units{stage="ready",state="Ready",type="dpu"} 1
carbide_site_health_probe_units{stage="ready",state="Ready",type="host"} 2
carbide_site_health_probe_units{stage="ready",state="ready",type="power_shelf"} 1
carbide_site_health_probe_units{stage="ready",state="ready",type="rack"} 1
carbide_site_health_probe_units{stage="registered",state="Failed",type="dpu"} 1
carbide_site_health_probe_units{stage="registered",state="Failed",type="host"} 1
carbide_site_health_probe_units{stage="registered",state="HostInitializing",type="host"} 1
carbide_site_health_probe_units{stage="registered",state="Ready",type="dpu"} 1
carbide_site_health_probe_units{stage="registered",state="Ready",type="host"} 2
carbide_site_health_probe_units{stage="registered",state="other",type="switch"} 1
carbide_site_health_probe_units{stage="registered",state="provisioning",type="instance"} 1
carbide_site_health_probe_units{stage="registered",state="ready",type="power_shelf"} 1
carbide_site_health_probe_units{stage="registered",state="ready",type="rack"} 1
# HELP carbide_site_health_probe_unknown_states_total Units published with state other because their state is not in the pinned table, per metric and type.
# TYPE carbide_site_health_probe_unknown_states_total counter
carbide_site_health_probe_unknown_states_total{metric="units",type="switch"} 1
# HELP carbide_site_health_probe_unresolved_units Units the probe counts but cannot place, per type (DPUs no host lists).
# TYPE carbide_site_health_probe_unresolved_units gauge
carbide_site_health_probe_unresolved_units{type="dpu"} 1
"#;

    /// Every state gauge's descriptor: name, labels in order, help.
    const DESCRIPTORS: &str = r#"
carbide_site_health_probe_units [type,stage,state] Units per type, stage (registered, ready, error) and state, from the newest complete sweep.
carbide_site_health_probe_hosts [model,instance_type,state,allocatable,update_complete] Hosts per model, instance type, state, allocatability and update completion.
carbide_site_health_probe_hosts_update_eligible [model,eligible,reason] Hosts per model and firmware update eligibility (true, false, unknown) with the blocking reason.
carbide_site_health_probe_host_firmware_requests [model,initiator] Open host firmware requests per model and initiator (automatic, manual, other).
carbide_site_health_probe_unresolved_units [type] Units the probe counts but cannot place, per type (DPUs no host lists).
carbide_site_health_probe_expected_records [type,linked] Expected records per type and whether exactly one device in the census links to them.
carbide_site_health_probe_identity_mismatches [type,kind] Expected records linked to several devices and devices without a record, per type and kind.
carbide_site_health_probe_rest_totals [resource] REST site totals per resource. Absent while the total is unavailable.
carbide_site_health_probe_rest_totals_available [resource] 1 while the REST total of a resource exists, 0 after the site answered 412.
carbide_site_health_probe_rest_site_match [result] Whether the REST site's machines are this NICo site's (match, mismatch, unknown), one-hot.
carbide_site_health_probe_sweep_start_timestamp_seconds [reader,family] Unix time the sweep behind a family's current snapshot started.
carbide_site_health_probe_sweep_complete_timestamp_seconds [reader,family] Unix time the sweep behind a family's current snapshot completed.
carbide_site_health_probe_sweep_duration_seconds [reader,family] Duration of the sweep behind a family's current snapshot.
carbide_site_health_probe_last_attempt_failed [reader,family,kind] Why the newest attempt to read a family failed (none, timeout, pushback, rpc, unresolved, decode), one-hot.
carbide_site_health_probe_process_start_timestamp_seconds [] Unix time this probe process started.
carbide_site_health_probe_host_read_mode [mode] How the current host snapshot was read (full, compact), one-hot.
carbide_site_health_probe_host_predicates_known [predicate] 1 when the current host snapshot has the predicate for every host (allocatable, updateEligible), else 0.
"#;

    #[test]
    fn exposition_golden() {
        let setup = metrics_endpoint::new_metrics_setup("site-health-probe", "test", false)
            .expect("metrics setup");
        let store = Arc::new(SnapshotStore::new(&Paging::default()));
        setup
            .registry
            .register(Box::new(StateCollector::new(
                Arc::clone(&store),
                UNIX_EPOCH + Duration::from_secs(1_000_000),
            )))
            .expect("register");
        let sink = CountingSink::new(Arc::clone(&store), &setup.meter);
        sink.start_counters(&[Family::Host, Family::Dpu, Family::RestTotals]);
        let mut hosts = vec![
            host(1, "Ready", "gb200", "b1"),
            host(2, "Failed", "gb200", "b1"),
            host(3, "HostInitializing", "r750", "b2"),
            host(4, "Ready", "gb200", "b1"),
        ];
        // A host with no model is the `unknown` model.
        if let Some(fields) = hosts[3].host.as_mut() {
            fields.model = None;
        }
        if let Some(fields) = hosts[0].host.as_mut() {
            fields.instance_type = Some("it-1".to_string());
            fields.allocatable = Some(true);
            fields.update_eligible = Some(false);
            fields.update_blocking_reason = Some("in_use".to_string());
            fields.update_complete = true;
        }
        let firmware = Unit {
            unit_type: UnitType::HostFirmwareRequest,
            host: Some(HostFields {
                open_firmware_request: Some(FirmwareRequest {
                    initiator: Initiator::Manual,
                    requested_at: None,
                    initiated_at: None,
                }),
                ..host_fields("gb200", "b1")
            }),
            ..unit(
                UnitType::HostFirmwareRequest,
                "fm100h00001",
                "open",
                Stage::Registered,
            )
        };
        // A request whose initiator is unknown.
        let unknown_firmware = Unit {
            unit_type: UnitType::HostFirmwareRequest,
            host: Some(host_fields("r750", "b2")),
            ..unit(
                UnitType::HostFirmwareRequest,
                "fm100h00003",
                "open",
                Stage::Registered,
            )
        };
        let dpus = vec![
            Unit {
                dpu: Some(carbide_site_health_probe_api::DpuFields { host_id: None }),
                ..unit(UnitType::Dpu, "fm100d1", "Ready", Stage::Ready)
            },
            unit(UnitType::Dpu, "fm100d2", "Failed", Stage::Error),
        ];
        let expected = |id: &str, unit_type, state: &str, record_type, linked| Unit {
            expected: Some(carbide_site_health_probe_api::ExpectedFields {
                record_type,
                linked,
                device_id: None,
                bmc_mac: None,
                explored_address: None,
            }),
            ..unit(unit_type, id, state, Stage::Expected)
        };
        publish_units(&sink, Family::Host, 1, 100, hosts);
        publish_units(&sink, Family::Dpu, 1, 110, dpus);
        publish_units(
            &sink,
            Family::Expected,
            1,
            120,
            vec![
                expected(
                    "e1",
                    UnitType::ExpectedRecord,
                    "linked",
                    RecordType::Host,
                    true,
                ),
                expected(
                    "e2",
                    UnitType::ExpectedRecord,
                    "unlinked",
                    RecordType::Rack,
                    false,
                ),
                expected(
                    "m1",
                    UnitType::IdentityMismatch,
                    "ambiguous_link",
                    RecordType::Switch,
                    true,
                ),
            ],
        );
        publish_units(
            &sink,
            Family::HostFirmwareRequests,
            1,
            130,
            vec![firmware, unknown_firmware],
        );
        publish_units(
            &sink,
            Family::PowerShelf,
            1,
            160,
            vec![unit(UnitType::PowerShelf, "ps-1", "ready", Stage::Ready)],
        );
        publish_units(
            &sink,
            Family::Instance,
            1,
            170,
            vec![unit(
                UnitType::Instance,
                "i-1",
                "provisioning",
                Stage::Registered,
            )],
        );
        publish_units(
            &sink,
            Family::Rack,
            1,
            140,
            vec![unit(UnitType::Rack, "rack-1", "ready", Stage::Ready)],
        );
        publish_totals(&sink, 1, None);
        sink.publish(
            Family::RestSiteMatch,
            Arc::new(FamilySnapshot {
                meta: meta(1, Reader::RestInventory, 100),
                data: SiteMatchData {
                    result: SiteMatch::Mismatch,
                },
            }),
        );
        sink.publish(
            Family::Switch,
            Arc::new(FamilySnapshot {
                meta: meta(1, Reader::Devices, 150),
                data: UnitsData::new(
                    vec![unit(UnitType::Switch, "sw-1", "other", Stage::Registered)],
                    1,
                ),
            }),
        );
        sink.failed(Reader::Devices, Family::Dpu, FailureKind::Pushback, "slow");
        let collector = StateCollector::new(Arc::clone(&store), UNIX_EPOCH);
        let descs: Vec<String> = collector
            .desc()
            .iter()
            .map(|d| format!("{} [{}] {}", d.fq_name, d.variable_labels.join(","), d.help))
            .collect();
        assert_eq!(descs, DESCRIPTORS.trim().lines().collect::<Vec<_>>());
        let out = exposition(&setup.registry);
        let got: Vec<&str> = out
            .lines()
            .filter(|line| {
                line.starts_with(PREFIX)
                    || line.starts_with(&format!("# HELP {PREFIX}"))
                    || line.starts_with(&format!("# TYPE {PREFIX}"))
            })
            .collect();
        assert_eq!(got, EXPOSITION.trim().lines().collect::<Vec<_>>());
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
