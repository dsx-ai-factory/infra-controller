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

//! Client for the site health probe's `/v1` API. Every state read of every
//! test goes through [`Probe`].
//!
//! The wire types are the probe's own crate, re-exported as [`wire`]. This
//! module defines no wire struct.

mod http;

#[cfg(any(test, feature = "test-support"))]
pub mod fake;

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
pub use carbide_site_health_probe_api as wire;
pub use http::HttpProbe;
use serde::Serialize;
use tokio::time::Instant;
use wire::{
    ConfigResponse, CountResponse, ErrorCode, ErrorDetail, Filter, Unit, UnitsQuery, UnitsResponse,
};

use crate::control::ControlWatch;
use crate::sweep::sweep_info_fresh;

/// Paging of `GET /v1/units`. Query parameters only, not a wire body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Query of `GET /v1/config`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<wire::Component>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_type: Option<String>,
}

/// Largest page the probe serves.
pub const MAX_PAGE: u32 = 1_000;

/// One whole snapshot of one unit type, well above a site's units of one
/// type. [`PageBudget::READ`] is sized for two of them, so one read may hold
/// up to twice this.
pub const MAX_SNAPSHOT_UNITS: usize = 200_000;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProbeError {
    #[error("probe request timed out")]
    Timeout,
    #[error("probe unreachable: {0}")]
    Transport(String),
    /// 400.
    #[error("probe rejected the request: {}", .0.message)]
    InvalidParam(ErrorDetail),
    /// 409, a paged snapshot is gone.
    #[error("probe snapshot expired")]
    SnapshotExpired,
    /// 422: `unsupported_filter` or `field_unavailable`.
    #[error("probe cannot serve this filter: {}", .0.message)]
    Unsupported(ErrorDetail),
    /// 429. `retry_after` is the probe's `Retry-After`, capped.
    #[error("probe is busy")]
    Busy { retry_after: Option<Duration> },
    /// 503. `retry_after` is the probe's `Retry-After`, capped.
    #[error("probe has no complete sweep yet")]
    NoCompleteSweep { retry_after: Option<Duration> },
    /// Any other status. Only the structured error code is kept. The body of
    /// a reply that is not a probe error is never copied.
    #[error("probe returned status {status}")]
    Status {
        status: u16,
        code: Option<ErrorCode>,
    },
    /// The reply is not what the wire says. The text is local: what kind of
    /// error and where, or that the body is over its cap. Never the body.
    #[error("probe reply did not decode: {0}")]
    Decode(String),
    /// A paged units read that is not one fresh list: a stale page, pages
    /// from two snapshots, a cursor that does not move on, or a read over its
    /// [`PageBudget`]. Never part of a list.
    #[error("probe units read unavailable: {0}")]
    Unavailable(String),
    /// The caller's [`ControlWatch`] was cancelled during the call.
    #[error("probe call cancelled")]
    Cancelled,
}

/// Longest `Retry-After` honoured. A longer one is cut to this.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

impl ProbeError {
    /// True when the state behind the request is unknown. Such a reply never
    /// marks anything done. Only a request this client got wrong says
    /// nothing about state.
    pub fn is_unknown(&self) -> bool {
        !matches!(self, Self::InvalidParam(_))
    }

    /// True when the same request may succeed later, so a poller retries it
    /// until its deadline. Everything else is final: a filter the probe
    /// never serves, a reply that does not decode, a 4xx, a cancel. A poller
    /// stops at the first such error.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Timeout
            | Self::Transport(_)
            | Self::SnapshotExpired
            | Self::Busy { .. }
            | Self::NoCompleteSweep { .. }
            | Self::Unavailable(_) => true,
            // The probe has no value yet, as with a host answer that comes
            // back on a later sweep. `unsupported_filter` is a request it
            // will never serve.
            Self::Unsupported(d) => d.code == ErrorCode::FieldUnavailable,
            Self::Status { status, .. } => *status >= 500,
            Self::InvalidParam(_) | Self::Decode(_) | Self::Cancelled => false,
        }
    }

    /// How long the probe asked to wait before the next try.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Busy { retry_after } | Self::NoCompleteSweep { retry_after } => *retry_after,
            _ => None,
        }
    }

    /// Maps a non-2xx reply, built from its `{"error": {..}}` body and its
    /// `Retry-After`. A 422 is a filter the probe cannot serve with or
    /// without a readable body, and a 408 is a timeout: both are unknown
    /// evidence.
    pub fn from_reply(status: u16, retry_after: Option<Duration>, body: &str) -> Self {
        let detail = serde_json::from_str::<wire::ErrorBody>(body)
            .ok()
            .map(|b| local_detail(b.error));
        match (status, detail) {
            (400, Some(d)) => Self::InvalidParam(d),
            (408, _) => Self::Timeout,
            (409, _) => Self::SnapshotExpired,
            (422, Some(d)) => Self::Unsupported(d),
            // Never `field_unavailable`, which a caller may accept as a
            // known gap.
            (422, None) => Self::Unsupported(ErrorDetail {
                code: ErrorCode::UnsupportedFilter,
                message: "422 without a usable error body".to_string(),
                field: None,
            }),
            (429, _) => Self::Busy { retry_after },
            (503, _) => Self::NoCompleteSweep { retry_after },
            (status, detail) => Self::Status {
                status,
                code: detail.map(|d| d.code),
            },
        }
    }

    /// A request this client refuses to send.
    fn local_invalid(message: String) -> Self {
        Self::InvalidParam(ErrorDetail {
            code: ErrorCode::InvalidParam,
            message,
            field: None,
        })
    }
}

