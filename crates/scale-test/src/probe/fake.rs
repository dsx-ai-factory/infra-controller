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

//! A fake probe: scripted sweeps, replies, delays and failures, in process or
//! over HTTP with the probe's paths, wire types and error codes.
//!
//! A count or units read answers from the latest complete sweep whose
//! `read_at` is not in the future. Before one exists it answers 503
//! `no_complete_sweep`, like the probe.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::wire::{
    ConfigResponse, CountResponse, ErrorBody, ErrorCode, ErrorDetail, Family, Filter, Source,
    SweepInfo, Unit, UnitType, UnitsQuery, UnitsResponse,
};
use super::{ConfigFilter, MAX_PAGE, Page, Probe, ProbeError, filter_key, filter_matches};
use crate::clock::Clock;

/// One sweep the fake serves once its `read_at` has passed.
#[derive(Debug, Clone)]
pub struct FakeSweep {
    pub family: Family,
    pub sweep_start: DateTime<Utc>,
    pub read_at: DateTime<Utc>,
    pub interval: Duration,
    /// An incomplete sweep is never served.
    pub complete: bool,
    /// Counts by [`filter_key`].
    pub counts: BTreeMap<String, u64>,
    pub units: Vec<Unit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Endpoint {
    Count,
    Units,
    UnitsQuery,
    Config,
}

/// Default page size, as the probe's.
const DEFAULT_PAGE: u32 = 100;

struct Stored {
    snapshot_id: String,
    sweep: FakeSweep,
    /// False for sweeps made by `push_curve`, which carry counts only.
    has_units: bool,
}

#[derive(Default)]
struct FakeState {
    sweeps: Vec<Stored>,
    config: Option<ConfigResponse>,
    delays: BTreeMap<Endpoint, Duration>,
    failures: BTreeMap<Endpoint, VecDeque<ProbeError>>,
    calls: Vec<(Endpoint, String)>,
    /// Endpoints whose calls wait until released.
    held: BTreeSet<Endpoint>,
    /// Calls waiting on a hold, by endpoint.
    waiting: BTreeMap<Endpoint, usize>,
}

pub struct FakeProbe {
    clock: Arc<dyn Clock>,
    state: Mutex<FakeState>,
    /// Wakes holds and their watchers on every hold change.
    changed: Notify,
}

impl FakeProbe {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            state: Mutex::default(),
            changed: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push_sweep(&self, sweep: FakeSweep) {
        self.store(sweep, true);
    }

    fn store(&self, sweep: FakeSweep, has_units: bool) {
        let mut state = self.lock();
        let snapshot_id = format!("fake-{}", state.sweeps.len() + 1);
        state.sweeps.push(Stored {
            snapshot_id,
            sweep,
            has_units,
        });
    }

    /// One complete count-only sweep per point, ending at its time.
    pub fn push_curve(&self, filter: &Filter, points: &[(DateTime<Utc>, u64)], sweep: Duration) {
        let key = filter_key(filter);
        let length = TimeDelta::from_std(sweep).unwrap_or(TimeDelta::zero());
        for (at, count) in points {
            self.store(
                FakeSweep {
                    family: family_of(filter),
                    sweep_start: *at - length,
                    read_at: *at,
                    interval: sweep,
                    complete: true,
                    counts: BTreeMap::from([(key.clone(), *count)]),
                    units: Vec::new(),
                },
                false,
            );
        }
    }

    pub fn set_config(&self, reply: ConfigResponse) {
        self.lock().config = Some(reply);
    }

    /// Later calls of `endpoint` wait until [`release`](Self::release). Lets a
    /// test order events without guessing durations.
    pub fn hold(&self, endpoint: Endpoint) {
        self.lock().held.insert(endpoint);
        self.changed.notify_waiters();
    }

    pub fn release(&self, endpoint: Endpoint) {
        self.lock().held.remove(&endpoint);
        self.changed.notify_waiters();
    }

