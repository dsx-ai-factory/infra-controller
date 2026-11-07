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

//! The `/v1` HTTP API, on its own port. Counts, units and config are
//! answered from the snapshot store in memory; nothing here calls a site
//! API. The wire types and their JSON are `carbide-site-health-probe-api`.

pub(crate) mod closable;
pub(crate) mod spec;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use carbide_site_health_probe_api::metrics::{CONTRACT_VERSION, FilterError, gauge_labels};
use carbide_site_health_probe_api::{
    Component, ConfigResponse, ConfigRest, CountResponse, ErrorBody, ErrorCode, ErrorDetail,
    Family, Filter, MetricsInfo, ReadInfo, RestResource, RestSiteBinding, Source, SweepInfo,
    TenantAllocation, Unit, UnitType, UnitsQuery, UnitsResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::api::spec::{Capabilities, Need, SeriesSpec, series_spec, units_listing};
use crate::config::{HostReadMode, StateConfig};
use crate::readers::devices::DEVICE_FAMILIES;
use crate::readers::rest_inventory::{RestTotalsData, SiteMatchData};
use crate::snapshot::AnySnapshot;
use crate::state::store::SnapshotStore;
use crate::state_metrics::{Series, data_of, family_series, units_of, unix_seconds};

/// Units per page unless the client asks for fewer.
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 1_000;
/// IDs one units query may ask for.
const MAX_IDS: usize = 10_000;
/// Seconds a client waits before asking again for a family with no sweep.
const RETRY_AFTER_SECONDS: u32 = 10;

/// What the API reads.
pub(crate) struct ApiState {
    pub store: Arc<SnapshotStore>,
    /// The REST inventory reader runs, so `/v1/config` waits for its first
    /// site check.
    pub rest_inventory: bool,
    /// The REST reader sweeps the site totals.
    rest_totals: bool,
    /// Each family's reader interval.
    pub intervals: BTreeMap<Family, Duration>,
    pub caps: Capabilities,
    /// `/v1/config`'s metrics block, when a metrics store is declared.
    pub metrics: Option<MetricsInfo>,
}

impl ApiState {
    /// `process_start` is the time the collector publishes as the process
    /// start.
    pub(crate) fn new(
        store: Arc<SnapshotStore>,
        state: &StateConfig,
        process_start: SystemTime,
    ) -> Self {
        let readers = &state.readers;
        let mut intervals: BTreeMap<Family, Duration> = DEVICE_FAMILIES
            .iter()
            .map(|family| (*family, readers.devices.interval))
            .collect();
        intervals.insert(Family::RestSiteMatch, readers.rest_inventory.interval);
        if !state.rest.skip_totals {
            intervals.insert(Family::RestTotals, readers.rest_inventory.interval);
        }
        if state.rest.tenant.is_some() {
            for family in crate::readers::rest_instances::FAMILIES {
                intervals.insert(family, readers.rest_inventory.interval);
            }
        }
        let metrics = state.metrics.as_ref().map(|metrics| MetricsInfo {
            contract_version: CONTRACT_VERSION,
            selector: metrics.selector.clone(),
            scrape_interval_seconds: metrics.scrape_interval_seconds,
            // Only families a reader sweeps: a disabled reader's families
            // have no series to read.
            reader_intervals_seconds: intervals
                .iter()
                .filter(|(family, _)| {
                    if DEVICE_FAMILIES.contains(family) {
                        readers.devices.enabled
                    } else {
                        readers.rest_inventory.enabled
                    }
                })
                .map(|(family, interval)| (*family, interval.as_secs_f64()))
                .collect(),
            process_start_seconds: unix_seconds(process_start),
        });
        Self {
            store,
            rest_inventory: readers.rest_inventory.enabled,
            rest_totals: readers.rest_inventory.enabled && !state.rest.skip_totals,
            intervals,
            caps: Capabilities {
                full_host_read: state.nico_api.host_read == HostReadMode::Full,
                rest_fixture: readers.rest_inventory.enabled && state.rest.tenant.is_some(),
            },
            metrics,
        }
    }

    fn interval_seconds(&self, family: Family) -> f64 {
        self.intervals
            .get(&family)
            .map_or(0.0, Duration::as_secs_f64)
    }

    /// The current snapshot of `family`, or 503 before its first sweep.
    fn current(&self, family: Family) -> Result<Arc<dyn AnySnapshot>, ApiError> {
        self.store.current(family).ok_or_else(|| {
            let mut error = ApiError::no_complete_sweep(family);
            if let Some((_, failure)) = self.store.last_attempt(family).and_then(|a| a.failure) {
                error
                    .detail
                    .message
                    .push_str(&format!(". Last attempt: {failure}"));
            }
            error
        })
    }

    fn sweep_info(&self, family: Family, snap: &dyn AnySnapshot) -> SweepInfo {
        let meta = snap.meta();
        let attempt = self.store.last_attempt(family);
        SweepInfo {
            last_attempt_succeeded: attempt.as_ref().map(|a| a.failure.is_none()),
            last_attempt_error: attempt.and_then(|a| a.failure).map(|(_, e)| e),
            snapshot_id: meta.snapshot_id(),
            family,
            sweep_start: DateTime::<Utc>::from(meta.start),
            read_at: DateTime::<Utc>::from(meta.end),
            sweep_age_seconds: age_seconds(meta.end),
            sweep_interval_seconds: self.interval_seconds(family),
        }
    }

    fn read_info(&self, family: Family, snap: &dyn AnySnapshot) -> ReadInfo {
        let meta = snap.meta();
        ReadInfo {
            read_start: DateTime::<Utc>::from(meta.start),
            read_at: DateTime::<Utc>::from(meta.end),
            read_age_seconds: age_seconds(meta.end),
            read_interval_seconds: self.interval_seconds(family),
            last_attempt_error: self
                .store
                .last_attempt(family)
                .and_then(|attempt| attempt.failure)
                .map(|(_, error)| error),
        }
    }
}

fn age_seconds(at: SystemTime) -> f64 {
    crate::clock::now()
        .duration_since(at)
        .unwrap_or_default()
        .as_secs_f64()
}

/// An error reply: a status and an [`ErrorBody`].
#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    detail: ErrorDetail,
}