/// The request parameters a probe error may name. Any other `field` is
/// dropped.
const KNOWN_FIELDS: &[&str] = &[
    "type",
    "source",
    "stage",
    "state",
    "model",
    "instanceType",
    "bmcVersion",
    "uefiVersion",
    "allocatable",
    "updateComplete",
    "updateEligible",
    "tenant",
    "status",
    "recordType",
    "linked",
    "resource",
    "initiator",
    "kind",
    "done",
    "name",
    "ids",
    "limit",
    "cursor",
    "vendor",
    "component",
];

/// What the client says about a probe error code. Reply text is never
/// copied: a reply is not trusted to hold nothing secret.
fn local_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidParam => "the probe rejected a request parameter",
        ErrorCode::UnsupportedFilter => "the probe cannot serve this filter",
        ErrorCode::FieldUnavailable => "the probe has no value for this field now",
        ErrorCode::NoCompleteSweep => "the probe has no complete sweep",
        ErrorCode::SnapshotExpired => "the paged snapshot expired",
        ErrorCode::EndInFuture => "end is ahead of the probe's clock",
        ErrorCode::Busy => "the probe is busy",
        ErrorCode::StoreUnavailable => "the probe's metrics store is unavailable",
        ErrorCode::StoreTimeout => "the probe's metrics store timed out",
        ErrorCode::NotFound => "the probe found nothing",
        ErrorCode::Internal => "the probe failed",
    }
}

/// An error detail built here: the structured code, the message of
/// [`local_message`], and the parameter name only when it is a known one.
fn local_detail(detail: ErrorDetail) -> ErrorDetail {
    ErrorDetail {
        code: detail.code,
        message: local_message(detail.code).to_string(),
        field: detail.field.filter(|f| KNOWN_FIELDS.contains(&f.as_str())),
    }
}

#[async_trait]
pub trait Probe: Send + Sync {
    async fn count(&self, filter: &Filter) -> Result<CountResponse, ProbeError>;
    async fn units(
        &self,
        filter: &Filter,
        name: Option<&str>,
        page: &Page,
    ) -> Result<UnitsResponse, ProbeError>;
    /// IDs, done and name travel in the body.
    async fn units_query(&self, query: &UnitsQuery) -> Result<UnitsResponse, ProbeError>;
    async fn config(&self, filter: &ConfigFilter) -> Result<ConfigResponse, ProbeError>;
}

/// What one paged units read may fetch in all, its one restart included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageBudget {
    pub pages: usize,
    pub units: usize,
}

impl PageBudget {
    /// Two whole snapshots of [`MAX_SNAPSHOT_UNITS`] in pages of
    /// [`MAX_PAGE`]: 402 pages and 400,000 units.
    pub const READ: Self = Self {
        pages: 2 * (MAX_SNAPSHOT_UNITS / MAX_PAGE as usize) + 2,
        units: 2 * MAX_SNAPSHOT_UNITS,
    };
}

/// The progress rules of one paged units read. Each page must move on: no
/// cursor twice in one pass, and no empty page that asks for more. The
/// budget counts every page and unit fetched, over the restart too, and a
/// read that needs more fails rather than stop short.
#[derive(Debug)]
pub struct PageGuard {
    budget: PageBudget,
    pages: usize,
    units: usize,
    cursors: HashSet<String>,
}

impl PageGuard {
    pub fn new(budget: PageBudget) -> Self {
        Self {
            budget,
            pages: 0,
            units: 0,
            cursors: HashSet::new(),
        }
    }

    /// The one restart after a 409 starts a new pass, so cursors may come
    /// again. What the budget spent stays spent.
    pub fn restart(&mut self) {
        self.cursors.clear();
    }

    /// Checks one page before its units are kept. `Err` says why the read
    /// cannot go on.
    pub fn admit(&mut self, reply: &UnitsResponse) -> Result<(), String> {
        self.pages += 1;
        self.units = self.units.saturating_add(reply.units.len());
        if self.units > self.budget.units {
            return Err(format!("the read holds over {} units", self.budget.units));
        }
        let Some(cursor) = &reply.next_cursor else {
            return Ok(());
        };
        if reply.units.is_empty() {
            return Err("an empty page asked for more".to_string());
        }
        if !self.cursors.insert(cursor.clone()) {
            return Err("a cursor came back twice".to_string());
        }
        if self.pages >= self.budget.pages {
            return Err(format!("the read needs over {} pages", self.budget.pages));
        }
        Ok(())
    }
}

/// Every unit matching `filter`, following the cursor, from one fresh
/// snapshot: every page must be fresh when it is read and come from the
/// snapshot of the first. A 409 (the snapshot expired between pages)
/// restarts from the first page once. A stale page, a second snapshot, a
/// cursor that does not move on or a read over [`PageBudget::READ`] is
/// [`ProbeError::Unavailable`].
///
/// The whole read, restart included, ends at `until` with
/// [`ProbeError::Timeout`], and a cancel of `control` ends it with
/// [`ProbeError::Cancelled`]. Each page also ends there, so a page in flight
/// never outlives either.
pub async fn all_units(
    probe: &dyn Probe,
    filter: &Filter,
    name: Option<&str>,
    until: Instant,
    control: &ControlWatch,
) -> Result<Vec<Unit>, ProbeError> {
    all_units_within(probe, filter, name, PageBudget::READ, until, control).await
}

