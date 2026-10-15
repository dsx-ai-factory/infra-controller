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

//! Which controller source simulates which rack.
//!
//! Every machine-a-tron instance publishes the racks it simulates on `/racks/status`. The gateway
//! polls it on every source, folds the answers into one table and routes RMS requests by it: a
//! rack goes to the single source that reports it and nowhere else.
//!
//! - A rack reported by more than one source is a conflict. It has no owner and ownership is not
//!   ready until one of the sources stops reporting it.
//! - A source whose polls fail keeps its last answer for `ownership.stale_after` and is then
//!   dropped: requests for its racks are answered as unavailable, naming the source, and are never
//!   moved to another source.
//! - A source that has not answered since the gateway started blocks readiness for the same
//!   `stale_after`, measured from its first failed poll, and is then dropped like any other so one
//!   dead instance cannot hold the rest of the fleet at not-ready.
//! - A rack no source has ever reported is unknown.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use eyre::WrapErr;
use metrics_endpoint::HealthController;
use serde::Deserialize;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::config::{OwnershipConfig, SourceClientConfig};
use crate::health::WAITING_FOR_SOURCE_LIST;
use crate::sources::{Source, SourceList};

/// The controller's name for one machine-a-tron instance, [`Source::name`].
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SourceId(pub(crate) String);

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<&Source> for SourceId {
    fn from(source: &Source) -> Self {
        Self(source.name.clone())
    }
}

/// What a rack lookup found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Owner {
    /// Exactly one source whose answer is current or stale reports the rack.
    Source(SourceId),
    /// The named source reported the rack before its polls failed for `stale_after`. Requests for
    /// it should be retried, not redirected.
    Dropped(SourceId),
    /// Several sources report the rack; routing to any of them would be a guess.
    Ambiguous(Vec<SourceId>),
    /// No source has ever reported the rack.
    Unknown,
}

/// Routing decisions and readiness shared by the RMS proxy and `/readyz`.
pub(crate) trait Ownership: Send + Sync {
    /// Who reports `rack_id`; see [`Owner`].
    fn owner_of_rack(&self, rack_id: &str) -> Owner;
    /// True once every source has answered or been dropped and no conflict is open.
    fn is_ready(&self) -> bool;
    /// Why [`Self::is_ready`] is false, one reason per `/readyz` line; empty when it is true.
    fn blockers(&self) -> Vec<String>;
}

/// A rack reported by more than one source.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Conflict {
    rack_id: String,
    /// The claiming sources, in name order.
    sources: Vec<SourceId>,
}

impl fmt::Display for Conflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "rack {} is reported by ", self.rack_id)?;
        for (index, source) in self.sources.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            source.fmt(formatter)?;
        }
        Ok(())
    }
}

/// Poll state of one source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceState {
    /// No successful poll yet; blocks readiness until it answers or has failed for `stale_after`.
    Pending,
    /// The last poll succeeded.
    Fresh,
    /// Polls are failing but the last answer is younger than `stale_after` and still routes.
    Stale,
    /// Polls have failed for `stale_after`; the source is out of routing and readiness, its last
    /// answer kept so its racks can be told from unknown ones.
    Dropped,
}

impl fmt::Display for SourceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pending => "pending",
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Dropped => "dropped",
        })
    }
}

#[derive(Clone, Debug)]
struct SourceRecord {
    state: SourceState,
    racks: BTreeSet<String>,
    last_success: Option<Instant>,
    /// First failed poll since the last success, or since start for a source that never answered.
    first_failure: Option<Instant>,
    last_error: Option<String>,
}

impl SourceRecord {
    fn routes(&self) -> bool {
        matches!(self.state, SourceState::Fresh | SourceState::Stale)
    }
}

/// The ownership table proper: per-source answers and the rack map derived from them.
///
/// This is the synchronous core behind [`OwnershipMap`]. It takes the clock as an argument so
/// staleness can be tested without waiting.
#[derive(Clone, Debug)]
struct OwnershipTable {
    stale_after: Duration,
    sources: BTreeMap<SourceId, SourceRecord>,
    racks: HashMap<String, SourceId>,
    /// Racks of dropped sources, consulted only when `racks` misses.
    dropped_racks: HashMap<String, SourceId>,
    conflicts: Vec<Conflict>,
}