impl ApiError {
    pub(crate) fn new(
        status: StatusCode,
        code: ErrorCode,
        field: Option<&str>,
        message: String,
    ) -> Self {
        Self {
            status,
            detail: ErrorDetail {
                code,
                message,
                field: field.map(str::to_string),
            },
        }
    }

    /// 400 `invalid_param`.
    pub(crate) fn bad_param(field: &str, message: String) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidParam,
            Some(field),
            message,
        )
    }

    /// 422 with `code`.
    pub(crate) fn unprocessable(code: ErrorCode, field: Option<&str>, message: String) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, field, message)
    }

    fn no_complete_sweep(family: Family) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::NoCompleteSweep,
            None,
            format!("no complete {} sweep yet", spec::wire(family)),
        )
    }

    fn snapshot_expired() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            ErrorCode::SnapshotExpired,
            Some("cursor"),
            "the snapshot of this cursor expired, start paging again".to_string(),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry = self.status == StatusCode::SERVICE_UNAVAILABLE;
        let mut response = (self.status, Json(ErrorBody { error: self.detail })).into_response();
        if retry {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECONDS));
        }
        response
    }
}

/// The `/v1` routes.
pub(crate) fn router(state: ApiState) -> Router {
    Router::new()
        .route("/v1/count", get(count))
        .route("/v1/units", get(units))
        .route("/v1/units/query", post(units_query))
        .route("/v1/config", get(config))
        .fallback(not_found)
        .with_state(Arc::new(state))
}

/// `router` with every request cut short once `cancel` fires, its body
/// extraction included: a 503, and the connection closes after it.
pub(crate) fn cancellable(router: Router, cancel: CancellationToken) -> Router {
    router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let cancel = cancel.clone();
            async move {
                tokio::select! {
                    response = next.run(request) => response,
                    () = cancel.cancelled() => {
                        let mut response = ApiError::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            ErrorCode::Internal,
                            None,
                            "the probe is shutting down".to_string(),
                        )
                        .into_response();
                        response
                            .headers_mut()
                            .insert(header::CONNECTION, HeaderValue::from_static("close"));
                        response
                    }
                }
            }
        },
    ))
}

async fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        ErrorCode::NotFound,
        None,
        "no such route".to_string(),
    )
}

/// Splits a raw query into the filter's own parameters, re-encoded, and the
/// named `extras`. A repeated parameter is 400.
fn split_query(
    raw: Option<&str>,
    extras: &[&str],
) -> Result<(String, BTreeMap<String, String>), ApiError> {
    let mut seen = BTreeSet::new();
    let mut filter = url::form_urlencoded::Serializer::new(String::new());
    let mut named = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if !seen.insert(key.to_string()) {
            return Err(ApiError::bad_param(
                &key,
                format!("{key} is given more than once"),
            ));
        }
        if extras.contains(&key.as_ref()) {
            named.insert(key.into_owned(), value.into_owned());
        } else {
            filter.append_pair(&key, &value);
        }
    }
    Ok((filter.finish(), named))
}

/// The parameter a serde error is about. Only "unknown/missing/duplicate
/// field" errors name a field between backticks. An "unknown variant" error
/// names the rejected value, so the field is the one parameter in `given`
/// that holds that value. Anything else is the whole `query`.
fn named_field(message: &str, given: &[(String, String)]) -> String {
    let token = message.split('`').nth(1);
    let field = if ["unknown field", "missing field", "duplicate field"]
        .iter()
        .any(|kind| message.starts_with(kind))
    {
        token.map(str::to_string)
    } else if message.starts_with("unknown variant") {
        let mut holders = given
            .iter()
            .filter(|(_, value)| Some(value.as_str()) == token);
        match (holders.next(), holders.next()) {
            (Some((key, _)), None) => Some(key.clone()),
            _ => None,
        }
    } else {
        None
    };
    field.unwrap_or_else(|| "query".to_string())
}

fn parse_query<T: serde::de::DeserializeOwned>(query: &str) -> Result<T, ApiError> {
    serde_urlencoded::from_str(query).map_err(|e| {
        let message = e.to_string();
        let given: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        ApiError::bad_param(&named_field(&message, &given), message)
    })
}