async fn all_units_within(
    probe: &dyn Probe,
    filter: &Filter,
    name: Option<&str>,
    budget: PageBudget,
    until: Instant,
    control: &ControlWatch,
) -> Result<Vec<Unit>, ProbeError> {
    let mut guard = PageGuard::new(budget);
    let mut restarted = false;
    'restart: loop {
        let mut units = Vec::new();
        let mut snapshot: Option<String> = None;
        let mut page = Page {
            limit: Some(MAX_PAGE),
            cursor: None,
        };
        loop {
            // Both before the page and after it, cancel first: a timeout
            // polls its call before its timer, so a page that is ready at
            // once would be accepted past the deadline.
            stopped(until, control)?;
            let call = tokio::time::timeout_at(until, probe.units(filter, name, &page));
            let reply = tokio::select! {
                biased;
                _ = control.cancelled() => return Err(ProbeError::Cancelled),
                reply = call => reply.map_err(|_| ProbeError::Timeout)?,
            };
            stopped(until, control)?;
            let reply = match reply {
                Ok(reply) => reply,
                Err(ProbeError::SnapshotExpired) if !restarted => {
                    restarted = true;
                    guard.restart();
                    continue 'restart;
                }
                Err(e) => return Err(e),
            };
            // The probe gives the age at each reply: a later page of the
            // same snapshot can be older than the stale bound.
            if !sweep_info_fresh(&reply.sweep) {
                return Err(ProbeError::Unavailable(format!(
                    "the {:?} sweep is {:.0} s old, interval {:.0} s",
                    reply.sweep.family,
                    reply.sweep.sweep_age_seconds,
                    reply.sweep.sweep_interval_seconds
                )));
            }
            // The IDs are reply text of any length, so the error names none.
            match &snapshot {
                None => snapshot = Some(reply.sweep.snapshot_id.clone()),
                Some(first) if *first != reply.sweep.snapshot_id => {
                    return Err(ProbeError::Unavailable(
                        "pages came from two snapshots".to_string(),
                    ));
                }
                Some(_) => {}
            }
            guard.admit(&reply).map_err(ProbeError::Unavailable)?;
            units.extend(reply.units);
            match reply.next_cursor {
                Some(cursor) => page.cursor = Some(cursor),
                None => return Ok(units),
            }
        }
    }
}

/// `Err` when `control` is cancelled or `until` has passed. Cancel first.
fn stopped(until: Instant, control: &ControlWatch) -> Result<(), ProbeError> {
    if control.is_cancelled() {
        Err(ProbeError::Cancelled)
    } else if Instant::now() >= until {
        Err(ProbeError::Timeout)
    } else {
        Ok(())
    }
}

/// Query parameters of a wire request: its top-level scalars. An optional
/// field that is `None` is not serialized, so it is not a parameter.
pub fn to_query(value: &impl Serialize) -> Result<Vec<(String, String)>, ProbeError> {
    let value = serde_json::to_value(value)
        .map_err(|e| ProbeError::local_invalid(format!("encode query: {e}")))?;
    let serde_json::Value::Object(map) = value else {
        return Err(ProbeError::local_invalid(
            "a query must encode as a JSON object".to_string(),
        ));
    };
    let mut pairs = Vec::with_capacity(map.len());
    for (key, value) in map {
        let text = match value {
            serde_json::Value::String(s) => s,
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Null
            | serde_json::Value::Array(_)
            | serde_json::Value::Object(_) => {
                return Err(ProbeError::local_invalid(format!(
                    "query parameter {key} is not a scalar"
                )));
            }
        };
        pairs.push((key, text));
    }
    Ok(pairs)
}