impl OwnershipTable {
    /// An empty table tracking `sources`; every source starts [`SourceState::Pending`].
    fn new(sources: impl IntoIterator<Item = SourceId>, stale_after: Duration) -> Self {
        Self {
            stale_after,
            sources: sources
                .into_iter()
                .map(|source| {
                    (
                        source,
                        SourceRecord {
                            state: SourceState::Pending,
                            racks: BTreeSet::new(),
                            last_success: None,
                            first_failure: None,
                            last_error: None,
                        },
                    )
                })
                .collect(),
            racks: HashMap::new(),
            dropped_racks: HashMap::new(),
            conflicts: Vec::new(),
        }
    }

    /// Replaces the racks of `source` and rebuilds the rack map.
    fn record_success(&mut self, source: &SourceId, racks: BTreeSet<String>, now: Instant) {
        let Some(record) = self.sources.get_mut(source) else {
            tracing::warn!(%source, "Ignoring rack status from a source outside the startup list");
            return;
        };
        if record.state != SourceState::Fresh {
            tracing::info!(
                %source,
                previous_state = %record.state,
                rack_count = racks.len(),
                "Source reports its racks"
            );
        }
        record.state = SourceState::Fresh;
        record.racks = racks;
        record.last_success = Some(now);
        record.first_failure = None;
        record.last_error = None;
        self.rebuild();
    }

    /// Notes a failed poll of `source` at `now`; [`Self::expire`] decides when it is dropped.
    fn record_failure(&mut self, source: &SourceId, error: &str, now: Instant) {
        let Some(record) = self.sources.get_mut(source) else {
            tracing::warn!(%source, "Ignoring a poll failure of a source outside the startup list");
            return;
        };
        let first_failure = record.first_failure.is_none();
        record.first_failure.get_or_insert(now);
        record.last_error = Some(error.to_string());
        match record.state {
            SourceState::Fresh => {
                record.state = SourceState::Stale;
                tracing::warn!(
                    %source,
                    error,
                    stale_after = ?self.stale_after,
                    "Source rack status unavailable; keeping its last answer"
                );
            }
            SourceState::Pending if first_failure => {
                tracing::warn!(
                    %source,
                    error,
                    stale_after = ?self.stale_after,
                    "Source rack status not available yet; readiness waits for it until stale_after"
                );
            }
            SourceState::Pending | SourceState::Stale | SourceState::Dropped => {
                tracing::debug!(%source, error, source_state = %record.state, "Source rack status still unavailable");
            }
        }
    }

    /// Drops every source that has been failing for `stale_after`: a stale one whose last success
    /// is that old, or a pending one whose first failure is. Returns the sources dropped now.
    fn expire(&mut self, now: Instant) -> Vec<SourceId> {
        let mut dropped = Vec::new();
        for (source, record) in &mut self.sources {
            let expired = match record.state {
                SourceState::Stale => record
                    .last_success
                    .is_none_or(|last| now.saturating_duration_since(last) >= self.stale_after),
                SourceState::Pending => record
                    .first_failure
                    .is_some_and(|first| now.saturating_duration_since(first) >= self.stale_after),
                SourceState::Fresh | SourceState::Dropped => false,
            };
            if !expired {
                continue;
            }
            tracing::error!(
                %source,
                previous_state = %record.state,
                racks = ?record.racks,
                error = record.last_error.as_deref().unwrap_or(""),
                "Source has not answered for stale_after; taking it out of routing and readiness"
            );
            record.state = SourceState::Dropped;
            dropped.push(source.clone());
        }
        if !dropped.is_empty() {
            self.rebuild();
        }
        dropped
    }

    fn rebuild(&mut self) {
        let mut claims: BTreeMap<&str, BTreeSet<&SourceId>> = BTreeMap::new();
        let mut dropped_racks = HashMap::new();
        for (source, record) in &self.sources {
            if !record.routes() {
                // Name order decides between two dropped sources; nothing is routed to either.
                for rack in &record.racks {
                    dropped_racks
                        .entry(rack.clone())
                        .or_insert_with(|| source.clone());
                }
                continue;
            }
            for rack in &record.racks {
                claims.entry(rack).or_default().insert(source);
            }
        }

        let mut racks = HashMap::with_capacity(claims.len());
        let mut conflicts = Vec::new();
        for (rack, claimants) in claims {
            if claimants.len() == 1 {
                racks.insert(rack.to_string(), (*claimants.first().unwrap()).clone());
            } else {
                conflicts.push(Conflict {
                    rack_id: rack.to_string(),
                    sources: claimants.into_iter().cloned().collect(),
                });
            }
        }
        for conflict in conflicts.iter().filter(|c| !self.conflicts.contains(c)) {
            tracing::warn!(%conflict, "Rack ownership conflict; routing to it is refused");
        }
        for resolved in self.conflicts.iter().filter(|c| !conflicts.contains(c)) {
            tracing::info!(rack_id = %resolved.rack_id, "Rack ownership conflict resolved");
        }

        self.racks = racks;
        self.dropped_racks = dropped_racks;
        self.conflicts = conflicts;
    }