    /// Returns once `n` calls of `endpoint` are waiting on its hold.
    pub async fn wait_held(&self, endpoint: Endpoint, n: usize) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .lock()
                .waiting
                .get(&endpoint)
                .copied()
                .unwrap_or_default()
                >= n
            {
                return;
            }
            changed.await;
        }
    }

    async fn wait_released(&self, endpoint: Endpoint) {
        /// Uncounts a waiting call when it returns or is dropped.
        struct Waiting<'a> {
            fake: &'a FakeProbe,
            endpoint: Endpoint,
        }
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                if let Some(n) = self.fake.lock().waiting.get_mut(&self.endpoint) {
                    *n = n.saturating_sub(1);
                }
                self.fake.changed.notify_waiters();
            }
        }
        let mut counted: Option<Waiting<'_>> = None;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.lock().held.contains(&endpoint) {
                return;
            }
            if counted.is_none() {
                *self.lock().waiting.entry(endpoint).or_default() += 1;
                counted = Some(Waiting {
                    fake: self,
                    endpoint,
                });
                // Tell `wait_held`. This wakes this call too, once.
                self.changed.notify_waiters();
            }
            changed.await;
        }
    }

    /// Every later reply of `endpoint` waits `by` first.
    pub fn delay(&self, endpoint: Endpoint, by: Duration) {
        self.lock().delays.insert(endpoint, by);
    }

    /// The next `times` calls of `endpoint` fail with `error`.
    pub fn fail_next(&self, endpoint: Endpoint, error: ProbeError, times: u32) {
        let mut state = self.lock();
        let queue = state.failures.entry(endpoint).or_default();
        for _ in 0..times {
            queue.push_back(error.clone());
        }
    }

    /// Every call so far, with its filter key or other detail.
    pub fn calls(&self) -> Vec<(Endpoint, String)> {
        self.lock().calls.clone()
    }

    /// Records the call, waits its delay, then fails it if scripted.
    async fn enter(&self, endpoint: Endpoint, detail: String) -> Result<(), ProbeError> {
        let delay = {
            let mut state = self.lock();
            state.calls.push((endpoint, detail));
            state.delays.get(&endpoint).copied()
        };
        self.wait_released(endpoint).await;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        match self
            .lock()
            .failures
            .get_mut(&endpoint)
            .and_then(VecDeque::pop_front)
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn sweep_info(&self, stored: &Stored) -> SweepInfo {
        let now = self.clock.now();
        SweepInfo {
            snapshot_id: stored.snapshot_id.clone(),
            family: stored.sweep.family,
            sweep_start: stored.sweep.sweep_start,
            read_at: stored.sweep.read_at,
            sweep_age_seconds: seconds(now - stored.sweep.read_at),
            sweep_interval_seconds: stored.sweep.interval.as_secs_f64(),
        }
    }

    /// Units of one snapshot: the cursor's, or the newest served one.
    fn page(
        &self,
        filter: &Filter,
        name: Option<&str>,
        ids: &[String],
        page: &Page,
    ) -> Result<UnitsResponse, ProbeError> {
        let name = match name {
            Some(_) if filter.unit_type != UnitType::Instance => {
                return Err(invalid("name needs type=instance"));
            }
            Some(raw) => {
                Some(regex::Regex::new(raw).map_err(|e| invalid(&format!("name regex: {e}")))?)
            }
            None => None,
        };
        let limit = page.limit.unwrap_or(DEFAULT_PAGE);
        if limit == 0 || limit > MAX_PAGE {
            return Err(invalid(&format!("limit must be 1 to {MAX_PAGE}")));
        }
        let state = self.lock();
        let now = self.clock.now();
        let family = family_of(filter);
        let (stored, offset) = match &page.cursor {
            Some(cursor) => {
                let (snapshot, offset) = cursor
                    .strip_prefix("fake:")
                    .and_then(|c| c.rsplit_once(':'))
                    .and_then(|(s, o)| Some((s, o.parse::<usize>().ok()?)))
                    .ok_or_else(|| invalid("cursor"))?;
                let stored = state
                    .sweeps
                    .iter()
                    .find(|s| s.snapshot_id == snapshot)
                    .ok_or(ProbeError::SnapshotExpired)?;
                (stored, offset)
            }
            None => {
                let stored = state
                    .sweeps
                    .iter()
                    .filter(|s| {
                        s.has_units
                            && s.sweep.complete
                            && s.sweep.family == family
                            && s.sweep.read_at <= now
                    })
                    .max_by_key(|s| s.sweep.read_at)
                    .ok_or_else(no_sweep)?;
                (stored, 0)
            }
        };

        let mut units: Vec<&Unit> = stored
            .sweep
            .units
            .iter()
            .filter(|u| ids.is_empty() || ids.contains(&u.id))
            .filter(|u| filter_matches(filter, u))
            .filter(|u| match &name {
                Some(re) => u
                    .instance
                    .as_ref()
                    .and_then(|i| i.name.as_deref())
                    .is_some_and(|n| re.is_match(n)),
                None => true,
            })
            .collect();
        units.sort_by_key(|u| (wire_name(&u.source), wire_name(&u.unit_type), u.id.clone()));
        let missing_ids = ids
            .iter()
            .filter(|id| !stored.sweep.units.iter().any(|u| &u.id == *id))
            .cloned()
            .collect();
        // A cursor is caller text: an offset near the top must not overflow.
        let end = offset
            .checked_add(limit as usize)
            .ok_or_else(|| invalid("cursor"))?
            .min(units.len());
        let next_cursor = (end < units.len()).then(|| format!("fake:{}:{end}", stored.snapshot_id));
        Ok(UnitsResponse {
            sweep: self.sweep_info(stored),
            units: units
                .get(offset..end)
                .unwrap_or_default()
                .iter()
                .map(|u| (*u).clone())
                .collect(),
            missing_ids,
            next_cursor,
        })
    }
}