/// The canonical query string of a filter, keys sorted. The key of count maps
/// and of the fake probe.
pub fn filter_key(f: &Filter) -> String {
    // A filter holds only scalars, so encoding cannot fail.
    let mut pairs = to_query(f).unwrap_or_default();
    pairs.sort();
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// The probe's label of a host without a model.
const UNKNOWN_MODEL: &str = "unknown";
/// The probe's label of a host without an instance type.
const NO_INSTANCE_TYPE: &str = "none";

/// True when `u` is one of the units `f` selects, as the probe's `/v1/units`
/// predicate decides it. Used by the fake probe. A filter field the unit
/// cannot carry never matches.
pub fn filter_matches(f: &Filter, u: &Unit) -> bool {
    fn eq<T: PartialEq>(want: &Option<T>, got: Option<&T>) -> bool {
        match want {
            None => true,
            Some(want) => got == Some(want),
        }
    }
    let host = u.host.as_ref();
    let instance = u.instance.as_ref();
    let expected = u.expected.as_ref();
    // A host without a model or instance type has the probe's label for it,
    // so a listing of that label matches it, as a count does.
    let label = |want: &Option<String>, have: fn(&wire::HostFields) -> Option<&String>, none| {
        want.as_ref()
            .is_none_or(|w| host.is_some_and(|h| have(h).map_or(none, String::as_str) == w))
    };
    let kind = |k: wire::MismatchKind| {
        serde_json::to_value(k)
            .ok()
            .is_some_and(|v| v.as_str() == Some(u.state.as_str()))
    };
    f.unit_type == u.unit_type
        && eq(&f.source, Some(&u.source))
        // Every unit is registered. Ready and error units are also their own.
        && match f.stage {
            None | Some(wire::Stage::Registered) => true,
            Some(stage) => u.stage == stage,
        }
        && eq(&f.state, Some(&u.state))
        && eq(&f.done, Some(&u.done))
        && label(&f.model, |h| h.model.as_ref(), UNKNOWN_MODEL)
        && label(
            &f.instance_type,
            |h| h.instance_type.as_ref(),
            NO_INSTANCE_TYPE,
        )
        && eq(&f.bmc_version, host.and_then(|h| h.bmc_version.as_ref()))
        && eq(&f.uefi_version, host.and_then(|h| h.uefi_version.as_ref()))
        && eq(&f.allocatable, host.and_then(|h| h.allocatable.as_ref()))
        && eq(&f.update_complete, host.map(|h| &h.update_complete))
        && eq(
            &f.update_eligible,
            host.and_then(|h| h.update_eligible.as_ref()),
        )
        && eq(
            &f.initiator,
            host.and_then(|h| h.open_firmware_request.as_ref())
                .map(|r| &r.initiator),
        )
        && eq(&f.tenant, instance.and_then(|i| i.tenant.as_ref()))
        && eq(&f.status, instance.map(|i| &i.status))
        && eq(&f.record_type, expected.map(|e| &e.record_type))
        && eq(&f.linked, expected.map(|e| &e.linked))
        && f.resource.is_none()
        && f.kind.is_none_or(kind)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use carbide_test_support::value_scenarios;
    use chrono::{DateTime, Utc};

    use super::fake::{Endpoint, FakeProbe, FakeSweep, unit};
    use super::wire::{Component, Family, Source, Stage, UnitType};
    use super::*;
    use crate::clock::TokioClock;

    /// The instant every fixture is built around.
    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-23T10:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn status_err(status: u16) -> ProbeError {
        ProbeError::Status { status, code: None }
    }

    fn budget(pages: usize, units: usize) -> PageBudget {
        PageBudget { pages, units }
    }

    fn sorted(mut pairs: Vec<(String, String)>) -> Vec<(String, String)> {
        pairs.sort();
        pairs
    }

    fn pairs(raw: &[(&str, &str)]) -> Vec<(String, String)> {
        raw.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn filter_query_params() {
        let ready = Filter {
            stage: Some(Stage::Ready),
            allocatable: Some(true),
            ..Filter::new(UnitType::Host)
        };
        assert_eq!(
            sorted(to_query(&ready).unwrap()),
            pairs(&[
                ("allocatable", "true"),
                ("stage", "ready"),
                ("type", "host")
            ])
        );
        assert_eq!(filter_key(&ready), "allocatable=true&stage=ready&type=host");
        let rest = Filter {
            source: Some(Source::Rest),
            ..Filter::new(UnitType::Instance)
        };
        assert_eq!(filter_key(&rest), "source=rest&type=instance");
        let odd = Filter {
            state: Some("Host Init&x".to_string()),
            ..Filter::new(UnitType::Host)
        };
        assert_eq!(filter_key(&odd), "state=Host+Init%26x&type=host");

        assert!(to_query(&Page::default()).unwrap().is_empty());
        let page = Page {
            limit: Some(MAX_PAGE),
            cursor: Some("c1".to_string()),
        };
        assert_eq!(
            sorted(to_query(&page).unwrap()),
            pairs(&[("cursor", "c1"), ("limit", "1000")])
        );
        let config = ConfigFilter {
            component: Some(Component::BmcUefi),
            ..ConfigFilter::default()
        };
        assert_eq!(
            to_query(&config).unwrap(),
            pairs(&[("component", "bmc_uefi")])
        );
        assert!(to_query(&vec![1]).is_err(), "not an object");
    }

    fn host(id: &str, stage: Stage, model: Option<&str>, instance_type: Option<&str>) -> Unit {
        Unit {
            stage,
            host: Some(wire::HostFields {
                vendor: None,
                model: model.map(str::to_string),
                instance_type: instance_type.map(str::to_string),
                bmc_version: None,
                uefi_version: None,
                version_source: wire::VersionSource::MachineRecord,
                report_at: None,
                report_error: None,
                allocatable: None,
                blocking_reason: None,
                firmware_autoupdate: None,
                update_eligible: None,
                update_blocking_reason: None,
                update_complete: false,
                open_firmware_request: None,
                nvlink_domain: None,
                rack_id: None,
                dpu_ids: Vec::new(),
            }),
            ..unit(UnitType::Host, id, stage == Stage::Ready)
        }
    }

    /// Hosts of every stage, with and without a model and instance type,
    /// and identity mismatches of both kinds.
    pub(crate) fn predicate_units() -> Vec<Unit> {
        let mismatch = |id: &str, kind: &str| Unit {
            state: kind.to_string(),
            ..unit(UnitType::IdentityMismatch, id, false)
        };
        vec![
            host("h-ready", Stage::Ready, Some("R750"), Some("gb200")),
            host("h-error", Stage::Error, None, None),
            Unit {
                stage: Stage::Registered,
                ..unit(UnitType::Host, "h-new", false)
            },
            mismatch("m-ambiguous", "ambiguous_link"),
            mismatch("m-orphan", "no_expected_record"),
        ]
    }

    /// Filters and the IDs of [`predicate_units`] the probe lists for each.
    pub(crate) fn predicate_rows() -> Vec<(&'static str, Filter, Vec<&'static str>)> {
        let hosts = |f: fn(&mut Filter)| {
            let mut filter = Filter::new(UnitType::Host);
            f(&mut filter);
            filter
        };
        let mismatches = |kind| Filter {
            kind: Some(kind),
            ..Filter::new(UnitType::IdentityMismatch)
        };
        vec![
            (
                "registered holds ready and error hosts",
                hosts(|f| f.stage = Some(Stage::Registered)),
                vec!["h-error", "h-new", "h-ready"],
            ),
            (
                "ready",
                hosts(|f| f.stage = Some(Stage::Ready)),
                vec!["h-ready"],
            ),
            (
                "error",
                hosts(|f| f.stage = Some(Stage::Error)),
                vec!["h-error"],
            ),
            (
                "a host with no model is unknown",
                hosts(|f| f.model = Some("unknown".to_string())),
                vec!["h-error"],
            ),
            (
                "a model",
                hosts(|f| f.model = Some("R750".to_string())),
                vec!["h-ready"],
            ),
            (
                "a host with no instance type is none",
                hosts(|f| f.instance_type = Some("none".to_string())),
                vec!["h-error"],
            ),
            (
                "an instance type",
                hosts(|f| f.instance_type = Some("gb200".to_string())),
                vec!["h-ready"],
            ),
            (
                "ambiguous links",
                mismatches(wire::MismatchKind::AmbiguousLink),
                vec!["m-ambiguous"],
            ),
            (
                "devices with no expected record",
                mismatches(wire::MismatchKind::NoExpectedRecord),
                vec!["m-orphan"],
            ),
        ]
    }

    #[test]
    fn filter_matches_as_the_probe_lists() {
        let units = predicate_units();
        for (scenario, filter, want) in predicate_rows() {
            let mut got: Vec<&str> = units
                .iter()
                .filter(|u| filter_matches(&filter, u))
                .map(|u| u.id.as_str())
                .collect();
            got.sort();
            assert_eq!(got, want, "{scenario}");
        }
    }

    /// A deadline no test read reaches.
    fn far() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    fn detail(code: ErrorCode) -> ErrorDetail {
        ErrorDetail {
            code,
            message: local_message(code).to_string(),
            field: None,
        }
    }

    fn busy() -> ProbeError {
        ProbeError::Busy { retry_after: None }
    }

    fn no_sweep() -> ProbeError {
        ProbeError::NoCompleteSweep { retry_after: None }
    }

    #[test]
    fn unknown_errors_are_unknown() {
        value_scenarios!(run = |e: ProbeError| e.is_unknown();
            "no reply" {
                ProbeError::Timeout => true,
                ProbeError::Transport("reset".to_string()) => true,
                ProbeError::Cancelled => true,
            }
            "the probe cannot answer yet: 422, 429, 503, 5xx" {
                ProbeError::Unsupported(detail(ErrorCode::FieldUnavailable)) => true,
                ProbeError::Unsupported(detail(ErrorCode::UnsupportedFilter)) => true,
                busy() => true,
                no_sweep() => true,
                status_err(500) => true,
                status_err(501) => true,
            }
            "a paged read that is not one fresh list" {
                ProbeError::Unavailable("stale page".to_string()) => true,
            }
            "a reply that says nothing about state: 409, 4xx, bad body" {
                ProbeError::SnapshotExpired => true,
                status_err(404) => true,
                ProbeError::Decode("eof".to_string()) => true,
            }
            "a request this client got wrong" {
                ProbeError::InvalidParam(detail(ErrorCode::InvalidParam)) => false,
            }
        );
    }

    #[test]
    fn only_transient_errors_are_retried() {
        value_scenarios!(run = |e: ProbeError| e.is_transient();
            "the same request may work later" {
                ProbeError::Timeout => true,
                ProbeError::Transport("reset".to_string()) => true,
                busy() => true,
                no_sweep() => true,
                ProbeError::SnapshotExpired => true,
                ProbeError::Unavailable("stale page".to_string()) => true,
                // A value the probe has not got yet, as a host answer that
                // comes back on a later sweep.
                ProbeError::Unsupported(detail(ErrorCode::FieldUnavailable)) => true,
                status_err(500) => true,
                status_err(503) => true,
            }
            "retrying cannot help" {
                ProbeError::InvalidParam(detail(ErrorCode::InvalidParam)) => false,
                ProbeError::Unsupported(detail(ErrorCode::UnsupportedFilter)) => false,
                ProbeError::Decode("eof".to_string()) => false,
                ProbeError::Cancelled => false,
                status_err(400) => false,
                status_err(404) => false,
            }
        );
    }

    #[test]
    fn raw_replies_map_by_status() {
        let synthetic = || {
            ProbeError::Unsupported(ErrorDetail {
                code: ErrorCode::UnsupportedFilter,
                message: "422 without a usable error body".to_string(),
                field: None,
            })
        };
        let malformed = r#"{"error":{"code":"unsupported_filter""#;
        let proper =
            r#"{"error":{"code":"field_unavailable","message":"m","field":"allocatable"}}"#;
        let internal = r#"{"error":{"code":"internal","message":"m","field":null}}"#;
        let wait = Some(Duration::from_secs(7));
        // Status, Retry-After and body as the probe or a proxy sent them, the
        // error, and whether a poller may retry it.
        let rows = [
            ("empty 422", 422, None, "", synthetic(), false),
            ("malformed 422", 422, None, malformed, synthetic(), false),
            ("HTML 422", 422, None, "<html>no</html>", synthetic(), false),
            (
                "422 with its error body",
                422,
                None,
                proper,
                ProbeError::Unsupported(ErrorDetail {
                    field: Some("allocatable".to_string()),
                    ..detail(ErrorCode::FieldUnavailable)
                }),
                true,
            ),
            ("empty 408", 408, None, "", ProbeError::Timeout, true),
            (
                "408 with a body",
                408,
                None,
                "request timeout",
                ProbeError::Timeout,
                true,
            ),
            ("empty 400", 400, None, "", status_err(400), false),
            (
                "502 with an error body",
                502,
                None,
                internal,
                ProbeError::Status {
                    status: 502,
                    code: Some(ErrorCode::Internal),
                },
                true,
            ),
            (
                "503 with Retry-After",
                503,
                wait,
                "",
                ProbeError::NoCompleteSweep { retry_after: wait },
                true,
            ),
            (
                "429 with Retry-After",
                429,
                wait,
                "",
                ProbeError::Busy { retry_after: wait },
                true,
            ),
            ("bodiless 503 from a proxy", 503, None, "", no_sweep(), true),
        ];
        for (scenario, status, retry_after, body, want, transient) in rows {
            let got = ProbeError::from_reply(status, retry_after, body);
            assert_eq!(format!("{got:?}"), format!("{want:?}"), "{scenario}");
            assert_eq!(got.is_transient(), transient, "{scenario}");
            assert_eq!(got.retry_after(), retry_after, "{scenario}");
        }
    }

    #[test]
    fn error_bodies_never_reach_the_error_text() {
        let leaks = |e: &ProbeError| {
            let text = format!("{e} {e:?}");
            ["hunter2", "TOPSECRET", "sentinel-host"]
                .iter()
                .any(|secret| text.contains(secret))
        };
        // Raw bodies that are no probe error: a proxy's HTML, a 5xx page.
        let html =
            "<html>http://user:hunter2@sentinel-host/x?t=1 Authorization: Bearer TOPSECRET</html>";
        for status in [400, 403, 404, 422, 500, 502, 503] {
            let got = ProbeError::from_reply(status, None, html);
            assert!(!leaks(&got), "{status}: {got:?}");
        }
        // A structured error: only its code, a known field name and a local
        // message survive, whatever the message and the field say.
        let body = |code: &str, message: &str, field: &str| {
            format!(r#"{{"error":{{"code":"{code}","message":"{message}","field":"{field}"}}}}"#)
        };
        let hostile = [
            "bad http://u:hunter2@sentinel-host/p",
            "Authorization: Bearer TOPSECRET",
            "password=TOPSECRET",
            "plain TOPSECRET",
        ];
        for message in hostile {
            for field in ["limit", message] {
                let raw = body("invalid_param", message, field);
                let got = ProbeError::from_reply(400, None, &raw);
                assert!(!leaks(&got), "{message} / {field}: {got:?}");
                let ProbeError::InvalidParam(d) = got else {
                    panic!("not InvalidParam");
                };
                assert_eq!(d.code, ErrorCode::InvalidParam);
                assert_eq!(d.message, local_message(ErrorCode::InvalidParam));
                // Only an allowlisted parameter name is kept.
                assert_eq!(d.field.as_deref(), (field == "limit").then_some("limit"));
            }
        }
        // The code of an unexpected status survives, the text does not.
        let internal = body("internal", "http://sentinel-host", "password=TOPSECRET");
        let got = ProbeError::from_reply(500, None, &internal);
        assert!(!leaks(&got), "{got:?}");
        assert_eq!(
            format!("{got:?}"),
            format!(
                "{:?}",
                ProbeError::Status {
                    status: 500,
                    code: Some(ErrorCode::Internal)
                }
            )
        );
    }

    /// Answers 409 on the listed `units` calls (0-based), else asks the fake.
    struct ExpireOn {
        inner: FakeProbe,
        expire: Vec<usize>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Probe for ExpireOn {
        async fn count(&self, filter: &Filter) -> Result<CountResponse, ProbeError> {
            self.inner.count(filter).await
        }

        async fn units(
            &self,
            filter: &Filter,
            name: Option<&str>,
            page: &Page,
        ) -> Result<UnitsResponse, ProbeError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.expire.contains(&call) {
                return Err(ProbeError::SnapshotExpired);
            }
            self.inner.units(filter, name, page).await
        }

        async fn units_query(&self, query: &UnitsQuery) -> Result<UnitsResponse, ProbeError> {
            self.inner.units_query(query).await
        }

        async fn config(&self, filter: &ConfigFilter) -> Result<ConfigResponse, ProbeError> {
            self.inner.config(filter).await
        }
    }

    fn probe_with_hosts(n: usize, expire: Vec<usize>) -> ExpireOn {
        let now = at();
        let inner = FakeProbe::new(Arc::new(TokioClock::starting_at(now)));
        inner.push_sweep(FakeSweep {
            family: Family::Host,
            sweep_start: now - chrono::TimeDelta::seconds(20),
            read_at: now,
            interval: std::time::Duration::from_secs(20),
            complete: true,
            counts: Default::default(),
            units: (0..n)
                .map(|i| unit(UnitType::Host, &format!("h{i:05}"), true))
                .collect(),
        });
        ExpireOn {
            inner,
            expire,
            calls: AtomicUsize::new(0),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn all_units_follows_cursor_and_restarts_once() {
        // 2,500 units are three pages of 1,000. The second call expires.
        let probe = probe_with_hosts(2500, vec![1]);
        let units = all_units(
            &probe,
            &Filter::new(UnitType::Host),
            None,
            far(),
            &ControlWatch::new(),
        )
        .await
        .unwrap();
        let ids: Vec<String> = units.into_iter().map(|u| u.id).collect();
        let want: Vec<String> = (0..2500).map(|i| format!("h{i:05}")).collect();
        assert_eq!(ids, want, "every unit once, in order");
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            5,
            "page 1, 409, then pages 1 to 3"
        );

        // A second 409 is returned, not retried again.
        let probe = probe_with_hosts(2500, vec![1, 3]);
        assert!(matches!(
            all_units(
                &probe,
                &Filter::new(UnitType::Host),
                None,
                far(),
                &ControlWatch::new()
            )
            .await,
            Err(ProbeError::SnapshotExpired)
        ));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn all_units_stops_at_its_deadline_and_on_cancel() {
        let filter = Filter::new(UnitType::Host);
        let probe = probe_with_hosts(10, vec![]);
        probe.inner.hold(Endpoint::Units);

        // A page that never comes ends the read at the deadline.
        let until = Instant::now() + Duration::from_secs(30);
        let started = Instant::now();
        let result = all_units(&probe, &filter, None, until, &ControlWatch::new()).await;
        assert!(matches!(result, Err(ProbeError::Timeout)), "{result:?}");
        assert_eq!(started.elapsed(), Duration::from_secs(30));

        // A cancel ends it at once, with its own error.
        let control = ControlWatch::new();
        let read = all_units(&probe, &filter, None, far(), &control);
        let cancel = async {
            probe.inner.wait_held(Endpoint::Units, 1).await;
            control.cancel();
        };
        let (result, ()) = tokio::join!(read, cancel);
        assert!(matches!(result, Err(ProbeError::Cancelled)), "{result:?}");
        assert!(!ProbeError::Cancelled.is_transient());
    }

    type Reply = Result<UnitsResponse, ProbeError>;

    /// Answers `units` call `n` (0-based) with `reply(n)`.
    struct Scripted {
        reply: Box<dyn Fn(usize) -> Reply + Send + Sync>,
        calls: AtomicUsize,
    }

    impl Scripted {
        fn new(reply: impl Fn(usize) -> Reply + Send + Sync + 'static) -> Self {
            Self {
                reply: Box::new(reply),
                calls: AtomicUsize::new(0),
            }
        }

        /// The pages in order, then 500s.
        fn pages(pages: Vec<Reply>) -> Self {
            Self::new(move |n| pages.get(n).cloned().unwrap_or_else(not_scripted))
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    fn not_scripted<T>() -> Result<T, ProbeError> {
        Err(ProbeError::Status {
            status: 500,
            code: None,
        })
    }

    #[async_trait]
    impl Probe for Scripted {
        async fn count(&self, _: &Filter) -> Result<CountResponse, ProbeError> {
            not_scripted()
        }

        async fn units(&self, _: &Filter, _: Option<&str>, _: &Page) -> Reply {
            (self.reply)(self.calls.fetch_add(1, Ordering::SeqCst))
        }

        async fn units_query(&self, _: &UnitsQuery) -> Reply {
            not_scripted()
        }

        async fn config(&self, _: &ConfigFilter) -> Result<ConfigResponse, ProbeError> {
            not_scripted()
        }
    }

    /// A host page of `snapshot`, `age` seconds old on a 20 s interval.
    fn page(snapshot: &str, age: f64, ids: &[&str], next: Option<&str>) -> Reply {
        let at = at();
        Ok(UnitsResponse {
            sweep: wire::SweepInfo {
                snapshot_id: snapshot.to_string(),
                family: Family::Host,
                sweep_start: at,
                read_at: at,
                sweep_age_seconds: age,
                sweep_interval_seconds: 20.0,
            },
            units: ids
                .iter()
                .map(|id| unit(UnitType::Host, id, true))
                .collect(),
            missing_ids: Vec::new(),
            next_cursor: next.map(str::to_string),
        })
    }

    /// The unit IDs, or why the read is unavailable.
    async fn read(probe: &Scripted, budget: PageBudget) -> Result<Vec<String>, String> {
        let (until, control) = (far(), ControlWatch::new());
        match all_units_within(
            probe,
            &Filter::new(UnitType::Host),
            None,
            budget,
            until,
            &control,
        )
        .await
        {
            Ok(units) => Ok(units.into_iter().map(|u| u.id).collect()),
            Err(ProbeError::Unavailable(why)) => Err(why),
            Err(other) => Err(format!("not unavailable: {other}")),
        }
    }

    fn ids(ids: &[&str]) -> Result<Vec<String>, String> {
        Ok(ids.iter().map(|id| id.to_string()).collect())
    }

    #[tokio::test(start_paused = true)]
    async fn all_units_takes_no_ready_page_after_a_stop() {
        let filter = Filter::new(UnitType::Host);
        let ready_page = || Scripted::new(|_| page("s1", 5.0, &["a"], None));
        let is = |result: Result<Vec<Unit>, ProbeError>, want: &str| {
            let got = match result {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("{e:?}"),
            };
            assert_eq!(got, want);
        };

        // The deadline has passed and the page would be ready at once.
        let probe = ready_page();
        let result = all_units(&probe, &filter, None, Instant::now(), &ControlWatch::new()).await;
        is(result, "Timeout");
        assert_eq!(probe.calls(), 0, "no page is asked for");

        // Cancelled before the read, deadline passed too: cancel comes first.
        let control = ControlWatch::new();
        control.cancel();
        let probe = ready_page();
        is(
            all_units(&probe, &filter, None, far(), &control).await,
            "Cancelled",
        );
        is(
            all_units(&probe, &filter, None, Instant::now(), &control).await,
            "Cancelled",
        );
        assert_eq!(probe.calls(), 0);

        // The call cancels while it makes its reply, which is then dropped.
        let control = ControlWatch::new();
        let inner = control.clone();
        let probe = Scripted::new(move |_| {
            inner.cancel();
            page("s1", 5.0, &["a"], None)
        });
        is(
            all_units(&probe, &filter, None, far(), &control).await,
            "Cancelled",
        );
        assert_eq!(probe.calls(), 1);

        // Nothing stopped: the same page is a list.
        let probe = ready_page();
        let units = all_units(&probe, &filter, None, far(), &ControlWatch::new())
            .await
            .unwrap();
        assert_eq!(units.len(), 1);
    }

    #[tokio::test]
    async fn all_units_needs_one_fresh_snapshot() {
        let stale = "the Host sweep is 41 s old, interval 20 s".to_string();
        let rows = [
            (
                "fresh pages of one snapshot",
                vec![
                    page("s1", 5.0, &["a"], Some("c1")),
                    page("s1", 40.0, &["b"], None),
                ],
                ids(&["a", "b"]),
            ),
            (
                "a stale first page",
                vec![page("s1", 41.0, &["a"], Some("c1"))],
                Err(stale.clone()),
            ),
            (
                "a later page goes stale",
                vec![
                    page("s1", 5.0, &["a"], Some("c1")),
                    page("s1", 41.0, &["b"], None),
                ],
                Err(stale),
            ),
            (
                "the snapshot changes between pages",
                vec![
                    page("s1", 5.0, &["a"], Some("c1")),
                    page("s2", 5.0, &["b"], None),
                ],
                Err("pages came from two snapshots".to_string()),
            ),
            (
                "a 1 MiB snapshot ID stays out of the error",
                vec![
                    page("s1", 5.0, &["a"], Some("c1")),
                    page(&"x".repeat(1 << 20), 5.0, &["b"], None),
                ],
                Err("pages came from two snapshots".to_string()),
            ),
            (
                "the one restart may read a new snapshot",
                vec![
                    page("s1", 5.0, &["a"], Some("c1")),
                    Err(ProbeError::SnapshotExpired),
                    page("s2", 5.0, &["b"], None),
                ],
                ids(&["b"]),
            ),
        ];
        for (scenario, pages, want) in rows {
            let probe = Scripted::pages(pages);
            assert_eq!(read(&probe, PageBudget::READ).await, want, "{scenario}");
        }
    }

    #[tokio::test]
    async fn all_units_fails_when_paging_does_not_progress() {
        // The same cursor again would loop for ever.
        let probe = Scripted::new(|n| page("s1", 5.0, &[&format!("u{n}")], Some("same")));
        assert_eq!(
            read(&probe, PageBudget::READ).await,
            Err("a cursor came back twice".to_string())
        );
        assert_eq!(probe.calls(), 2);

        let probe = Scripted::pages(vec![page("s1", 5.0, &[], Some("c1"))]);
        assert_eq!(
            read(&probe, PageBudget::READ).await,
            Err("an empty page asked for more".to_string())
        );

        // New cursors for ever stop at the page budget, with no list.
        let probe = Scripted::new(|n| page("s1", 5.0, &[&format!("u{n}")], Some(&format!("c{n}"))));
        assert_eq!(
            read(&probe, budget(5, 100)).await,
            Err("the read needs over 5 pages".to_string())
        );
        assert_eq!(probe.calls(), 5, "no page past the budget is asked for");

        let probe = Scripted::new(|n| page("s1", 5.0, &["a", "b"], Some(&format!("c{n}"))));
        assert_eq!(
            read(&probe, budget(100, 3)).await,
            Err("the read holds over 3 units".to_string())
        );
        assert_eq!(probe.calls(), 2);

        // The budget spans the restart. Each pass is 3 pages, and the
        // second pass reuses the first one's cursors.
        let passes = || {
            vec![
                page("s1", 5.0, &["a"], Some("c1")),
                page("s1", 5.0, &["b"], Some("c2")),
                Err(ProbeError::SnapshotExpired),
                page("s2", 5.0, &["a"], Some("c1")),
                page("s2", 5.0, &["b"], Some("c2")),
                page("s2", 5.0, &["c"], None),
            ]
        };
        let probe = Scripted::pages(passes());
        assert_eq!(read(&probe, budget(5, 100)).await, ids(&["a", "b", "c"]));
        let probe = Scripted::pages(passes());
        assert_eq!(
            read(&probe, budget(4, 100)).await,
            Err("the read needs over 4 pages".to_string()),
            "the restart does not reset the budget"
        );
        assert_eq!(PageBudget::READ, budget(402, 400_000));
    }
}
