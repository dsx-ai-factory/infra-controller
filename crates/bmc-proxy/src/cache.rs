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

//! Response cache for `GET` requests in classes that carry a cache policy.
//!
//! The cache stores the raw upstream body and headers of a `200` response,
//! keyed by BMC, class, and request path. A policy gives an entry three
//! windows counted from the moment it was stored: `ttl`, during which it is
//! served as-is; `stale_while_revalidate`, during which it is still served
//! while a background fetch replaces it; and `stale_if_error`, during which
//! it is served if the BMC fails to answer a fetch at all.
//!
//! Every fetch runs through one single-flight path: concurrent misses for
//! the same key wait on one upstream request, and that request runs on its
//! own task so a caller that gives up does not abandon it. After a fetch
//! yields nothing the store can hold, the key is held off for a short while
//! so a stored response that can stand in is served, or the request is
//! forwarded directly, without asking the BMC through the cache again on
//! every request.
//!
//! A write the BMC did not reject invalidates the entries of every class whose
//! policy the write matches, by bumping a per-BMC, per-class generation that
//! stored entries are checked against on lookup. A policy may also hold its
//! class off the cache for a while after such a write, for writes whose
//! effect lands asynchronously.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use carbide_instrument::emit;
use http::uri::PathAndQuery;
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use moka::Expiry;
use moka::future::Cache as MokaCache;
use serde::Deserialize;
use tokio::sync::{Semaphore, watch};
use tokio::time::Instant;
use tracing::Instrument;

use crate::class::{ClassName, ClassTable, RequestClass};
use crate::metrics::{CacheInvalidated, CacheRefreshCompleted, RefreshResult};
use crate::pattern::RequestPattern;

/// How long a key stays held off after a fetch for it yielded nothing the
/// store can hold. While held, the BMC is not asked through the cache for
/// that resource again: after an error a stored response that can stand in
/// is served, and otherwise the request is forwarded directly. So a BMC that
/// fails fast or serves an unstorable resource is not asked through the
/// cache once per client request.
const UNSTORABLE_FETCH_HOLD_OFF: Duration = Duration::from_secs(30);

/// Distinct keys the hold-off record retains at most; each record is a
/// timestamp and a reason, so this is a memory bound, not a behavior one.
const HOLD_OFF_RECORD_CAPACITY: u64 = 65_536;

/// Fetches the cache runs against one BMC at a time. A fetch outlives the
/// caller that started it, so without this a caller asking for many distinct
/// resources and leaving would pile that many requests onto a BMC with a
/// handful of session slots. Further fetches queue behind these; the
/// caller-facing wait is the same as for a busy BMC.
const MAX_FETCHES_PER_BMC: usize = 4;

/// Fetches the cache accepts for one BMC at a time, running or waiting for
/// one of the [`MAX_FETCHES_PER_BMC`] slots. A burst of distinct resources
/// must not queue an unbounded number of tasks that all reach the BMC after
/// their callers left; past this bound a fetch is refused at once.
const MAX_PENDING_FETCHES_PER_BMC: usize = 32;

/// The query parameters Redfish defines. A BMC ignores any other parameter
/// and answers the same resource, so a request carrying one is not cached:
/// keying it would let a caller mint an unbounded number of entries, each
/// its own fetch and its own stored body.
const REDFISH_QUERY_PARAMETERS: &[&str] = &[
    "$expand",
    "$select",
    "$filter",
    "$top",
    "$skip",
    "$skiptoken",
    "only",
    "excerpt",
];

/// Whether a request's query can key a cache entry: absent, or made only of
/// [`REDFISH_QUERY_PARAMETERS`].
pub(crate) fn is_cacheable_query(path_and_query: &PathAndQuery) -> bool {
    path_and_query.query().is_none_or(|query| {
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .all(|pair| REDFISH_QUERY_PARAMETERS.contains(&pair.split('=').next().unwrap_or(pair)))
    })
}

/// Locks one of the cache's bookkeeping maps, recovering the data if a panic
/// left the mutex poisoned: the maps hold plain records no half-done
/// operation can corrupt, and the in-flight guard's `Drop` runs while
/// unwinding, where a second panic would abort the process.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The `cache` table of a `[[class]]`, as written in the config file. An
/// unknown field is a configuration error: a misspelled window must not be
/// silently ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CachePolicyConfig {
    /// How long a stored response is served without asking the BMC.
    #[serde(with = "humantime_serde")]
    ttl: Duration,
    /// After `ttl`, how much longer a stored response is still served while
    /// the proxy refreshes it in the background. Zero disables.
    #[serde(with = "humantime_serde", default)]
    stale_while_revalidate: Duration,
    /// After `ttl`, how much longer a stored response is served when the BMC
    /// fails to answer a fetch or answers with a server error. Zero disables.
    #[serde(with = "humantime_serde", default)]
    stale_if_error: Duration,
    /// Writes that drop this class's entries for the written BMC. Empty
    /// means any successful write to the BMC does.
    #[serde(default)]
    invalidated_by: Vec<RequestPattern>,
    /// After an invalidating write, how long requests in this class for the
    /// written BMC are forwarded without the cache, for writes whose effect
    /// lands after the response. Zero disables.
    #[serde(with = "humantime_serde", default)]
    hold_after_write: Duration,
}

/// A validated cache policy.
pub(crate) struct CachePolicy {
    ttl: Duration,
    stale_while_revalidate: Duration,
    stale_if_error: Duration,
    invalidated_by: Vec<RequestPattern>,
    hold_after_write: Duration,
    /// How long an entry stays retrievable at all: fresh, then the longer of
    /// the two stale windows.
    lifetime: Duration,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum CachePolicyError {
    #[error("ttl must be greater than zero")]
    ZeroTtl,
    #[error("ttl plus the longer stale window exceeds the representable duration")]
    WindowOverflow,
    #[error("hold_after_write exceeds the representable duration")]
    HoldOverflow,
}

impl TryFrom<CachePolicyConfig> for CachePolicy {
    type Error = CachePolicyError;

    fn try_from(config: CachePolicyConfig) -> Result<Self, Self::Error> {
        if config.ttl.is_zero() {
            return Err(CachePolicyError::ZeroTtl);
        }
        let lifetime = config
            .ttl
            .checked_add(config.stale_while_revalidate.max(config.stale_if_error))
            .ok_or(CachePolicyError::WindowOverflow)?;
        // The hold is added to an instant on every invalidating write; a value
        // that cannot be added now cannot be added then either.
        Instant::now()
            .checked_add(config.hold_after_write)
            .ok_or(CachePolicyError::HoldOverflow)?;
        Ok(Self {
            ttl: config.ttl,
            stale_while_revalidate: config.stale_while_revalidate,
            stale_if_error: config.stale_if_error,
            invalidated_by: config.invalidated_by,
            hold_after_write: config.hold_after_write,
            lifetime,
        })
    }
}

impl CachePolicy {
    /// The policy's windows as configured: `ttl`, `stale_while_revalidate`,
    /// `stale_if_error`, `hold_after_write`.
    #[cfg(test)]
    pub(crate) fn windows_for_test(&self) -> [Duration; 4] {
        [
            self.ttl,
            self.stale_while_revalidate,
            self.stale_if_error,
            self.hold_after_write,
        ]
    }

    /// Whether a successful write of `method` to `path` on a BMC drops this
    /// policy's entries for that BMC.
    fn invalidated_by(&self, method: &Method, path: &str) -> bool {
        self.invalidated_by.is_empty()
            || self
                .invalidated_by
                .iter()
                .any(|pattern| pattern.matches(method, path))
    }
}

/// Identity of one cacheable resource: the BMC, the class whose policy
/// governs it, and the request path with its query. A single trailing slash
/// names the same resource as none.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct CacheKey {
    bmc: IpAddr,
    class: ClassName,
    path_and_query: String,
}