    /// See [`Ownership::owner_of_rack`].
    fn owner_of_rack(&self, rack_id: &str) -> Owner {
        let rack_id = rack_id.trim();
        if let Some(source) = self.racks.get(rack_id) {
            return Owner::Source(source.clone());
        }
        if let Some(conflict) = self
            .conflicts
            .iter()
            .find(|conflict| conflict.rack_id == rack_id)
        {
            return Owner::Ambiguous(conflict.sources.clone());
        }
        if let Some(source) = self.dropped_racks.get(rack_id) {
            return Owner::Dropped(source.clone());
        }
        Owner::Unknown
    }

    /// See [`Ownership::is_ready`].
    fn is_ready(&self) -> bool {
        self.conflicts.is_empty()
            && self
                .sources
                .values()
                .all(|record| record.state != SourceState::Pending)
    }

    /// Poll state of every source, in name order.
    fn source_states(&self) -> BTreeMap<SourceId, SourceState> {
        self.sources
            .iter()
            .map(|(source, record)| (source.clone(), record.state))
            .collect()
    }

    /// See [`Ownership::blockers`].
    fn blockers(&self) -> Vec<String> {
        let mut blockers = Vec::new();
        for (source, record) in &self.sources {
            if record.state == SourceState::Pending {
                let mut blocker = format!("no rack status from source {source} yet");
                if let Some(error) = &record.last_error {
                    blocker.push_str(": ");
                    blocker.push_str(error);
                }
                blockers.push(blocker);
            }
        }
        blockers.extend(self.conflicts.iter().map(ToString::to_string));
        blockers
    }
}

/// `GET /racks/status`: `machine_a_tron::RacksStatusResponse`, rack ids only.
#[derive(Deserialize)]
struct RacksStatusBody {
    #[serde(default)]
    racks: Vec<RackBody>,
}

#[derive(Deserialize)]
struct RackBody {
    rack_id: String,
}

/// The non-blank rack ids of a `/racks/status` body.
fn parse_racks(body: &[u8]) -> Result<BTreeSet<String>, serde_json::Error> {
    let body: RacksStatusBody = serde_json::from_slice(body)?;
    Ok(body
        .racks
        .iter()
        .map(|rack| rack.rack_id.trim())
        .filter(|rack_id| !rack_id.is_empty())
        .map(str::to_owned)
        .collect())
}

/// HTTP client for the rack status of every source.
///
/// Sources share one trust configuration, as for the UFM inventory: they are replicas of the
/// same deployment presenting certificates from the same issuer.
#[derive(Clone, Debug)]
pub(crate) struct RackStatusClient {
    client: reqwest::Client,
}

impl RackStatusClient {
    /// Builds the client from the `[sources]` TLS settings and the `[ownership]` timeout.
    pub(crate) fn new(
        sources: &SourceClientConfig,
        ownership: &OwnershipConfig,
    ) -> eyre::Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(ownership.request_timeout)
            .danger_accept_invalid_certs(sources.insecure_skip_verify);
        if let Some(path) = sources.ca_cert_path.as_ref() {
            let certificate = reqwest::Certificate::from_pem(&std::fs::read(path)?)?;
            builder = builder.add_root_certificate(certificate);
        }
        Ok(Self {
            client: builder.build()?,
        })
    }

    /// Fetches the racks `source` simulates.
    async fn fetch(&self, source: &Source) -> eyre::Result<BTreeSet<String>> {
        let url = source.racks_status_url();
        let body = async {
            self.client
                .get(url.as_str())
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await
        }
        .await
        .wrap_err_with(|| format!("GET {url}"))?;
        parse_racks(&body).wrap_err_with(|| format!("invalid rack status from {url}"))
    }
}

