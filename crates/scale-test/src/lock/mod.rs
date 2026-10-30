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

//! The site lock: one Lease per site, so only one scale test, ingestion or
//! fleet-up runs at a time.
//!
//! The Lease is cooperative exclusion, not server-side fencing. A request
//! already sent when the lock is lost can still land. The journal records
//! it, and an unclear write leaves the Lease `needsCleanup`.

#[cfg(any(test, feature = "test-support"))]
pub mod fake;
mod kube;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

pub use self::kube::SiteLock;
use crate::cluster::{ClusterError, LeaseObject};
use crate::config::RunId;

/// Annotation that holds the JSON [`HolderRecord`].
pub const HOLDER_ANNOTATION: &str = "nvidia-infra-controller/scale-test-holder";
/// Annotation that holds the unique token of the acquire that wrote the
/// Lease. Only it proves a lost acquire reply was this attempt's.
pub const ACQUIRE_TOKEN_ANNOTATION: &str = "nvidia-infra-controller/scale-test-acquire-token";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HolderKind {
    Test,
    Ingestion,
    FleetUp,
    Deploy,
}

impl HolderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Ingestion => "ingestion",
            Self::FleetUp => "fleet-up",
            Self::Deploy => "deploy",
        }
    }
}

impl fmt::Display for HolderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HolderState {
    Running,
    /// Done and clean. A `final` release clears `holderIdentity`.
    Final,
    /// Unclear writes: no run takes it until an operator settles them.
    NeedsCleanup,
}

/// Who holds the Lease, kept as JSON in [`HOLDER_ANNOTATION`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderRecord {
    pub kind: HolderKind,
    pub run_id: RunId,
    pub execution_id: String,
    pub state: HolderState,
    pub test: Option<String>,
}

impl HolderRecord {
    /// `spec.holderIdentity`: `<kind>/<runId>/<executionId>`. When it and the
    /// annotation disagree the holder is foreign.
    pub fn holder_identity(&self) -> String {
        format!("{}/{}/{}", self.kind, self.run_id, self.execution_id)
    }
}

/// Why the lock can be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takeable {
    Missing,
    Released,
    ExpiredFinal,
}

/// Why the lock was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    HeldLive(HolderRecord),
    NeedsCleanup(HolderRecord),
    CrashedRun(HolderRecord),
    /// `holderIdentity` and the annotation disagree, or the annotation is unreadable.
    Foreign(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The kind and the run ID only: the execution ID is free text any
        // writer of the Lease chose.
        match self {
            Self::HeldLive(h) => write!(f, "held by live {} run {}", h.kind, h.run_id),
            Self::NeedsCleanup(h) => write!(f, "run {} needs cleanup first", h.run_id),
            Self::CrashedRun(h) => write!(f, "run {} crashed and needs recovery", h.run_id),
            Self::Foreign(identity) => write!(f, "held by {identity}"),
        }
    }
}

/// Lease timing: renew every 10 s, 60 s lease, writes stop when the last good
/// renewal is over 30 s old, 5 s skew margin on expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockTiming {
    pub renew_every: Duration,
    pub lease_duration: Duration,
    pub max_renew_age: Duration,
    pub expiry_skew: Duration,
}

impl Default for LockTiming {
    fn default() -> Self {
        Self {
            renew_every: Duration::from_secs(10),
            lease_duration: Duration::from_secs(60),
            max_renew_age: Duration::from_secs(30),
            expiry_skew: Duration::from_secs(5),
        }
    }
}

/// The longest any lock duration may be, so no deadline math overflows.
const MAX_LOCK_DURATION: Duration = Duration::from_secs(86_400);

