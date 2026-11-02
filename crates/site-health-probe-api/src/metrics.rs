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

//! The probe's metric contract: the names, labels and filter rules of the
//! series the probe publishes. The probe builds its gauges and answers
//! `/v1/count` from these declarations, and a client that reads the series
//! back from the metrics store uses the same ones, so a count always equals
//! the metric sum on both sides.
//!
//! Label rules: no unit IDs, URLs or regexes in labels. Host versions are no
//! labels: exact-version counts wait for the firmware tests (#8154).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

use crate::{
    ErrorCode, Family, Filter, HostFields, RecordType, RestResource, Source, Stage, UnitType,
};

/// The version of this contract, served in `/v1/config`. It changes when a
/// name, label or filter rule changes meaning.
pub const CONTRACT_VERSION: u32 = 1;

/// The prefix of every probe metric name.
pub const PREFIX: &str = "carbide_site_health_probe_";

/// The value of an absent label.
pub const UNKNOWN: &str = "unknown";
/// The value of an absent model-like label that has no `unknown` meaning.
pub const NONE: &str = "none";

/// A state gauge: its name without [`PREFIX`], help, and label names in the
/// order the probe declares them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gauge {
    pub name: &'static str,
    pub help: &'static str,
    pub labels: &'static [&'static str],
}

impl Gauge {
    /// The full metric name.
    pub fn metric_name(&self) -> String {
        format!("{PREFIX}{}", self.name)
    }
}

pub const UNITS: Gauge = Gauge {
    name: "units",
    help: "Units per type, stage (registered, ready, error) and state, from the newest complete sweep.",
    labels: &["type", "stage", "state"],
};
pub const HOSTS: Gauge = Gauge {
    name: "hosts",
    help: "Hosts per model, instance type, state, allocatability and update completion.",
    labels: &[
        "model",
        "instance_type",
        "state",
        "allocatable",
        "update_complete",
    ],
};
pub const HOSTS_UPDATE_ELIGIBLE: Gauge = Gauge {
    name: "hosts_update_eligible",
    help: "Hosts per model and firmware update eligibility (true, false, unknown) with the blocking reason.",
    labels: &["model", "eligible", "reason"],
};
pub const HOST_FIRMWARE_REQUESTS: Gauge = Gauge {
    name: "host_firmware_requests",
    help: "Open host firmware requests per model and initiator (automatic, manual, other).",
    labels: &["model", "initiator"],
};
pub const UNRESOLVED_UNITS: Gauge = Gauge {
    name: "unresolved_units",
    help: "Units the probe counts but cannot place, per type (DPUs no host lists).",
    labels: &["type"],
};
pub const EXPECTED_RECORDS: Gauge = Gauge {
    name: "expected_records",
    help: "Expected records per type and whether exactly one device in the census links to them.",
    labels: &["type", "linked"],
};
pub const IDENTITY_MISMATCHES: Gauge = Gauge {
    name: "identity_mismatches",
    help: "Expected records linked to several devices and devices without a record, per type and kind.",
    labels: &["type", "kind"],
};
pub const REST_TOTALS: Gauge = Gauge {
    name: "rest_totals",
    help: "REST site totals per resource. Absent while the total is unavailable.",
    labels: &["resource"],
};
pub const REST_TOTALS_AVAILABLE: Gauge = Gauge {
    name: "rest_totals_available",
    help: "1 while the REST total of a resource exists, 0 after the site answered 412.",
    labels: &["resource"],
};
pub const REST_SITE_MATCH: Gauge = Gauge {
    name: "rest_site_match",
    help: "Whether the REST site's machines are this NICo site's (match, mismatch, unknown), one-hot.",
    labels: &["result"],
};
pub const SWEEP_START: Gauge = Gauge {
    name: "sweep_start_timestamp_seconds",
    help: "Unix time the sweep behind a family's current snapshot started.",
    labels: &["reader", "family"],
};
pub const SWEEP_COMPLETE: Gauge = Gauge {
    name: "sweep_complete_timestamp_seconds",
    help: "Unix time the sweep behind a family's current snapshot completed.",
    labels: &["reader", "family"],
};
pub const SWEEP_DURATION: Gauge = Gauge {
    name: "sweep_duration_seconds",
    help: "Duration of the sweep behind a family's current snapshot.",
    labels: &["reader", "family"],
};
pub const LAST_ATTEMPT_FAILED: Gauge = Gauge {
    name: "last_attempt_failed",
    help: "Why the newest attempt to read a family failed (none, timeout, pushback, rpc, unresolved, decode), one-hot.",
    labels: &["reader", "family", "kind"],
};
pub const PROCESS_START: Gauge = Gauge {
    name: "process_start_timestamp_seconds",
    help: "Unix time this probe process started.",
    labels: &[],
};
pub const HOST_READ_MODE: Gauge = Gauge {
    name: "host_read_mode",
    help: "How host details are read (full, compact), one-hot.",
    labels: &["mode"],
};
/// Published with the compact host read. Until then it is absent, and an
/// absent capability is unavailable.
pub const HOST_PREDICATES_KNOWN: Gauge = Gauge {
    name: "host_predicates_known",
    help: "1 when the current host snapshot has the predicate for every host (allocatable, updateEligible), else 0.",
    labels: &["predicate"],
};