impl CacheKey {
    pub(crate) fn new(bmc: IpAddr, class: ClassName, path_and_query: &PathAndQuery) -> Self {
        let path = path_and_query.path();
        let path = path
            .strip_suffix('/')
            .filter(|stripped| !stripped.is_empty())
            .unwrap_or(path);
        let path_and_query = match path_and_query.query() {
            Some(query) => format!("{path}?{query}"),
            None => path.to_string(),
        };
        Self {
            bmc,
            class,
            path_and_query,
        }
    }
}

/// One stored upstream response.
pub(crate) struct CachedResponse {
    /// The upstream response headers as the caller should receive them.
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    stored_at: Instant,
    /// The class generation for this BMC when the entry was stored; a later
    /// generation means a write invalidated it.
    generation: u64,
    /// Total retention, after which the store drops the entry.
    lifetime: Duration,
}

/// Where an entry sits in its policy's windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Freshness {
    /// Within `ttl`: serve as-is.
    Fresh,
    /// Past `ttl`, within `stale_while_revalidate`: serve, refresh behind.
    Stale,
    /// Past both: fetch before serving.
    Expired,
}

impl CachedResponse {
    /// The BMC's `ETag`, sent back as `If-None-Match` when refreshing and
    /// matched against a caller's `If-None-Match`.
    pub(crate) fn etag(&self) -> Option<&HeaderValue> {
        self.headers.get(header::ETAG)
    }

    pub(crate) fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.stored_at)
    }

    pub(crate) fn freshness(&self, policy: &CachePolicy, now: Instant) -> Freshness {
        let age = self.age(now);
        if age < policy.ttl {
            Freshness::Fresh
        } else if age < policy.ttl + policy.stale_while_revalidate {
            Freshness::Stale
        } else {
            Freshness::Expired
        }
    }

    /// Whether the entry may stand in for a fetch the BMC failed.
    pub(crate) fn usable_on_error(&self, policy: &CachePolicy, now: Instant) -> bool {
        self.age(now) < policy.ttl + policy.stale_if_error
    }
}

/// Retains each entry for the lifetime its policy computed when it was
/// stored, so one store serves classes with different windows.
struct EntryLifetime;

impl Expiry<CacheKey, Arc<CachedResponse>> for EntryLifetime {
    fn expire_after_create(
        &self,
        _key: &CacheKey,
        value: &Arc<CachedResponse>,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.lifetime)
    }

    fn expire_after_update(
        &self,
        _key: &CacheKey,
        value: &Arc<CachedResponse>,
        _updated_at: std::time::Instant,
        _duration_until_expiry: Option<Duration>,
    ) -> Option<Duration> {
        Some(value.lifetime)
    }
}

/// The conditional part of an upstream fetch the cache asks for.
pub(crate) struct FetchRequest {
    /// The stored entry's `ETag`, when refreshing one.
    pub(crate) if_none_match: Option<HeaderValue>,
}

/// What the upstream leg produced, in the shape the cache stores.
pub(crate) enum UpstreamReply {
    /// The BMC answered; `headers` are already filtered for the caller.
    Response {
        status: StatusCode,
        headers: HeaderMap,
        body: Bytes,
    },
    /// The BMC answered with a body too large to store, so the response was
    /// not read in full; each waiter forwards its own request instead.
    TooLarge,
    /// The forward produced no response.
    Failed { status: StatusCode, message: String },
}

/// Outcome of one fetch, delivered to every request that waited on it.
#[derive(Clone)]
pub(crate) enum FetchOutcome {
    /// A `200` fetched now, and stored unless the class is held for the BMC.
    Fetched(Arc<CachedResponse>),
    /// A `304` to the cache's conditional request; the existing entry was
    /// refreshed and is returned.
    Revalidated(Arc<CachedResponse>),
    /// Any other response, handed to waiters as-is and not stored.
    Passthrough {
        status: StatusCode,
        headers: HeaderMap,
        body: Bytes,
    },
    /// The body was too large to store; the caller must forward for itself.
    TooLarge,
    /// The forward produced no response.
    Failed { status: StatusCode, message: String },
}

/// Why a key is held off the cache after a fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldOffReason {
    /// The BMC failed to answer or answered with a server error; a stored
    /// response may stand in.
    Error,
    /// The BMC answered, but with nothing the store can hold: an oversized
    /// or encoded body, or a client error about the request. What it
    /// answered is what callers should see, so nothing stands in.
    Unstorable,
}

/// One hold-off: when it was recorded and why.
#[derive(Debug, Clone, Copy)]
struct HoldOff {
    at: Instant,
    reason: HoldOffReason,
}

/// Per-BMC, per-class invalidation state.
#[derive(Default, Clone, Copy)]
struct ClassState {
    /// Bumped by every invalidating write; entries record it when stored.
    generation: u64,
    /// While set and in the future, the class is forwarded uncached for
    /// this BMC.
    held_until: Option<Instant>,
}

pub(crate) struct ResponseCache {
    /// Weighted by body size, so the bound is bytes rather than entries.
    entries: MokaCache<CacheKey, Arc<CachedResponse>>,
    class_states: Mutex<HashMap<(IpAddr, ClassName), ClassState>>,
    /// Fetches in progress, for requests to join instead of starting another.
    /// A write to the BMC removes the affected class's entries here too, so
    /// a request arriving after the write cannot join a fetch that began
    /// before it and receive what the BMC held then.
    in_flight: Mutex<HashMap<CacheKey, InFlight>>,
    /// Source of [`InFlight::id`].
    next_fetch_id: AtomicU64,
    /// Keys whose last fetch yielded nothing storable, for
    /// [`UNSTORABLE_FETCH_HOLD_OFF`]. Expiry here bounds memory; the window
    /// itself is checked against the recorded instant.
    hold_offs: MokaCache<CacheKey, HoldOff>,
    /// Per-BMC bound on concurrent fetches, see [`MAX_FETCHES_PER_BMC`].
    fetch_permits: Mutex<HashMap<IpAddr, Arc<Semaphore>>>,
    /// Per-BMC bound on running plus waiting fetches, see
    /// [`MAX_PENDING_FETCHES_PER_BMC`].
    pending_slots: Mutex<HashMap<IpAddr, Arc<Semaphore>>>,
}

/// One fetch in progress: what requests join, and which fetch it is.
struct InFlight {
    /// Distinguishes this fetch from one that superseded it after a write,
    /// so a finished fetch removes only its own entry.
    id: u64,
    receiver: watch::Receiver<Option<FetchOutcome>>,
}

/// Removes a fetch's entry from the in-flight map when its task ends,
/// however it ends: a panic must not leave a dead receiver that every later
/// request for the key would wait on. Only the entry it made: a write may
/// have replaced it with a newer fetch that must stay.
struct InFlightGuard {
    cache: Arc<ResponseCache>,
    key: CacheKey,
    id: u64,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut in_flight = lock(&self.cache.in_flight);
        if in_flight
            .get(&self.key)
            .is_some_and(|entry| entry.id == self.id)
        {
            in_flight.remove(&self.key);
        }
    }
}

impl ResponseCache {
    /// A store holding at most `max_bytes` of response bodies.
    pub(crate) fn new(max_bytes: u64) -> Self {
        Self {
            entries: MokaCache::builder()
                .weigher(|_key: &CacheKey, value: &Arc<CachedResponse>| {
                    u32::try_from(value.body.len()).unwrap_or(u32::MAX).max(1)
                })
                .max_capacity(max_bytes)
                .expire_after(EntryLifetime)
                .support_invalidation_closures()
                .build(),
            class_states: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
            next_fetch_id: AtomicU64::new(0),
            hold_offs: MokaCache::builder()
                .max_capacity(HOLD_OFF_RECORD_CAPACITY)
                .time_to_live(2 * UNSTORABLE_FETCH_HOLD_OFF)
                .build(),
            fetch_permits: Mutex::new(HashMap::new()),
            pending_slots: Mutex::new(HashMap::new()),
        }
    }

    fn fetch_permits_for(&self, bmc: IpAddr) -> Arc<Semaphore> {
        per_bmc_semaphore(&self.fetch_permits, bmc, MAX_FETCHES_PER_BMC)
    }