impl LockTiming {
    /// Renewal must keep the gate open, and the gate must close before the
    /// Lease expires for another taker.
    pub fn check(&self) -> Result<(), LockError> {
        let all = [
            self.renew_every,
            self.lease_duration,
            self.max_renew_age,
            self.expiry_skew,
        ];
        if all.iter().any(|d| *d > MAX_LOCK_DURATION) {
            return Err(LockError::Timing("a duration is over a day"));
        }
        if self.renew_every.is_zero() {
            return Err(LockError::Timing("renewEvery must be over 0"));
        }
        if self.lease_duration < Duration::from_secs(1) || self.lease_duration.subsec_nanos() != 0 {
            return Err(LockError::Timing(
                "leaseDuration must be whole seconds, at least 1",
            ));
        }
        if self.renew_every >= self.max_renew_age {
            return Err(LockError::Timing("renewEvery must be below maxRenewAge"));
        }
        if self.max_renew_age >= self.lease_duration {
            return Err(LockError::Timing("maxRenewAge must be below leaseDuration"));
        }
        Ok(())
    }
}

/// The current holder as read from the Lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderView {
    pub record: HolderRecord,
    /// Renewed within the lease duration plus the skew margin.
    pub live: bool,
    /// Wall-clock age of the last renewal, when known.
    pub renew_age: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LockError {
    #[error("site lock refused: {0}")]
    Refused(Refusal),
    #[error("site lock lost")]
    Lost,
    #[error("site lock: {0}")]
    Cluster(#[from] ClusterError),
    /// A release write that may have landed got no clear answer, and no
    /// later try settled it: the Lease may or may not be free.
    #[error("site lock release unconfirmed: {0}")]
    Unconfirmed(ClusterError),
    #[error("site lock record unreadable: {0}")]
    Corrupt(String),
    #[error("site lock timing invalid: {0}")]
    Timing(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("site lock lost")]
pub struct LockLost;

/// Answers "may this process still write?". Closed for good once the lock is
/// lost, or when the last good renewal is too old.
#[derive(Debug, Clone)]
pub struct WriteGate {
    inner: Arc<GateInner>,
}

#[derive(Debug)]
struct GateInner {
    origin: Instant,
    closed: AtomicBool,
    /// Set when the release starts: no more writes, while the release
    /// itself may still write the Lease.
    sealed: AtomicBool,
    /// Milliseconds after `origin` at which the gate closes.
    open_until_ms: AtomicU64,
}

impl WriteGate {
    /// A gate that stays open until closed.
    pub fn open() -> Self {
        Self::with_limit(u64::MAX)
    }

    /// A gate open until `until`, moved forward by [`extend_until`](Self::extend_until).
    pub fn open_until(until: Instant) -> Self {
        let now = Instant::now();
        let ms =
            u64::try_from(until.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX);
        Self {
            inner: Arc::new(GateInner {
                origin: now,
                closed: AtomicBool::new(false),
                sealed: AtomicBool::new(false),
                open_until_ms: AtomicU64::new(ms),
            }),
        }
    }

    fn with_limit(ms: u64) -> Self {
        Self {
            inner: Arc::new(GateInner {
                origin: Instant::now(),
                closed: AtomicBool::new(false),
                sealed: AtomicBool::new(false),
                open_until_ms: AtomicU64::new(ms),
            }),
        }
    }

    fn ms(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.inner.origin).as_millis())
            .unwrap_or(u64::MAX)
    }

    /// After a good renewal. Never moves the limit back. A renewal that
    /// returns after the limit passed reopens nothing: expiry latches, whether
    /// or not anyone checked the gate in between.
    pub fn extend_until(&self, until: Instant) {
        if self.expired() {
            return;
        }
        let ms = self.ms(until);
        self.inner.open_until_ms.fetch_max(ms, Ordering::SeqCst);
    }

    /// True once closed or past the limit. Passing the limit closes the gate
    /// for good.
    fn expired(&self) -> bool {
        if self.inner.closed.load(Ordering::SeqCst) {
            return true;
        }
        let now = self.ms(Instant::now());
        if now >= self.inner.open_until_ms.load(Ordering::SeqCst) {
            self.inner.closed.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }

    /// Closes the gate at `at` at the latest.
    pub fn close_at(&self, at: Instant) {
        let ms = self.ms(at);
        self.inner.open_until_ms.fetch_min(ms, Ordering::SeqCst);
    }

    /// Closes the gate for good.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
    }

    /// Closes the gate for every write but the holder's own Lease writes,
    /// which still stop at the renewal age.
    pub(crate) fn seal(&self) {
        self.inner.sealed.store(true, Ordering::SeqCst);
    }

    /// Whether the holder may still write its Lease: not closed and not past
    /// the renewal age. A sealed gate still allows it.
    pub(crate) fn lease_writable(&self) -> bool {
        !self.expired()
    }

    pub fn check(&self) -> Result<(), LockLost> {
        if self.inner.sealed.load(Ordering::SeqCst) || self.expired() {
            return Err(LockLost);
        }
        Ok(())
    }
}

