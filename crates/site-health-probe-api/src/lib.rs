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

//! Wire types for the site health probe `/v1` HTTP API.
//!
//! This crate is the one source of the JSON contract between the probe and
//! its clients. It holds types only. The `schema` feature also derives
//! `schemars::JsonSchema` on every type.
//!
//! Conventions:
//! - Field names and query parameters are camelCase. Enum values are
//!   snake_case, except [`Source`].
//! - Request types ([`Filter`], [`UnitsQuery`], [`SummaryParams`]) omit absent
//!   fields and reject unknown ones.
//! - Response types serialize absent values as JSON `null`. A `null` count,
//!   time or flag means unknown, never 0 or false.
//! - Times are RFC 3339 UTC. `snapshotId` and `cursor` are opaque strings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Quantiles `/v1/summary` reports when the request has no `q`.
pub const DEFAULT_Q: &str = "0.5,0.9,0.95,0.99,0.995,1";

/// Kind of unit a [`Filter`] selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UnitType {
    Rack,
    Host,
    Dpu,
    Switch,
    PowerShelf,
    Instance,
    ExpectedRecord,
    IdentityMismatch,
    HostFirmwareRequest,
    RestTotal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Expected,
    Registered,
    Ready,
    Error,
}

/// API a unit was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Source {
    #[serde(rename = "nico-api")]
    NicoApi,
    #[serde(rename = "rest")]
    Rest,
}

/// Kind of expected record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RecordType {
    Host,
    Switch,
    PowerShelf,
    Rack,
    RackGroup,
}

/// REST collection whose total `type=rest_total` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RestResource {
    Machine,
    Instance,
    Rack,
    Tray,
}

/// Who started a host firmware request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Initiator {
    Automatic,
    Manual,
    Other,
}

/// Firmware component of a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Component {
    Bmc,
    Uefi,
    BmcUefi,
    Other,
}

/// Why a device and its expected record do not pair up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MismatchKind {
    /// One expected record links to two or more devices.
    AmbiguousLink,
    /// A device has no expected record.
    NoExpectedRecord,
}

/// Whether a summary waits for the count to rise or to fall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
}

/// Where host firmware versions were read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum VersionSource {
    MachineRecord,
    ExploredReport,
}

/// Snapshot family. Each family is read and replaced as a whole, and has
/// its own sweep times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Host,
    Dpu,
    Switch,
    PowerShelf,
    Rack,
    Instance,
    Expected,
    HostFirmwareRequests,
    RestInstances,
    RestTotals,
    RestSiteMatch,
    RestTenantInstances,
    ConfigNicoApi,
    ConfigRest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidParam,
    UnsupportedFilter,
    FieldUnavailable,
    NoCompleteSweep,
    SnapshotExpired,
    EndInFuture,
    Busy,
    StoreUnavailable,
    StoreTimeout,
    NotFound,
    Internal,
}

/// Selects units for `/v1/count`, `/v1/units` and `/v1/summary`. Sent as
/// query parameters, or flattened into the [`UnitsQuery`] body. Which
/// fields go with which `type` is the probe's call: an unsupported
/// combination is 422 `unsupported_filter`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Filter {
    #[serde(rename = "type")]
    pub unit_type: UnitType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bmc_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uefi_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocatable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_eligible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_type: Option<RecordType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<RestResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiator: Option<Initiator>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<MismatchKind>,
    /// `false` selects units that are not done. `/v1/units` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
}

impl Filter {
    /// A filter on `unit_type` alone.
    pub fn new(unit_type: UnitType) -> Self {
        Self {
            unit_type,
            source: None,
            stage: None,
            state: None,
            model: None,
            instance_type: None,
            bmc_version: None,
            uefi_version: None,
            allocatable: None,
            update_complete: None,
            update_eligible: None,
            tenant: None,
            status: None,
            record_type: None,
            linked: None,
            resource: None,
            initiator: None,
            kind: None,
            done: None,
        }
    }
}

/// The snapshot a count or units reply was read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SweepInfo {
    pub snapshot_id: String,
    pub family: Family,
    pub sweep_start: DateTime<Utc>,
    /// Sweep end. Every unit in the snapshot was read by then.
    pub read_at: DateTime<Utc>,
    pub sweep_age_seconds: f64,
    pub sweep_interval_seconds: f64,
}