    fn pending_slots_for(&self, bmc: IpAddr) -> Arc<Semaphore> {
        per_bmc_semaphore(&self.pending_slots, bmc, MAX_PENDING_FETCHES_PER_BMC)
    }

    fn class_state(&self, bmc: IpAddr, class: &ClassName) -> ClassState {
        lock(&self.class_states)
            .get(&(bmc, class.clone()))
            .copied()
            .unwrap_or_default()
    }

    /// Whether `class` is currently held off the cache for `bmc` because of
    /// a recent invalidating write.
    pub(crate) fn is_held(&self, bmc: IpAddr, class: &ClassName, now: Instant) -> bool {
        self.class_state(bmc, class)
            .held_until
            .is_some_and(|until| now < until)
    }

    /// Why `key` is held off the cache, if a fetch for it yielded nothing
    /// storable within [`UNSTORABLE_FETCH_HOLD_OFF`].
    pub(crate) async fn hold_off_reason(
        &self,
        key: &CacheKey,
        now: Instant,
    ) -> Option<HoldOffReason> {
        self.hold_offs
            .get(key)
            .await
            .filter(|hold| now.saturating_duration_since(hold.at) < UNSTORABLE_FETCH_HOLD_OFF)
            .map(|hold| hold.reason)
    }

    async fn record_hold_off(&self, key: &CacheKey, reason: HoldOffReason, now: Instant) {
        self.hold_offs
            .insert(key.clone(), HoldOff { at: now, reason })
            .await;
    }

    /// The stored entry for `key`, if one exists and no write to the BMC has
    /// invalidated its class since it was stored.
    pub(crate) async fn lookup(&self, key: &CacheKey) -> Option<Arc<CachedResponse>> {
        let entry = self.entries.get(key).await?;
        if entry.generation != self.class_state(key.bmc, &key.class).generation {
            self.entries.invalidate(key).await;
            return None;
        }
        Some(entry)
    }

    /// Fetches `key` through the fetch `make_fetch` builds, or joins the fetch
    /// already in flight for it, and returns the outcome plus whether this
    /// call joined one. `make_fetch` runs only when a fetch is started.
    ///
    /// The fetch runs on its own task: dropping this future, as a caller
    /// that timed out does, does not cancel the upstream request, so the
    /// next caller finds the entry stored. `existing` is the entry being
    /// refreshed, whose `ETag` makes the fetch conditional.
    pub(crate) async fn fetch<M, F, Fut>(
        self: &Arc<Self>,
        key: CacheKey,
        class: &Arc<RequestClass>,
        existing: Option<Arc<CachedResponse>>,
        make_fetch: M,
    ) -> (FetchOutcome, bool)
    where
        M: FnOnce() -> F,
        F: FnOnce(FetchRequest) -> Fut + Send + 'static,
        Fut: Future<Output = UpstreamReply> + Send + 'static,
    {
        let (mut receiver, joined) = self.start_or_join(key, class, existing, make_fetch);
        loop {
            let outcome = receiver.borrow_and_update().clone();
            if let Some(outcome) = outcome {
                return (outcome, joined);
            }
            if receiver.changed().await.is_err() {
                return (
                    FetchOutcome::Failed {
                        status: StatusCode::BAD_GATEWAY,
                        message: "upstream fetch ended without a result".to_string(),
                    },
                    joined,
                );
            }
        }
    }

    /// Refreshes `key` in the background through the same single-flight
    /// path, so a stale entry is replaced once however many requests see it.
    pub(crate) fn spawn_refresh<M, F, Fut>(
        self: &Arc<Self>,
        key: CacheKey,
        class: &Arc<RequestClass>,
        existing: Arc<CachedResponse>,
        make_fetch: M,
    ) where
        M: FnOnce() -> F,
        F: FnOnce(FetchRequest) -> Fut + Send + 'static,
        Fut: Future<Output = UpstreamReply> + Send + 'static,
    {
        let _ = self.start_or_join(key, class, Some(existing), make_fetch);
    }

    fn start_or_join<M, F, Fut>(
        self: &Arc<Self>,
        key: CacheKey,
        class: &Arc<RequestClass>,
        existing: Option<Arc<CachedResponse>>,
        make_fetch: M,
    ) -> (watch::Receiver<Option<FetchOutcome>>, bool)
    where
        M: FnOnce() -> F,
        F: FnOnce(FetchRequest) -> Fut + Send + 'static,
        Fut: Future<Output = UpstreamReply> + Send + 'static,
    {
        let mut in_flight = lock(&self.in_flight);
        if let Some(entry) = in_flight.get(&key) {
            return (entry.receiver.clone(), true);
        }
        // Past the pending bound nothing is spawned: the refusal is published
        // at once, and the request decides what to do with it.
        let Ok(pending_slot) = self.pending_slots_for(key.bmc).try_acquire_owned() else {
            emit(CacheRefreshCompleted {
                class: class.name.clone(),
                result: RefreshResult::Refused,
            });
            let (_sender, receiver) = watch::channel(Some(FetchOutcome::Failed {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: format!("too many fetches pending for BMC {}", key.bmc),
            }));
            return (receiver, false);
        };
        // Build the fetch and the guard that removes the map entry before
        // inserting it, so nothing that can fail sits between the entry and
        // its remover; a task the runtime drops unrun drops the guard too.
        let fetch = make_fetch();
        let id = self.next_fetch_id.fetch_add(1, Ordering::Relaxed);
        let guard = InFlightGuard {
            cache: Arc::clone(self),
            key: key.clone(),
            id,
        };
        let (sender, receiver) = watch::channel(None);
        in_flight.insert(
            key.clone(),
            InFlight {
                id,
                receiver: receiver.clone(),
            },
        );
        drop(in_flight);

        // The task outlives the request that started it, so it gets its own
        // span, linked to the request's rather than nested in it: nesting
        // would hold the request span open until the fetch ends.
        let span = tracing::info_span!(
            parent: None,
            "bmc_proxy_cache_fetch",
            bmc.ip_address = %key.bmc,
            bmc_proxy.class = %key.class,
            logfmt.suppress = true,
        );
        span.follows_from(tracing::Span::current());

        let cache = Arc::clone(self);
        let class = Arc::clone(class);
        let permits = self.fetch_permits_for(key.bmc);
        tokio::spawn(
            async move {
                // Queue behind the BMC's other fetches; joiners keep waiting
                // on the receiver meanwhile. The wait is bounded by the class
                // budget, so a fetch cannot sit in the queue longer than it
                // could have run.
                let outcome =
                    match tokio::time::timeout(class.upstream_timeout, permits.acquire_owned())
                        .await
                    {
                        Ok(Ok(_permit)) => cache.run_fetch(&key, &class, existing, fetch).await,
                        Ok(Err(_)) | Err(_) => {
                            emit(CacheRefreshCompleted {
                                class: class.name.clone(),
                                result: RefreshResult::Refused,
                            });
                            FetchOutcome::Failed {
                                status: StatusCode::SERVICE_UNAVAILABLE,
                                message: format!(
                                    "no fetch slot for BMC {} within {:?}",
                                    key.bmc, class.upstream_timeout
                                ),
                            }
                        }
                    };
                // Publish before leaving the in-flight map, so a request
                // arriving now either joins this fetch or finds what it stored.
                sender.send_replace(Some(outcome));
                drop(guard);
                drop(pending_slot);
            }
            .instrument(span),
        );
        (receiver, false)
    }