/// Whether the snapshot can answer the filter's needs: REST totals only
/// where they exist, and allocatability and update eligibility only from
/// hosts read with the compact host read, when every host has the answer.
fn check_needs(needs: &[Need], snap: &dyn AnySnapshot) -> Result<(), ApiError> {
    for need in needs {
        match need {
            Need::RestTotal(resource) => {
                let available = data_of::<RestTotalsData>(snap)
                    .and_then(|d| d.totals.get(resource).copied())
                    .is_some_and(|t| t.is_some());
                if !available {
                    let why = match resource {
                        RestResource::Instance => {
                            "(the REST API has no provider-scoped instance count)"
                        }
                        _ => "on this site (no NICo Flow)",
                    };
                    return Err(ApiError::unprocessable(
                        ErrorCode::FieldUnavailable,
                        Some("resource"),
                        format!("the {} total is unavailable {why}", spec::wire(resource)),
                    ));
                }
            }
            Need::CompactHostRead(field) => {
                let Some(data) = units_of(snap).filter(|data| data.compact_host_read) else {
                    return Err(FilterError::compact_host_read(field).into());
                };
                // Snapshot-wide, as `host_predicates_known` publishes it,
                // also for a query that names hosts.
                if !data.host_predicate_known(field) {
                    return Err(ApiError::unprocessable(
                        ErrorCode::FieldUnavailable,
                        Some(field),
                        format!("{field} is unknown for a host: nico-api sent it no answer"),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The sum of the spec's gauge over the series that match, from one
/// snapshot: the same series the metrics publish.
pub(crate) fn count_series(spec: &SeriesSpec, snap: &dyn AnySnapshot) -> u64 {
    let mut series = Series::new();
    family_series(spec.family, snap, &mut series);
    let labels = gauge_labels(spec.metric);
    let matches = |values: &Vec<String>| {
        spec.matchers.iter().all(|(label, want)| {
            labels
                .iter()
                .position(|l| l == label)
                .is_some_and(|i| values[i] == *want)
        })
    };
    let total: f64 = series
        .get(spec.metric)
        .map(|sets| {
            sets.iter()
                .filter(|(values, _)| matches(values))
                .map(|(_, value)| *value)
                .sum()
        })
        .unwrap_or(0.0);
    total as u64
}

async fn count(
    State(api): State<Arc<ApiState>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<CountResponse>, ApiError> {
    let (query, _) = split_query(raw.as_deref(), &[])?;
    let filter: Filter = parse_query(&query)?;
    let spec = series_spec(&filter, &api.caps)?;
    // No sweep will ever publish totals the reader does not read.
    if spec.family == Family::RestTotals && !api.rest_totals {
        return Err(ApiError::unprocessable(
            ErrorCode::FieldUnavailable,
            Some("resource"),
            "REST totals are not read: the REST reader or its totals are disabled".to_string(),
        ));
    }
    let snap = api.current(spec.family)?;
    check_needs(&spec.needs, snap.as_ref())?;
    Ok(Json(CountResponse {
        count: count_series(&spec, snap.as_ref()),
        sweep: api.sweep_info(spec.family, snap.as_ref()),
    }))
}

/// Where a page ends: the snapshot, the family, and the last unit's sort
/// key. Opaque to clients.
#[derive(Debug, Serialize, Deserialize)]
struct Cursor {
    s: String,
    f: Family,
    k: (Source, UnitType, String),
}

impl Cursor {
    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    fn decode(raw: &str) -> Result<Self, ApiError> {
        URL_SAFE_NO_PAD
            .decode(raw)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| ApiError::bad_param("cursor", "cursor is not a cursor".to_string()))
    }
}

fn sort_key(unit: &Unit) -> (Source, UnitType, String) {
    (unit.source, unit.unit_type, unit.id.clone())
}

/// One page of a units listing.
struct Page {
    name: Option<String>,
    ids: Vec<String>,
    limit: Option<u32>,
    cursor: Option<String>,
}

fn list_units(api: &ApiState, filter: &Filter, page: Page) -> Result<UnitsResponse, ApiError> {
    let limit = page.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ApiError::bad_param(
            "limit",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    if page.ids.len() > MAX_IDS {
        return Err(ApiError::bad_param(
            "ids",
            format!("at most {MAX_IDS} ids per query"),
        ));
    }
    let spec = units_listing(filter, page.name.as_deref(), &api.caps)?;
    let (snap, after) = match page.cursor.as_deref() {
        Some(raw) => {
            let cursor = Cursor::decode(raw)?;
            if cursor.f != spec.family {
                return Err(ApiError::bad_param(
                    "cursor",
                    "the cursor is from another family".to_string(),
                ));
            }
            let snap = api
                .store
                .by_id(spec.family, &cursor.s)
                .ok_or_else(ApiError::snapshot_expired)?;
            (snap, Some(cursor.k))
        }
        None => (api.current(spec.family)?, None),
    };
    check_needs(&spec.needs, snap.as_ref())?;
    let all = units_of(snap.as_ref()).map_or(&[][..], |data| data.units.as_slice());

    let present: BTreeSet<&str> = all.iter().map(|u| u.id.as_str()).collect();
    let mut missing_ids = Vec::new();
    for id in &page.ids {
        if !present.contains(id.as_str()) && !missing_ids.contains(id) {
            missing_ids.push(id.clone());
        }
    }
    let asked: BTreeSet<&str> = page.ids.iter().map(String::as_str).collect();
    let mut matching = all
        .iter()
        .filter(|u| asked.is_empty() || asked.contains(u.id.as_str()))
        .filter(|u| spec.matches(u))
        .filter(|u| after.as_ref().is_none_or(|key| sort_key(u) > *key));
    let units: Vec<Unit> = matching
        .by_ref()
        .take(limit as usize)
        .map(|unit| Unit::clone(unit))
        .collect();
    let next_cursor = match (matching.next(), units.last()) {
        (Some(_), Some(last)) => {
            // A first page registers its snapshot for retention, even one
            // replaced since it was loaded. A continuation never does: its
            // snapshot was registered by its first page, and registering it
            // again would restart an expiry that may already have passed.
            if after.is_none() {
                api.store.hold_for_paging(spec.family, &snap);
            }
            Some(
                Cursor {
                    s: snap.meta().snapshot_id(),
                    f: spec.family,
                    k: sort_key(last),
                }
                .encode(),
            )
        }
        _ => None,
    };
    Ok(UnitsResponse {
        sweep: api.sweep_info(spec.family, snap.as_ref()),
        units,
        missing_ids,
        next_cursor,
    })
}

async fn units(
    State(api): State<Arc<ApiState>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<UnitsResponse>, ApiError> {
    let (query, mut extras) = split_query(raw.as_deref(), &["name", "limit", "cursor"])?;
    let filter: Filter = parse_query(&query)?;
    let limit = extras
        .remove("limit")
        .map(|raw| {
            raw.parse::<u32>()
                .map_err(|_| ApiError::bad_param("limit", "limit is not a number".to_string()))
        })
        .transpose()?;
    let page = Page {
        name: extras.remove("name"),
        ids: Vec::new(),
        limit,
        cursor: extras.remove("cursor"),
    };
    list_units(&api, &filter, page).map(Json)
}

async fn units_query(
    State(api): State<Arc<ApiState>>,
    body: Bytes,
) -> Result<Json<UnitsResponse>, ApiError> {
    let query: UnitsQuery = serde_json::from_slice(&body).map_err(|e| {
        let message = e.to_string();
        let given: Vec<(String, String)> =
            serde_json::from_slice::<BTreeMap<String, serde_json::Value>>(&body)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(k, v)| Some((k, v.as_str()?.to_string())))
                .collect();
        ApiError::bad_param(&named_field(&message, &given), message)
    })?;
    let page = Page {
        name: query.name,
        ids: query.ids,
        limit: query.limit,
        cursor: query.cursor,
    };
    list_units(&api, &query.filter, page).map(Json)
}

/// `/v1/config` parameters. `instanceType` selects the tenant allocation.
/// Firmware targets wait for their tests (#8154): accepted, so client
/// filters stay valid, while `nicoApi` stays null.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(dead_code, reason = "no config reader serves the firmware filters yet")]
struct ConfigQuery {
    vendor: Option<String>,
    model: Option<String>,
    component: Option<Component>,
    instance_type: Option<String>,
}

async fn config(
    State(api): State<Arc<ApiState>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<ConfigResponse>, ApiError> {
    let (query, _) = split_query(raw.as_deref(), &[])?;
    let query: ConfigQuery = parse_query(&query)?;
    // The inventory reader's site check, with its own read times. No config
    // filter applies to it. With the reader off there is no check to wait
    // for: `restSite` is null, not an unknown check.
    let rest_site = if api.rest_inventory {
        let snap = api
            .store
            .current(Family::RestSiteMatch)
            .ok_or_else(|| ApiError::no_complete_sweep(Family::RestSiteMatch))?;
        data_of::<SiteMatchData>(snap.as_ref()).map(|data| RestSiteBinding {
            read: api.read_info(Family::RestSiteMatch, snap.as_ref()),
            result: data.result,
        })
    } else {
        None
    };
    Ok(Json(ConfigResponse {
        nico_api: None,
        rest: api.store.current(Family::ConfigRest).and_then(|snap| {
            let allocation = data_of::<TenantAllocation>(snap.as_ref())?;
            if query
                .instance_type
                .as_ref()
                .is_some_and(|id| id != &allocation.instance_type)
            {
                return None;
            }
            Some(ConfigRest {
                read: api.read_info(Family::ConfigRest, snap.as_ref()),
                tenant_allocation: Some(allocation.clone()),
            })
        }),
        rest_site,
        metrics: api.metrics.clone(),
    }))
}

#[cfg(test)]
mod tests;