/// Every gauge the probe publishes today, in declaration order.
pub const GAUGES: &[Gauge] = &[
    UNITS,
    HOSTS,
    HOSTS_UPDATE_ELIGIBLE,
    HOST_FIRMWARE_REQUESTS,
    UNRESOLVED_UNITS,
    EXPECTED_RECORDS,
    IDENTITY_MISMATCHES,
    REST_TOTALS,
    REST_TOTALS_AVAILABLE,
    REST_SITE_MATCH,
    SWEEP_START,
    SWEEP_COMPLETE,
    SWEEP_DURATION,
    LAST_ATTEMPT_FAILED,
    PROCESS_START,
    HOST_READ_MODE,
];

/// The label names of the gauge `name`, empty for an unknown gauge.
pub fn gauge_labels(name: &str) -> &'static [&'static str] {
    GAUGES
        .iter()
        .chain([&HOST_PREDICATES_KNOWN])
        .find(|gauge| gauge.name == name)
        .map_or(&[], |gauge| gauge.labels)
}

/// A counter. The probe counts it on the OTel meter as `name`; the exporter
/// publishes it as `name` with a `_total` suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counter {
    /// The OTel instrument name, prefix included.
    pub name: &'static str,
    pub help: &'static str,
    pub labels: &'static [&'static str],
}

impl Counter {
    /// The name the metrics store holds.
    pub fn exported_name(&self) -> String {
        format!("{}_total", self.name)
    }
}

pub const SWEEPS: Counter = Counter {
    name: "carbide_site_health_probe_sweeps",
    help: "Reader sweeps per family by result: complete (published) or incomplete (the family \
           kept its last snapshot).",
    labels: &["reader", "family", "result"],
};
pub const UNKNOWN_STATES: Counter = Counter {
    name: "carbide_site_health_probe_unknown_states",
    help: "Units published with state other because their state is not in the pinned table, per \
           metric and type.",
    labels: &["metric", "type"],
};

/// The `result` of a sweep the family published.
pub const SWEEP_RESULT_COMPLETE: &str = "complete";
/// The `result` of a sweep after which the family kept its last snapshot.
pub const SWEEP_RESULT_INCOMPLETE: &str = "incomplete";

/// The labels of the sweep times and counters that name the sweep. Every
/// other label on those series names the scrape source.
pub const TIME_DIMS: [&str; 3] = ["reader", "family", "result"];

/// Every label a probe gauge or counter carries. A selector must not set
/// one: a client's own matcher on it would conflict with the selector's.
pub fn series_labels() -> BTreeSet<&'static str> {
    GAUGES
        .iter()
        .chain([&HOST_PREDICATES_KNOWN])
        .flat_map(|gauge| gauge.labels.iter().copied())
        .chain(
            [SWEEPS, UNKNOWN_STATES]
                .iter()
                .flat_map(|c| c.labels.iter().copied()),
        )
        .collect()
}

/// The longest selector value: Kubernetes names are at most 63 bytes.
pub const MAX_SELECTOR_VALUE: usize = 256;

/// Why `selector` cannot select the probe's series, one reason per problem.
/// Empty when it can.
pub fn selector_problems(selector: &BTreeMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    if selector.is_empty() {
        problems.push("must name at least one label".to_string());
    }
    let intrinsic = series_labels();
    for (name, value) in selector {
        let mut chars = name.chars();
        let valid = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            problems.push(format!("label {name:?} is not a label name"));
        } else if name.starts_with("__") {
            problems.push(format!(
                "must not set {name}: labels starting with __ are reserved"
            ));
        } else if intrinsic.contains(name.as_str()) {
            problems.push(format!("must not set {name}: the probe's series carry it"));
        }
        if value.trim().is_empty() {
            problems.push(format!("value of {name} must not be blank"));
        } else if value.chars().any(char::is_control) {
            problems.push(format!("value of {name} must not hold control characters"));
        } else if value.len() > MAX_SELECTOR_VALUE {
            problems.push(format!(
                "value of {name} is longer than {MAX_SELECTOR_VALUE} bytes"
            ));
        }
    }
    problems
}

/// `value` as a PromQL string literal. A JSON string is one: PromQL takes Go
/// escapes, and JSON escapes quotes, backslashes and control characters with
/// a subset of them.
pub fn promql_quoted(value: &str) -> String {
    serde_json::Value::from(value).to_string()
}

/// The selector as PromQL equality matchers, `job="a",namespace="b"`, in
/// label order.
pub fn selector_matchers(selector: &BTreeMap<String, String>) -> String {
    selector
        .iter()
        .map(|(name, value)| format!("{name}={}", promql_quoted(value)))
        .collect::<Vec<_>>()
        .join(",")
}

/// The wire name of an API enum, which is also its label value.
pub fn wire<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The `model` label of a host. `/v1/units` filters match this value too, so
/// a count and a listing of the same filter cannot disagree.
pub fn model_label(host: &HostFields) -> String {
    host.model.clone().unwrap_or_else(|| UNKNOWN.to_string())
}

