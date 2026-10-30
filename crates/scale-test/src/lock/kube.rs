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

//! The site lock on a Lease.
//!
//! Renewal, `set_state` and `release` share one async mutex and replace the
//! Lease with a compare-and-swap on the cached resource version and UID.
//! Each write first checks that the Lease still names this exact holder. A
//! conflict re-reads: the same holder retries, another holder closes the
//! gate at once and this process never writes that Lease again. A deleted
//! Lease, seen as a 404 or as no Lease on the re-read, does the same, and so
//! does a Lease with another UID, even one that names this holder: it was
//! deleted and made again.
//!
//! Only acquire makes the Lease. The API makes a missing Lease on a replace
//! unless the UID check refuses it. A replace answered with 201, or with
//! another UID, is a lost lock, and the Lease it made is cleared with one
//! compare-and-swap on the UID and version of that reply, so it reads as
//! released, not as this holder's. Only that reply proves the Lease is the
//! one this write made: a Lease that changed since, or one only seen on a
//! read, is left as it is. If the clearing write fails, or the 201 reply
//! did not carry the Lease, the Lease is left too, and once it expires it
//! is refused as a crashed run.
//!
//! When renewals fail for the max renewal age the gate closes by itself, and
//! `needsCleanup` is set only if a write on the cached version still
//! succeeds.
//!
//! A definite `AlreadyExists` or `Conflict` on the acquire write always
//! refuses: another acquire won. An acquire write whose reply is lost may
//! still have committed. It is read back: a Lease that carries this attempt's
//! unique token in [`ACQUIRE_TOKEN_ANNOTATION`], and names this holder, is
//! held, as if the reply had come. If the read back fails too, the run stops
//! before its journal exists, and the Lease may still name it. That Lease
//! expires and is then refused as a crashed run, with no journal.
//!
//! Recovery is by hand, one step at a time. Stop at the first that fails.
//!
//! 1. Inspect the Lease. Set `LEASE_RV` and `LEASE_UID` to its resource
//!    version and UID, and read the holder record in
//!    [`HOLDER_ANNOTATION`]: `kubectl -n "$NS" get lease "$NAME" -o yaml`.
//!    Not `UID`: the shell already sets it to the user's ID.
//! 2. Check that the Lease has expired, and that no pod of that execution
//!    runs. The execution ID is the pod name, so
//!    `kubectl -n "$NS" get pod "$EXECUTION"` must answer `NotFound`.
//! 3. Check that the journal claim holds no journal of the run. A missing
//!    journal alone proves nothing: a live process may still be about to
//!    write it. A run with no journal and no live process never sent a
//!    write.
//! 4. Delete the Lease only while it is the one inspected, with `LEASE_RV`
//!    and `LEASE_UID` from step 1:
//!
//!    ```bash
//!    kubectl delete --raw "/apis/coordination.k8s.io/v1/namespaces/$NS/leases/$NAME" -f - <<EOF
//!    {"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"resourceVersion":"$LEASE_RV","uid":"$LEASE_UID"}}
//!    EOF
//!    ```
//!
//!    A 409 `Precondition failed` means the Lease changed after step 1:
//!    stop, and start again at step 1. Plain `kubectl delete` checks no
//!    version.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    ACQUIRE_TOKEN_ANNOTATION, HOLDER_ANNOTATION, HeldLock, HolderRecord, HolderState, HolderView,
    LockError, LockPort, LockTiming, Refusal, WriteGate, decide_take, holder_of, is_live, plain,
};
use crate::clock::Clock;
#[cfg(doc)]
use crate::cluster::BoundedCluster;
use crate::cluster::{Cluster, ClusterError, LeaseObject, LeaseReplaced};

/// The production [`LockPort`]: one Lease per site in the run's namespace.
pub struct SiteLock {
    cluster: Arc<dyn Cluster>,
    clock: Arc<dyn Clock>,
    name: String,
    timing: LockTiming,
}

impl SiteLock {
    pub fn new(cluster: Arc<dyn Cluster>, clock: Arc<dyn Clock>, name: &str) -> Self {
        Self::with_timing(cluster, clock, name, LockTiming::default())
    }

    /// `cluster` bounds every call itself, so no renewal or release waits
    /// forever on the API: [`KubeCluster`](crate::cluster::KubeCluster)
    /// does, and a test wraps its fake in [`BoundedCluster`]. A second
    /// timeout here would race the adapter's own and could turn a refusal
    /// into a timeout.
    pub fn with_timing(
        cluster: Arc<dyn Cluster>,
        clock: Arc<dyn Clock>,
        name: &str,
        timing: LockTiming,
    ) -> Self {
        Self {
            cluster,
            clock,
            name: name.to_string(),
            timing,
        }
    }
}

/// True when this acquire attempt wrote `lease`: it carries the attempt's
/// token and names `record` exactly.
fn written_by(lease: &LeaseObject, record: &HolderRecord, token: &str) -> bool {
    let identity = record.holder_identity();
    lease
        .annotations
        .get(ACQUIRE_TOKEN_ANNOTATION)
        .map(String::as_str)
        == Some(token)
        && lease.holder_identity.as_deref() == Some(identity.as_str())
        && holder_of(lease, &identity).is_ok_and(|held| held == *record)
}

/// A token unique to one acquire attempt: a random UUID v4.
fn acquire_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn annotate(
    mut annotations: BTreeMap<String, String>,
    record: &HolderRecord,
) -> Result<BTreeMap<String, String>, LockError> {
    let json = serde_json::to_string(record).map_err(|e| LockError::Corrupt(e.to_string()))?;
    annotations.insert(HOLDER_ANNOTATION.to_string(), json);
    Ok(annotations)
}

#[async_trait]
impl LockPort for SiteLock {
    async fn acquire(&self, me: HolderRecord) -> Result<Arc<dyn HeldLock>, LockError> {
        // Before any write: a bad timing would panic in the renewal task
        // after the Lease is already taken, and a record the Lease cannot
        // carry would never read back as this holder's.
        self.timing.check()?;
        if !plain(me.run_id.as_str()) || !plain(&me.execution_id) {
            return Err(LockError::Corrupt(
                "the execution ID has characters a Lease cannot carry".to_string(),
            ));
        }
        let current = self.cluster.get_lease(&self.name).await?;
        // The Lease times and the gate start from the same moment, after the
        // read: the gate must close before the Lease these times write ends.
        let now = self.clock.now();
        let sent = Instant::now();
        decide_take(current.as_ref(), now, &self.timing).map_err(LockError::Refused)?;

        let record = HolderRecord {
            state: HolderState::Running,
            ..me
        };
        let mut annotations = current
            .as_ref()
            .map(|l| l.annotations.clone())
            .unwrap_or_default();
        let token = acquire_token();
        annotations.insert(ACQUIRE_TOKEN_ANNOTATION.to_string(), token.clone());
        let lease = LeaseObject {
            name: self.name.clone(),
            resource_version: current.as_ref().and_then(|l| l.resource_version.clone()),
            uid: current.as_ref().and_then(|l| l.uid.clone()),
            holder_identity: Some(record.holder_identity()),
            lease_duration_seconds: i32::try_from(self.timing.lease_duration.as_secs())
                .unwrap_or(i32::MAX),
            acquire_time: Some(now),
            renew_time: Some(now),
            annotations: annotate(annotations, &record)?,
        };
        // Only acquire may make the Lease: a replace that made it again is
        // this attempt's write all the same.
        let written = match current {
            None => self.cluster.create_lease(&lease).await,
            // A 201 with no Lease in it is read back as a lost reply.
            Some(_) => self
                .cluster
                .replace_lease(&lease)
                .await
                .and_then(|written| {
                    written.into_lease().ok_or_else(|| {
                        ClusterError::Invalid("the reply did not decode".to_string())
                    })
                }),
        };
        let stored = match written {
            Ok(stored) => stored,
            // Another acquire won between the read and the write, even one
            // with this same holder record.
            // Another taker wrote first, whether or not the read after it
            // works: the site is busy, not the API down.
            Err(ClusterError::AlreadyExists | ClusterError::Conflict) => {
                let current = self.cluster.get_lease(&self.name).await.ok().flatten();
                let refusal = current
                    .and_then(|c| decide_take(Some(&c), self.clock.now(), &self.timing).err())
                    .unwrap_or_else(|| Refusal::Foreign("a concurrent taker".to_string()));
                return Err(LockError::Refused(refusal));
            }
            // The write may have committed with its reply lost: only this
            // attempt's token proves it. Another holder seen instead refuses.
            Err(e) => match self.cluster.get_lease(&self.name).await {
                Ok(Some(lease)) if written_by(&lease, &record, &token) => lease,
                Ok(Some(lease)) => {
                    return Err(
                        match decide_take(Some(&lease), self.clock.now(), &self.timing) {
                            Err(refusal) => LockError::Refused(refusal),
                            Ok(_) => e.into(),
                        },
                    );
                }
                _ => return Err(e.into()),
            },
        };
        // Every Lease the API returns has a UID. The renewals cannot tell
        // this Lease from one made again without it.
        let Some(uid) = stored.uid.clone().filter(|uid| !uid.is_empty()) else {
            return Err(ClusterError::Invalid("the Lease has no UID".to_string()).into());
        };
        let gate = WriteGate::open_until(sent + self.timing.max_renew_age);
        let held = Arc::new(Held {
            cluster: self.cluster.clone(),
            clock: self.clock.clone(),
            timing: self.timing,
            record,
            token,
            uid,
            gate,
            state: Mutex::new(Cached {
                lease: stored,
                lost: false,
                made: None,
            }),
            stop: CancellationToken::new(),
            renewal: std::sync::Mutex::new(None),
        });
        held.spawn_renewal();
        Ok(Arc::new(Handle(held)))
    }