fn seconds(d: TimeDelta) -> f64 {
    d.num_milliseconds() as f64 / 1000.0
}

fn invalid(message: &str) -> ProbeError {
    ProbeError::InvalidParam(ErrorDetail {
        code: ErrorCode::InvalidParam,
        message: message.to_string(),
        field: None,
    })
}

fn wire_name(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The snapshot family a filter reads.
pub fn family_of(filter: &Filter) -> Family {
    match filter.unit_type {
        UnitType::Host => Family::Host,
        UnitType::Dpu => Family::Dpu,
        UnitType::Switch => Family::Switch,
        UnitType::PowerShelf => Family::PowerShelf,
        UnitType::Rack => Family::Rack,
        UnitType::Instance if filter.source == Some(Source::Rest) => Family::RestTenantInstances,
        UnitType::Instance => Family::Instance,
        UnitType::ExpectedRecord | UnitType::IdentityMismatch => Family::Expected,
        UnitType::HostFirmwareRequest => Family::HostFirmwareRequests,
        UnitType::RestTotal => Family::RestTotals,
    }
}

#[async_trait]
impl Probe for FakeProbe {
    async fn count(&self, filter: &Filter) -> Result<CountResponse, ProbeError> {
        let key = filter_key(filter);
        self.enter(Endpoint::Count, key.clone()).await?;
        if filter.done.is_some() {
            return Err(ProbeError::Unsupported(ErrorDetail {
                code: ErrorCode::UnsupportedFilter,
                message: "done is a /v1/units filter".to_string(),
                field: Some("done".to_string()),
            }));
        }
        let state = self.lock();
        let now = self.clock.now();
        let stored = state
            .sweeps
            .iter()
            .filter(|s| {
                s.sweep.complete && s.sweep.read_at <= now && s.sweep.counts.contains_key(&key)
            })
            .max_by_key(|s| s.sweep.read_at)
            .ok_or_else(no_sweep)?;
        Ok(CountResponse {
            count: stored.sweep.counts[&key],
            sweep: self.sweep_info(stored),
        })
    }

    async fn units(
        &self,
        filter: &Filter,
        name: Option<&str>,
        page: &Page,
    ) -> Result<UnitsResponse, ProbeError> {
        self.enter(Endpoint::Units, filter_key(filter)).await?;
        self.page(filter, name, &[], page)
    }

    async fn units_query(&self, query: &UnitsQuery) -> Result<UnitsResponse, ProbeError> {
        self.enter(Endpoint::UnitsQuery, filter_key(&query.filter))
            .await?;
        let page = Page {
            limit: query.limit,
            cursor: query.cursor.clone(),
        };
        self.page(&query.filter, query.name.as_deref(), &query.ids, &page)
    }

    async fn config(&self, filter: &ConfigFilter) -> Result<ConfigResponse, ProbeError> {
        self.enter(Endpoint::Config, format!("{filter:?}")).await?;
        let mut reply = self.lock().config.clone().ok_or_else(no_sweep)?;
        if let Some(nico) = reply.nico_api.as_mut() {
            nico.firmware_targets.retain(|t| {
                filter.vendor.as_ref().is_none_or(|v| *v == t.vendor)
                    && filter.model.as_ref().is_none_or(|m| *m == t.model)
                    && filter.component.is_none_or(|c| c == t.component)
            });
        }
        Ok(reply)
    }
}

/// Serves `fake` on `127.0.0.1:0` with the probe's `/v1` paths. Cancel the
/// token to stop it.
pub async fn serve(fake: Arc<FakeProbe>) -> (SocketAddr, CancellationToken) {
    let router = Router::new()
        .route("/v1/count", get(count))
        .route("/v1/units", get(units))
        .route("/v1/units/query", post(units_query))
        .route("/v1/config", get(config))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake probe");
    let addr = listener.local_addr().expect("fake probe address");
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await;
    });
    (addr, stop)
}