/// The `instance_type` label of a host, shared with `/v1/units` like
/// [`model_label`].
pub fn instance_type_label(host: &HostFields) -> String {
    host.instance_type
        .clone()
        .unwrap_or_else(|| NONE.to_string())
}

/// The label of a flag that may be unknown.
pub fn flag_label(value: Option<bool>) -> String {
    value.map_or_else(|| UNKNOWN.to_string(), |v| v.to_string())
}

/// What the answering side can know about hosts before any sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Hosts are read with the full machine record, which never has
    /// allocatability or update eligibility: those filters fail at once.
    /// Otherwise they compile to [`Need::CompactHostRead`], checked per
    /// sweep.
    pub full_host_read: bool,
}

/// A capability a filter needs from the sweep it is answered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// A REST total exists only on a site with NICo Flow for it.
    RestTotal(RestResource),
    /// The named host filter (`allocatable` or `updateEligible`) is known
    /// only from the compact host read, and only when every host has it.
    CompactHostRead(&'static str),
}

/// The series that says whether a sweep meets a [`Need`]: it is met only
/// where `gauge{label=value}` is exactly 1. A missing series is unmet,
/// never 0 or permission to count unknown units as done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub gauge: Gauge,
    pub label: &'static str,
    pub value: String,
}

impl Need {
    pub fn capability(&self) -> Capability {
        match self {
            Need::RestTotal(resource) => Capability {
                gauge: REST_TOTALS_AVAILABLE,
                label: "resource",
                value: wire(resource),
            },
            Need::CompactHostRead(field) => Capability {
                gauge: HOST_PREDICATES_KNOWN,
                label: "predicate",
                value: (*field).to_string(),
            },
        }
    }
}

/// The gauge and label values a count reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesSpec {
    /// The gauge name without [`PREFIX`].
    pub metric: &'static str,
    pub family: Family,
    /// Label equality matchers. A label may repeat: conflicting values match
    /// nothing.
    pub matchers: Vec<(&'static str, String)>,
    pub needs: Vec<Need>,
}

/// Why a filter cannot be answered: `unsupported_filter` for a combination
/// no rule names, `field_unavailable` for a field the probe cannot read yet.
/// The probe replies 422 with these three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterError {
    pub code: ErrorCode,
    pub field: String,
    pub reason: String,
}

impl FilterError {
    fn unsupported(field: &str, reason: String) -> Self {
        Self {
            code: ErrorCode::UnsupportedFilter,
            field: field.to_string(),
            reason,
        }
    }

    fn unavailable(field: &str, reason: &str) -> Self {
        Self {
            code: ErrorCode::FieldUnavailable,
            field: field.to_string(),
            reason: reason.to_string(),
        }
    }

    /// A host `field` the full host read cannot answer.
    pub fn compact_host_read(field: &str) -> Self {
        let what = match field {
            "allocatable" => "allocatability",
            _ => "update eligibility",
        };
        Self::unavailable(
            field,
            &format!("{what} is unknown until nico-api serves the compact host read"),
        )
    }
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.reason)
    }
}

impl std::error::Error for FilterError {}

/// The camelCase names of the filter fields that are set, `type` aside.
fn set_fields(filter: &Filter) -> Vec<String> {
    serde_json::to_value(filter)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()))
        .unwrap_or_default()
        .into_iter()
        .filter(|k| k != "type")
        .collect()
}

/// Rejects every set field outside `allowed` (`source` is always allowed).
fn only(filter: &Filter, allowed: &[&str]) -> Result<(), FilterError> {
    for field in set_fields(filter) {
        if field != "source" && !allowed.contains(&field.as_str()) {
            return Err(FilterError::unsupported(
                &field,
                format!("{field} is not a filter of type={}", wire(filter.unit_type)),
            ));
        }
    }
    Ok(())
}

fn record_type_of(unit_type: UnitType) -> Option<RecordType> {
    match unit_type {
        UnitType::Host => Some(RecordType::Host),
        UnitType::Switch => Some(RecordType::Switch),
        UnitType::PowerShelf => Some(RecordType::PowerShelf),
        UnitType::Rack => Some(RecordType::Rack),
        _ => None,
    }
}

/// The snapshot family that holds units of `unit_type`.
pub fn family_of(unit_type: UnitType) -> Family {
    match unit_type {
        UnitType::Host => Family::Host,
        UnitType::Dpu => Family::Dpu,
        UnitType::Switch => Family::Switch,
        UnitType::PowerShelf => Family::PowerShelf,
        UnitType::Rack => Family::Rack,
        UnitType::Instance => Family::Instance,
        UnitType::ExpectedRecord | UnitType::IdentityMismatch => Family::Expected,
        UnitType::HostFirmwareRequest => Family::HostFirmwareRequests,
        UnitType::RestTotal => Family::RestTotals,
    }
}

/// Host filters that only the hosts gauge carries.
const HOST_FIELDS: [&str; 4] = ["model", "instanceType", "allocatable", "updateComplete"];