    async fn read(&self) -> Result<Option<HolderView>, LockError> {
        let Some(lease) = self.cluster.get_lease(&self.name).await? else {
            return Ok(None);
        };
        let Some(identity) = lease.holder_identity.as_deref().filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        let record = holder_of(&lease, identity).map_err(LockError::Refused)?;
        let now = self.clock.now();
        Ok(Some(HolderView {
            record,
            live: is_live(&lease, now, &self.timing),
            renew_age: lease
                .renew_time
                .or(lease.acquire_time)
                .and_then(|t| (now - t).to_std().ok()),
        }))
    }
}

struct Cached {
    lease: LeaseObject,
    /// Set once another holder is seen or the gate closed. Never cleared.
    lost: bool,
    /// A Lease this holder's write made after its own was deleted, until
    /// the write that clears it is answered. A release that cancels the
    /// renewal while that write is out tries it again.
    made: Option<LeaseObject>,
}

struct Held {
    cluster: Arc<dyn Cluster>,
    clock: Arc<dyn Clock>,
    timing: LockTiming,
    record: HolderRecord,
    /// The token of the acquire that took the Lease. Every later write
    /// needs the Lease to still carry it.
    token: String,
    /// The UID of the Lease acquire took. Every later write sends it, and a
    /// Lease with another UID is not this holder's.
    uid: String,
    gate: WriteGate,
    state: Mutex<Cached>,
    stop: CancellationToken,
    renewal: std::sync::Mutex<Option<JoinHandle<()>>>,
}

/// How an update changes the cached Lease.
#[derive(Clone, Copy)]
enum Change {
    Renew,
    State(HolderState),
    Release(HolderState),
}

impl Held {
    fn spawn_renewal(self: &Arc<Self>) {
        let held = Arc::downgrade(self);
        let stop = self.stop.clone();
        let every = self.timing.renew_every;
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval_at(Instant::now() + every, every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    _ = tick.tick() => {}
                }
                let Some(held) = held.upgrade() else { return };
                if !held.gate.lease_writable() {
                    tokio::select! {
                        () = stop.cancelled() => {}
                        () = held.give_up() => {}
                    }
                    return;
                }
                // The Lease counts the renewal from when it was sent.
                let sent = Instant::now();
                let renewed = tokio::select! {
                    () = stop.cancelled() => return,
                    renewed = held.update(Change::Renew) => renewed,
                };
                match renewed {
                    Ok(()) => held.gate.extend_until(sent + held.timing.max_renew_age),
                    Err(LockError::Lost) => return,
                    Err(e) => tracing::warn!(error = %e, "site lock renewal failed"),
                }
            }
        });
        *self.renewal.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
    }

    /// The renewal age passed: the run stops writing. One write on the cached
    /// version marks `needsCleanup`. A conflict is never retried.
    async fn give_up(&self) {
        let mut cached = self.state.lock().await;
        if cached.lost {
            return;
        }
        cached.lost = true;
        self.gate.close();
        tracing::error!("site lock renewals failed too long, the lock is lost");
        let Ok(next) = self.changed(&cached.lease, Change::State(HolderState::NeedsCleanup)) else {
            return;
        };
        match self.cluster.replace_lease(&next).await {
            Ok(LeaseReplaced::Replaced(stored)) if self.ours(&stored) => cached.lease = stored,
            Ok(made) => {
                cached.made = made.into_lease();
                self.disown(&mut cached).await;
            }
            Err(e) => tracing::warn!(error = %e, "needsCleanup not recorded on the Lease"),
        }
    }

    /// True when a replace that answered [`LeaseReplaced::Replaced`] wrote
    /// the Lease acquire took. Any other reply made a Lease, or wrote one
    /// with another UID: the one taken was deleted.
    fn ours(&self, stored: &LeaseObject) -> bool {
        stored.uid.as_deref() == Some(self.uid.as_str())
    }

    /// Clears `cached.made`, a Lease this holder's write made after the one
    /// it held was deleted, so it reads as released: no holder and none of
    /// this holder's annotations. One compare-and-swap on the UID and
    /// version of the reply that made it: the Lease is left as it is when
    /// it changed since, and when the write fails.
    async fn disown(&self, cached: &mut Cached) {
        let Some(made) = cached.made.clone() else {
            return;
        };
        let mut cleared = made;
        cleared.holder_identity = None;
        cleared.annotations.remove(HOLDER_ANNOTATION);
        cleared.annotations.remove(ACQUIRE_TOKEN_ANNOTATION);
        let written = self.cluster.replace_lease(&cleared).await;
        cached.made = None;
        match written {
            Ok(_) => tracing::warn!("a write made the deleted site Lease again, it is cleared"),
            Err(e) => tracing::error!(
                error = %e,
                "a write made the deleted site Lease again, it may not be cleared"
            ),
        }
    }

    /// True when `lease` is this holder's own `Final` release: its token, no
    /// holder, and this execution's record in `state`. A release whose reply
    /// was lost reads back this way.
    fn released_by_me(&self, lease: &LeaseObject, state: HolderState) -> bool {
        lease.uid.as_deref() == Some(self.uid.as_str())
            && lease
                .annotations
                .get(ACQUIRE_TOKEN_ANNOTATION)
                .map(String::as_str)
                == Some(self.token.as_str())
            && lease.holder_identity.is_none()
            && holder_of(lease, &self.record.holder_identity())
                .is_ok_and(|r| r.execution_id == self.record.execution_id && r.state == state)
    }

    /// True when `lease` is the Lease acquire took, still names this exact
    /// holder and execution, and carries this acquire's token. A Lease
    /// replaced by another acquire of the same holder record has another
    /// token, and a Lease deleted and made again another UID: neither is
    /// ours.
    fn names_me(&self, lease: &LeaseObject) -> bool {
        let identity = self.record.holder_identity();
        lease.uid.as_deref() == Some(self.uid.as_str())
            && lease
                .annotations
                .get(ACQUIRE_TOKEN_ANNOTATION)
                .map(String::as_str)
                == Some(self.token.as_str())
            && lease.holder_identity.as_deref() == Some(identity.as_str())
            && holder_of(lease, &identity).is_ok_and(|r| r.execution_id == self.record.execution_id)
    }

    fn changed(&self, lease: &LeaseObject, change: Change) -> Result<LeaseObject, LockError> {
        let mut next = lease.clone();
        let mut record =
            holder_of(lease, &self.record.holder_identity()).map_err(LockError::Refused)?;
        match change {
            Change::Renew => next.renew_time = Some(self.clock.now()),
            // A recorded needsCleanup is never settled by its own holder:
            // only an operator clears it.
            Change::State(state) | Change::Release(state)
                if record.state == HolderState::NeedsCleanup
                    && state != HolderState::NeedsCleanup =>
            {
                return Err(LockError::Refused(Refusal::NeedsCleanup(record)));
            }
            Change::State(state) => record.state = state,
            Change::Release(state) => {
                record.state = state;
                if state == HolderState::Final {
                    next.holder_identity = None;
                }
            }
        }
        next.annotations = annotate(next.annotations, &record)?;
        Ok(next)
    }

    /// One compare-and-swap write, retried once after a conflict when the
    /// Lease still names this holder.
    async fn update(&self, change: Change) -> Result<(), LockError> {
        self.update_traced(change)
            .await
            .map_err(|failed| failed.error)
    }

    /// [`update`](Self::update), saying whether the error came from a
    /// Lease write, which may have landed, or from a read.
    async fn update_traced(&self, change: Change) -> Result<(), Failed> {
        let read = |error: LockError| Failed {
            error,
            wrote: false,
        };
        let mut cached = self.state.lock().await;
        // A Lease made again whose clearing a release cancelled.
        self.disown(&mut cached).await;
        for _ in 0..2 {
            // Past the renewal age only `give_up` may write, once.
            if cached.lost || !self.gate.lease_writable() {
                return Err(read(LockError::Lost));
            }
            if !self.names_me(&cached.lease) {
                return Err(read(self.lose(&mut cached)));
            }
            let next = self.changed(&cached.lease, change).map_err(read)?;
            match self.cluster.replace_lease(&next).await {
                Ok(LeaseReplaced::Replaced(stored)) if self.ours(&stored) => {
                    cached.lease = stored;
                    return Ok(());
                }
                // The Lease was deleted, and this write made it again. The
                // gate closes before the clearing write goes out, and the
                // renewal stops only after it.
                Ok(made) => {
                    cached.lost = true;
                    self.gate.close();
                    cached.made = made.into_lease();
                    self.disown(&mut cached).await;
                    return Err(read(self.lose(&mut cached)));
                }
                Err(ClusterError::Conflict) => {
                    let name = cached.lease.name.clone();
                    let current = self.cluster.get_lease(&name).await;
                    match current.map_err(|e| read(e.into()))? {
                        Some(lease) if self.names_me(&lease) => cached.lease = lease,
                        // An earlier try of this release committed with its
                        // reply lost.
                        Some(lease)
                            if matches!(change, Change::Release(state)
                                if self.released_by_me(&lease, state)) =>
                        {
                            cached.lease = lease;
                            return Ok(());
                        }
                        // Another holder, or no Lease at all.
                        _ => return Err(read(self.lose(&mut cached))),
                    }
                }
                // The Lease was deleted: it can never name this holder again.
                Err(ClusterError::Api { code: 404, .. }) => {
                    return Err(read(self.lose(&mut cached)));
                }
                Err(e) => {
                    return Err(Failed {
                        error: e.into(),
                        wrote: true,
                    });
                }
            }
        }
        // The last read back may carry a needsCleanup that refuses this
        // change.
        self.changed(&cached.lease, change).map_err(read)?;
        Err(read(LockError::Cluster(ClusterError::Conflict)))
    }

    fn lose(&self, cached: &mut Cached) -> LockError {
        cached.lost = true;
        self.gate.close();
        self.stop.cancel();
        tracing::error!("the Lease no longer names this holder, the site lock is lost");
        LockError::Lost
    }
}