#[async_trait]
pub trait LockPort: Send + Sync {
    /// Takes the lock for `me` and starts renewal.
    async fn acquire(&self, me: HolderRecord) -> Result<Arc<dyn HeldLock>, LockError>;
    /// The current holder, without taking anything.
    async fn read(&self) -> Result<Option<HolderView>, LockError>;
}

#[async_trait]
pub trait HeldLock: Send + Sync {
    fn gate(&self) -> WriteGate;
    /// A recorded `needsCleanup` stays for an operator: any other state is
    /// refused with [`Refusal::NeedsCleanup`], and nothing is written.
    async fn set_state(&self, state: HolderState) -> Result<(), LockError>;
    /// A `Final` release clears `holderIdentity`. A recorded
    /// `needsCleanup` refuses it as [`set_state`](Self::set_state) does, and
    /// the holder stays.
    async fn release(&self, state: HolderState) -> Result<(), LockError>;
}

/// Whether `me` may take the Lease `current`, as of `now`. Pure.
///
/// Every command takes only a missing, released or expired final Lease. An
/// expired running holder is a crashed run, and a `needsCleanup` holder has
/// unclear writes: both wait for an operator.
pub fn decide_take(
    current: Option<&LeaseObject>,
    now: DateTime<Utc>,
    t: &LockTiming,
) -> Result<Takeable, Refusal> {
    let Some(lease) = current else {
        return Ok(Takeable::Missing);
    };
    let Some(identity) = lease.holder_identity.as_deref().filter(|id| !id.is_empty()) else {
        return Ok(Takeable::Released);
    };
    let holder = holder_of(lease, identity)?;
    let live = is_live(lease, now, t);

    if holder.state == HolderState::Final {
        return if live {
            Err(Refusal::HeldLive(holder))
        } else {
            Ok(Takeable::ExpiredFinal)
        };
    }
    Err(match (holder.state, live) {
        (HolderState::NeedsCleanup, _) => Refusal::NeedsCleanup(holder),
        (_, true) => Refusal::HeldLive(holder),
        (_, false) => Refusal::CrashedRun(holder),
    })
}

/// The holder record, when the annotation agrees with `holderIdentity`.
pub(crate) fn holder_of(lease: &LeaseObject, identity: &str) -> Result<HolderRecord, Refusal> {
    let record = lease
        .annotations
        .get(HOLDER_ANNOTATION)
        .and_then(|raw| serde_json::from_str::<HolderRecord>(raw).ok())
        .ok_or_else(|| Refusal::Foreign(shown(identity)))?;
    if record.holder_identity() != identity || !plain(&record.execution_id) {
        return Err(Refusal::Foreign(shown(identity)));
    }
    Ok(record)
}

/// Kubernetes name characters only.
pub(crate) fn plain(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 300
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '/' | '_'))
}

/// A foreign `holderIdentity` as an error shows it: never its text, which
/// anyone who can write the Lease chose.
fn shown(_identity: &str) -> String {
    "a holder this lock cannot read".to_string()
}