    async fn run_fetch<F, Fut>(
        &self,
        key: &CacheKey,
        class: &RequestClass,
        existing: Option<Arc<CachedResponse>>,
        fetch: F,
    ) -> FetchOutcome
    where
        F: FnOnce(FetchRequest) -> Fut,
        Fut: Future<Output = UpstreamReply>,
    {
        let Some(policy) = &class.cache else {
            return FetchOutcome::Failed {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: format!("class {} has no cache policy", class.name),
            };
        };
        // Read before the fetch: a write that lands while the BMC is
        // answering must invalidate what that answer stores.
        let generation = self.class_state(key.bmc, &key.class).generation;
        let if_none_match = existing.as_ref().and_then(|entry| entry.etag().cloned());

        let reply = fetch(FetchRequest { if_none_match }).await;
        let now = Instant::now();
        // A write that landed between the caller's hold check and the
        // generation read above left the class held; what the BMC answered
        // may predate that write's effect and must not outlive the hold.
        let held = self.is_held(key.bmc, &key.class, now);

        let (result, outcome) = match (reply, existing) {
            (
                UpstreamReply::Response {
                    status: StatusCode::OK,
                    headers,
                    body,
                },
                _,
            ) if !is_encoded(&headers) && !forbids_shared_storage(&headers) => {
                let entry = Arc::new(CachedResponse {
                    headers: storable_headers(headers),
                    body,
                    stored_at: now,
                    generation,
                    lifetime: policy.lifetime,
                });
                if held {
                    (RefreshResult::Uncacheable, FetchOutcome::Fetched(entry))
                } else {
                    self.entries.insert(key.clone(), Arc::clone(&entry)).await;
                    (RefreshResult::Stored, FetchOutcome::Fetched(entry))
                }
            }
            (
                UpstreamReply::Response {
                    status: StatusCode::NOT_MODIFIED,
                    headers,
                    ..
                },
                Some(previous),
            ) => {
                let headers =
                    storable_headers(merge_validated_headers(&previous.headers, &headers));
                let forbidden = forbids_shared_storage(&headers);
                let entry = Arc::new(CachedResponse {
                    headers,
                    body: previous.body.clone(),
                    stored_at: now,
                    generation,
                    lifetime: policy.lifetime,
                });
                if forbidden {
                    // The BMC now forbids a shared cache to hold this; the
                    // waiters get the body it just confirmed, the store does
                    // not keep it.
                    self.entries.invalidate(key).await;
                    self.record_hold_off(key, HoldOffReason::Unstorable, now)
                        .await;
                    (RefreshResult::Uncacheable, FetchOutcome::Revalidated(entry))
                } else if held {
                    (RefreshResult::Uncacheable, FetchOutcome::Revalidated(entry))
                } else {
                    self.entries.insert(key.clone(), Arc::clone(&entry)).await;
                    (RefreshResult::Revalidated, FetchOutcome::Revalidated(entry))
                }
            }
            (
                UpstreamReply::Response {
                    status: StatusCode::NOT_MODIFIED,
                    ..
                },
                None,
            ) => {
                // The fetch was unconditional, so the BMC had nothing to
                // compare against; a 304 here is its error, not an answer.
                self.record_hold_off(key, HoldOffReason::Error, now).await;
                (
                    RefreshResult::Failed,
                    FetchOutcome::Failed {
                        status: StatusCode::BAD_GATEWAY,
                        message: "BMC answered 304 Not Modified to an unconditional request"
                            .to_string(),
                    },
                )
            }
            (
                UpstreamReply::Response {
                    status,
                    headers,
                    body,
                },
                _,
            ) => {
                if is_definitive_rejection(status) {
                    // The BMC answered for the resource and the answer is
                    // not the stored one; a stale copy must not outlive it.
                    self.entries.invalidate(key).await;
                } else if status.is_server_error() {
                    self.record_hold_off(key, HoldOffReason::Error, now).await;
                } else {
                    if status == StatusCode::OK {
                        // A 200 the store cannot hold, encoded or marked
                        // unshareable, is the resource's current body; a
                        // stored one is superseded and must not be served.
                        self.entries.invalidate(key).await;
                    }
                    // The BMC answered, so callers see that answer rather
                    // than a stored one, and nothing is stored.
                    self.record_hold_off(key, HoldOffReason::Unstorable, now)
                        .await;
                }
                (
                    RefreshResult::Uncacheable,
                    FetchOutcome::Passthrough {
                        status,
                        headers,
                        body,
                    },
                )
            }
            (UpstreamReply::TooLarge, _) => {
                self.record_hold_off(key, HoldOffReason::Unstorable, now)
                    .await;
                (RefreshResult::Uncacheable, FetchOutcome::TooLarge)
            }
            (UpstreamReply::Failed { status, message }, _) => {
                self.record_hold_off(key, HoldOffReason::Error, now).await;
                (
                    RefreshResult::Failed,
                    FetchOutcome::Failed { status, message },
                )
            }
        };

        emit(CacheRefreshCompleted {
            class: class.name.clone(),
            result,
        });
        outcome
    }

    /// A write of `method` to `path` on `bmc` was not rejected by the BMC, or
    /// may have reached it: drop the entries of every cached class whose
    /// policy the write matches, and hold each such class off the cache for
    /// its configured window.
    pub(crate) fn invalidate_for_write(
        &self,
        bmc: IpAddr,
        method: &Method,
        path: &str,
        classes: &ClassTable,
        now: Instant,
    ) {
        for class in classes.cached_classes() {
            let Some(policy) = &class.cache else {
                continue;
            };
            if !policy.invalidated_by(method, path) {
                continue;
            }
            {
                let mut states = lock(&self.class_states);
                let state = states.entry((bmc, class.name.clone())).or_default();
                state.generation += 1;
                if !policy.hold_after_write.is_zero() {
                    // Validated to fit at config time.
                    state.held_until = now.checked_add(policy.hold_after_write);
                }
            }
            // A fetch that began before the write may bring back what the
            // BMC held then. Its result is stored under the old generation
            // and never served from the store, but a request arriving now
            // must not join it either: forget it so the next request starts
            // a fetch of its own. The old task still finishes for its waiters
            // and removes only its own entry.
            lock(&self.in_flight).retain(|key, _| !(key.bmc == bmc && key.class == class.name));
            // The generation already hides the entries; dropping them too
            // frees their weight for live ones instead of letting dead bodies
            // sit out their lifetime.
            let class_name = class.name.clone();
            if let Err(error) = self
                .entries
                .invalidate_entries_if(move |key, _| key.bmc == bmc && key.class == class_name)
            {
                tracing::warn!(error = %error, "could not drop invalidated cache entries");
            }
            emit(CacheInvalidated {
                class: class.name.clone(),
                bmc_ip_address: bmc.to_string(),
            });
        }
    }
}

/// The headers a stored entry keeps. `Set-Cookie` addresses the one caller
/// the BMC answered; the store is shared by every caller, so it is dropped.
fn storable_headers(mut headers: HeaderMap) -> HeaderMap {
    headers.remove(header::SET_COOKIE);
    headers
}

/// One semaphore per BMC, created on first use with `permits`.
fn per_bmc_semaphore(
    map: &Mutex<HashMap<IpAddr, Arc<Semaphore>>>,
    bmc: IpAddr,
    permits: usize,
) -> Arc<Semaphore> {
    Arc::clone(
        lock(map)
            .entry(bmc)
            .or_insert_with(|| Arc::new(Semaphore::new(permits))),
    )
}

/// Whether the BMC forbade a shared cache to hold this response, with
/// `Cache-Control: no-store` or `private`, or asked for validation on every
/// reuse with `no-cache`, which the store does not do within `ttl`.
fn forbids_shared_storage(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|directive| {
            directive
                .split('=')
                .next()
                .unwrap_or(directive)
                .trim()
                .to_ascii_lowercase()
        })
        .any(|directive| matches!(directive.as_str(), "no-store" | "private" | "no-cache"))
}

/// Whether the BMC applied a transfer encoding the store cannot hand to a
/// caller that did not ask for it. The fetch requests `identity`, so this
/// only catches a BMC that ignores that.
fn is_encoded(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
}

/// A client-error status that says the resource is gone, as opposed to one
/// about the request, its credentials, or its rate, which a stored response
/// may outlive.
fn is_definitive_rejection(status: StatusCode) -> bool {
    matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE)
}