/// Rejects host filters no read answers. Allocatability and update
/// eligibility fail under the full host read, and are needs otherwise.
fn host_read_needs(filter: &Filter, caps: &Capabilities) -> Result<Vec<Need>, FilterError> {
    for (set, field) in [
        (filter.bmc_version.is_some(), "bmcVersion"),
        (filter.uefi_version.is_some(), "uefiVersion"),
    ] {
        if set {
            return Err(FilterError::unavailable(
                field,
                "exact host versions are not counted until the firmware tests (#8154)",
            ));
        }
    }
    let mut needs = Vec::new();
    for (set, field) in [
        (filter.allocatable.is_some(), "allocatable"),
        (filter.update_eligible.is_some(), "updateEligible"),
    ] {
        if !set {
            continue;
        }
        if caps.full_host_read {
            return Err(FilterError::compact_host_read(field));
        }
        needs.push(Need::CompactHostRead(field));
    }
    Ok(needs)
}

/// `stage=expected` on a device type counts that type's expected records.
fn expected_stage_spec(filter: &Filter) -> Result<SeriesSpec, FilterError> {
    only(filter, &["stage"])?;
    let record_type = record_type_of(filter.unit_type).ok_or_else(|| {
        FilterError::unsupported(
            "stage",
            format!("type={} has no expected records", wire(filter.unit_type)),
        )
    })?;
    Ok(SeriesSpec {
        metric: EXPECTED_RECORDS.name,
        family: Family::Expected,
        matchers: vec![("type", wire(record_type))],
        needs: vec![],
    })
}

fn units_spec(filter: &Filter) -> SeriesSpec {
    let mut matchers = vec![
        ("type", wire(filter.unit_type)),
        ("stage", wire(filter.stage.unwrap_or(Stage::Registered))),
    ];
    if let Some(state) = &filter.state {
        matchers.push(("state", state.clone()));
    }
    SeriesSpec {
        metric: UNITS.name,
        family: family_of(filter.unit_type),
        matchers,
        needs: vec![],
    }
}

fn host_spec(filter: &Filter, caps: &Capabilities) -> Result<SeriesSpec, FilterError> {
    let needs = host_read_needs(filter, caps)?;
    if filter.stage == Some(Stage::Expected) {
        return expected_stage_spec(filter);
    }
    if filter.update_eligible.is_some() {
        only(filter, &["updateEligible", "model"])?;
        let mut matchers = vec![(
            "eligible",
            filter
                .update_eligible
                .map(|e| e.to_string())
                .unwrap_or_default(),
        )];
        if let Some(model) = &filter.model {
            matchers.push(("model", model.clone()));
        }
        return Ok(SeriesSpec {
            metric: HOSTS_UPDATE_ELIGIBLE.name,
            family: Family::Host,
            matchers,
            needs,
        });
    }
    let mut allowed = vec!["stage", "state"];
    allowed.extend(HOST_FIELDS);
    only(filter, &allowed)?;
    if set_fields(filter)
        .iter()
        .all(|f| ["stage", "state", "source"].contains(&f.as_str()))
    {
        return Ok(units_spec(filter));
    }
    let mut matchers = Vec::new();
    match filter.stage {
        Some(Stage::Ready) => matchers.push(("state", "Ready".to_string())),
        Some(Stage::Error) => matchers.push(("state", "Failed".to_string())),
        _ => {}
    }
    for (label, value) in [
        ("state", filter.state.clone()),
        ("model", filter.model.clone()),
        ("instance_type", filter.instance_type.clone()),
        ("allocatable", filter.allocatable.map(|v| v.to_string())),
        (
            "update_complete",
            filter.update_complete.map(|v| v.to_string()),
        ),
    ] {
        if let Some(value) = value {
            matchers.push((label, value));
        }
    }
    Ok(SeriesSpec {
        metric: HOSTS.name,
        family: Family::Host,
        matchers,
        needs,
    })
}