/// Shared, concurrently readable ownership table with the poll loop that keeps it current.
///
/// Clones share one table. Reads take a read lock and copy out small values, so request handlers
/// never wait on a poll in progress.
#[derive(Clone)]
pub(crate) struct OwnershipMap {
    inner: Arc<Inner>,
}

struct Inner {
    table: RwLock<OwnershipTable>,
    sources: Vec<Source>,
    poll_interval: Duration,
}

impl fmt::Debug for OwnershipMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnershipMap")
            .field("states", &self.read().source_states())
            .field("conflicts", &self.read().conflicts)
            .finish()
    }
}

impl OwnershipMap {
    /// A map tracking every source of `sources`; nothing is polled until [`Self::poll_once`] or
    /// [`Self::spawn`] runs.
    pub(crate) fn new(sources: &SourceList, config: &OwnershipConfig) -> Self {
        let table = OwnershipTable::new(
            sources.sources.iter().map(SourceId::from),
            config.stale_after,
        );
        Self {
            inner: Arc::new(Inner {
                table: RwLock::new(table),
                sources: sources.sources.clone(),
                poll_interval: config.poll_interval,
            }),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, OwnershipTable> {
        self.inner
            .table
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, OwnershipTable> {
        self.inner
            .table
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Polls every source once, concurrently, then applies the results together and expires
    /// stale sources, so a poll cycle is observed as one change.
    pub(crate) async fn poll_once(&self, client: &RackStatusClient) {
        let mut fetches = JoinSet::new();
        for source in &self.inner.sources {
            let client = client.clone();
            let source = source.clone();
            fetches.spawn(async move {
                let result = client.fetch(&source).await;
                (SourceId::from(&source), result)
            });
        }
        let mut results = Vec::with_capacity(self.inner.sources.len());
        while let Some(joined) = fetches.join_next().await {
            match joined {
                Ok(result) => results.push(result),
                Err(error) => tracing::error!(%error, "Rack status poll task failed"),
            }
        }

        let now = Instant::now();
        let mut table = self.write();
        for (source, result) in results {
            match result {
                Ok(racks) => table.record_success(&source, racks, now),
                Err(error) => table.record_failure(&source, &format!("{error:#}"), now),
            }
        }
        table.expire(now);
    }

    /// Polls every `poll_interval` until `cancellation` fires, following each poll with the
    /// readiness flag `/readyz` and the metrics endpoint share.
    pub(crate) fn spawn(
        &self,
        client: RackStatusClient,
        readiness: HealthController,
        cancellation: CancellationToken,
    ) -> JoinHandle<()> {
        let map = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(map.inner.poll_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick completes immediately; bootstrap has just polled.
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = cancellation.cancelled() => return,
                    _ = interval.tick() => {}
                }
                tokio::select! {
                    _ = cancellation.cancelled() => return,
                    _ = map.poll_once(&client) => {}
                }
                readiness.set_ready(map.is_ready());
            }
        })
    }
}

impl Ownership for OwnershipMap {
    fn owner_of_rack(&self, rack_id: &str) -> Owner {
        self.read().owner_of_rack(rack_id)
    }

    fn is_ready(&self) -> bool {
        self.read().is_ready()
    }

    fn blockers(&self) -> Vec<String> {
        self.read().blockers()
    }
}

/// An [`Ownership`] whose map is supplied after construction.
///
/// The router is built when the listener starts, before the controller's source list is known,
/// while an [`OwnershipMap`] needs that list. Unbound, every lookup is [`Owner::Unknown`] and the
/// only blocker is the missing source list, so the RMS proxy answers `UNAVAILABLE` instead of
/// routing.
#[derive(Clone, Default)]
pub(crate) struct OwnershipHandle {
    bound: Arc<RwLock<Option<Arc<dyn Ownership>>>>,
}

impl fmt::Debug for OwnershipHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnershipHandle")
            .field("bound", &self.ownership().is_some())
            .finish()
    }
}

impl OwnershipHandle {
    /// A handle with no map.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Routes every later lookup through `ownership`.
    pub(crate) fn bind(&self, ownership: Arc<dyn Ownership>) {
        *self.bound.write().unwrap_or_else(PoisonError::into_inner) = Some(ownership);
    }