/// The stored headers after a `304`: each header the `304` carries replaces
/// the stored one of that name, as RFC 9111 requires, and the rest stay.
fn merge_validated_headers(previous: &HeaderMap, fresh: &HeaderMap) -> HeaderMap {
    let mut merged = previous.clone();
    for name in fresh.keys() {
        merged.remove(name);
        for value in fresh.get_all(name) {
            merged.append(name.clone(), value.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async, value_scenarios};
    use figment::providers::{Format, Toml};
    use tokio::sync::Notify;

    use super::*;

    const BMC: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8));
    const OTHER_BMC: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
    const MAX_BYTES: u64 = 16 * 1024 * 1024;

    /// Two cached classes: `inventory` is dropped only by firmware-update
    /// posts and held off for a while after one, `catalog` by any write.
    const CLASSES: &str = r#"
        [[class]]
        name = "inventory"
        match = ["GET /redfish/v1/UpdateService/FirmwareInventory/**"]
        [class.cache]
        ttl = "1h"
        stale_while_revalidate = "2h"
        stale_if_error = "6h"
        invalidated_by = ["POST /redfish/v1/UpdateService/**"]
        hold_after_write = "30m"

        [[class]]
        name = "catalog"
        match = ["GET /redfish/v1/Systems/*/Processors/**"]
        cache = { ttl = "5m" }
    "#;

    #[derive(Deserialize)]
    struct MockConfig {
        #[serde(rename = "class")]
        classes: ClassTable,
    }

    fn classes() -> ClassTable {
        figment::Figment::new()
            .merge(Toml::string(CLASSES))
            .extract::<MockConfig>()
            .expect("test classes parse")
            .classes
    }

    fn class(table: &ClassTable, name: &str) -> Arc<RequestClass> {
        table
            .cached_classes()
            .find(|class| class.name.as_str() == name)
            .cloned()
            .expect("a cached class of that name")
    }

    fn key(bmc: IpAddr, class: &RequestClass, path: &str) -> CacheKey {
        CacheKey::new(
            bmc,
            class.name.clone(),
            &path.parse::<PathAndQuery>().expect("valid path"),
        )
    }

    fn ok_reply(body: &'static str, etag: Option<&'static str>) -> UpstreamReply {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if let Some(etag) = etag {
            headers.insert(header::ETAG, HeaderValue::from_static(etag));
        }
        UpstreamReply::Response {
            status: StatusCode::OK,
            headers,
            body: Bytes::from_static(body.as_bytes()),
        }
    }

    fn status_reply(status: StatusCode) -> UpstreamReply {
        UpstreamReply::Response {
            status,
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }

    fn body_of(outcome: &FetchOutcome) -> Option<String> {
        match outcome {
            FetchOutcome::Fetched(entry) | FetchOutcome::Revalidated(entry) => {
                Some(String::from_utf8_lossy(&entry.body).into_owned())
            }
            FetchOutcome::Passthrough { body, .. } => {
                Some(String::from_utf8_lossy(body).into_owned())
            }
            FetchOutcome::TooLarge | FetchOutcome::Failed { .. } => None,
        }
    }

    async fn store(cache: &Arc<ResponseCache>, class: &Arc<RequestClass>, key: &CacheKey) {
        let (outcome, _) = cache
            .fetch(key.clone(), class, None, || {
                |_| async { ok_reply(r#"{"Members":[]}"#, Some("\"v1\"")) }
            })
            .await;
        assert!(matches!(outcome, FetchOutcome::Fetched(_)));
    }

    /// A write that lands while a fetch is in flight leaves the class held,
    /// and what that fetch brings back is handed to its waiters but never
    /// stored, or the pre-write body would surface once the hold ends.
    #[tokio::test]
    async fn store_is_skipped_while_the_class_is_held() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        let write_landed = Arc::new(Notify::new());
        let fetch_started = Arc::new(Notify::new());

        let fetch = {
            let cache = Arc::clone(&cache);
            let inventory = Arc::clone(&inventory);
            let key = key.clone();
            let write_landed = Arc::clone(&write_landed);
            let fetch_started = Arc::clone(&fetch_started);
            tokio::spawn(async move {
                cache
                    .fetch(key, &inventory, None, move || {
                        move |_| async move {
                            fetch_started.notify_one();
                            write_landed.notified().await;
                            ok_reply("pre-update", None)
                        }
                    })
                    .await
            })
        };
        fetch_started.notified().await;
        cache.invalidate_for_write(
            BMC,
            &Method::POST,
            "/redfish/v1/UpdateService/update-multipart",
            &table,
            Instant::now(),
        );
        write_landed.notify_one();

        let (outcome, _) = fetch.await.expect("fetch completes");
        assert_eq!(
            body_of(&outcome).as_deref(),
            Some("pre-update"),
            "the waiter still receives the body"
        );
        assert!(
            cache.lookup(&key).await.is_none(),
            "nothing is stored while the class is held"
        );
    }

    /// Invalidation drops the entries themselves, not just their validity,
    /// so dead bodies do not sit out their lifetime in the weight budget.
    #[tokio::test]
    async fn invalidation_drops_entries_from_the_store() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let catalog = class(&table, "catalog");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        store(
            &cache,
            &inventory,
            &key(
                BMC,
                &inventory,
                "/redfish/v1/UpdateService/FirmwareInventory",
            ),
        )
        .await;
        store(
            &cache,
            &catalog,
            &key(BMC, &catalog, "/redfish/v1/Systems/S/Processors"),
        )
        .await;
        store(
            &cache,
            &catalog,
            &key(OTHER_BMC, &catalog, "/redfish/v1/Systems/S/Processors"),
        )
        .await;
        cache.entries.run_pending_tasks().await;
        assert_eq!(cache.entries.entry_count(), 3);

        cache.invalidate_for_write(
            BMC,
            &Method::PATCH,
            "/redfish/v1/Chassis/X/EnvironmentMetrics",
            &table,
            Instant::now(),
        );
        cache.entries.run_pending_tasks().await;
        assert_eq!(
            cache.entries.entry_count(),
            2,
            "only the written BMC's catalog entry is dropped"
        );
    }

    #[test]
    fn cache_keys_normalize_a_trailing_slash_and_keep_the_query() {
        let table = classes();
        let inventory = class(&table, "inventory");
        value_scenarios!(
            run = |path: &str| key(BMC, &inventory, path).path_and_query;
            "paths" {
                "/redfish/v1/Systems/" => "/redfish/v1/Systems".to_string(),
                "/redfish/v1/Systems" => "/redfish/v1/Systems".to_string(),
                "/" => "/".to_string(),
                "/redfish/v1/Systems?$expand=*" => "/redfish/v1/Systems?$expand=*".to_string(),
            }
        );
    }

    /// Two cached classes may match one path; their entries must not share
    /// a slot, or each class's invalidation would evict the other's entry.
    #[tokio::test]
    async fn entries_are_kept_per_class() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let catalog = class(&table, "catalog");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let path = "/redfish/v1/UpdateService/FirmwareInventory";
        store(&cache, &inventory, &key(BMC, &inventory, path)).await;
        store(&cache, &catalog, &key(BMC, &catalog, path)).await;

        // A write that drops catalog entries leaves the inventory entry.
        cache.invalidate_for_write(
            BMC,
            &Method::PATCH,
            "/redfish/v1/Chassis/X/EnvironmentMetrics",
            &table,
            Instant::now(),
        );
        assert!(cache.lookup(&key(BMC, &inventory, path)).await.is_some());
        assert!(cache.lookup(&key(BMC, &catalog, path)).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn freshness_follows_the_policy_windows() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let policy = inventory.cache.as_ref().expect("inventory is cached");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        store(&cache, &inventory, &key).await;
        let entry = cache.lookup(&key).await.expect("the stored entry");

        value_scenarios!(
            run = |age: Duration| {
                let now = entry.stored_at + age;
                (entry.freshness(policy, now), entry.usable_on_error(policy, now))
            };
            "within ttl" {
                Duration::ZERO => (Freshness::Fresh, true),
                Duration::from_secs(3599) => (Freshness::Fresh, true),
            }

            "within stale-while-revalidate" {
                Duration::from_secs(3600) => (Freshness::Stale, true),
                Duration::from_secs(3 * 3600 - 1) => (Freshness::Stale, true),
            }

            "past stale-while-revalidate but within stale-if-error" {
                Duration::from_secs(3 * 3600) => (Freshness::Expired, true),
                Duration::from_secs(7 * 3600 - 1) => (Freshness::Expired, true),
            }

            "past every window" {
                Duration::from_secs(7 * 3600) => (Freshness::Expired, false),
            }
        );
    }

    /// Concurrent misses for one key make one upstream request; every
    /// waiter receives what it stored, and only the first started it.
    #[tokio::test]
    async fn concurrent_misses_share_one_fetch() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let built = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());

        let mut waiters = Vec::new();
        for _ in 0..4 {
            let cache = Arc::clone(&cache);
            let inventory = Arc::clone(&inventory);
            let key = key.clone();
            let calls = Arc::clone(&calls);
            let built = Arc::clone(&built);
            let release = Arc::clone(&release);
            waiters.push(tokio::spawn(async move {
                cache
                    .fetch(key, &inventory, None, move || {
                        built.fetch_add(1, Ordering::SeqCst);
                        move |_| async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            release.notified().await;
                            ok_reply("shared", None)
                        }
                    })
                    .await
            }));
        }
        // Let every waiter reach the in-flight map before the fetch answers.
        tokio::task::yield_now().await;
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        release.notify_waiters();
        release.notify_one();

        let mut started = 0;
        for waiter in waiters {
            let (outcome, joined) = waiter.await.expect("waiter completes");
            assert_eq!(body_of(&outcome).as_deref(), Some("shared"));
            if !joined {
                started += 1;
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one upstream fetch");
        assert_eq!(started, 1, "exactly one waiter started the fetch");
        assert_eq!(
            built.load(Ordering::SeqCst),
            1,
            "joiners never build a fetch they would not run"
        );
        assert!(
            cache.in_flight.lock().unwrap().is_empty(),
            "the fetch leaves the in-flight map once published"
        );
    }

    /// A caller that stops waiting does not cancel the fetch it started;
    /// the entry still lands for the next caller.
    #[tokio::test]
    async fn fetch_outlives_a_caller_that_gave_up() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        let release = Arc::new(Notify::new());
        let started = Arc::new(Notify::new());

        let caller = {
            let cache = Arc::clone(&cache);
            let inventory = Arc::clone(&inventory);
            let key = key.clone();
            let release = Arc::clone(&release);
            let started = Arc::clone(&started);
            tokio::spawn(async move {
                cache
                    .fetch(key, &inventory, None, move || {
                        move |_| async move {
                            started.notify_one();
                            release.notified().await;
                            ok_reply("late", None)
                        }
                    })
                    .await
            })
        };
        started.notified().await;
        caller.abort();
        assert!(caller.await.is_err(), "the caller was aborted");

        release.notify_one();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(entry) = cache.lookup(&key).await {
                assert_eq!(String::from_utf8_lossy(&entry.body), "late");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the detached fetch stores its result"
            );
            tokio::task::yield_now().await;
        }
    }

    /// A request that arrives after a write must not join a fetch that
    /// began before it: the write forgets the in-flight fetch, the next
    /// request starts its own, and the old fetch's end removes only its own
    /// entry.
    #[tokio::test]
    async fn a_write_supersedes_fetches_that_began_before_it() {
        let table = classes();
        let catalog = class(&table, "catalog");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(BMC, &catalog, "/redfish/v1/Systems/S/Processors");

        let start_fetch = |body: &'static str| {
            let cache = Arc::clone(&cache);
            let catalog = Arc::clone(&catalog);
            let key = key.clone();
            let started = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let handle = {
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                tokio::spawn(async move {
                    cache
                        .fetch(key, &catalog, None, move || {
                            move |_| async move {
                                started.notify_one();
                                release.notified().await;
                                ok_reply(body, None)
                            }
                        })
                        .await
                })
            };
            (handle, started, release)
        };

        let (before, before_started, before_release) = start_fetch("pre-write");
        before_started.notified().await;

        cache.invalidate_for_write(
            BMC,
            &Method::PATCH,
            "/redfish/v1/Systems/S/Actions/ComputerSystem.Reset",
            &table,
            Instant::now(),
        );

        let (after, after_started, after_release) = start_fetch("post-write");
        after_started.notified().await;

        before_release.notify_one();
        let (outcome, joined) = before.await.expect("the older fetch completes");
        assert!(!joined);
        assert_eq!(body_of(&outcome).as_deref(), Some("pre-write"));
        assert!(
            cache.in_flight.lock().unwrap().contains_key(&key),
            "the older fetch's end leaves the newer fetch in flight"
        );

        after_release.notify_one();
        let (outcome, joined) = after.await.expect("the newer fetch completes");
        assert!(!joined, "the request after the write started its own fetch");
        assert_eq!(body_of(&outcome).as_deref(), Some("post-write"));
        assert!(cache.in_flight.lock().unwrap().is_empty());
        let stored = cache.lookup(&key).await.expect("the newer fetch's entry");
        assert_eq!(String::from_utf8_lossy(&stored.body), "post-write");
    }

    /// A fetch that panics must not leave its key wedged: the next fetch
    /// for the key runs instead of waiting on a dead receiver.
    #[tokio::test]
    async fn panicking_fetch_releases_the_in_flight_slot() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );

        let (outcome, _) = cache
            .fetch(key.clone(), &inventory, None, || {
                |_| async { panic!("fetch exploded") }
            })
            .await;
        assert!(
            matches!(outcome, FetchOutcome::Failed { .. }),
            "the waiting caller sees a failure"
        );
        assert!(
            cache.in_flight.lock().unwrap().is_empty(),
            "the guard removed the key"
        );

        let (outcome, joined) = cache
            .fetch(key.clone(), &inventory, None, || {
                |_| async { ok_reply("recovered", None) }
            })
            .await;
        assert!(!joined, "nothing was left to join");
        assert_eq!(body_of(&outcome).as_deref(), Some("recovered"));
    }

    /// The store is shared by every caller, so a cookie the BMC set for the
    /// caller whose request fetched the entry is not kept with it.
    #[tokio::test]
    async fn stored_entries_drop_set_cookie() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        let (outcome, _) = cache
            .fetch(key.clone(), &inventory, None, || {
                |_| async {
                    let mut headers = HeaderMap::new();
                    headers.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    headers.append(header::SET_COOKIE, HeaderValue::from_static("sid=abc"));
                    UpstreamReply::Response {
                        status: StatusCode::OK,
                        headers,
                        body: Bytes::from_static(b"{}"),
                    }
                }
            })
            .await;
        assert!(matches!(outcome, FetchOutcome::Fetched(_)));
        let entry = cache.lookup(&key).await.expect("the stored entry");
        assert!(entry.headers.contains_key(header::CONTENT_TYPE));
        assert!(!entry.headers.contains_key(header::SET_COOKIE));
    }

    /// A refresh sends the stored `ETag`; a `304` keeps the body, adopts
    /// the headers the `304` carries, and restarts the entry's age.
    #[tokio::test(start_paused = true)]
    async fn revalidation_keeps_the_body_on_not_modified() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        store(&cache, &inventory, &key).await;
        let first = cache.lookup(&key).await.expect("the stored entry");

        tokio::time::advance(Duration::from_secs(3601)).await;
        let (outcome, _) = cache
            .fetch(key.clone(), &inventory, Some(Arc::clone(&first)), || {
                |request: FetchRequest| async move {
                    assert_eq!(
                        request.if_none_match.as_ref().and_then(|v| v.to_str().ok()),
                        Some("\"v1\""),
                        "the stored ETag makes the fetch conditional"
                    );
                    let mut headers = HeaderMap::new();
                    headers.insert(header::DATE, HeaderValue::from_static("later"));
                    headers.insert(header::ETAG, HeaderValue::from_static("W/\"v1\""));
                    UpstreamReply::Response {
                        status: StatusCode::NOT_MODIFIED,
                        headers,
                        body: Bytes::new(),
                    }
                }
            })
            .await;

        let FetchOutcome::Revalidated(entry) = outcome else {
            panic!("a 304 to a conditional fetch revalidates");
        };
        assert_eq!(entry.body, first.body);
        assert_eq!(
            entry.etag().and_then(|v| v.to_str().ok()),
            Some("W/\"v1\""),
            "the ETag the 304 carries becomes the one matched and served"
        );
        assert_eq!(
            entry.headers.get(header::CONTENT_TYPE),
            first.headers.get(header::CONTENT_TYPE),
            "headers the 304 omits are kept"
        );
        assert_eq!(
            entry
                .headers
                .get(header::DATE)
                .and_then(|v| v.to_str().ok()),
            Some("later"),
            "headers the 304 carries replace the stored ones"
        );
        assert!(entry.stored_at > first.stored_at, "age restarts");
        assert_eq!(
            entry.freshness(inventory.cache.as_ref().unwrap(), Instant::now()),
            Freshness::Fresh
        );
    }

    struct RefreshInput {
        reply: fn() -> UpstreamReply,
        prior: bool,
    }

    /// What a fetch does to the store: (an entry is present afterwards,
    /// the key is held off after a failure).
    async fn after_refresh(input: RefreshInput) -> (bool, bool) {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        let existing = if input.prior {
            store(&cache, &inventory, &key).await;
            cache.lookup(&key).await
        } else {
            None
        };
        let reply = input.reply;
        let _ = cache
            .fetch(key.clone(), &inventory, existing, || {
                move |_| async move { reply() }
            })
            .await;
        (
            cache.lookup(&key).await.is_some(),
            cache.hold_off_reason(&key, Instant::now()).await.is_some(),
        )
    }

    #[tokio::test]
    async fn refresh_outcomes_update_the_store() {
        check_cases_async(
            [
                Case {
                    scenario: "a 200 is stored",
                    input: RefreshInput {
                        reply: || ok_reply("present", None),
                        prior: false,
                    },
                    expect: Yields((true, false)),
                },
                Case {
                    scenario: "a gzip-encoded 200 is not stored and holds the key off",
                    input: RefreshInput {
                        reply: || {
                            let mut headers = HeaderMap::new();
                            headers
                                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                            UpstreamReply::Response {
                                status: StatusCode::OK,
                                headers,
                                body: Bytes::from_static(b"zipped"),
                            }
                        },
                        prior: false,
                    },
                    expect: Yields((false, true)),
                },
                Case {
                    scenario: "an encoded 200 on refresh drops the stale entry",
                    input: RefreshInput {
                        reply: || {
                            let mut headers = HeaderMap::new();
                            headers
                                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                            UpstreamReply::Response {
                                status: StatusCode::OK,
                                headers,
                                body: Bytes::from_static(b"zipped"),
                            }
                        },
                        prior: true,
                    },
                    expect: Yields((false, true)),
                },
                Case {
                    scenario: "a private 200 on refresh drops the stale entry",
                    input: RefreshInput {
                        reply: || {
                            let mut headers = HeaderMap::new();
                            headers
                                .insert(header::CACHE_CONTROL, HeaderValue::from_static("private"));
                            UpstreamReply::Response {
                                status: StatusCode::OK,
                                headers,
                                body: Bytes::from_static(b"mine"),
                            }
                        },
                        prior: true,
                    },
                    expect: Yields((false, true)),
                },
                Case {
                    scenario: "a 200 the BMC marks private is not stored and holds the key off",
                    input: RefreshInput {
                        reply: || {
                            let mut headers = HeaderMap::new();
                            headers.insert(
                                header::CACHE_CONTROL,
                                HeaderValue::from_static("private, max-age=60"),
                            );
                            UpstreamReply::Response {
                                status: StatusCode::OK,
                                headers,
                                body: Bytes::from_static(b"mine"),
                            }
                        },
                        prior: false,
                    },
                    expect: Yields((false, true)),
                },
                Case {
                    scenario: "a 304 that now says no-store drops the entry",
                    input: RefreshInput {
                        reply: || {
                            let mut headers = HeaderMap::new();
                            headers.insert(
                                header::CACHE_CONTROL,
                                HeaderValue::from_static("no-store"),
                            );
                            UpstreamReply::Response {
                                status: StatusCode::NOT_MODIFIED,
                                headers,
                                body: Bytes::new(),
                            }
                        },
                        prior: true,
                    },
                    expect: Yields((false, true)),
                },
                Case {
                    scenario: "a 404 on refresh evicts the stale entry",
                    input: RefreshInput {
                        reply: || status_reply(StatusCode::NOT_FOUND),
                        prior: true,
                    },
                    expect: Yields((false, false)),
                },
                Case {
                    scenario: "a 403 on refresh keeps the entry and holds the key off",
                    input: RefreshInput {
                        reply: || status_reply(StatusCode::FORBIDDEN),
                        prior: true,
                    },
                    expect: Yields((true, true)),
                },
                Case {
                    scenario: "a 406 to the cache's own Accept keeps the entry",
                    input: RefreshInput {
                        reply: || status_reply(StatusCode::NOT_ACCEPTABLE),
                        prior: true,
                    },
                    expect: Yields((true, true)),
                },
                Case {
                    scenario: "a 503 on refresh keeps the entry and holds the key off",
                    input: RefreshInput {
                        reply: || status_reply(StatusCode::SERVICE_UNAVAILABLE),
                        prior: true,
                    },
                    expect: Yields((true, true)),
                },
                Case {
                    scenario: "a failed forward keeps the entry and holds the key off",
                    input: RefreshInput {
                        reply: || UpstreamReply::Failed {
                            status: StatusCode::BAD_GATEWAY,
                            message: "timed out".to_string(),
                        },
                        prior: true,
                    },
                    expect: Yields((true, true)),
                },
                Case {
                    scenario: "an unconditional 304 stores nothing",
                    input: RefreshInput {
                        reply: || status_reply(StatusCode::NOT_MODIFIED),
                        prior: false,
                    },
                    expect: Yields((false, true)),
                },
            ],
            |input| async { Ok::<_, Infallible>(after_refresh(input).await) },
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn hold_off_expires_and_keeps_its_reason() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        let now = Instant::now();
        cache
            .record_hold_off(&key, HoldOffReason::Unstorable, now)
            .await;
        assert_eq!(
            cache
                .hold_off_reason(
                    &key,
                    now + UNSTORABLE_FETCH_HOLD_OFF - Duration::from_secs(1)
                )
                .await,
            Some(HoldOffReason::Unstorable)
        );
        assert_eq!(
            cache
                .hold_off_reason(&key, now + UNSTORABLE_FETCH_HOLD_OFF)
                .await,
            None
        );
    }

    /// However many distinct resources are asked for at once, a BMC sees at
    /// most [`MAX_FETCHES_PER_BMC`] fetches in flight; the rest queue.
    #[tokio::test]
    async fn fetches_per_bmc_are_bounded() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let started = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut waiters = Vec::new();
        for index in 0..(MAX_FETCHES_PER_BMC + 2) {
            let cache = Arc::clone(&cache);
            let inventory = Arc::clone(&inventory);
            let key = key(
                BMC,
                &inventory,
                &format!("/redfish/v1/UpdateService/FirmwareInventory/FW_{index}"),
            );
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            waiters.push(tokio::spawn(async move {
                cache
                    .fetch(key, &inventory, None, move || {
                        move |_| async move {
                            started.fetch_add(1, Ordering::SeqCst);
                            while !release.load(Ordering::SeqCst) {
                                tokio::task::yield_now().await;
                            }
                            ok_reply("bounded", None)
                        }
                    })
                    .await
            }));
        }
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            started.load(Ordering::SeqCst),
            MAX_FETCHES_PER_BMC,
            "only the permitted number of fetches reach the BMC at once"
        );

        release.store(true, Ordering::SeqCst);
        for waiter in waiters {
            let (outcome, _) = waiter.await.expect("waiter completes");
            assert_eq!(body_of(&outcome).as_deref(), Some("bounded"));
        }
        assert_eq!(started.load(Ordering::SeqCst), MAX_FETCHES_PER_BMC + 2);
    }

    /// Past the pending bound a fetch is refused at once instead of queuing a
    /// task that would reach the BMC after its caller left.
    #[tokio::test]
    async fn pending_fetches_per_bmc_are_bounded() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut waiters = Vec::new();
        for index in 0..MAX_PENDING_FETCHES_PER_BMC {
            let cache = Arc::clone(&cache);
            let inventory = Arc::clone(&inventory);
            let key = key(
                BMC,
                &inventory,
                &format!("/redfish/v1/UpdateService/FirmwareInventory/FW_{index}"),
            );
            let release = Arc::clone(&release);
            waiters.push(tokio::spawn(async move {
                cache
                    .fetch(key, &inventory, None, move || {
                        move |_| async move {
                            while !release.load(Ordering::SeqCst) {
                                tokio::task::yield_now().await;
                            }
                            ok_reply("admitted", None)
                        }
                    })
                    .await
            }));
        }
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        let refused_key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory/FW_x",
        );
        let (outcome, joined) = tokio::time::timeout(
            Duration::from_secs(1),
            cache.fetch(refused_key.clone(), &inventory, None, || {
                |_| async { ok_reply("never", None) }
            }),
        )
        .await
        .expect("a refused fetch answers at once");
        assert!(!joined);
        assert!(
            matches!(
                outcome,
                FetchOutcome::Failed {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    ..
                }
            ),
            "the fetch past the bound is refused"
        );
        assert!(
            !cache.in_flight.lock().unwrap().contains_key(&refused_key),
            "nothing was spawned for it"
        );

        release.store(true, Ordering::SeqCst);
        for waiter in waiters {
            let (outcome, _) = waiter.await.expect("waiter completes");
            assert_eq!(body_of(&outcome).as_deref(), Some("admitted"));
        }
    }

    /// A fetch that cannot get a slot within the class budget fails instead
    /// of waiting indefinitely; nothing reaches the BMC for it.
    #[tokio::test(start_paused = true)]
    async fn waiting_for_a_fetch_slot_is_bounded_by_the_class_budget() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let release = Arc::new(Notify::new());
        let ran = Arc::new(AtomicUsize::new(0));

        let mut holders = Vec::new();
        for index in 0..MAX_FETCHES_PER_BMC {
            let cache = Arc::clone(&cache);
            let inventory = Arc::clone(&inventory);
            let key = key(
                BMC,
                &inventory,
                &format!("/redfish/v1/UpdateService/FirmwareInventory/FW_{index}"),
            );
            let release = Arc::clone(&release);
            let ran = Arc::clone(&ran);
            holders.push(tokio::spawn(async move {
                cache
                    .fetch(key, &inventory, None, move || {
                        move |_| async move {
                            ran.fetch_add(1, Ordering::SeqCst);
                            release.notified().await;
                            ok_reply("held", None)
                        }
                    })
                    .await
            }));
        }
        while ran.load(Ordering::SeqCst) < MAX_FETCHES_PER_BMC {
            tokio::task::yield_now().await;
        }

        let waiting = {
            let cache = Arc::clone(&cache);
            let inventory = Arc::clone(&inventory);
            let key = key(
                BMC,
                &inventory,
                "/redfish/v1/UpdateService/FirmwareInventory/FW_late",
            );
            let ran = Arc::clone(&ran);
            tokio::spawn(async move {
                cache
                    .fetch(key, &inventory, None, move || {
                        move |_| async move {
                            ran.fetch_add(1, Ordering::SeqCst);
                            ok_reply("late", None)
                        }
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        tokio::time::advance(inventory.upstream_timeout + Duration::from_secs(1)).await;

        let (outcome, _) = waiting.await.expect("the waiting fetch completes");
        assert!(
            matches!(
                outcome,
                FetchOutcome::Failed {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    ..
                }
            ),
            "the fetch gave up waiting for a slot"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            MAX_FETCHES_PER_BMC,
            "its request never reached the BMC"
        );

        release.notify_waiters();
        for holder in holders {
            holder.await.expect("holder completes");
        }
    }

    #[test]
    fn only_redfish_query_parameters_are_cacheable() {
        value_scenarios!(
            run = |path: &str| is_cacheable_query(&path.parse::<PathAndQuery>().expect("valid"));
            "no query or Redfish queries" {
                "/redfish/v1/Systems" => true,
                "/redfish/v1/Systems?$expand=*($levels=1)" => true,
                "/redfish/v1/Systems?$select=Id,Name&$top=5" => true,
                "/redfish/v1/Chassis/X/Sensors?excerpt" => true,
            }

            "caller-minted parameters" {
                "/redfish/v1/Systems?x=1" => false,
                "/redfish/v1/Systems?$expand=*&nocache=7" => false,
            }
        );
    }

    struct WriteInput {
        method: Method,
        path: &'static str,
        bmc: IpAddr,
    }

    /// Which entries survive a write and whether inventory is held:
    /// (inventory on BMC present, catalog on BMC present, inventory held).
    async fn survivors(input: WriteInput) -> (bool, bool, bool) {
        let table = classes();
        let inventory = class(&table, "inventory");
        let catalog = class(&table, "catalog");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let inventory_key = key(
            BMC,
            &inventory,
            "/redfish/v1/UpdateService/FirmwareInventory",
        );
        let catalog_key = key(BMC, &catalog, "/redfish/v1/Systems/System_0/Processors");
        store(&cache, &inventory, &inventory_key).await;
        store(&cache, &catalog, &catalog_key).await;

        let now = Instant::now();
        cache.invalidate_for_write(input.bmc, &input.method, input.path, &table, now);

        (
            cache.lookup(&inventory_key).await.is_some(),
            cache.lookup(&catalog_key).await.is_some(),
            cache.is_held(BMC, &inventory.name, now),
        )
    }

    #[tokio::test]
    async fn writes_invalidate_by_policy_and_bmc() {
        check_cases_async(
            [
                Case {
                    scenario: "a power write drops only classes without patterns",
                    input: WriteInput {
                        method: Method::PATCH,
                        path: "/redfish/v1/Chassis/HGX_Chassis_0/EnvironmentMetrics",
                        bmc: BMC,
                    },
                    expect: Yields((true, false, false)),
                },
                Case {
                    scenario: "a firmware post drops both and holds inventory",
                    input: WriteInput {
                        method: Method::POST,
                        path: "/redfish/v1/UpdateService/update-multipart",
                        bmc: BMC,
                    },
                    expect: Yields((false, false, true)),
                },
                Case {
                    scenario: "a write to another BMC drops nothing",
                    input: WriteInput {
                        method: Method::POST,
                        path: "/redfish/v1/UpdateService/update-multipart",
                        bmc: OTHER_BMC,
                    },
                    expect: Yields((true, true, false)),
                },
            ],
            |input| async { Ok::<_, Infallible>(survivors(input).await) },
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn write_hold_expires_with_its_window() {
        let table = classes();
        let inventory = class(&table, "inventory");
        let cache = Arc::new(ResponseCache::new(MAX_BYTES));
        let now = Instant::now();
        cache.invalidate_for_write(
            BMC,
            &Method::POST,
            "/redfish/v1/UpdateService/update-multipart",
            &table,
            now,
        );
        assert!(cache.is_held(BMC, &inventory.name, now + Duration::from_secs(29 * 60)));
        assert!(!cache.is_held(BMC, &inventory.name, now + Duration::from_secs(30 * 60)));
        assert!(
            !cache.is_held(OTHER_BMC, &inventory.name, now),
            "holds are per BMC"
        );
    }
}