/// The gauge a count of `filter` reads. Anything the table does not name is
/// an error.
pub fn series_spec(filter: &Filter, caps: &Capabilities) -> Result<SeriesSpec, FilterError> {
    if filter.done.is_some() {
        return Err(FilterError::unsupported(
            "done",
            "done is a /v1/units filter".to_string(),
        ));
    }
    rest_source(filter)?;
    match filter.unit_type {
        UnitType::Host => host_spec(filter, caps),
        UnitType::Dpu
        | UnitType::Switch
        | UnitType::PowerShelf
        | UnitType::Rack
        | UnitType::Instance => {
            if filter.stage == Some(Stage::Expected) {
                return expected_stage_spec(filter);
            }
            only(filter, &["stage", "state"])?;
            Ok(units_spec(filter))
        }
        UnitType::ExpectedRecord => {
            only(filter, &["recordType", "linked"])?;
            let record_type = required_record_type(filter)?;
            let mut matchers = vec![("type", wire(record_type))];
            if let Some(linked) = filter.linked {
                matchers.push(("linked", linked.to_string()));
            }
            Ok(SeriesSpec {
                metric: EXPECTED_RECORDS.name,
                family: Family::Expected,
                matchers,
                needs: vec![],
            })
        }
        UnitType::IdentityMismatch => {
            only(filter, &["recordType", "kind"])?;
            let mut matchers = Vec::new();
            if let Some(record_type) = mismatch_record_type(filter)? {
                matchers.push(("type", wire(record_type)));
            }
            if let Some(kind) = filter.kind {
                matchers.push(("kind", wire(kind)));
            }
            Ok(SeriesSpec {
                metric: IDENTITY_MISMATCHES.name,
                family: Family::Expected,
                matchers,
                needs: vec![],
            })
        }
        UnitType::HostFirmwareRequest => {
            only(filter, &["model", "initiator"])?;
            let mut matchers = Vec::new();
            if let Some(model) = &filter.model {
                matchers.push(("model", model.clone()));
            }
            if let Some(initiator) = filter.initiator {
                matchers.push(("initiator", wire(initiator)));
            }
            Ok(SeriesSpec {
                metric: HOST_FIRMWARE_REQUESTS.name,
                family: Family::HostFirmwareRequests,
                matchers,
                needs: vec![],
            })
        }
        UnitType::RestTotal => {
            only(filter, &["resource"])?;
            let resource = filter.resource.ok_or_else(|| {
                FilterError::unsupported("resource", "type=rest_total needs resource".to_string())
            })?;
            Ok(SeriesSpec {
                metric: REST_TOTALS.name,
                family: Family::RestTotals,
                matchers: vec![("resource", wire(resource))],
                needs: vec![Need::RestTotal(resource)],
            })
        }
    }
}

/// The tenant's REST instances, per status and one by one, wait for the
/// instance tests (#8152). Site totals are `type=rest_total`.
fn rest_source(filter: &Filter) -> Result<(), FilterError> {
    match filter.source {
        Some(Source::Rest) if filter.unit_type == UnitType::Instance => {
            Err(FilterError::unavailable(
                "source",
                "the tenant's REST instances are not read until the instance tests (#8152)",
            ))
        }
        Some(Source::Rest) => Err(FilterError::unsupported(
            "source",
            format!("type={} has no REST source", wire(filter.unit_type)),
        )),
        _ => Ok(()),
    }
}

fn required_record_type(filter: &Filter) -> Result<RecordType, FilterError> {
    filter.record_type.ok_or_else(|| {
        FilterError::unsupported(
            "recordType",
            "type=expected_record needs recordType".to_string(),
        )
    })
}

fn mismatch_record_type(filter: &Filter) -> Result<Option<RecordType>, FilterError> {
    match filter.record_type {
        Some(RecordType::Rack | RecordType::RackGroup) => Err(FilterError::unsupported(
            "recordType",
            "identity mismatches exist for host, switch and power_shelf records".to_string(),
        )),
        other => Ok(other),
    }
}

/// The family and needs of a valid units listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitsListing {
    pub family: Family,
    pub needs: Vec<Need>,
}

/// Rejects a `name` filter on a listing of `filter`: only instances have
/// names. The caller checks this before it compiles the name.
pub fn name_filter(filter: &Filter) -> Result<(), FilterError> {
    if filter.unit_type != UnitType::Instance {
        return Err(FilterError::unsupported(
            "name",
            "name is a filter of type=instance".to_string(),
        ));
    }
    Ok(())
}