#[async_trait]
impl HeldLock for Held {
    fn gate(&self) -> WriteGate {
        self.gate.clone()
    }

    async fn set_state(&self, state: HolderState) -> Result<(), LockError> {
        self.update(Change::State(state)).await
    }

    async fn release(&self, state: HolderState) -> Result<(), LockError> {
        // No write may race the release: once the Lease is free another run
        // can take it before this reply arrives.
        self.gate.seal();
        self.stop.cancel();
        let task = self
            .renewal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
        // A transient failure is tried again while the Lease may still be
        // written: an unreleased Lease waits for an operator once it expires.
        let mut released = self.update_traced(Change::Release(state)).await;
        // A write whose reply never came may have released the Lease, so a
        // later loss to another holder is no proof the release failed.
        let mut unconfirmed = None;
        for tried in 1..=RELEASE_TRIES {
            let Err(Failed {
                error: LockError::Cluster(e),
                wrote,
            }) = &released
            else {
                break;
            };
            // The first write that may have landed decides: a read, or a
            // definite refusal such as a 429, never landed.
            if *wrote && maybe_committed(e) {
                unconfirmed.get_or_insert_with(|| e.clone());
            }
            if tried == RELEASE_TRIES || !transient(e) || !self.gate.lease_writable() {
                break;
            }
            tokio::time::sleep(RELEASE_PAUSE).await;
            if !self.gate.lease_writable() {
                break;
            }
            released = self.update_traced(Change::Release(state)).await;
        }
        self.gate.close();
        match (released, unconfirmed) {
            (Ok(()), _) => Ok(()),
            // The Lease still names this holder, with needsCleanup on it: no
            // earlier try released it.
            (
                Err(Failed {
                    error: refused @ LockError::Refused(_),
                    ..
                }),
                _,
            ) => Err(refused),
            // Whatever the later tries met, the release stays unconfirmed.
            (Err(_), Some(earlier)) => Err(LockError::Unconfirmed(earlier)),
            (Err(failed), None) => Err(failed.error),
        }
    }
}

/// What a caller holds. Dropping the last one closes the gate at once,
/// even while a renewal in flight still holds the lock's state.
struct Handle(Arc<Held>);

impl Drop for Handle {
    fn drop(&mut self) {
        self.0.gate.close();
        self.0.stop.cancel();
    }
}

#[async_trait]
impl HeldLock for Handle {
    fn gate(&self) -> WriteGate {
        self.0.gate()
    }

    async fn set_state(&self, state: HolderState) -> Result<(), LockError> {
        self.0.set_state(state).await
    }

    async fn release(&self, state: HolderState) -> Result<(), LockError> {
        self.0.release(state).await
    }
}

/// An update's error, and whether it came from a Lease write.
struct Failed {
    error: LockError,
    wrote: bool,
}

/// Most writes one release makes.
const RELEASE_TRIES: u32 = 3;
/// The wait before a release is tried again.
const RELEASE_PAUSE: Duration = Duration::from_secs(1);

/// A failure after which the write may have landed: no answer, an answer
/// that is not valid, or any server error. A 503 is one too: a proxy
/// answers 503 when the server went away after the request reached it.
fn maybe_committed(e: &ClusterError) -> bool {
    match e {
        ClusterError::Timeout(_) | ClusterError::Transport(_) | ClusterError::Invalid(_) => true,
        ClusterError::Api { code, .. } => *code >= 500,
        _ => false,
    }
}