/// `GET /v1/count` reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CountResponse {
    pub count: u64,
    #[serde(flatten)]
    pub sweep: SweepInfo,
}

/// `POST /v1/units/query` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitsQuery {
    #[serde(flatten)]
    pub filter: Filter,
    /// Regex on the instance name. `type=instance` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Unit IDs to read. Empty means all units.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// `GET /v1/units` and `POST /v1/units/query` reply. Units sort by
/// `(source, type, id)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UnitsResponse {
    #[serde(flatten)]
    pub sweep: SweepInfo,
    pub units: Vec<Unit>,
    /// Asked IDs that do not exist in the snapshot, repeated on every page.
    pub missing_ids: Vec<String>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Unit {
    pub id: String,
    #[serde(rename = "type")]
    pub unit_type: UnitType,
    pub source: Source,
    pub stage: Stage,
    pub done: bool,
    pub state: String,
    pub reason: Option<String>,
    pub bmc_ip: Option<String>,
    pub host: Option<HostFields>,
    pub dpu: Option<DpuFields>,
    pub instance: Option<InstanceFields>,
    pub expected: Option<ExpectedFields>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HostFields {
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub instance_type: Option<String>,
    pub bmc_version: Option<String>,
    pub uefi_version: Option<String>,
    pub version_source: VersionSource,
    pub report_at: Option<DateTime<Utc>>,
    pub report_error: Option<String>,
    /// `null` until the probe can read allocatability.
    pub allocatable: Option<bool>,
    pub blocking_reason: Option<String>,
    pub firmware_autoupdate: Option<bool>,
    /// `null` until the probe can read update eligibility.
    pub update_eligible: Option<bool>,
    pub update_blocking_reason: Option<String>,
    pub update_complete: bool,
    pub open_firmware_request: Option<FirmwareRequest>,
    pub nvlink_domain: Option<String>,
    pub rack_id: Option<String>,
    pub dpu_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FirmwareRequest {
    pub initiator: Initiator,
    pub requested_at: Option<DateTime<Utc>>,
    pub initiated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DpuFields {
    /// `null` while no host lists this DPU.
    pub host_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct InstanceFields {
    pub name: Option<String>,
    pub tenant: Option<String>,
    pub host_id: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ExpectedFields {
    pub record_type: RecordType,
    pub linked: bool,
    pub device_id: Option<String>,
    pub bmc_mac: Option<String>,
    pub explored_address: Option<String>,
}

/// Times of one config read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ReadInfo {
    pub read_start: DateTime<Utc>,
    pub read_at: DateTime<Utc>,
    pub read_age_seconds: f64,
    pub read_interval_seconds: f64,
    /// Error of the newest attempt when it failed. The values are then from
    /// the last good read, at `readAt`.
    pub last_attempt_error: Option<String>,
}

/// `GET /v1/config` reply. A source is `null` before its first good read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConfigResponse {
    pub nico_api: Option<ConfigNicoApi>,
    pub rest: Option<ConfigRest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConfigNicoApi {
    #[serde(flatten)]
    pub read: ReadInfo,
    pub firmware_targets: Vec<FirmwareTarget>,
    pub runtime: RuntimeSettings,
    pub site_explorer: Option<SiteExplorerRun>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FirmwareTarget {
    pub vendor: String,
    pub model: String,
    pub component: Component,
    pub version: String,
    pub explicit_start: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSettings {
    pub max_concurrent_machine_updates_absolute: Option<i64>,
    pub max_concurrent_machine_updates_percent: Option<i64>,
    pub auto_host_firmware_update: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteExplorerRun {
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub last_successful_finished_at: Option<DateTime<Utc>>,
    pub success: bool,
    pub explorations_attempted: i64,
    pub explorations_succeeded: i64,
    pub explorations_failed: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConfigRest {
    #[serde(flatten)]
    pub read: ReadInfo,
    pub tenant_allocation: Option<TenantAllocation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TenantAllocation {
    pub tenant: String,
    pub instance_type: String,
    pub assigned: i64,
    pub total: i64,
    pub used: i64,
    pub unused: i64,
    pub unused_usable: i64,
}

/// `GET /v1/summary` parameters, next to a [`Filter`] in the query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SummaryParams {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub base: u64,
    pub n: u64,
    pub direction: Direction,
    /// Comma list of quantiles. [`DEFAULT_Q`] when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    pub add_window_seconds: f64,
}

impl SummaryParams {
    /// The requested quantiles, or [`DEFAULT_Q`], parsed by [`parse_q`].
    pub fn quantiles(&self) -> Result<Vec<f64>, String> {
        parse_q(self.q.as_deref().unwrap_or(DEFAULT_Q))
    }
}

/// `GET /v1/summary` reply. Without a usable sweep, every crossing keeps its
/// entry with `null` times and counts, the summary values are `null` and the
/// curve is empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SummaryResponse {
    pub family: Family,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub base: u64,
    pub n: u64,
    pub direction: Direction,
    /// One entry per requested quantile, in ascending order.
    pub crossings: Vec<Crossing>,
    pub final_count: Option<u64>,
    pub final_sweep_at: Option<DateTime<Utc>>,
    pub units_per_minute: UnitsPerMinute,
    pub max_zero_progress_seconds: Option<f64>,
    pub max_sweep_gap_seconds: Option<f64>,
    pub max_sweep_duration_seconds: Option<f64>,
    pub incomplete_sweeps: u64,
    pub scrape_gaps: Vec<ScrapeGap>,
    pub scrape_gaps_total: u32,
    pub uncovered_scrape_gap_seconds: f64,
    pub add_window_seconds: f64,
    pub timing_error_seconds: Option<f64>,
    pub final_scrape_seen: bool,
    pub probe_restarts: u32,
    pub sweeps_seen: u32,
    pub curve: Vec<CurvePoint>,
    /// `worstCountAfter` of the smallest quantile reached.
    pub worst_count_after_first_crossing: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Crossing {
    pub q: f64,
    pub target: u64,
    pub reached_at: Option<DateTime<Utc>>,
    pub seconds: Option<f64>,
    pub worst_count_after: Option<u64>,
    pub scrape_gap_before: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UnitsPerMinute {
    pub sustained: Option<f64>,
    #[serde(rename = "peak10m")]
    pub peak_10m: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScrapeGap {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub seconds: f64,
    pub new_sweep_across: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CurvePoint {
    pub at: DateTime<Utc>,
    pub count: u64,
}

/// `GET /v1/store-status` reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StoreStatus {
    pub last_sample_at: Option<DateTime<Utc>>,
    pub sample_age_seconds: Option<f64>,
    pub sweep_complete_at: Option<DateTime<Utc>>,
}

/// Body of every non-2xx reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    pub code: ErrorCode,
    pub message: String,
    /// The request parameter at fault, when there is one.
    pub field: Option<String>,
}

/// Parses a `q` comma list. Every entry must be in (0, 1]. The result is
/// sorted and deduplicated.
pub fn parse_q(raw: &str) -> Result<Vec<f64>, String> {
    let mut qs = Vec::new();
    for entry in raw.split(',').map(str::trim) {
        if entry.is_empty() {
            return Err("q has an empty entry".to_string());
        }
        let q: f64 = entry
            .parse()
            .map_err(|_| format!("q entry {entry:?} is not a number"))?;
        if q.is_nan() || q <= 0.0 || q > 1.0 {
            return Err(format!("q entry {entry:?} is outside (0, 1]"));
        }
        qs.push(q);
    }
    qs.sort_by(f64::total_cmp);
    qs.dedup();
    Ok(qs)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Debug;

    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn assert_wire_names<T>(cases: &[(T, &str)])
    where
        T: Serialize + DeserializeOwned + PartialEq + Debug,
    {
        let names: BTreeSet<&str> = cases.iter().map(|(_, name)| *name).collect();
        assert_eq!(names.len(), cases.len(), "duplicate wire name in {cases:?}");
        for (value, name) in cases {
            assert_eq!(serde_json::to_value(value).unwrap(), json!(name));
            let back: T = serde_json::from_value(json!(name)).unwrap();
            assert_eq!(&back, value);
        }
    }

    fn assert_json_golden<T>(value: &T, golden: serde_json::Value)
    where
        T: Serialize + DeserializeOwned + PartialEq + Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), golden);
        let back: T = serde_json::from_value(golden).unwrap();
        assert_eq!(&back, value);
    }

    fn assert_query_golden<T>(value: &T, golden: &str)
    where
        T: Serialize + DeserializeOwned + PartialEq + Debug,
    {
        assert_eq!(serde_urlencoded::to_string(value).unwrap(), golden);
        let back: T = serde_urlencoded::from_str(golden).unwrap();
        assert_eq!(&back, value);
    }

    fn full_filter() -> Filter {
        Filter {
            unit_type: UnitType::Host,
            source: Some(Source::NicoApi),
            stage: Some(Stage::Ready),
            state: Some("Ready".to_string()),
            model: Some("GB200 NVL".to_string()),
            instance_type: Some("it-1".to_string()),
            bmc_version: Some("1.2.3".to_string()),
            uefi_version: Some("4.5".to_string()),
            allocatable: Some(true),
            update_complete: Some(false),
            update_eligible: Some(true),
            tenant: Some("t1".to_string()),
            status: Some("Ready".to_string()),
            record_type: Some(RecordType::PowerShelf),
            linked: Some(false),
            resource: Some(RestResource::Tray),
            initiator: Some(Initiator::Manual),
            kind: Some(MismatchKind::AmbiguousLink),
            done: Some(false),
        }
    }

    fn sweep_info() -> SweepInfo {
        SweepInfo {
            snapshot_id: "s-7".to_string(),
            family: Family::Host,
            sweep_start: t("2026-10-23T10:00:00Z"),
            read_at: t("2026-10-23T10:00:40.500Z"),
            sweep_age_seconds: 3.5,
            sweep_interval_seconds: 120.0,
        }
    }

    fn sweep_info_json() -> serde_json::Value {
        json!({
            "snapshotId": "s-7",
            "family": "host",
            "sweepStart": "2026-10-23T10:00:00Z",
            "readAt": "2026-10-23T10:00:40.500Z",
            "sweepAgeSeconds": 3.5,
            "sweepIntervalSeconds": 120.0,
        })
    }

    #[test]
    fn every_enum_wire_name_golden() {
        assert_wire_names(&[
            (UnitType::Rack, "rack"),
            (UnitType::Host, "host"),
            (UnitType::Dpu, "dpu"),
            (UnitType::Switch, "switch"),
            (UnitType::PowerShelf, "power_shelf"),
            (UnitType::Instance, "instance"),
            (UnitType::ExpectedRecord, "expected_record"),
            (UnitType::IdentityMismatch, "identity_mismatch"),
            (UnitType::HostFirmwareRequest, "host_firmware_request"),
            (UnitType::RestTotal, "rest_total"),
        ]);
        assert_wire_names(&[
            (Stage::Expected, "expected"),
            (Stage::Registered, "registered"),
            (Stage::Ready, "ready"),
            (Stage::Error, "error"),
        ]);
        assert_wire_names(&[(Source::NicoApi, "nico-api"), (Source::Rest, "rest")]);
        assert_wire_names(&[
            (RecordType::Host, "host"),
            (RecordType::Switch, "switch"),
            (RecordType::PowerShelf, "power_shelf"),
            (RecordType::Rack, "rack"),
            (RecordType::RackGroup, "rack_group"),
        ]);
        assert_wire_names(&[
            (RestResource::Machine, "machine"),
            (RestResource::Instance, "instance"),
            (RestResource::Rack, "rack"),
            (RestResource::Tray, "tray"),
        ]);
        assert_wire_names(&[
            (Initiator::Automatic, "automatic"),
            (Initiator::Manual, "manual"),
            (Initiator::Other, "other"),
        ]);
        assert_wire_names(&[
            (Component::Bmc, "bmc"),
            (Component::Uefi, "uefi"),
            (Component::BmcUefi, "bmc_uefi"),
            (Component::Other, "other"),
        ]);
        assert_wire_names(&[
            (MismatchKind::AmbiguousLink, "ambiguous_link"),
            (MismatchKind::NoExpectedRecord, "no_expected_record"),
        ]);
        assert_wire_names(&[(Direction::Up, "up"), (Direction::Down, "down")]);
        assert_wire_names(&[
            (VersionSource::MachineRecord, "machine_record"),
            (VersionSource::ExploredReport, "explored_report"),
        ]);
        assert_wire_names(&[
            (Family::Host, "host"),
            (Family::Dpu, "dpu"),
            (Family::Switch, "switch"),
            (Family::PowerShelf, "power_shelf"),
            (Family::Rack, "rack"),
            (Family::Instance, "instance"),
            (Family::Expected, "expected"),
            (Family::HostFirmwareRequests, "host_firmware_requests"),
            (Family::RestInstances, "rest_instances"),
            (Family::RestTotals, "rest_totals"),
            (Family::RestSiteMatch, "rest_site_match"),
            (Family::RestTenantInstances, "rest_tenant_instances"),
            (Family::ConfigNicoApi, "config_nico_api"),
            (Family::ConfigRest, "config_rest"),
        ]);
        assert_wire_names(&[
            (ErrorCode::InvalidParam, "invalid_param"),
            (ErrorCode::UnsupportedFilter, "unsupported_filter"),
            (ErrorCode::FieldUnavailable, "field_unavailable"),
            (ErrorCode::NoCompleteSweep, "no_complete_sweep"),
            (ErrorCode::SnapshotExpired, "snapshot_expired"),
            (ErrorCode::EndInFuture, "end_in_future"),
            (ErrorCode::Busy, "busy"),
            (ErrorCode::StoreUnavailable, "store_unavailable"),
            (ErrorCode::StoreTimeout, "store_timeout"),
            (ErrorCode::NotFound, "not_found"),
            (ErrorCode::Internal, "internal"),
        ]);
    }

    #[test]
    fn filter_query_string_golden() {
        assert_query_golden(
            &full_filter(),
            "type=host&source=nico-api&stage=ready&state=Ready&model=GB200+NVL\
             &instanceType=it-1&bmcVersion=1.2.3&uefiVersion=4.5&allocatable=true\
             &updateComplete=false&updateEligible=true&tenant=t1&status=Ready\
             &recordType=power_shelf&linked=false&resource=tray&initiator=manual\
             &kind=ambiguous_link&done=false",
        );
        let mut mismatch = Filter::new(UnitType::IdentityMismatch);
        mismatch.record_type = Some(RecordType::Host);
        mismatch.kind = Some(MismatchKind::NoExpectedRecord);
        assert_query_golden(
            &mismatch,
            "type=identity_mismatch&recordType=host&kind=no_expected_record",
        );
    }

    #[test]
    fn units_query_body_golden() {
        let mut filter = Filter::new(UnitType::Instance);
        filter.source = Some(Source::Rest);
        filter.done = Some(false);
        let query = UnitsQuery {
            filter,
            name: Some("^scale-.*$".to_string()),
            ids: vec!["i-1".to_string(), "i-2".to_string()],
            limit: Some(500),
            cursor: Some("eyJz".to_string()),
        };
        assert_json_golden(
            &query,
            json!({
                "type": "instance",
                "source": "rest",
                "done": false,
                "name": "^scale-.*$",
                "ids": ["i-1", "i-2"],
                "limit": 500,
                "cursor": "eyJz",
            }),
        );
    }

    #[test]
    fn summary_params_query_golden() {
        let params = SummaryParams {
            start: t("2026-10-23T10:00:00Z"),
            end: t("2026-10-23T11:30:00.250Z"),
            base: 0,
            n: 4500,
            direction: Direction::Up,
            q: Some("0.5,0.99,1".to_string()),
            add_window_seconds: 12.5,
        };
        assert_query_golden(
            &params,
            "start=2026-10-23T10%3A00%3A00Z&end=2026-10-23T11%3A30%3A00.250Z\
             &base=0&n=4500&direction=up&q=0.5%2C0.99%2C1&addWindowSeconds=12.5",
        );
        assert_eq!(params.quantiles().unwrap(), vec![0.5, 0.99, 1.0]);
    }

    #[test]
    fn count_response_json_golden() {
        let reply = CountResponse {
            count: 4321,
            sweep: sweep_info(),
        };
        let mut golden = sweep_info_json();
        golden["count"] = json!(4321);
        assert_json_golden(&reply, golden);
    }

    #[test]
    fn units_response_json_golden() {
        let host = Unit {
            id: "fm100h".to_string(),
            unit_type: UnitType::Host,
            source: Source::NicoApi,
            stage: Stage::Registered,
            done: false,
            state: "HostInitializing".to_string(),
            reason: Some("waiting for DPU".to_string()),
            bmc_ip: Some("10.0.0.5".to_string()),
            host: Some(HostFields {
                vendor: Some("NVIDIA".to_string()),
                model: Some("GB200 NVL".to_string()),
                instance_type: None,
                bmc_version: Some("1.2.3".to_string()),
                uefi_version: None,
                version_source: VersionSource::MachineRecord,
                report_at: None,
                report_error: None,
                allocatable: None,
                blocking_reason: None,
                firmware_autoupdate: Some(true),
                update_eligible: None,
                update_blocking_reason: None,
                update_complete: false,
                open_firmware_request: Some(FirmwareRequest {
                    initiator: Initiator::Automatic,
                    requested_at: Some(t("2026-10-23T09:59:00Z")),
                    initiated_at: None,
                }),
                nvlink_domain: None,
                rack_id: Some("r1".to_string()),
                dpu_ids: vec!["fm100d".to_string()],
            }),
            dpu: None,
            instance: None,
            expected: None,
        };
        let reply = UnitsResponse {
            sweep: sweep_info(),
            units: vec![host],
            missing_ids: vec!["gone-1".to_string()],
            next_cursor: None,
        };
        let mut golden = sweep_info_json();
        golden["units"] = json!([{
            "id": "fm100h",
            "type": "host",
            "source": "nico-api",
            "stage": "registered",
            "done": false,
            "state": "HostInitializing",
            "reason": "waiting for DPU",
            "bmcIp": "10.0.0.5",
            "host": {
                "vendor": "NVIDIA",
                "model": "GB200 NVL",
                "instanceType": null,
                "bmcVersion": "1.2.3",
                "uefiVersion": null,
                "versionSource": "machine_record",
                "reportAt": null,
                "reportError": null,
                "allocatable": null,
                "blockingReason": null,
                "firmwareAutoupdate": true,
                "updateEligible": null,
                "updateBlockingReason": null,
                "updateComplete": false,
                "openFirmwareRequest": {
                    "initiator": "automatic",
                    "requestedAt": "2026-10-23T09:59:00Z",
                    "initiatedAt": null,
                },
                "nvlinkDomain": null,
                "rackId": "r1",
                "dpuIds": ["fm100d"],
            },
            "dpu": null,
            "instance": null,
            "expected": null,
        }]);
        golden["missingIds"] = json!(["gone-1"]);
        golden["nextCursor"] = json!(null);
        assert_json_golden(&reply, golden);

        // The other types' fields, each on a unit of its own.
        let unit = |unit_type| Unit {
            id: "u-1".to_string(),
            unit_type,
            source: Source::NicoApi,
            stage: Stage::Registered,
            done: false,
            state: "s".to_string(),
            reason: None,
            bmc_ip: None,
            host: None,
            dpu: None,
            instance: None,
            expected: None,
        };
        for (unit, key, fields) in [
            (
                Unit {
                    dpu: Some(DpuFields { host_id: None }),
                    ..unit(UnitType::Dpu)
                },
                "dpu",
                json!({"hostId": null}),
            ),
            (
                Unit {
                    source: Source::Rest,
                    instance: Some(InstanceFields {
                        name: Some("scale-1".to_string()),
                        tenant: Some("t1".to_string()),
                        host_id: None,
                        status: "Ready".to_string(),
                    }),
                    ..unit(UnitType::Instance)
                },
                "instance",
                json!({"name": "scale-1", "tenant": "t1", "hostId": null, "status": "Ready"}),
            ),
            (
                Unit {
                    expected: Some(ExpectedFields {
                        record_type: RecordType::Switch,
                        linked: false,
                        device_id: None,
                        bmc_mac: Some("aa:bb:cc:dd:ee:ff".to_string()),
                        explored_address: None,
                    }),
                    ..unit(UnitType::ExpectedRecord)
                },
                "expected",
                json!({
                    "recordType": "switch",
                    "linked": false,
                    "deviceId": null,
                    "bmcMac": "aa:bb:cc:dd:ee:ff",
                    "exploredAddress": null,
                }),
            ),
        ] {
            let value = serde_json::to_value(&unit).unwrap();
            assert_eq!(value[key], fields, "{key}");
            for field in ["reason", "bmcIp", "host"] {
                assert_eq!(
                    value.get(field),
                    Some(&serde_json::Value::Null),
                    "{key}: {field}"
                );
            }
            assert_eq!(serde_json::from_value::<Unit>(value).unwrap(), unit);
        }
    }

    #[test]
    fn summary_response_json_golden() {
        // No usable sweep: every crossing entry stays, values are null, the
        // curve is empty. Never 0.
        let reply = SummaryResponse {
            family: Family::Host,
            start: t("2026-10-23T10:00:00Z"),
            end: t("2026-10-23T11:00:00Z"),
            base: 0,
            n: 4500,
            direction: Direction::Up,
            crossings: vec![Crossing {
                q: 0.5,
                target: 2250,
                reached_at: None,
                seconds: None,
                worst_count_after: None,
                scrape_gap_before: false,
            }],
            final_count: None,
            final_sweep_at: None,
            units_per_minute: UnitsPerMinute {
                sustained: None,
                peak_10m: None,
            },
            max_zero_progress_seconds: None,
            max_sweep_gap_seconds: None,
            max_sweep_duration_seconds: None,
            incomplete_sweeps: 2,
            scrape_gaps: vec![ScrapeGap {
                from: t("2026-10-23T10:10:00Z"),
                to: t("2026-10-23T10:10:30Z"),
                seconds: 30.0,
                new_sweep_across: true,
            }],
            scrape_gaps_total: 1,
            uncovered_scrape_gap_seconds: 0.0,
            add_window_seconds: 12.5,
            timing_error_seconds: None,
            final_scrape_seen: true,
            probe_restarts: 0,
            sweeps_seen: 0,
            curve: vec![],
            worst_count_after_first_crossing: None,
        };
        assert_json_golden(
            &reply,
            json!({
                "family": "host",
                "start": "2026-10-23T10:00:00Z",
                "end": "2026-10-23T11:00:00Z",
                "base": 0,
                "n": 4500,
                "direction": "up",
                "crossings": [
                    {
                        "q": 0.5,
                        "target": 2250,
                        "reachedAt": null,
                        "seconds": null,
                        "worstCountAfter": null,
                        "scrapeGapBefore": false,
                    },
                ],
                "finalCount": null,
                "finalSweepAt": null,
                "unitsPerMinute": {"sustained": null, "peak10m": null},
                "maxZeroProgressSeconds": null,
                "maxSweepGapSeconds": null,
                "maxSweepDurationSeconds": null,
                "incompleteSweeps": 2,
                "scrapeGaps": [
                    {
                        "from": "2026-10-23T10:10:00Z",
                        "to": "2026-10-23T10:10:30Z",
                        "seconds": 30.0,
                        "newSweepAcross": true,
                    },
                ],
                "scrapeGapsTotal": 1,
                "uncoveredScrapeGapSeconds": 0.0,
                "addWindowSeconds": 12.5,
                "timingErrorSeconds": null,
                "finalScrapeSeen": true,
                "probeRestarts": 0,
                "sweepsSeen": 0,
                "curve": [],
                "worstCountAfterFirstCrossing": null,
            }),
        );
    }

    #[test]
    fn request_none_fields_omitted() {
        let filter = Filter::new(UnitType::Host);
        assert_query_golden(&filter, "type=host");
        assert_json_golden(&filter, json!({"type": "host"}));

        let query = UnitsQuery {
            filter,
            name: None,
            ids: vec![],
            limit: None,
            cursor: None,
        };
        assert_json_golden(&query, json!({"type": "host"}));

        let params = SummaryParams {
            start: t("2026-10-23T10:00:00Z"),
            end: t("2026-10-23T11:00:00Z"),
            base: 4500,
            n: 4500,
            direction: Direction::Down,
            q: None,
            add_window_seconds: 0.0,
        };
        assert_query_golden(
            &params,
            "start=2026-10-23T10%3A00%3A00Z&end=2026-10-23T11%3A00%3A00Z\
             &base=4500&n=4500&direction=down&addWindowSeconds=0.0",
        );
        assert_eq!(
            params.quantiles().unwrap(),
            vec![0.5, 0.9, 0.95, 0.99, 0.995, 1.0]
        );
    }

    #[test]
    fn error_body_json_golden() {
        for (code, field, golden) in [
            (
                ErrorCode::UnsupportedFilter,
                Some("bmcVersion"),
                json!({"code": "unsupported_filter", "message": "m", "field": "bmcVersion"}),
            ),
            (
                ErrorCode::NoCompleteSweep,
                None,
                json!({"code": "no_complete_sweep", "message": "m", "field": null}),
            ),
        ] {
            let body = ErrorBody {
                error: ErrorDetail {
                    code,
                    message: "m".to_string(),
                    field: field.map(str::to_string),
                },
            };
            assert_json_golden(&body, json!({ "error": golden }));
        }
    }

    #[test]
    fn filter_rejects_unknown_field() {
        let err = serde_urlencoded::from_str::<Filter>("type=host&notDone=ready").unwrap_err();
        assert!(err.to_string().contains("notDone"), "{err}");
        assert!(serde_urlencoded::from_str::<Filter>("type=host&stage=done").is_err());
        assert!(serde_urlencoded::from_str::<Filter>("stage=ready").is_err());
        assert!(serde_json::from_value::<Filter>(json!({"type": "host", "bogus": 1})).is_err());

        // The flattened filter in the units body rejects unknown fields too.
        let err =
            serde_json::from_value::<UnitsQuery>(json!({"type": "host", "bogus": 1})).unwrap_err();
        assert!(err.to_string().contains("bogus"), "{err}");
        let ok: UnitsQuery =
            serde_json::from_value(json!({"type": "host", "done": false, "limit": 5})).unwrap();
        assert_eq!(ok.filter.done, Some(false));
        assert_eq!(ok.limit, Some(5));

        let err = serde_urlencoded::from_str::<SummaryParams>(
            "start=2026-10-23T10%3A00%3A00Z&end=2026-10-23T11%3A00%3A00Z\
             &base=0&n=1&direction=up&addWindowSeconds=0&extra=1",
        )
        .unwrap_err();
        assert!(err.to_string().contains("extra"), "{err}");
    }

    #[test]
    fn parse_q_defaults_and_bounds() {
        assert_eq!(
            parse_q(DEFAULT_Q).unwrap(),
            vec![0.5, 0.9, 0.95, 0.99, 0.995, 1.0]
        );
        assert_eq!(parse_q("1").unwrap(), vec![1.0]);
        assert_eq!(parse_q("0.9, 0.5,0.9").unwrap(), vec![0.5, 0.9]);
        for bad in ["", "0", "-0.5", "1.01", "0.5,", "x", "NaN", "inf"] {
            assert!(parse_q(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[cfg(feature = "schema")]
    #[test]
    fn schema_feature_generates_schemas() {
        fn schema<T: schemars::JsonSchema>() -> serde_json::Value {
            serde_json::to_value(schemars::schema_for!(T)).unwrap()
        }
        fn property_names(schema: &serde_json::Value) -> BTreeSet<String> {
            schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect()
        }

        let filter = schema::<Filter>();
        let expected: BTreeSet<String> = serde_json::to_value(full_filter())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(property_names(&filter), expected);
        assert_eq!(filter["required"], json!(["type"]));
        assert_eq!(filter["additionalProperties"], json!(false));
        assert_eq!(
            filter["$defs"]["Source"]["enum"],
            json!(["nico-api", "rest"])
        );

        let units_query = property_names(&schema::<UnitsQuery>());
        for name in ["type", "done", "name", "ids", "limit", "cursor"] {
            assert!(units_query.contains(name), "UnitsQuery lacks {name}");
        }
        let count = property_names(&schema::<CountResponse>());
        for name in ["count", "snapshotId", "family", "readAt", "sweepAgeSeconds"] {
            assert!(count.contains(name), "CountResponse lacks {name}");
        }
        let summary = property_names(&schema::<SummaryResponse>());
        for name in [
            "crossings",
            "timingErrorSeconds",
            "worstCountAfterFirstCrossing",
        ] {
            assert!(summary.contains(name), "SummaryResponse lacks {name}");
        }
        let units = property_names(&schema::<UnitsResponse>());
        for name in ["units", "missingIds", "nextCursor", "snapshotId"] {
            assert!(units.contains(name), "UnitsResponse lacks {name}");
        }

        // Every other top-level request and reply has a schema.
        for root in [
            schema::<SummaryParams>(),
            schema::<ConfigResponse>(),
            schema::<StoreStatus>(),
            schema::<ErrorBody>(),
        ] {
            assert_eq!(root["type"], json!("object"), "{root}");
        }
    }
}