type Fake = State<Arc<FakeProbe>>;

/// Query keys that are not [`Filter`] fields.
const UNITS_KEYS: &[&str] = &["name", "limit", "cursor"];

/// Splits a raw query into the filter and the named extra keys, each decoded
/// as the probe would.
fn split_query<T: serde::de::DeserializeOwned>(
    raw: Option<String>,
    extra_keys: &[&str],
) -> Result<(Filter, T), ProbeError> {
    let raw = raw.unwrap_or_default();
    let (extra, filter): (Vec<_>, Vec<_>) = url::form_urlencoded::parse(raw.as_bytes())
        .into_owned()
        .partition(|(k, _)| extra_keys.contains(&k.as_str()));
    let encode = |pairs: Vec<(String, String)>| {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish()
    };
    let filter = serde_urlencoded::from_str(&encode(filter))
        .map_err(|e| invalid(&format!("filter: {e}")))?;
    let extra =
        serde_urlencoded::from_str(&encode(extra)).map_err(|e| invalid(&format!("query: {e}")))?;
    Ok((filter, extra))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnitsParams {
    name: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
}

#[derive(serde::Deserialize)]
struct NoParams {}

async fn count(State(fake): Fake, RawQuery(raw): RawQuery) -> Response {
    match split_query::<NoParams>(raw, &[]) {
        Ok((filter, _)) => reply(fake.count(&filter).await).await,
        Err(e) => reply::<()>(Err(e)).await,
    }
}

async fn units(State(fake): Fake, RawQuery(raw): RawQuery) -> Response {
    match split_query::<UnitsParams>(raw, UNITS_KEYS) {
        Ok((filter, p)) => {
            let page = Page {
                limit: p.limit,
                cursor: p.cursor,
            };
            reply(fake.units(&filter, p.name.as_deref(), &page).await).await
        }
        Err(e) => reply::<()>(Err(e)).await,
    }
}

async fn units_query(State(fake): Fake, body: String) -> Response {
    match serde_json::from_str::<UnitsQuery>(&body) {
        Ok(query) => reply(fake.units_query(&query).await).await,
        Err(e) => reply::<()>(Err(invalid(&format!("body: {e}")))).await,
    }
}

async fn config(State(fake): Fake, RawQuery(raw): RawQuery) -> Response {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Params {
        vendor: Option<String>,
        model: Option<String>,
        component: Option<super::wire::Component>,
        instance_type: Option<String>,
    }
    match serde_urlencoded::from_str::<Params>(&raw.unwrap_or_default()) {
        Ok(p) => {
            let filter = ConfigFilter {
                vendor: p.vendor,
                model: p.model,
                component: p.component,
                instance_type: p.instance_type,
            };
            reply(fake.config(&filter).await).await
        }
        Err(e) => reply::<()>(Err(invalid(&format!("query: {e}")))).await,
    }
}

/// A reply as the probe sends it. Errors the wire cannot carry are acted
/// out: a timeout hangs, a decode error sends a broken body.
async fn reply<T: Serialize>(result: Result<T, ProbeError>) -> Response {
    let error = match result {
        Ok(body) => return axum::Json(body).into_response(),
        Err(error) => error,
    };
    let (status, detail) = match error {
        ProbeError::InvalidParam(d) => (StatusCode::BAD_REQUEST, d),
        ProbeError::SnapshotExpired => (
            StatusCode::CONFLICT,
            error_detail(ErrorCode::SnapshotExpired, "snapshot expired"),
        ),
        ProbeError::Unsupported(d) => (StatusCode::UNPROCESSABLE_ENTITY, d),
        ProbeError::Busy { retry_after } => {
            let body = ErrorBody {
                error: error_detail(ErrorCode::Busy, "the probe is busy"),
            };
            let response = (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();
            return with_retry_after(response, retry_after);
        }
        ProbeError::NoCompleteSweep { retry_after } => {
            let body = ErrorBody {
                error: error_detail(ErrorCode::NoCompleteSweep, "no complete sweep yet"),
            };
            let response = (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response();
            return with_retry_after(response, retry_after);
        }
        ProbeError::Status { status, code } => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return match code {
                Some(code) => {
                    let body = ErrorBody {
                        error: error_detail(code, "scripted"),
                    };
                    (status, axum::Json(body)).into_response()
                }
                None => status.into_response(),
            };
        }
        ProbeError::Decode(_) => return (StatusCode::OK, "not json").into_response(),
        ProbeError::Transport(message) | ProbeError::Unavailable(message) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, message).into_response();
        }
        ProbeError::Cancelled => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "cancelled").into_response();
        }
        ProbeError::Timeout => {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return StatusCode::GATEWAY_TIMEOUT.into_response();
        }
    };
    (status, axum::Json(ErrorBody { error: detail })).into_response()
}