    fn ownership(&self) -> Option<Arc<dyn Ownership>> {
        self.bound
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Ownership for OwnershipHandle {
    fn owner_of_rack(&self, rack_id: &str) -> Owner {
        self.ownership()
            .map_or(Owner::Unknown, |ownership| ownership.owner_of_rack(rack_id))
    }

    fn is_ready(&self) -> bool {
        self.ownership()
            .is_some_and(|ownership| ownership.is_ready())
    }

    fn blockers(&self) -> Vec<String> {
        self.ownership().map_or_else(
            || vec![WAITING_FOR_SOURCE_LIST.to_string()],
            |ownership| ownership.blockers(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> SourceId {
        SourceId(name.to_string())
    }

    fn racks(racks: &[&str]) -> BTreeSet<String> {
        racks.iter().map(ToString::to_string).collect()
    }

    fn owned(name: &str) -> Owner {
        Owner::Source(id(name))
    }

    const STALE_AFTER: Duration = Duration::from_secs(60);

    fn table(sources: &[&str]) -> OwnershipTable {
        OwnershipTable::new(sources.iter().map(|s| id(s)), STALE_AFTER)
    }

    #[test]
    fn parses_rack_ids_and_skips_blank_ones() {
        let body = br#"{"racks": [
            {"rack_id": "rack-001", "rack_type": "wiwynn_gb200_nvl72", "version": 1, "members": [{"position": 11}]},
            {"rack_id": "  ", "members": []}
        ]}"#;
        assert_eq!(parse_racks(body).unwrap(), racks(&["rack-001"]));
        assert_eq!(parse_racks(br#"{"racks": []}"#).unwrap(), BTreeSet::new());
        assert!(parse_racks(b"not json").is_err());
        assert!(parse_racks(br#"{"racks": [{"members": []}]}"#).is_err());
    }

    #[test]
    fn disjoint_sources_are_ready_and_own_their_racks() {
        let now = Instant::now();
        let mut table = table(&["mat-b", "mat-a"]);
        assert!(!table.is_ready());
        assert_eq!(
            table.blockers(),
            vec![
                "no rack status from source mat-a yet",
                "no rack status from source mat-b yet"
            ]
        );

        table.record_success(&id("mat-a"), racks(&["rack-001"]), now);
        assert!(!table.is_ready(), "mat-b has not reported yet");
        assert_eq!(
            table.blockers(),
            vec!["no rack status from source mat-b yet"]
        );

        table.record_success(&id("mat-b"), racks(&["rack-002"]), now);
        assert!(table.is_ready());
        assert!(table.blockers().is_empty());
        assert_eq!(table.owner_of_rack("rack-001"), owned("mat-a"));
        assert_eq!(table.owner_of_rack(" rack-002 "), owned("mat-b"));
        assert_eq!(table.owner_of_rack("rack-003"), Owner::Unknown);
    }

    #[test]
    fn a_rack_reported_by_two_sources_has_no_owner_until_one_withdraws() {
        let now = Instant::now();
        let mut table = table(&["mat-a", "mat-b"]);
        table.record_success(&id("mat-a"), racks(&["rack-001", "rack-shared"]), now);
        table.record_success(&id("mat-b"), racks(&["rack-002", "rack-shared"]), now);

        assert!(!table.is_ready());
        assert_eq!(
            table.blockers(),
            vec!["rack rack-shared is reported by mat-a, mat-b"]
        );
        assert_eq!(
            table.owner_of_rack("rack-shared"),
            Owner::Ambiguous(vec![id("mat-a"), id("mat-b")])
        );
        assert_eq!(
            table.owner_of_rack("rack-001"),
            owned("mat-a"),
            "the other racks stay routable"
        );

        table.record_success(&id("mat-b"), racks(&["rack-002"]), now);
        assert!(table.is_ready());
        assert_eq!(table.owner_of_rack("rack-shared"), owned("mat-a"));
    }

    #[test]
    fn a_failing_source_keeps_its_racks_until_stale_after_then_drops_them() {
        let start = Instant::now();
        let mut table = table(&["mat-a", "mat-b"]);
        table.record_success(&id("mat-a"), racks(&["rack-001"]), start);
        table.record_success(&id("mat-b"), racks(&["rack-002"]), start);

        table.record_failure(&id("mat-b"), "connection refused", start + STALE_AFTER / 2);
        assert!(table.expire(start + STALE_AFTER / 2).is_empty());
        assert_eq!(table.source_states()[&id("mat-b")], SourceState::Stale);
        assert_eq!(table.owner_of_rack("rack-002"), owned("mat-b"));
        assert!(table.is_ready(), "a stale source still routes");

        assert_eq!(table.expire(start + STALE_AFTER), vec![id("mat-b")]);
        assert_eq!(table.source_states()[&id("mat-b")], SourceState::Dropped);
        assert_eq!(
            table.owner_of_rack("rack-002"),
            Owner::Dropped(id("mat-b")),
            "dropped racks route nowhere but stay attributable"
        );
        assert_eq!(table.owner_of_rack("rack-001"), owned("mat-a"));
        assert_eq!(table.owner_of_rack("rack-009"), Owner::Unknown);
        assert!(table.is_ready(), "a dropped source is not a conflict");
        assert!(table.expire(start + STALE_AFTER * 10).is_empty());
        assert_eq!(table.source_states()[&id("mat-a")], SourceState::Fresh);

        table.record_success(&id("mat-b"), racks(&["rack-002"]), start + STALE_AFTER * 3);
        assert_eq!(table.source_states()[&id("mat-b")], SourceState::Fresh);
        assert_eq!(table.owner_of_rack("rack-002"), owned("mat-b"));
    }

    #[test]
    fn a_pending_source_blocks_readiness_until_it_answers_or_has_failed_for_stale_after() {
        let start = Instant::now();
        let mut table = table(&["mat-a", "mat-b"]);
        table.record_success(&id("mat-a"), racks(&["rack-001"]), start);
        table.record_failure(
            &id("mat-b"),
            "GET https://mat-b/racks/status: timed out",
            start,
        );

        assert!(!table.is_ready());
        assert_eq!(
            table.blockers(),
            vec!["no rack status from source mat-b yet: GET https://mat-b/racks/status: timed out"]
        );
        table.record_failure(&id("mat-b"), "connection refused", start + STALE_AFTER / 2);
        assert!(table.expire(start + STALE_AFTER / 2).is_empty());
        assert!(!table.is_ready());

        // Dropped after failing for stale_after; it never reported, so its racks are unknown.
        assert_eq!(table.expire(start + STALE_AFTER), vec![id("mat-b")]);
        assert!(table.is_ready());
        assert_eq!(table.owner_of_rack("rack-002"), Owner::Unknown);

        table.record_success(&id("mat-b"), racks(&["rack-002"]), start + STALE_AFTER * 3);
        assert!(table.is_ready());
        assert_eq!(table.owner_of_rack("rack-002"), owned("mat-b"));

        // Failing again is measured from the last success, not from bootstrap.
        table.record_failure(
            &id("mat-b"),
            "connection refused",
            start + STALE_AFTER * 3 + Duration::from_secs(1),
        );
        assert!(
            table
                .expire(start + STALE_AFTER * 3 + STALE_AFTER / 2)
                .is_empty()
        );
        assert_eq!(table.expire(start + STALE_AFTER * 4), vec![id("mat-b")]);
    }

    #[test]
    fn results_from_unknown_sources_are_ignored() {
        let mut table = table(&["mat-a"]);
        table.record_success(&id("mat-x"), racks(&["rack-009"]), Instant::now());
        table.record_failure(&id("mat-x"), "boom", Instant::now());

        assert_eq!(table.owner_of_rack("rack-009"), Owner::Unknown);
        assert_eq!(table.source_states().len(), 1);
    }

    #[test]
    fn handle_answers_nothing_until_a_map_is_bound() {
        let handle = OwnershipHandle::new();
        assert!(!handle.is_ready());
        assert_eq!(handle.owner_of_rack("rack-001"), Owner::Unknown);
        assert_eq!(handle.blockers(), vec![WAITING_FOR_SOURCE_LIST]);

        let list = SourceList {
            generation: 1,
            ready: true,
            sources: vec![Source {
                name: "mat-a".to_string(),
                base_url: "http://mat-a.example:8443/".parse().unwrap(),
                pod: String::new(),
            }],
        };
        let map = OwnershipMap::new(&list, &OwnershipConfig::default());
        map.write()
            .record_success(&id("mat-a"), racks(&["rack-001"]), Instant::now());
        handle.bind(Arc::new(map));

        assert!(handle.is_ready());
        assert!(handle.blockers().is_empty());
        assert_eq!(handle.owner_of_rack("rack-001"), owned("mat-a"));
    }
}