/// A failure the same call may succeed after: no answer, or a server error.
fn transient(e: &ClusterError) -> bool {
    match e {
        ClusterError::Timeout(_) | ClusterError::Transport(_) => true,
        ClusterError::Api { code, .. } => matches!(*code, 408 | 429) || *code >= 500,
        ClusterError::Conflict
        | ClusterError::AlreadyExists
        | ClusterError::Forbidden(_)
        | ClusterError::Invalid(_) => false,
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // With no holder left nothing renews: a gate clone kept by a writer
        // must not stay open until the renewal age runs out.
        self.gate.close();
        self.stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::{DateTime, TimeDelta, Utc};

    use super::*;
    use crate::clock::TokioClock;
    use crate::cluster::fake::{ClusterOp, FakeCluster};
    use crate::cluster::{BoundedCluster, CALL_TIMEOUT};
    use crate::config::RunId;
    use crate::lock::{HolderKind, LockTiming};

    const LOCK: &str = "nico-scale-test-site-lock";

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-23T10:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn holder(kind: HolderKind, run: &str) -> HolderRecord {
        HolderRecord {
            kind,
            run_id: RunId::parse(run).unwrap(),
            execution_id: format!("{run}-x"),
            state: HolderState::Running,
            test: None,
        }
    }

    /// `cluster` with every call bounded by `limit`, as the adapter bounds
    /// its own.
    fn bounded(cluster: &Arc<FakeCluster>, limit: Duration) -> Arc<dyn Cluster> {
        Arc::new(BoundedCluster::new(cluster.clone(), limit))
    }

    fn setup() -> (Arc<FakeCluster>, SiteLock) {
        let cluster = Arc::new(FakeCluster::new("nico-mat"));
        let clock = Arc::new(TokioClock::starting_at(t0()));
        let lock = SiteLock::new(bounded(&cluster, CALL_TIMEOUT), clock, LOCK);
        (cluster, lock)
    }

    /// A site lock with `r1` holding it.
    async fn setup_held() -> (Arc<FakeCluster>, SiteLock, Arc<dyn HeldLock>) {
        let (cluster, lock) = setup();
        let held = lock.acquire(holder(HolderKind::Test, "r1")).await.unwrap();
        (cluster, lock, held)
    }

    /// A site lock, and the call a new holder's write goes through: a create
    /// on a missing Lease, or a replace of a released one.
    async fn create_or_replace(released: bool) -> (Arc<FakeCluster>, SiteLock, ClusterOp) {
        let (cluster, lock) = setup();
        if released {
            let first = lock.acquire(holder(HolderKind::Test, "r0")).await.unwrap();
            first.release(HolderState::Final).await.unwrap();
        }
        let op = if released {
            ClusterOp::ReplaceLease
        } else {
            ClusterOp::CreateLease
        };
        (cluster, lock, op)
    }

    /// Runs `act` on the cluster whenever `want` is called.
    fn on_op(
        cluster: &Arc<FakeCluster>,
        want: ClusterOp,
        act: impl Fn(&FakeCluster) + Send + Sync + 'static,
    ) {
        let hook = Arc::downgrade(cluster);
        cluster.on_call(move |op, _| {
            if op == want
                && let Some(cluster) = hook.upgrade()
            {
                act(&cluster);
            }
        });
    }

    /// Another process creates the Lease for `holder` as this create goes out.
    fn create_races(cluster: &Arc<FakeCluster>, holder: &HolderRecord, token: Option<&str>) {
        let mut annotations = annotate(BTreeMap::new(), holder).unwrap();
        if let Some(token) = token {
            annotations.insert(ACQUIRE_TOKEN_ANNOTATION.to_string(), token.to_string());
        }
        let lease = LeaseObject {
            name: LOCK.to_string(),
            resource_version: None,
            uid: None,
            holder_identity: Some(holder.holder_identity()),
            lease_duration_seconds: 30,
            acquire_time: Some(t0()),
            renew_time: Some(t0()),
            annotations,
        };
        on_op(cluster, ClusterOp::CreateLease, move |cluster| {
            cluster.set_lease(lease.clone());
        });
    }

    /// The next `n` renewals fail.
    fn fail_renewals(cluster: &FakeCluster, n: usize) {
        for _ in 0..n {
            cluster.fail_next(
                ClusterOp::ReplaceLease,
                ClusterError::Transport("api down".to_string()),
            );
        }
    }

    /// Calls of any of `ops` so far.
    fn count(cluster: &FakeCluster, ops: &[ClusterOp]) -> usize {
        cluster
            .calls()
            .iter()
            .filter(|(op, _)| ops.contains(op))
            .count()
    }

    fn replaces(cluster: &FakeCluster) -> usize {
        count(cluster, &[ClusterOp::ReplaceLease])
    }

    /// The holder is told the lock is lost when it sets `state`.
    async fn assert_lost(held: &Arc<dyn HeldLock>, state: HolderState) {
        assert_eq!(held.set_state(state).await, Err(LockError::Lost));
    }

    fn stored_state(cluster: &FakeCluster) -> HolderState {
        let lease = cluster.lease(LOCK).unwrap();
        let raw = &lease.annotations[HOLDER_ANNOTATION];
        serde_json::from_str::<HolderRecord>(raw).unwrap().state
    }

    /// Another process takes the Lease, with a new resource version.
    fn steal(cluster: &FakeCluster) {
        let mut lease = cluster.lease(LOCK).unwrap();
        let other = holder(HolderKind::Test, "r9");
        lease.holder_identity = Some(other.holder_identity());
        lease.annotations = annotate(lease.annotations, &other).unwrap();
        cluster.set_lease(lease);
    }

    /// A live Lease of `r9`, taken at T0.
    fn r9_lease() -> LeaseObject {
        let other = holder(HolderKind::Test, "r9");
        LeaseObject {
            name: LOCK.to_string(),
            resource_version: None,
            uid: None,
            holder_identity: Some(other.holder_identity()),
            lease_duration_seconds: 60,
            acquire_time: Some(t0()),
            renew_time: Some(t0()),
            annotations: annotate(BTreeMap::new(), &other).unwrap(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_concurrent_taker_refuses_even_when_its_read_fails() {
        // Each row: r9 takes the Lease while r1 creates it, and the read
        // after r1's failed create fails too, or r1's create got no answer.
        for create_timed_out in [false, true] {
            let (cluster, lock) = setup();
            if create_timed_out {
                cluster.fail_next(ClusterOp::CreateLease, ClusterError::Timeout(10));
            }
            let fake = Arc::downgrade(&cluster);
            let once = std::sync::atomic::AtomicBool::new(false);
            cluster.on_call(move |op, _| {
                let Some(fake) = fake.upgrade() else { return };
                if op == ClusterOp::CreateLease
                    && !once.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    fake.set_lease(r9_lease());
                    if !create_timed_out {
                        fake.fail_next(
                            ClusterOp::GetLease,
                            ClusterError::Transport("the connection failed".to_string()),
                        );
                    }
                }
            });
            let refused = lock.acquire(holder(HolderKind::Test, "r1")).await;
            let refusal = match refused {
                Err(LockError::Refused(refusal)) => refusal,
                other => panic!("timed out: {create_timed_out}: {:?}", other.map(|_| ())),
            };
            if create_timed_out {
                assert!(matches!(refusal, Refusal::HeldLive(h) if h.run_id.as_str() == "r9"));
            } else {
                assert_eq!(refusal, Refusal::Foreign("a concurrent taker".to_string()));
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn second_run_refused_while_lock_held() {
        let (cluster, lock, held) = setup_held().await;
        let second = lock.acquire(holder(HolderKind::Ingestion, "r2")).await;
        assert!(matches!(
            second.err(),
            Some(LockError::Refused(Refusal::HeldLive(h))) if h.run_id.as_str() == "r1"
        ));
        let view = lock.read().await.unwrap().unwrap();
        assert!(view.live);
        assert_eq!(view.record.run_id.as_str(), "r1");
        assert_eq!(
            cluster.lease(LOCK).unwrap().holder_identity.as_deref(),
            Some("test/r1/r1-x")
        );
        assert!(held.gate().check().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_that_commits_but_loses_its_reply_is_held() {
        // A create on a missing Lease, then a replace of a released one.
        for released in [false, true] {
            let (cluster, lock, op) = create_or_replace(released).await;
            cluster.lose_reply(op, ClusterError::Timeout(10));
            let held = lock
                .acquire(holder(HolderKind::Test, "r1"))
                .await
                .expect("the committed write is read back as held");
            assert!(held.gate().check().is_ok());
            assert_eq!(stored_state(&cluster), HolderState::Running);
            // Renewals use the version read back, and keep the gate open.
            tokio::time::sleep(Duration::from_secs(31)).await;
            assert!(held.gate().check().is_ok(), "renewed on the read back");
            held.release(HolderState::Final).await.unwrap();
            assert_eq!(cluster.lease(LOCK).unwrap().holder_identity, None);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_with_a_lost_reply_and_no_own_lease_is_an_error() {
        let (cluster, lock) = setup();
        cluster.fail_next(ClusterOp::CreateLease, ClusterError::Timeout(10));
        let taken = lock.acquire(holder(HolderKind::Test, "r1")).await;
        assert!(taken.is_err(), "nothing committed");
        assert_eq!(cluster.lease(LOCK), None);

        // The write fails, and the read back shows another holder.
        let other = holder(HolderKind::Test, "r9");
        create_races(&cluster, &other, None);
        cluster.fail_next(ClusterOp::CreateLease, ClusterError::Timeout(10));
        let taken = lock.acquire(holder(HolderKind::Test, "r1")).await;
        assert!(taken.is_err(), "another holder's Lease is not ours");
        assert_eq!(
            cluster.lease(LOCK).unwrap().holder_identity,
            Some(other.holder_identity())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_identical_holders_get_one_owner() {
        // Both read a missing Lease and create it, or both read a released
        // one and replace it. The slower write gets a definite refusal.
        for released in [false, true] {
            let (cluster, lock, op) = create_or_replace(released).await;
            cluster.delay(op, Duration::from_secs(1));
            let me = holder(HolderKind::Test, "r1");
            let (a, b) = tokio::join!(lock.acquire(me.clone()), lock.acquire(me.clone()));
            assert_eq!(
                usize::from(a.is_ok()) + usize::from(b.is_ok()),
                1,
                "exactly one owner (released: {released})"
            );
            let refused = a.err().or(b.err()).unwrap();
            assert!(
                matches!(refused, LockError::Refused(Refusal::HeldLive(_))),
                "{refused:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn lost_reply_needs_this_attempts_token() {
        let (cluster, lock) = setup();
        // The write never commits, and another attempt with the same holder
        // record writes the Lease meanwhile.
        let me = holder(HolderKind::Test, "r1");
        create_races(&cluster, &me, Some("other"));
        cluster.fail_next(ClusterOp::CreateLease, ClusterError::Timeout(10));
        let taken = lock.acquire(me).await;
        assert!(
            taken.is_err(),
            "the same record with another token is not ours"
        );
    }

    /// Another acquire of the same holder record replaces the Lease: only
    /// its token and resource version differ.
    fn replace_token(cluster: &FakeCluster) -> LeaseObject {
        let mut lease = cluster.lease(LOCK).unwrap();
        lease
            .annotations
            .insert(ACQUIRE_TOKEN_ANNOTATION.to_string(), "other".to_string());
        cluster.set_lease(lease)
    }

    #[tokio::test(start_paused = true)]
    async fn same_holder_with_another_token_is_never_written() {
        for change in ["renew", "state", "release"] {
            let (cluster, _lock, held) = setup_held().await;
            let replacement = replace_token(&cluster);
            let outcome = match change {
                // The renewal at 10 s sees the other token, well before the
                // gate's own 30 s.
                "renew" => {
                    tokio::time::sleep(Duration::from_secs(11)).await;
                    held.gate().check().map_err(|_| LockError::Lost)
                }
                "state" => held.set_state(HolderState::NeedsCleanup).await,
                _ => held.release(HolderState::Final).await,
            };
            assert_eq!(outcome, Err(LockError::Lost), "{change}");
            assert!(held.gate().check().is_err(), "{change}: the gate closes");
            assert_eq!(
                cluster.lease(LOCK).unwrap(),
                replacement,
                "{change}: the replacement Lease is untouched"
            );
            // Later writes stay refused and touch nothing.
            assert_lost(&held, HolderState::NeedsCleanup).await;
            assert_eq!(cluster.lease(LOCK).unwrap(), replacement, "{change}");
        }
    }

    /// Deletes the Lease at its current version, as an operator would.
    fn delete(cluster: &FakeCluster) {
        let version = cluster.lease(LOCK).unwrap().resource_version.unwrap();
        cluster.delete_lease(LOCK, &version).unwrap();
    }

    /// Lease creates and replaces so far.
    fn writes(cluster: &FakeCluster) -> usize {
        count(cluster, &[ClusterOp::CreateLease, ClusterOp::ReplaceLease])
    }

    /// True when `lease` reads as released and carries nothing of `r1`'s.
    fn cleared(lease: &LeaseObject) -> bool {
        lease.holder_identity.is_none()
            && !lease.annotations.contains_key(HOLDER_ANNOTATION)
            && !lease.annotations.contains_key(ACQUIRE_TOKEN_ANNOTATION)
    }

    #[tokio::test(start_paused = true)]
    async fn deleted_lease_loses_the_lock_at_once() {
        // The API's UID check refuses each write, or an API that skips it
        // makes the Lease again and answers 201.
        for uid_checked in [true, false] {
            for change in ["renew", "state", "release", "conflict"] {
                deleted_lease_case(uid_checked, change).await;
            }
        }
    }

    async fn deleted_lease_case(uid_checked: bool, change: &str) {
        let (cluster, lock, held) = setup_held().await;
        if !uid_checked {
            cluster.skip_uid_check();
        }
        let case = format!("{change}, UID checked: {uid_checked}");
        let gate = held.gate();
        let outcome = match change {
            // The renewal at 10 s fails, well before the gate's 30 s.
            "renew" => {
                delete(&cluster);
                tokio::time::sleep(Duration::from_secs(11)).await;
                gate.check().map_err(|_| LockError::Lost)
            }
            "state" => {
                delete(&cluster);
                held.set_state(HolderState::NeedsCleanup).await
            }
            "release" => {
                delete(&cluster);
                held.release(HolderState::Final).await
            }
            // The write conflicts, and the re-read finds no Lease.
            _ => {
                cluster.set_lease(cluster.lease(LOCK).unwrap());
                on_op(&cluster, ClusterOp::GetLease, delete);
                held.set_state(HolderState::NeedsCleanup).await
            }
        };
        assert_eq!(outcome, Err(LockError::Lost), "{case}");
        assert!(gate.check().is_err(), "{case}: the retained gate closes");
        let sent = writes(&cluster);
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_lost(&held, HolderState::Running).await;
        assert_eq!(held.release(HolderState::Final).await, Err(LockError::Lost));
        assert_eq!(writes(&cluster), sent, "{case}: no write after the loss");
        match (uid_checked, change) {
            // The re-read found no Lease, so this holder wrote nothing.
            (true, _) | (false, "conflict") => {
                assert_eq!(cluster.lease(LOCK), None, "{case}: nothing made it again");
            }
            // The Lease made again reads as released, and the site is free.
            (false, _) => {
                let made = cluster.lease(LOCK).expect("made again");
                assert!(cleared(&made), "{case}: {made:?}");
                let next = lock.acquire(holder(HolderKind::Test, "r2")).await;
                assert!(next.is_ok(), "{case}: another run takes the site");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_lease_made_again_by_another_writer_is_not_ours() {
        // An operator deletes the Lease and puts back a copy: the same
        // holder and token, but a new UID.
        for change in ["renew", "state", "release"] {
            let (cluster, _lock, held) = setup_held().await;
            let gate = held.gate();
            let copy = cluster.lease(LOCK).unwrap();
            delete(&cluster);
            let again = cluster.set_lease(copy.clone());
            assert_ne!(again.uid, copy.uid);
            assert_eq!(again.annotations, copy.annotations);
            let outcome = match change {
                "renew" => {
                    tokio::time::sleep(Duration::from_secs(11)).await;
                    gate.check().map_err(|_| LockError::Lost)
                }
                "state" => held.set_state(HolderState::NeedsCleanup).await,
                _ => held.release(HolderState::Final).await,
            };
            assert_eq!(outcome, Err(LockError::Lost), "{change}");
            assert!(gate.check().is_err(), "{change}: the gate closes");
            assert_eq!(
                cluster.lease(LOCK).unwrap(),
                again,
                "{change}: the Lease made again is never written"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_201_with_no_lease_in_it_still_loses_the_lock() {
        let (cluster, _lock, held) = setup_held().await;
        let gate = held.gate();
        cluster.skip_uid_check();
        cluster.unread_created();
        delete(&cluster);
        assert_eq!(
            held.set_state(HolderState::NeedsCleanup).await,
            Err(LockError::Lost)
        );
        assert!(gate.check().is_err());
        // Nothing proves which Lease the write made: it is left.
        let sent = writes(&cluster);
        let made = cluster.lease(LOCK).expect("made again");
        assert!(!cleared(&made));
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(held.release(HolderState::Final).await, Err(LockError::Lost));
        assert_eq!(writes(&cluster), sent, "no write after the loss");
        assert_eq!(cluster.lease(LOCK), Some(made));
    }

    #[tokio::test(start_paused = true)]
    async fn a_release_clears_a_lease_whose_clearing_it_cancelled() {
        let (cluster, _lock, held) = setup_held().await;
        cluster.skip_uid_check();
        delete(&cluster);
        // The renewal at 10 s makes the Lease again, and the write that
        // clears it waits at the server when the release cancels it.
        let made = Arc::new(std::sync::Mutex::new(0));
        let parks = made.clone();
        on_op(&cluster, ClusterOp::ReplaceLease, move |cluster| {
            let mut replaces = parks.lock().unwrap();
            *replaces += 1;
            if *replaces == 2 {
                cluster.park_next(ClusterOp::ReplaceLease);
            }
        });
        tokio::time::timeout(Duration::from_secs(11), cluster.wait_parked())
            .await
            .expect("the clearing write is parked");
        assert!(!cleared(&cluster.lease(LOCK).unwrap()), "made, not cleared");
        assert_eq!(held.release(HolderState::Final).await, Err(LockError::Lost));
        assert!(
            cleared(&cluster.lease(LOCK).unwrap()),
            "the release cleared it"
        );
        // The cancelled write lands later on a version that moved on.
        assert_eq!(cluster.commit_parked(), Some(Err(ClusterError::Conflict)));
        assert!(cleared(&cluster.lease(LOCK).unwrap()));
        assert!(held.gate().check().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_last_held_lock_closes_its_gate() {
        let (cluster, _lock, held) = setup_held().await;
        let gate = held.gate();
        let other = held.clone();
        drop(held);
        assert!(gate.check().is_ok(), "another HeldLock is left");
        drop(other);
        assert!(gate.check().is_err(), "closed with the last HeldLock");
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(
            writes(&cluster),
            1,
            "only the acquire, no renewal after the drop"
        );
        assert_eq!(
            stored_state(&cluster),
            HolderState::Running,
            "the Lease is left for recovery"
        );
    }

    fn ops_since(cluster: &FakeCluster, from: usize) -> Vec<ClusterOp> {
        cluster.calls()[from..].iter().map(|(op, _)| *op).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_that_commits_with_a_lost_reply_still_releases_final() {
        for retaken in [false, true] {
            let (cluster, _lock, held) = setup_held().await;
            let token = cluster.lease(LOCK).unwrap().annotations[ACQUIRE_TOKEN_ANNOTATION].clone();
            // The renewal at 10 s lands, but its reply is lost: this holder
            // still caches the version before it.
            cluster.lose_reply(ClusterOp::ReplaceLease, ClusterError::Timeout(10));
            tokio::time::sleep(Duration::from_secs(11)).await;
            assert_eq!(
                cluster.lease(LOCK).unwrap().renew_time,
                Some(t0() + TimeDelta::seconds(10))
            );
            assert!(held.gate().check().is_ok());
            if retaken {
                replace_token(&cluster);
            }
            let from = cluster.calls().len();
            let released = held.release(HolderState::Final).await;
            let lease = cluster.lease(LOCK).unwrap();
            if retaken {
                // The read back shows the same record with another token.
                assert_eq!(released, Err(LockError::Lost));
                assert_eq!(
                    ops_since(&cluster, from),
                    [ClusterOp::ReplaceLease, ClusterOp::GetLease]
                );
                assert_eq!(stored_state(&cluster), HolderState::Running);
                assert!(lease.holder_identity.is_some(), "the Lease is untouched");
            } else {
                // Conflict, read back with this exact token, then Final.
                assert_eq!(released, Ok(()));
                assert_eq!(
                    ops_since(&cluster, from),
                    [
                        ClusterOp::ReplaceLease,
                        ClusterOp::GetLease,
                        ClusterOp::ReplaceLease
                    ]
                );
                assert_eq!(stored_state(&cluster), HolderState::Final);
                assert_eq!(lease.holder_identity, None);
                assert_eq!(lease.annotations[ACQUIRE_TOKEN_ANNOTATION], token);
            }
            assert!(held.gate().check().is_err(), "retaken: {retaken}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_cancelled_by_release_may_still_commit() {
        for commits_first in [true, false] {
            let (cluster, _lock, held) = setup_held().await;
            cluster.park_next(ClusterOp::ReplaceLease);
            // The renewal at 10 s is at the server, its reply pending.
            cluster.wait_parked().await;
            if commits_first {
                // The server commits it just before the release's write,
                // after release cancelled the renewal.
                on_op(&cluster, ClusterOp::ReplaceLease, |cluster| {
                    cluster.commit_parked();
                });
            }
            let from = cluster.calls().len();
            let started = Instant::now();
            assert_eq!(held.release(HolderState::Final).await, Ok(()));
            assert_eq!(
                started.elapsed(),
                Duration::ZERO,
                "the renewal was cancelled, not timed out"
            );
            let mut ops = vec![ClusterOp::ReplaceLease];
            if commits_first {
                // Conflict on the renewal's version, read back, Final.
                ops.extend([ClusterOp::GetLease, ClusterOp::ReplaceLease]);
            } else {
                // Committed later, the renewal conflicts: Final stays.
                assert_eq!(cluster.commit_parked(), Some(Err(ClusterError::Conflict)));
            }
            assert_eq!(ops_since(&cluster, from), ops, "{commits_first}");
            let lease = cluster.lease(LOCK).unwrap();
            assert_eq!(lease.holder_identity, None, "{commits_first}");
            assert_eq!(stored_state(&cluster), HolderState::Final);
            let renewed = commits_first.then(|| t0() + TimeDelta::seconds(10));
            assert_eq!(lease.renew_time, renewed.or(Some(t0())));
            assert!(held.gate().check().is_err());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_that_commits_after_its_read_back_is_refused() {
        let (cluster, lock) = setup();
        // The create sits at the server past the call timeout, and the read
        // back finds nothing.
        cluster.park_next(ClusterOp::CreateLease);
        let taken = lock.acquire(holder(HolderKind::Test, "r1")).await;
        assert_eq!(
            taken.err(),
            Some(LockError::Cluster(ClusterError::Timeout(10)))
        );
        assert_eq!(
            ops_since(&cluster, 0),
            [
                ClusterOp::GetLease,
                ClusterOp::CreateLease,
                ClusterOp::GetLease
            ]
        );
        // Then it commits: a running Lease with no HeldLock, so no gate and
        // no renewal.
        let orphan = cluster
            .commit_parked()
            .unwrap()
            .unwrap()
            .into_lease()
            .unwrap();
        assert_eq!(stored_state(&cluster), HolderState::Running);
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(cluster.lease(LOCK).unwrap(), orphan);
        assert_eq!(writes(&cluster), 1);

        // Never taken over, live or expired, even by its own holder record.
        let again = lock.acquire(holder(HolderKind::Test, "r1")).await;
        assert!(matches!(
            again.err(),
            Some(LockError::Refused(Refusal::HeldLive(h))) if h.run_id.as_str() == "r1"
        ));
        tokio::time::sleep(Duration::from_secs(40)).await;
        let next = lock.acquire(holder(HolderKind::Test, "r2")).await;
        assert!(matches!(
            next.err(),
            Some(LockError::Refused(Refusal::CrashedRun(h))) if h.run_id.as_str() == "r1"
        ));
        assert_eq!(
            cluster.lease(LOCK).unwrap(),
            orphan,
            "refusals write nothing"
        );

        // Recovery deletes only the version inspected, then the site is free.
        assert_eq!(
            cluster.delete_lease(LOCK, "0"),
            Err(ClusterError::Conflict),
            "a Lease that changed is kept"
        );
        delete(&cluster);
        let next = lock.acquire(holder(HolderKind::Test, "r2")).await.unwrap();
        assert!(next.gate().check().is_ok());
    }

    #[test]
    fn acquire_tokens_are_random_uuids() {
        let (a, b) = (acquire_token(), acquire_token());
        assert_ne!(a, b);
        for token in [a, b] {
            let parsed = uuid::Uuid::try_parse(&token).unwrap();
            assert_eq!(parsed.get_version(), Some(uuid::Version::Random));
            assert_eq!(token, parsed.hyphenated().to_string());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_failure_closes_gate_after_30s() {
        let (cluster, _lock, held) = setup_held().await;
        fail_renewals(&cluster, 10);
        tokio::time::sleep(Duration::from_millis(29_900)).await;
        assert!(
            held.gate().check().is_ok(),
            "last good renewal under 30 s ago"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(held.gate().check().is_err(), "closed at 30 s");
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(held.gate().check().is_err(), "closed for good");
        assert_lost(&held, HolderState::NeedsCleanup).await;
    }

    #[tokio::test(start_paused = true)]
    async fn renewals_keep_the_gate_open() {
        let (cluster, _lock, held) = setup_held().await;
        tokio::time::sleep(Duration::from_secs(301)).await;
        assert!(held.gate().check().is_ok());
        let lease = cluster.lease(LOCK).unwrap();
        assert_eq!(lease.renew_time, Some(t0() + TimeDelta::seconds(300)));
        assert_eq!(replaces(&cluster), 30);
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_returning_after_cutoff_keeps_gate_closed() {
        for check_in_between in [false, true] {
            // A slow API that still answers within its call timeout.
            let cluster = Arc::new(FakeCluster::new("nico-mat"));
            let clock = Arc::new(TokioClock::starting_at(t0()));
            let lock = SiteLock::new(bounded(&cluster, Duration::from_secs(60)), clock, LOCK);
            let held = lock.acquire(holder(HolderKind::Test, "r1")).await.unwrap();
            // The renewal sent at 10 s succeeds, but only at 35 s.
            cluster.delay(ClusterOp::ReplaceLease, Duration::from_secs(25));
            if check_in_between {
                tokio::time::sleep(Duration::from_secs(31)).await;
                assert!(held.gate().check().is_err(), "closed at 30 s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            } else {
                tokio::time::sleep(Duration::from_secs(36)).await;
            }
            assert!(
                held.gate().check().is_err(),
                "a late renewal never reopens the gate (check in between: {check_in_between})"
            );
            assert_eq!(
                cluster.lease(LOCK).unwrap().renew_time,
                Some(t0() + TimeDelta::seconds(10)),
                "the renewal did land"
            );
            assert_lost(&held, HolderState::Running).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hung_renewal_is_bounded_and_release_returns() {
        let (cluster, _lock, held) = setup_held().await;
        let acquired = cluster.lease(LOCK).unwrap();
        cluster.delay(ClusterOp::ReplaceLease, Duration::MAX);
        // The renewal at 10 s hangs. Release at 15 s still returns, and says
        // it was not confirmed: one try from 15 s to 25 s, one more from 26 s
        // to 36 s after a 1 s pause, and none past the 30 s renewal age.
        tokio::time::sleep(Duration::from_secs(15)).await;
        let started = Instant::now();
        let released = held.release(HolderState::Final).await;
        assert_eq!(started.elapsed(), Duration::from_secs(21));
        assert_eq!(
            released,
            Err(LockError::Unconfirmed(ClusterError::Timeout(10)))
        );
        assert!(
            held.gate().check().is_err(),
            "no writes after a release attempt"
        );
        // Nothing was written: the Lease is as this holder took it.
        assert_eq!(cluster.lease(LOCK).unwrap(), acquired);
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(cluster.lease(LOCK).unwrap(), acquired);
    }

    #[tokio::test(start_paused = true)]
    async fn hung_renewal_still_closes_gate_on_time() {
        let (cluster, _lock, held) = setup_held().await;
        cluster.delay(ClusterOp::ReplaceLease, Duration::MAX);
        tokio::time::sleep(Duration::from_millis(29_900)).await;
        assert!(held.gate().check().is_ok());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(held.gate().check().is_err());
        let started = Instant::now();
        assert!(held.set_state(HolderState::Running).await.is_err());
        assert!(started.elapsed() <= Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn conflict_with_other_holder_loses_lock() {
        let (cluster, _lock, held) = setup_held().await;
        steal(&cluster);
        // The next renewal conflicts, re-reads and sees another holder.
        tokio::time::sleep(Duration::from_secs(11)).await;
        assert!(held.gate().check().is_err(), "gate closed at once");
        assert_lost(&held, HolderState::Final).await;
    }

    #[tokio::test(start_paused = true)]
    async fn lost_lock_never_writes_other_holders_lease() {
        let (cluster, _lock, held) = setup_held().await;
        steal(&cluster);
        let stolen = cluster.lease(LOCK).unwrap();
        tokio::time::sleep(Duration::from_secs(11)).await;
        let writes = replaces(&cluster);
        assert_eq!(writes, 1, "one conflicting renewal");
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_lost(&held, HolderState::NeedsCleanup).await;
        assert_eq!(held.release(HolderState::Final).await, Err(LockError::Lost));
        assert_eq!(replaces(&cluster), writes, "no write after the loss");
        assert_eq!(cluster.lease(LOCK).unwrap(), stolen);
    }

    #[tokio::test(start_paused = true)]
    async fn cas_ops_serialized_and_holder_checked() {
        let (cluster, _lock, held) = setup_held().await;
        // Concurrent state writes of one holder never conflict with each other.
        // Each replace takes 500 ms, so all ten are in flight at once unless
        // the holder's mutex puts them one after another.
        cluster.delay(ClusterOp::ReplaceLease, Duration::from_millis(500));
        let started = Instant::now();
        let writes: Vec<_> = (0..10)
            .map(|i| {
                let held = held.clone();
                tokio::spawn(async move {
                    let state = if i % 2 == 0 {
                        HolderState::Final
                    } else {
                        HolderState::Running
                    };
                    held.set_state(state).await
                })
            })
            .collect();
        for write in writes {
            assert_eq!(write.await.unwrap(), Ok(()));
        }
        assert_eq!(started.elapsed(), Duration::from_secs(5), "one at a time");
        let reads = count(&cluster, &[ClusterOp::GetLease]);
        assert_eq!(reads, 1, "only the acquire read, no conflict re-reads");
        assert_eq!(replaces(&cluster), 10);

        // Same holder identity, other execution: foreign to this process.
        let mut lease = cluster.lease(LOCK).unwrap();
        let mut twin = holder(HolderKind::Test, "r1");
        twin.execution_id = "r1-y".to_string();
        lease.annotations = annotate(lease.annotations, &twin).unwrap();
        cluster.set_lease(lease);
        assert_lost(&held, HolderState::Running).await;
        assert!(held.gate().check().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn lost_lock_sets_needs_cleanup_only_on_own_version() {
        // Renewals fail for 30 s, then the API is back: the cached version
        // still matches, so needsCleanup is recorded.
        let (cluster, _lock, held) = setup_held().await;
        // Renewals at 10 s and 20 s fail. At 30 s the gate has closed.
        fail_renewals(&cluster, 2);
        tokio::time::sleep(Duration::from_secs(41)).await;
        assert!(held.gate().check().is_err());
        assert_eq!(stored_state(&cluster), HolderState::NeedsCleanup);

        // Same, but another process took the Lease meanwhile: the write on the
        // cached version conflicts and is never retried.
        let (cluster, _lock, held) = setup_held().await;
        fail_renewals(&cluster, 2);
        tokio::time::sleep(Duration::from_secs(25)).await;
        steal(&cluster);
        let stolen = cluster.lease(LOCK).unwrap();
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert!(held.gate().check().is_err());
        assert_eq!(cluster.lease(LOCK).unwrap(), stolen);
    }

    #[tokio::test(start_paused = true)]
    async fn needs_cleanup_lease_not_taken_by_test() {
        let (cluster, lock, held) = setup_held().await;
        held.release(HolderState::NeedsCleanup).await.unwrap();
        tokio::time::sleep(Duration::from_secs(120)).await;
        let next = lock.acquire(holder(HolderKind::Test, "r2")).await;
        assert!(matches!(
            next.err(),
            Some(LockError::Refused(Refusal::NeedsCleanup(_)))
        ));
        // The Lease keeps its state for the operator.
        assert_eq!(stored_state(&cluster), HolderState::NeedsCleanup);
    }

    #[tokio::test(start_paused = true)]
    async fn bad_timing_is_refused_before_any_write() {
        let zero_renewal = LockTiming {
            renew_every: Duration::ZERO,
            ..LockTiming::default()
        };
        let huge_age = LockTiming {
            max_renew_age: Duration::MAX,
            ..LockTiming::default()
        };
        let gate_past_lease = LockTiming {
            max_renew_age: Duration::from_secs(90),
            ..LockTiming::default()
        };
        // The Lease holds whole seconds: under one second it would say 0.
        let sub_second = LockTiming {
            renew_every: Duration::from_millis(100),
            lease_duration: Duration::from_millis(500),
            max_renew_age: Duration::from_millis(300),
            expiry_skew: Duration::ZERO,
        };
        assert_eq!(
            sub_second.check(),
            Err(LockError::Timing(
                "leaseDuration must be whole seconds, at least 1"
            ))
        );
        for timing in [zero_renewal, huge_age, gate_past_lease, sub_second] {
            let cluster = Arc::new(FakeCluster::new("nico-mat"));
            let clock = Arc::new(TokioClock::starting_at(t0()));
            let lock = SiteLock::with_timing(bounded(&cluster, CALL_TIMEOUT), clock, LOCK, timing);
            let refused = lock.acquire(holder(HolderKind::Test, "r1")).await;
            assert!(
                matches!(refused, Err(LockError::Timing(_))),
                "{timing:?}: {:?}",
                refused.map(|_| ())
            );
            assert!(cluster.calls().is_empty(), "{timing:?}: no call");
        }
        assert_eq!(LockTiming::default().check(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn writes_stop_before_the_release_is_sent() {
        let (cluster, _lock, held) = setup_held().await;
        let gate = held.gate();
        cluster.park_next(ClusterOp::ReplaceLease);
        let release = tokio::spawn({
            let held = held.clone();
            async move { held.release(HolderState::Final).await }
        });
        cluster.wait_parked().await;
        // The release may already have freed the Lease with its reply late.
        assert!(
            gate.check().is_err(),
            "no write while the release is in flight"
        );
        cluster.commit_parked();
        assert_eq!(release.await.unwrap(), Ok(()));
        assert!(gate.check().is_err());
        assert_eq!(cluster.lease(LOCK).unwrap().holder_identity, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_release_is_tried_again_after_a_transient_failure() {
        // Each row: the first try fails before or after it commits.
        for committed in [false, true] {
            let (cluster, _lock, held) = setup_held().await;
            let timeout = ClusterError::Timeout(10);
            if committed {
                cluster.lose_reply(ClusterOp::ReplaceLease, timeout);
            } else {
                cluster.fail_next(ClusterOp::ReplaceLease, timeout);
            }
            assert_eq!(
                held.release(HolderState::Final).await,
                Ok(()),
                "committed: {committed}"
            );
            assert_eq!(cluster.lease(LOCK).unwrap().holder_identity, None);
            assert_eq!(stored_state(&cluster), HolderState::Final);
            assert!(held.gate().check().is_err());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_lock_adds_no_timeout_to_the_adapters_own() {
        // A slow API answers in 15 s. The adapter's own bound decides, so
        // the lock waits for it instead of cutting it at a timeout of its own.
        let cluster = Arc::new(FakeCluster::new("nico-mat"));
        cluster.delay(ClusterOp::GetLease, Duration::from_secs(15));
        let clock = Arc::new(TokioClock::starting_at(t0()));
        let lock = SiteLock::new(cluster.clone(), clock, LOCK);
        let started = Instant::now();
        assert!(lock.acquire(holder(HolderKind::Test, "r1")).await.is_ok());
        assert!(started.elapsed() >= Duration::from_secs(15));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_last_handle_closes_the_gate_during_a_renewal() {
        let (cluster, _lock, held) = setup_held().await;
        let gate = held.gate();
        cluster.park_next(ClusterOp::ReplaceLease);
        tokio::time::sleep(Duration::from_secs(10)).await;
        cluster.wait_parked().await;
        drop(held);
        assert!(gate.check().is_err(), "closed while the renewal is held");
        cluster.commit_parked();
    }

    fn refused_for_cleanup(result: Result<(), LockError>) -> bool {
        matches!(result, Err(LockError::Refused(Refusal::NeedsCleanup(_))))
    }

    #[tokio::test(start_paused = true)]
    async fn a_release_never_settles_a_recorded_needs_cleanup() {
        let (cluster, _lock, held) = setup_held().await;
        held.set_state(HolderState::NeedsCleanup).await.unwrap();
        let writes = replaces(&cluster);
        // Neither a later state nor the release clears it, and both say so.
        assert!(refused_for_cleanup(
            held.set_state(HolderState::Final).await
        ));
        assert!(refused_for_cleanup(held.release(HolderState::Final).await));
        assert_eq!(replaces(&cluster), writes, "nothing written");
        assert!(held.gate().check().is_err());
        assert_eq!(stored_state(&cluster), HolderState::NeedsCleanup);
        assert_eq!(
            cluster.lease(LOCK).unwrap().holder_identity.as_deref(),
            Some("test/r1/r1-x"),
            "the holder stays for an operator"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_needs_cleanup_read_after_the_last_conflict_refuses() {
        // Both tries conflict on a write of the same holder, and the second
        // read back carries needsCleanup.
        let (cluster, _lock, held) = setup_held().await;
        let fake = Arc::downgrade(&cluster);
        let calls = std::sync::atomic::AtomicU32::new(0);
        cluster.on_call(move |op, _| {
            let Some(fake) = fake.upgrade() else { return };
            if op != ClusterOp::ReplaceLease {
                return;
            }
            let tried = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut lease = fake.lease(LOCK).unwrap();
            if tried == 1 {
                let raw = &lease.annotations[HOLDER_ANNOTATION];
                let mut record: HolderRecord = serde_json::from_str(raw).unwrap();
                record.state = HolderState::NeedsCleanup;
                lease.annotations = annotate(lease.annotations, &record).unwrap();
            }
            if tried < 2 {
                fake.set_lease(lease);
            }
        });
        assert!(refused_for_cleanup(
            held.set_state(HolderState::Final).await
        ));
        assert_eq!(stored_state(&cluster), HolderState::NeedsCleanup);
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_closes_before_the_lease_the_acquire_wrote_ends() {
        // A slow read before the write: the Lease times and the gate both
        // start after it.
        let timing = LockTiming {
            renew_every: Duration::from_millis(100),
            lease_duration: Duration::from_secs(1),
            max_renew_age: Duration::from_millis(900),
            expiry_skew: Duration::ZERO,
        };
        let cluster = Arc::new(FakeCluster::new("nico-mat"));
        let clock = Arc::new(TokioClock::starting_at(t0()));
        let lock =
            SiteLock::with_timing(bounded(&cluster, CALL_TIMEOUT), clock.clone(), LOCK, timing);
        cluster.delay(ClusterOp::GetLease, Duration::from_millis(1_500));
        let held = lock.acquire(holder(HolderKind::Test, "r1")).await.unwrap();
        // No renewal lands.
        cluster.delay(ClusterOp::ReplaceLease, Duration::MAX);
        let ends = cluster.lease(LOCK).unwrap().renew_time.unwrap() + TimeDelta::seconds(1);
        if let Ok(left) = (ends - clock.now()).to_std() {
            tokio::time::sleep(left).await;
        }
        assert!(
            held.gate().check().is_err(),
            "the gate is closed when the Lease ends"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_needs_cleanup_set_during_the_release_pause_stays() {
        // Try 1 of the release fails before it lands. During the pause
        // before try 2, the run records needsCleanup.
        let (cluster, _lock, held) = setup_held().await;
        cluster.fail_next(ClusterOp::ReplaceLease, ClusterError::Timeout(10));
        let release = tokio::spawn({
            let held = held.clone();
            async move { held.release(HolderState::Final).await }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!release.is_finished(), "in the pause");
        held.set_state(HolderState::NeedsCleanup).await.unwrap();
        assert!(refused_for_cleanup(release.await.unwrap()));
        assert_eq!(stored_state(&cluster), HolderState::NeedsCleanup);
        assert_eq!(
            cluster.lease(LOCK).unwrap().holder_identity.as_deref(),
            Some("test/r1/r1-x"),
            "the holder stays for an operator"
        );
        assert!(held.gate().check().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn an_invalid_release_reply_is_unconfirmed_and_not_tried_again() {
        // The release lands, and its 2xx reply does not decode.
        let (cluster, _lock, held) = setup_held().await;
        let invalid = ClusterError::Invalid("the reply did not decode".to_string());
        cluster.lose_reply(ClusterOp::ReplaceLease, invalid.clone());
        let before = replaces(&cluster);
        assert_eq!(
            held.release(HolderState::Final).await,
            Err(LockError::Unconfirmed(invalid))
        );
        assert_eq!(replaces(&cluster), before + 1, "one try");
        assert!(!transient(&ClusterError::Invalid(String::new())));
        assert!(transient(&ClusterError::Transport(String::new())));
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_or_only_try_may_leave_the_release_unconfirmed() {
        // Two throttled tries, then a third that lands with its reply lost.
        let (cluster, _lock, held) = setup_held().await;
        let throttled = ClusterError::Api {
            code: 429,
            message: "TooManyRequests".to_string(),
        };
        cluster.fail_next(ClusterOp::ReplaceLease, throttled.clone());
        let fake = Arc::downgrade(&cluster);
        let calls = std::sync::atomic::AtomicU32::new(0);
        cluster.on_call(move |op, _| {
            let Some(fake) = fake.upgrade() else { return };
            if op != ClusterOp::ReplaceLease {
                return;
            }
            // Queued during one try, so the next try gets it.
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => fake.fail_next(ClusterOp::ReplaceLease, throttled.clone()),
                1 => fake.lose_reply(ClusterOp::ReplaceLease, ClusterError::Timeout(10)),
                _ => {}
            }
        });
        assert_eq!(
            held.release(HolderState::Final).await,
            Err(LockError::Unconfirmed(ClusterError::Timeout(10)))
        );

        // One try only: the renewal age passes while it waits.
        let (cluster, _lock, held) = setup_held().await;
        cluster.delay(ClusterOp::ReplaceLease, Duration::MAX);
        tokio::time::sleep(Duration::from_secs(21)).await;
        let started = Instant::now();
        assert_eq!(
            held.release(HolderState::Final).await,
            Err(LockError::Unconfirmed(ClusterError::Timeout(10)))
        );
        assert_eq!(started.elapsed(), Duration::from_secs(10), "one try");
    }

    #[tokio::test(start_paused = true)]
    async fn a_holder_the_lease_cannot_carry_is_refused_before_any_call() {
        let (cluster, lock) = setup();
        let mut me = holder(HolderKind::Test, "r1");
        me.execution_id = "r1:x".to_string();
        assert!(matches!(lock.acquire(me).await, Err(LockError::Corrupt(_))));
        assert!(cluster.calls().is_empty());
        let fraction = LockTiming {
            lease_duration: Duration::from_millis(60_500),
            ..LockTiming::default()
        };
        assert!(matches!(fraction.check(), Err(LockError::Timing(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn a_release_lost_after_an_unanswered_try_is_unconfirmed() {
        // The first try commits with its reply lost, or answered 503 by a
        // proxy. Another run then takes the free Lease before the retry.
        let unavailable = ClusterError::Api {
            code: 503,
            message: "no known reason".to_string(),
        };
        for lost in [ClusterError::Timeout(10), unavailable] {
            release_lost_after(lost).await;
        }
    }

    async fn release_lost_after(lost: ClusterError) {
        let (cluster, _lock, held) = setup_held().await;
        cluster.lose_reply(ClusterOp::ReplaceLease, lost.clone());
        let fake = Arc::downgrade(&cluster);
        let once = std::sync::atomic::AtomicBool::new(false);
        cluster.on_call(move |op, _| {
            let Some(fake) = fake.upgrade() else { return };
            if op == ClusterOp::ReplaceLease && once.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                fake.set_lease(r9_lease());
            }
        });
        assert_eq!(
            held.release(HolderState::Final).await,
            Err(LockError::Unconfirmed(lost.clone())),
            "{lost}: not reported as lost"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_throttled_unavailable_or_proxy_timed_out_release_is_tried_again() {
        for (code, reason) in [
            (429, "TooManyRequests"),
            (408, "no known reason"),
            (503, "no known reason"),
        ] {
            let (cluster, _lock, held) = setup_held().await;
            let refused = ClusterError::Api {
                code,
                message: reason.to_string(),
            };
            cluster.fail_next(ClusterOp::ReplaceLease, refused);
            assert_eq!(held.release(HolderState::Final).await, Ok(()), "{code}");
            assert_eq!(cluster.lease(LOCK).unwrap().holder_identity, None);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_is_never_the_unconfirmed_outcome() {
        // Try 1 conflicts and its read back fails, try 2 commits with its
        // reply lost, and another run has the Lease by try 3.
        let (cluster, _lock, held) = setup_held().await;
        cluster.fail_next(ClusterOp::ReplaceLease, ClusterError::Conflict);
        let fake = Arc::downgrade(&cluster);
        let calls = std::sync::atomic::AtomicU32::new(0);
        cluster.on_call(move |op, _| {
            let Some(fake) = fake.upgrade() else { return };
            if op != ClusterOp::ReplaceLease {
                return;
            }
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => {
                    let down = ClusterError::Transport("the connection failed".to_string());
                    fake.fail_next(ClusterOp::GetLease, down);
                    fake.lose_reply(ClusterOp::ReplaceLease, ClusterError::Timeout(10));
                }
                2 => {
                    fake.set_lease(r9_lease());
                }
                _ => {}
            }
        });
        assert_eq!(
            held.release(HolderState::Final).await,
            Err(LockError::Unconfirmed(ClusterError::Timeout(10)))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_throttled_try_before_an_unanswered_one_is_not_the_outcome() {
        // Try 1 is throttled, try 2 commits with its reply lost, try 3 is
        // refused.
        let (cluster, _lock, held) = setup_held().await;
        cluster.fail_next(
            ClusterOp::ReplaceLease,
            ClusterError::Api {
                code: 429,
                message: "TooManyRequests".to_string(),
            },
        );
        let fake = Arc::downgrade(&cluster);
        let calls = std::sync::atomic::AtomicU32::new(0);
        cluster.on_call(move |op, _| {
            let Some(fake) = fake.upgrade() else { return };
            if op != ClusterOp::ReplaceLease {
                return;
            }
            // Queued during one try, so the next try gets it.
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => fake.lose_reply(ClusterOp::ReplaceLease, ClusterError::Timeout(10)),
                1 => fake.fail_next(
                    ClusterOp::ReplaceLease,
                    ClusterError::Forbidden("Forbidden".to_string()),
                ),
                _ => {}
            }
        });
        assert_eq!(
            held.release(HolderState::Final).await,
            Err(LockError::Unconfirmed(ClusterError::Timeout(10)))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_try_then_refusals_is_still_unconfirmed() {
        // Try 1 commits with its reply lost, and both retries are throttled,
        // or the second meets a permanent refusal.
        let permanent = ClusterError::Forbidden("Forbidden".to_string());
        let throttled = ClusterError::Api {
            code: 429,
            message: "TooManyRequests".to_string(),
        };
        for last in [throttled.clone(), permanent] {
            let (cluster, _lock, held) = setup_held().await;
            cluster.lose_reply(ClusterOp::ReplaceLease, ClusterError::Timeout(10));
            let fake = Arc::downgrade(&cluster);
            let calls = std::sync::atomic::AtomicU32::new(0);
            let (second, third) = (throttled.clone(), last.clone());
            cluster.on_call(move |op, _| {
                let Some(fake) = fake.upgrade() else { return };
                if op != ClusterOp::ReplaceLease {
                    return;
                }
                // Queued during one try, so the next try gets it.
                match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => fake.fail_next(ClusterOp::ReplaceLease, second.clone()),
                    1 => fake.fail_next(ClusterOp::ReplaceLease, third.clone()),
                    _ => {}
                }
            });
            assert_eq!(
                held.release(HolderState::Final).await,
                Err(LockError::Unconfirmed(ClusterError::Timeout(10))),
                "then {last:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_unanswered_try_decides_an_unconfirmed_release() {
        // Try 1 commits with its reply lost, try 2 is throttled, and another
        // run has the Lease by try 3.
        let (cluster, _lock, held) = setup_held().await;
        cluster.lose_reply(ClusterOp::ReplaceLease, ClusterError::Timeout(10));
        let throttled = ClusterError::Api {
            code: 429,
            message: "TooManyRequests".to_string(),
        };
        let fake = Arc::downgrade(&cluster);
        let calls = std::sync::atomic::AtomicU32::new(0);
        cluster.on_call(move |op, _| {
            let Some(fake) = fake.upgrade() else { return };
            if op != ClusterOp::ReplaceLease {
                return;
            }
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                // Queued during try 1, so try 2 gets it.
                0 => fake.fail_next(ClusterOp::ReplaceLease, throttled.clone()),
                2 => {
                    fake.set_lease(r9_lease());
                }
                _ => {}
            }
        });
        assert_eq!(
            held.release(HolderState::Final).await,
            Err(LockError::Unconfirmed(ClusterError::Timeout(10)))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_release_is_not_tried_again_after_a_permanent_failure() {
        let (cluster, _lock, held) = setup_held().await;
        let forbidden = ClusterError::Forbidden("Forbidden".to_string());
        cluster.fail_next(ClusterOp::ReplaceLease, forbidden.clone());
        let before = replaces(&cluster);
        assert_eq!(
            held.release(HolderState::Final).await,
            Err(LockError::Cluster(forbidden))
        );
        assert_eq!(replaces(&cluster), before + 1, "one try");
    }

    #[tokio::test(start_paused = true)]
    async fn release_final_frees_lock() {
        let (cluster, lock) = setup();
        let held = lock
            .acquire(holder(HolderKind::FleetUp, "r1"))
            .await
            .unwrap();
        held.release(HolderState::Final).await.unwrap();
        assert!(held.gate().check().is_err(), "no writes after release");
        assert_eq!(cluster.lease(LOCK).unwrap().holder_identity, None);
        assert_eq!(stored_state(&cluster), HolderState::Final);
        assert_eq!(lock.read().await.unwrap(), None);
        let next = lock.acquire(holder(HolderKind::Test, "r2")).await.unwrap();
        assert!(next.gate().check().is_ok());
        // The released lock's renewal is gone: no write touches r2's Lease.
        let before = replaces(&cluster);
        tokio::time::sleep(Duration::from_secs(11)).await;
        assert_eq!(replaces(&cluster), before + 1, "only r2 renews");
    }
}