/// Sets `Retry-After` as the probe does: whole seconds, only when given.
fn with_retry_after(mut response: Response, retry_after: Option<Duration>) -> Response {
    if let Some(wait) = retry_after
        && let Ok(value) = HeaderValue::from_str(&wait.as_secs().to_string())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

fn no_sweep() -> ProbeError {
    ProbeError::NoCompleteSweep { retry_after: None }
}

fn error_detail(code: ErrorCode, message: &str) -> ErrorDetail {
    ErrorDetail {
        code,
        message: message.to_string(),
        field: None,
    }
}

/// A NICo unit with no type-specific fields: done units are `ready`, the
/// rest `registered`.
pub fn unit(unit_type: UnitType, id: &str, done: bool) -> Unit {
    Unit {
        id: id.to_string(),
        unit_type,
        source: Source::NicoApi,
        stage: if done {
            super::wire::Stage::Ready
        } else {
            super::wire::Stage::Registered
        },
        done,
        state: if done { "Ready" } else { "Initializing" }.to_string(),
        reason: (!done).then(|| "waiting".to_string()),
        bmc_ip: None,
        host: None,
        dpu: None,
        instance: None,
        expected: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TokioClock;

    fn t(seconds: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-23T10:00:00Z")
            .unwrap()
            .to_utc()
            + TimeDelta::seconds(seconds)
    }

    fn hosts() -> Filter {
        Filter::new(UnitType::Host)
    }

    fn sweep(read_at: i64, complete: bool, count: u64) -> FakeSweep {
        FakeSweep {
            family: Family::Host,
            sweep_start: t(read_at - 20),
            read_at: t(read_at),
            interval: Duration::from_secs(20),
            complete,
            counts: BTreeMap::from([(filter_key(&hosts()), count)]),
            units: (0..count)
                .map(|i| unit(UnitType::Host, &format!("h{i:04}"), i % 2 == 0))
                .collect(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fake_probe_answers_latest_complete_sweep() {
        let fake = FakeProbe::new(Arc::new(TokioClock::starting_at(t(0))));
        let no_sweep = |reply| matches!(reply, Err(ProbeError::NoCompleteSweep { .. }));
        assert!(no_sweep(fake.count(&hosts()).await.map(drop)), "empty");
        // Only an incomplete sweep, and a complete one not read yet.
        fake.push_sweep(sweep(-5, false, 9));
        fake.push_sweep(sweep(10, true, 11));
        assert!(no_sweep(fake.count(&hosts()).await.map(drop)));
        let page = Page::default();
        assert!(no_sweep(fake.units(&hosts(), None, &page).await.map(drop)));

        fake.push_sweep(sweep(-30, true, 5));
        fake.push_sweep(sweep(-10, true, 7));
        let reply = fake.count(&hosts()).await.unwrap();
        assert_eq!(reply.count, 7, "not the incomplete or future sweep");
        assert_eq!(reply.sweep.read_at, t(-10));
        assert_eq!(reply.sweep.sweep_age_seconds, 10.0);
        assert_eq!(reply.sweep.sweep_interval_seconds, 20.0);
        assert_eq!(reply.sweep.family, Family::Host);

        let units = fake.units(&hosts(), None, &page).await.unwrap();
        assert_eq!(units.units.len(), 7);
        assert_eq!(units.sweep.snapshot_id, reply.sweep.snapshot_id);

        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(
            fake.count(&hosts()).await.unwrap().count,
            11,
            "read_at reached"
        );

        // A filter no sweep holds has no complete sweep.
        let ready = Filter {
            stage: Some(super::super::wire::Stage::Ready),
            ..hosts()
        };
        assert!(no_sweep(fake.count(&ready).await.map(drop)));
        assert_eq!(
            fake.calls()
                .iter()
                .filter(|(e, _)| *e == Endpoint::Count)
                .count(),
            5
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fake_probe_pages_filters_and_names_missing_ids() {
        let fake = FakeProbe::new(Arc::new(TokioClock::starting_at(t(0))));
        fake.push_sweep(sweep(-1, true, 5));
        let not_done = Filter {
            done: Some(false),
            ..hosts()
        };
        let page = Page {
            limit: Some(1),
            cursor: None,
        };
        let first = fake.units(&not_done, None, &page).await.unwrap();
        assert_eq!(first.units[0].id, "h0001");
        let second = fake
            .units(
                &not_done,
                None,
                &Page {
                    limit: Some(1),
                    cursor: first.next_cursor,
                },
            )
            .await
            .unwrap();
        assert_eq!(second.units[0].id, "h0003");
        assert_eq!(second.next_cursor, None);

        let query = UnitsQuery {
            filter: hosts(),
            name: None,
            ids: vec!["h0002".to_string(), "gone".to_string()],
            limit: None,
            cursor: None,
        };
        let reply = fake.units_query(&query).await.unwrap();
        assert_eq!(reply.units.len(), 1);
        assert_eq!(reply.missing_ids, ["gone"]);

        assert!(matches!(
            fake.count(&not_done).await,
            Err(ProbeError::Unsupported(_))
        ));
        assert!(matches!(
            fake.units(&hosts(), Some("x"), &Page::default()).await,
            Err(ProbeError::InvalidParam(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_cursor_offset_at_the_top_is_invalid() {
        let fake = FakeProbe::new(Arc::new(TokioClock::starting_at(t(0))));
        fake.push_sweep(sweep(-1, true, 5));
        let snapshot = fake.count(&hosts()).await.unwrap().sweep.snapshot_id;
        let page = Page {
            limit: Some(MAX_PAGE),
            cursor: Some(format!("fake:{snapshot}:{}", usize::MAX)),
        };
        assert!(matches!(
            fake.units(&hosts(), None, &page).await,
            Err(ProbeError::InvalidParam(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_held_call_is_not_counted_as_waiting() {
        let fake = FakeProbe::new(Arc::new(TokioClock::starting_at(t(0))));
        fake.hold(Endpoint::Count);
        let timed_out = tokio::time::timeout(Duration::from_secs(1), fake.count(&hosts())).await;
        assert!(timed_out.is_err(), "held call must not finish");
        let waiting = |fake: &FakeProbe| {
            fake.lock()
                .waiting
                .get(&Endpoint::Count)
                .copied()
                .unwrap_or_default()
        };
        assert_eq!(waiting(&fake), 0, "a dropped call stops waiting");
        // A live call is counted again, and released ones are not.
        let release = async {
            fake.wait_held(Endpoint::Count, 1).await;
            assert_eq!(waiting(&fake), 1);
            fake.release(Endpoint::Count);
        };
        let filter = hosts();
        let both = async { tokio::join!(fake.count(&filter), release) };
        let (reply, ()) = tokio::time::timeout(Duration::from_secs(1), both)
            .await
            .expect("released call finishes");
        assert!(matches!(reply, Err(ProbeError::NoCompleteSweep { .. })));
        assert_eq!(waiting(&fake), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn aborted_held_call_does_not_satisfy_wait_held() {
        let fake = Arc::new(FakeProbe::new(Arc::new(TokioClock::starting_at(t(0)))));
        fake.hold(Endpoint::Count);
        let call = |fake: Arc<FakeProbe>| tokio::spawn(async move { fake.count(&hosts()).await });
        let first = call(fake.clone());
        fake.wait_held(Endpoint::Count, 1).await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        // Nothing is waiting now, so a second wait stays pending.
        let stale =
            tokio::time::timeout(Duration::from_secs(1), fake.wait_held(Endpoint::Count, 1)).await;
        assert!(stale.is_err(), "an aborted call was still counted");
        // Until another call really blocks.
        let second = call(fake.clone());
        tokio::time::timeout(Duration::from_secs(1), fake.wait_held(Endpoint::Count, 1))
            .await
            .expect("a live call is counted");
        second.abort();
    }
}