/// Renewed (or acquired) within the lease duration plus the skew margin.
pub(crate) fn is_live(lease: &LeaseObject, now: DateTime<Utc>, t: &LockTiming) -> bool {
    let Some(last) = lease.renew_time.or(lease.acquire_time) else {
        return false;
    };
    let duration = TimeDelta::seconds(i64::from(lease.lease_duration_seconds));
    let skew = TimeDelta::from_std(t.expiry_skew).unwrap_or_default();
    now < last + duration + skew
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn record() -> HolderRecord {
        HolderRecord {
            kind: HolderKind::FleetUp,
            run_id: RunId::parse("r1").unwrap(),
            execution_id: "r1-abcde".to_string(),
            state: HolderState::Running,
            test: None,
        }
    }

    fn lease(holder: Option<&HolderRecord>, renewed: i64) -> LeaseObject {
        let mut annotations = BTreeMap::new();
        if let Some(h) = holder {
            annotations.insert(
                HOLDER_ANNOTATION.to_string(),
                serde_json::to_string(h).unwrap(),
            );
        }
        LeaseObject {
            name: "lock".to_string(),
            resource_version: Some("1".to_string()),
            uid: Some("u1".to_string()),
            holder_identity: holder.map(HolderRecord::holder_identity),
            lease_duration_seconds: 60,
            acquire_time: Some(now() - TimeDelta::seconds(3600)),
            renew_time: Some(now() - TimeDelta::seconds(renewed)),
            annotations,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-23T10:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn h(state: HolderState) -> HolderRecord {
        HolderRecord {
            kind: HolderKind::Test,
            run_id: RunId::parse("r1").unwrap(),
            execution_id: "r1-x".to_string(),
            state,
            test: None,
        }
    }

    /// A Lease held by `h(state)`, last renewed `renewed` seconds ago.
    fn held(state: HolderState, renewed: i64) -> Option<LeaseObject> {
        Some(lease(Some(&h(state)), renewed))
    }

    #[test]
    fn decide_take_table() {
        use HolderState::{Final, NeedsCleanup, Running};
        // 65 s since renewal is past 60 s plus the 5 s margin. 64 s is not.
        const LIVE: i64 = 64;
        const EXPIRED: i64 = 66;
        let foreign = LeaseObject {
            holder_identity: Some("test/r1/someone-else".to_string()),
            ..lease(Some(&h(Running)), LIVE)
        };
        let no_annotation = LeaseObject {
            holder_identity: Some("test/r1/r1-x".to_string()),
            ..lease(None, LIVE)
        };
        let rows: [(&str, Option<LeaseObject>, Result<Takeable, &str>); 9] = [
            ("missing", None, Ok(Takeable::Missing)),
            ("released", Some(lease(None, LIVE)), Ok(Takeable::Released)),
            ("live test", held(Running, LIVE), Err("heldLive")),
            (
                "expired final",
                held(Final, EXPIRED),
                Ok(Takeable::ExpiredFinal),
            ),
            ("live final", held(Final, LIVE), Err("heldLive")),
            ("expired running", held(Running, EXPIRED), Err("crashedRun")),
            (
                "needsCleanup",
                held(NeedsCleanup, EXPIRED),
                Err("needsCleanup"),
            ),
            ("foreign identity", Some(foreign), Err("foreign")),
            ("no holder annotation", Some(no_annotation), Err("foreign")),
        ];
        for (scenario, current, want) in rows {
            let got =
                decide_take(current.as_ref(), now(), &LockTiming::default()).map_err(|r| match r {
                    Refusal::HeldLive(_) => "heldLive",
                    Refusal::NeedsCleanup(_) => "needsCleanup",
                    Refusal::CrashedRun(_) => "crashedRun",
                    Refusal::Foreign(_) => "foreign",
                });
            assert_eq!(got, want, "{scenario}");
        }
    }

    #[test]
    fn holder_record_wire_names() {
        assert_eq!(record().holder_identity(), "fleet-up/r1/r1-abcde");
        let json = serde_json::to_value(HolderRecord {
            state: HolderState::NeedsCleanup,
            ..record()
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "fleet-up", "runId": "r1", "executionId": "r1-abcde",
                "state": "needsCleanup", "test": null,
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_renewal_after_cutoff_never_reopens_gate() {
        // A renewal sent at 10 s comes back at 35 s, after the 30 s cutoff.
        let start = Instant::now();
        let unchecked = WriteGate::open_until(start + Duration::from_secs(30));
        let checked = WriteGate::open_until(start + Duration::from_secs(30));
        tokio::time::advance(Duration::from_secs(31)).await;
        assert_eq!(checked.check(), Err(LockLost));
        tokio::time::advance(Duration::from_secs(4)).await;
        for gate in [&unchecked, &checked] {
            gate.extend_until(start + Duration::from_secs(40));
            assert_eq!(gate.check(), Err(LockLost));
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            unchecked.check(),
            Err(LockLost),
            "no check in between, still latched"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn write_gate_follows_renewals_and_closes_for_good() {
        let gate = WriteGate::open_until(Instant::now() + Duration::from_secs(30));
        assert_eq!(gate.check(), Ok(()));
        tokio::time::advance(Duration::from_secs(20)).await;
        gate.extend_until(Instant::now() + Duration::from_secs(30));
        // An older renewal never moves the limit back.
        gate.extend_until(Instant::now());
        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(gate.check(), Ok(()));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(gate.check(), Err(LockLost), "renewal 30 s old");

        let gate = WriteGate::open();
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(gate.check(), Ok(()));
        gate.close();
        gate.extend_until(Instant::now() + Duration::from_secs(30));
        assert_eq!(gate.check(), Err(LockLost), "closed for good");
    }

    #[test]
    fn a_foreign_holder_shows_no_text_it_could_hide_a_secret_in() {
        let mut lease = LeaseObject {
            name: "lock".to_string(),
            resource_version: Some("1".to_string()),
            uid: Some("u1".to_string()),
            holder_identity: Some("https://user:password@example".to_string()),
            lease_duration_seconds: 60,
            acquire_time: None,
            renew_time: None,
            annotations: BTreeMap::new(),
        };
        let identity = lease.holder_identity.clone().unwrap();
        let refusal = holder_of(&lease, &identity).unwrap_err();
        assert_eq!(
            refusal,
            Refusal::Foreign("a holder this lock cannot read".to_string())
        );
        assert!(!refusal.to_string().contains("password"));
        // A token made of name characters is not shown either.
        let token = "SecretToken123";
        assert!(
            !holder_of(&lease, token)
                .unwrap_err()
                .to_string()
                .contains(token)
        );

        // A matching record shows its kind and run, never its execution.
        let live = HolderRecord {
            kind: HolderKind::Test,
            run_id: RunId::parse("r1").unwrap(),
            execution_id: "SecretToken123".to_string(),
            state: HolderState::Running,
            test: None,
        };
        for refusal in [
            Refusal::HeldLive(live.clone()),
            Refusal::NeedsCleanup(live.clone()),
            Refusal::CrashedRun(live),
        ] {
            assert!(!refusal.to_string().contains("SecretToken123"), "{refusal}");
        }

        // A readable identity whose record carries an unreadable execution.
        let record = HolderRecord {
            kind: HolderKind::Test,
            run_id: RunId::parse("r1").unwrap(),
            execution_id: "x?token=secret".to_string(),
            state: HolderState::Running,
            test: None,
        };
        lease.holder_identity = Some(record.holder_identity());
        lease.annotations.insert(
            HOLDER_ANNOTATION.to_string(),
            serde_json::to_string(&record).unwrap(),
        );
        let identity = lease.holder_identity.clone().unwrap();
        assert_eq!(
            holder_of(&lease, &identity),
            Err(Refusal::Foreign(
                "a holder this lock cannot read".to_string()
            ))
        );
    }
}