/// Validates a units listing of `filter` and names its family. A `name`
/// filter is checked apart, by [`name_filter`].
pub fn units_listing(filter: &Filter, caps: &Capabilities) -> Result<UnitsListing, FilterError> {
    rest_source(filter)?;
    let mut needs = Vec::new();
    let family = match filter.unit_type {
        UnitType::RestTotal => {
            return Err(FilterError::unsupported(
                "type",
                "type=rest_total is a count, not units".to_string(),
            ));
        }
        unit_type => {
            if filter.stage == Some(Stage::Expected) {
                return Err(FilterError::unsupported(
                    "stage",
                    "list expected records with type=expected_record".to_string(),
                ));
            }
            let mut allowed = vec!["done"];
            match unit_type {
                UnitType::Host => {
                    needs = host_read_needs(filter, caps)?;
                    // As a count: update eligibility goes with the model only.
                    if filter.update_eligible.is_some() {
                        allowed.extend(["updateEligible", "model"]);
                    } else {
                        allowed.extend(["stage", "state"]);
                        allowed.extend(HOST_FIELDS);
                    }
                }
                UnitType::ExpectedRecord => {
                    allowed.extend(["recordType", "linked"]);
                    required_record_type(filter)?;
                }
                UnitType::IdentityMismatch => {
                    allowed.extend(["recordType", "kind"]);
                    mismatch_record_type(filter)?;
                }
                UnitType::HostFirmwareRequest => allowed.extend(["model", "initiator"]),
                _ => allowed.extend(["stage", "state"]),
            }
            only(filter, &allowed)?;
            family_of(unit_type)
        }
    };
    Ok(UnitsListing { family, needs })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: Capabilities = Capabilities {
        full_host_read: true,
    };
    const COMPACT: Capabilities = Capabilities {
        full_host_read: false,
    };

    fn filter(query: &str) -> Filter {
        serde_urlencoded::from_str(query).unwrap()
    }

    /// `ok` or `code field reason` of compiling a count of `query`.
    fn count(query: &str, caps: &Capabilities) -> String {
        match series_spec(&filter(query), caps) {
            Ok(spec) => {
                let labels = gauge_labels(spec.metric);
                for (label, _) in &spec.matchers {
                    assert!(labels.contains(label), "{query}: {label} not on the gauge");
                }
                assert!(
                    GAUGES.iter().any(|g| g.name == spec.metric),
                    "{query}: {} is not published",
                    spec.metric
                );
                "ok".to_string()
            }
            Err(e) => format!("{} {} {}", wire(e.code), e.field, e.reason),
        }
    }

    fn listing(query: &str, name: bool, caps: &Capabilities) -> String {
        let filter = filter(query);
        let result = if name {
            name_filter(&filter).and_then(|()| units_listing(&filter, caps))
        } else {
            units_listing(&filter, caps)
        };
        match result {
            Ok(listing) => format!("ok {}", wire(listing.family)),
            Err(e) => format!("{} {} {}", wire(e.code), e.field, e.reason),
        }
    }

    /// Each row: the count query, then `ok` or the error's code, field and
    /// reason.
    const COUNT_FILTERS: &[&str] = &[
        "type=host => ok",
        "type=host&stage=ready&model=r750 => ok",
        "type=host&instanceType=none&updateComplete=true => ok",
        "type=host&stage=expected => ok",
        "type=dpu&stage=error&state=Failed => ok",
        "type=rack&stage=expected => ok",
        "type=expected_record&recordType=switch&linked=true => ok",
        "type=identity_mismatch&kind=no_expected_record => ok",
        "type=host_firmware_request&model=m&initiator=manual => ok",
        "type=rest_total&resource=machine => ok",
        "type=host&source=nico-api => ok",
        "type=host&stage=expected&state=Ready => unsupported_filter state state is not a filter of type=host",
        "type=host&allocatable=true&stage=expected => field_unavailable allocatable allocatability is unknown until nico-api serves the compact host read",
        "type=host&updateEligible=true&allocatable=false => field_unavailable allocatable allocatability is unknown until nico-api serves the compact host read",
        "type=host&updateEligible=true => field_unavailable updateEligible update eligibility is unknown until nico-api serves the compact host read",
        "type=host&uefiVersion=u1&allocatable=true => field_unavailable uefiVersion exact host versions are not counted until the firmware tests (#8154)",
        "type=host&done=true => unsupported_filter done done is a /v1/units filter",
        "type=host&source=rest => unsupported_filter source type=host has no REST source",
        "type=instance&source=rest => field_unavailable source the tenant's REST instances are not read until the instance tests (#8152)",
        "type=dpu&stage=expected => unsupported_filter stage type=dpu has no expected records",
        "type=dpu&model=x => unsupported_filter model model is not a filter of type=dpu",
        "type=expected_record => unsupported_filter recordType type=expected_record needs recordType",
        "type=identity_mismatch&recordType=rack_group => unsupported_filter recordType identity mismatches exist for host, switch and power_shelf records",
        "type=host_firmware_request&stage=ready => unsupported_filter stage stage is not a filter of type=host_firmware_request",
        "type=rest_total => unsupported_filter resource type=rest_total needs resource",
        "type=rest_total&resource=rack&model=x => unsupported_filter model model is not a filter of type=rest_total",
    ];

    /// Each row: the units query, with `name` when the listing has a name
    /// filter, then `ok` and the family, or the error.
    const UNITS_FILTERS: &[&str] = &[
        "type=host&done=true => ok host",
        "type=host&stage=ready&model=gb200 => ok host",
        "name type=instance => ok instance",
        "type=expected_record&recordType=host&done=false => ok expected",
        "type=identity_mismatch&kind=ambiguous_link => ok expected",
        "type=host_firmware_request&initiator=manual => ok host_firmware_requests",
        "type=switch&stage=ready => ok switch",
        "type=host&allocatable=true => field_unavailable allocatable allocatability is unknown until nico-api serves the compact host read",
        "type=host&stage=expected => unsupported_filter stage list expected records with type=expected_record",
        "name type=host => unsupported_filter name name is a filter of type=instance",
        "name type=rest_total&resource=rack => unsupported_filter name name is a filter of type=instance",
        "type=rest_total&resource=rack => unsupported_filter type type=rest_total is a count, not units",
        "name type=instance&source=rest => field_unavailable source the tenant's REST instances are not read until the instance tests (#8152)",
        "type=expected_record => unsupported_filter recordType type=expected_record needs recordType",
        "type=identity_mismatch&recordType=rack => unsupported_filter recordType identity mismatches exist for host, switch and power_shelf records",
        "type=dpu&model=x => unsupported_filter model model is not a filter of type=dpu",
        "type=host&tenant=t => unsupported_filter tenant tenant is not a filter of type=host",
        "type=power_shelf&source=rest => unsupported_filter source type=power_shelf has no REST source",
    ];

    #[test]
    fn count_filters_accept_and_reject() {
        for row in COUNT_FILTERS {
            let (query, want) = row.split_once(" => ").unwrap();
            assert_eq!(count(query, &FULL), want, "{query}");
        }
    }

    #[test]
    fn units_filters_accept_and_reject() {
        for row in UNITS_FILTERS {
            let (query, want) = row.split_once(" => ").unwrap();
            let (query, name) = match query.strip_prefix("name ") {
                Some(query) => (query, true),
                None => (query, false),
            };
            assert_eq!(listing(query, name, &FULL), want, "{row}");
        }
    }

    /// A count and a listing of one filter agree on whether it is valid and
    /// on the family, wherever both are defined.
    #[test]
    fn count_and_listing_validate_alike() {
        for query in [
            "type=host",
            "type=host&stage=ready",
            "type=host&model=gb200&state=Ready",
            "type=host&allocatable=true",
            "type=host&updateEligible=false&model=m",
            "type=host&bmcVersion=b",
            "type=host&tenant=t",
            "type=dpu&stage=error",
            "type=dpu&model=x",
            "type=instance&source=rest",
            "type=switch&source=rest",
            "type=expected_record&recordType=rack&linked=true",
            "type=expected_record",
            "type=identity_mismatch&recordType=rack",
            "type=identity_mismatch&kind=ambiguous_link",
            "type=host_firmware_request&initiator=automatic",
            "type=host&updateEligible=true&allocatable=false",
            "type=host&updateEligible=true&state=Ready",
        ] {
            for caps in [&FULL, &COMPACT] {
                let filter = filter(query);
                match (series_spec(&filter, caps), units_listing(&filter, caps)) {
                    (Ok(spec), Ok(listing)) => {
                        assert_eq!(spec.family, listing.family, "{query}");
                        assert_eq!(spec.needs, listing.needs, "{query}");
                    }
                    (Err(count), Err(units)) => assert_eq!(count, units, "{query}"),
                    (count, units) => panic!("{query}: count {count:?}, units {units:?}"),
                }
            }
        }
    }

    #[test]
    fn compact_host_read_filters_are_needs() {
        let spec = series_spec(&filter("type=host&allocatable=true&model=m"), &COMPACT).unwrap();
        assert_eq!(spec.metric, "hosts");
        assert_eq!(spec.needs, vec![Need::CompactHostRead("allocatable")]);
        let spec = series_spec(&filter("type=host&updateEligible=false"), &COMPACT).unwrap();
        assert_eq!(spec.metric, "hosts_update_eligible");
        assert_eq!(spec.matchers, vec![("eligible", "false".to_string())]);
        assert_eq!(spec.needs, vec![Need::CompactHostRead("updateEligible")]);
        for (query, field) in [
            ("type=host&allocatable=true&done=false", "allocatable"),
            ("type=host&updateEligible=true&model=m", "updateEligible"),
        ] {
            let listing = units_listing(&filter(query), &COMPACT).unwrap();
            assert_eq!(listing.needs, vec![Need::CompactHostRead(field)], "{query}");
        }
        // Like a count, a listing takes update eligibility with the model
        // only.
        let err = units_listing(
            &filter("type=host&allocatable=true&updateEligible=true"),
            &COMPACT,
        )
        .unwrap_err();
        assert_eq!(
            (err.code, err.field.as_str()),
            (ErrorCode::UnsupportedFilter, "allocatable")
        );
        assert!(
            series_spec(&filter("type=host"), &COMPACT)
                .unwrap()
                .needs
                .is_empty()
        );
    }

    #[test]
    fn needs_name_their_capability_series() {
        let rest = Need::RestTotal(RestResource::Instance).capability();
        assert_eq!(
            (
                rest.gauge.metric_name().as_str(),
                rest.label,
                rest.value.as_str()
            ),
            (
                "carbide_site_health_probe_rest_totals_available",
                "resource",
                "instance"
            )
        );
        let host = Need::CompactHostRead("updateEligible").capability();
        assert_eq!(
            (
                host.gauge.metric_name().as_str(),
                host.label,
                host.value.as_str()
            ),
            (
                "carbide_site_health_probe_host_predicates_known",
                "predicate",
                "updateEligible"
            )
        );
        assert_eq!(gauge_labels("host_predicates_known"), ["predicate"]);
    }

    #[test]
    fn names_and_labels_are_valid_and_unique() {
        let valid = |s: &str| {
            s.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !s.starts_with("__")
        };
        let mut names = std::collections::BTreeSet::new();
        for gauge in GAUGES.iter().chain([&HOST_PREDICATES_KNOWN]) {
            assert!(valid(gauge.name), "{}", gauge.name);
            assert!(names.insert(gauge.metric_name()), "{} twice", gauge.name);
            let labels: std::collections::BTreeSet<_> = gauge.labels.iter().collect();
            assert_eq!(labels.len(), gauge.labels.len(), "{}", gauge.name);
            assert!(gauge.labels.iter().all(|l| valid(l)), "{}", gauge.name);
        }
        for counter in [SWEEPS, UNKNOWN_STATES] {
            assert!(counter.name.starts_with(PREFIX), "{}", counter.name);
            assert!(names.insert(counter.exported_name()), "{}", counter.name);
        }
        assert_eq!(
            SWEEPS.exported_name(),
            "carbide_site_health_probe_sweeps_total"
        );
        assert_eq!(SWEEPS.labels, TIME_DIMS);
        assert_eq!(gauge_labels("no_such_gauge"), [] as [&str; 0]);
    }

    /// The contract a client reads. Bump [`CONTRACT_VERSION`] when a name,
    /// label or label order here changes meaning.
    const CONTRACT: &str = r#"
version 1
units [type,stage,state]
hosts [model,instance_type,state,allocatable,update_complete]
hosts_update_eligible [model,eligible,reason]
host_firmware_requests [model,initiator]
unresolved_units [type]
expected_records [type,linked]
identity_mismatches [type,kind]
rest_totals [resource]
rest_totals_available [resource]
rest_site_match [result]
sweep_start_timestamp_seconds [reader,family]
sweep_complete_timestamp_seconds [reader,family]
sweep_duration_seconds [reader,family]
last_attempt_failed [reader,family,kind]
process_start_timestamp_seconds []
host_read_mode [mode]
host_predicates_known [predicate]
carbide_site_health_probe_sweeps_total [reader,family,result]
carbide_site_health_probe_unknown_states_total [metric,type]
"#;

    #[test]
    fn contract_golden() {
        let mut got = vec![format!("version {CONTRACT_VERSION}")];
        for gauge in GAUGES.iter().chain([&HOST_PREDICATES_KNOWN]) {
            got.push(format!("{} [{}]", gauge.name, gauge.labels.join(",")));
        }
        for counter in [SWEEPS, UNKNOWN_STATES] {
            got.push(format!(
                "{} [{}]",
                counter.exported_name(),
                counter.labels.join(",")
            ));
        }
        assert_eq!(got, CONTRACT.trim().lines().collect::<Vec<_>>());
    }

    fn selector(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn selector_problems_name_each_bad_label() {
        assert!(selector_problems(&selector(&[("job", "j"), ("namespace", "n")])).is_empty());
        assert!(selector_problems(&selector(&[("cluster", "c1"), ("_x", "y")])).is_empty());
        let long = "x".repeat(MAX_SELECTOR_VALUE + 1);
        for (pairs, want) in [
            (vec![], "must name at least one label"),
            (vec![("0job", "j")], "label \"0job\" is not a label name"),
            (
                vec![("app-name", "j")],
                "label \"app-name\" is not a label name",
            ),
            (
                vec![("__name__", "x")],
                "must not set __name__: labels starting with __ are reserved",
            ),
            (
                vec![("type", "host")],
                "must not set type: the probe's series carry it",
            ),
            (
                vec![("predicate", "p")],
                "must not set predicate: the probe's series carry it",
            ),
            (
                vec![("metric", "m")],
                "must not set metric: the probe's series carry it",
            ),
            (vec![("job", " ")], "value of job must not be blank"),
            (
                vec![("job", "a\nb")],
                "value of job must not hold control characters",
            ),
            (
                vec![("job", "a\u{85}b")],
                "value of job must not hold control characters",
            ),
            (
                vec![("job", long.as_str())],
                "value of job is longer than 256 bytes",
            ),
        ] {
            assert_eq!(
                selector_problems(&selector(&pairs)),
                vec![want],
                "{pairs:?}"
            );
        }
        for label in [
            "reader", "family", "state", "resource", "result", "mode", "kind",
        ] {
            assert!(series_labels().contains(label), "{label}");
        }
    }

    #[test]
    fn selector_matchers_escape_values() {
        assert_eq!(
            selector_matchers(&selector(&[("namespace", "n"), ("job", "a\"b\\c\nd")])),
            r#"job="a\"b\\c\nd",namespace="n""#
        );
    }

    #[test]
    fn label_normalization() {
        let mut host: HostFields = serde_json::from_value(serde_json::json!({
            "vendor": null, "model": null, "instanceType": null, "bmcVersion": null,
            "uefiVersion": null, "versionSource": "machine_record", "reportAt": null,
            "reportError": null, "allocatable": null, "blockingReason": null,
            "firmwareAutoupdate": null, "updateEligible": null,
            "updateBlockingReason": null, "updateComplete": false,
            "openFirmwareRequest": null, "nvlinkDomain": null, "rackId": null, "dpuIds": [],
        }))
        .unwrap();
        assert_eq!(model_label(&host), "unknown");
        assert_eq!(instance_type_label(&host), "none");
        host.model = Some("gb200".to_string());
        host.instance_type = Some("it-1".to_string());
        assert_eq!(model_label(&host), "gb200");
        assert_eq!(instance_type_label(&host), "it-1");
        assert_eq!(flag_label(None), "unknown");
        assert_eq!(flag_label(Some(true)), "true");
        assert_eq!(wire(Family::HostFirmwareRequests), "host_firmware_requests");
        assert_eq!(wire(Source::NicoApi), "nico-api");
    }
}
