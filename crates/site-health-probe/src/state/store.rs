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

//! The snapshot store. Every reader publishes into it; the metrics collector
//! and the `/v1` API read from it. A family's current snapshot is replaced
//! whole. Paged families keep a replaced snapshot that a cursor points into
//! for `keep` after it was replaced, so a client paging through one snapshot
//! finishes on it even when newer sweeps land meanwhile. A snapshot no
//! cursor points into is dropped when it is replaced, so a site nobody pages
//! holds one snapshot per family. An unexpired snapshot is never evicted.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use carbide_site_health_probe_api::Family;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::config::Paging;
use crate::readers::devices::DEVICE_FAMILIES;
use crate::snapshot::{
    AnySnapshot, CensusView, FailureKind, FamilySnapshot, Reader, SnapshotSink, UnitsData,
};

/// Every family the readers publish. The firmware config, tenant
/// allocation and tenant instance families wait for their tests (#8154,
/// #8152).
pub(crate) const ALL_FAMILIES: [Family; 10] = [
    Family::Host,
    Family::Dpu,
    Family::Switch,
    Family::PowerShelf,
    Family::Rack,
    Family::Instance,
    Family::Expected,
    Family::HostFirmwareRequests,
    Family::RestTotals,
    Family::RestSiteMatch,
];

/// Families whose units `/v1/units` pages through.
pub(crate) fn is_paged(family: Family) -> bool {
    DEVICE_FAMILIES.contains(&family)
}

/// The newest attempt to read a family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastAttempt {
    pub reader: Reader,
    /// `None` when the attempt published.
    pub failure: Option<(FailureKind, String)>,
    pub at: SystemTime,
}

/// A family's current snapshot and the replaced ones still kept. One lock
/// covers both, so a lookup never misses a snapshot that is moving from
/// current to replaced.
#[derive(Default)]
struct Snapshots {
    current: Option<Arc<dyn AnySnapshot>>,
    /// Whether a cursor points into the current snapshot.
    current_paged: bool,
    /// Replaced snapshots, oldest first, with their replacement time.
    superseded: VecDeque<(Arc<dyn AnySnapshot>, Instant)>,
}

#[derive(Default)]
struct Slot {
    snapshots: Mutex<Snapshots>,
    last_attempt: Mutex<Option<LastAttempt>>,
}

/// The newest snapshot of every family.
pub(crate) struct SnapshotStore {
    keep: Duration,
    max_kept: usize,
    slots: BTreeMap<Family, Slot>,
    ready: watch::Sender<BTreeSet<Family>>,
    /// Runs inside a publication, after the new snapshot became current and
    /// before the old one is kept, for tests that look up meanwhile.
    #[cfg(test)]
    mid_publish: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Runs when a family's snapshot lock is already held, just before
    /// waiting for it, for tests that need to see a lookup blocked.
    #[cfg(test)]
    lock_contended: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Runs after a lookup by ID found its snapshot, outside the lock, for
    /// tests that expire it before the caller uses it.
    #[cfg(test)]
    pub(crate) after_lookup: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl SnapshotStore {
    pub(crate) fn new(paging: &Paging) -> Self {
        Self {
            keep: Duration::from_secs(paging.keep_seconds),
            max_kept: paging.max_kept_snapshots,
            slots: ALL_FAMILIES
                .iter()
                .map(|family| (*family, Slot::default()))
                .collect(),
            ready: watch::Sender::new(BTreeSet::new()),
            #[cfg(test)]
            mid_publish: Mutex::new(None),
            #[cfg(test)]
            lock_contended: Mutex::new(None),
            #[cfg(test)]
            after_lookup: Mutex::new(None),
        }
    }

    fn slot(&self, family: Family) -> &Slot {
        &self.slots[&family]
    }

    fn snapshots(&self, family: Family) -> std::sync::MutexGuard<'_, Snapshots> {
        let lock = &self.slot(family).snapshots;
        #[cfg(test)]
        if let Err(std::sync::TryLockError::WouldBlock) = lock.try_lock()
            && let Some(hook) = self.lock_contended.lock().unwrap().as_ref()
        {
            hook();
        }
        lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The newest complete snapshot of `family`.
    pub(crate) fn current(&self, family: Family) -> Option<Arc<dyn AnySnapshot>> {
        self.snapshots(family).current.clone()
    }

    /// The snapshot `snapshot_id` of `family`: the current one, or a replaced
    /// one still within its keep time. `None` once it expired.
    pub(crate) fn by_id(&self, family: Family, snapshot_id: &str) -> Option<Arc<dyn AnySnapshot>> {
        let found = {
            let mut snapshots = self.snapshots(family);
            match &snapshots.current {
                Some(current) if current.meta().snapshot_id() == snapshot_id => {
                    Some(Arc::clone(current))
                }
                _ => {
                    self.expire(&mut snapshots.superseded);
                    snapshots
                        .superseded
                        .iter()
                        .find(|(snap, _)| snap.meta().snapshot_id() == snapshot_id)
                        .map(|(snap, _)| Arc::clone(snap))
                }
            }
        };
        #[cfg(test)]
        if found.is_some()
            && let Some(hook) = self.after_lookup.lock().unwrap().as_ref()
        {
            hook();
        }
        found
    }

    /// Drops every kept snapshot of `family`, as their expiry would.
    #[cfg(test)]
    pub(crate) fn drop_kept(&self, family: Family) {
        self.snapshots(family).superseded.clear();
    }

    /// How many replaced snapshots of `family` are kept.
    #[cfg(test)]
    pub(crate) fn kept(&self, family: Family) -> usize {
        self.snapshots(family).superseded.len()
    }

    /// Keeps `snap` of `family` readable for the keep time after it is
    /// replaced, because a cursor now points into it. A snapshot replaced
    /// before its first cursor was handed out is kept from now on.
    pub(crate) fn hold_for_paging(&self, family: Family, snap: &Arc<dyn AnySnapshot>) {
        if !is_paged(family) {
            return;
        }
        let mut snapshots = self.snapshots(family);
        if snapshots
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, snap))
        {
            snapshots.current_paged = true;
        } else if !snapshots
            .superseded
            .iter()
            .any(|(kept, _)| Arc::ptr_eq(kept, snap))
        {
            snapshots
                .superseded
                .push_back((Arc::clone(snap), Instant::now()));
        }
    }

    /// The newest attempt to read `family`, published or not.
    pub(crate) fn last_attempt(&self, family: Family) -> Option<LastAttempt> {
        self.slot(family)
            .last_attempt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Resolves once every family in `enabled` has had one complete
    /// snapshot. It never resolves again to not-ready: a family that fails
    /// later keeps its last snapshot.
    pub(crate) fn wait_all_ready(&self, enabled: &[Family]) -> impl Future<Output = ()> + use<> {
        let mut ready = self.ready.subscribe();
        let enabled: BTreeSet<Family> = enabled.iter().copied().collect();
        async move {
            // The sender lives as long as the store; a dropped store means
            // shutdown, where readiness no longer matters.
            let _ = ready.wait_for(|ready| enabled.is_subset(ready)).await;
        }
    }

    fn expire(&self, superseded: &mut VecDeque<(Arc<dyn AnySnapshot>, Instant)>) {
        let now = Instant::now();
        while superseded
            .front()
            .is_some_and(|(_, replaced)| now.duration_since(*replaced) >= self.keep)
        {
            superseded.pop_front();
        }
    }

    fn record_attempt(&self, family: Family, attempt: LastAttempt) {
        *self
            .slot(family)
            .last_attempt
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(attempt);
    }
}

impl SnapshotSink for SnapshotStore {
    fn publish(&self, family: Family, snap: Arc<dyn AnySnapshot>) {
        let reader = snap.meta().reader;
        let mut snapshots = self.snapshots(family);
        let replaced = snapshots.current.replace(snap);
        let paged = std::mem::take(&mut snapshots.current_paged);
        #[cfg(test)]
        if let Some(hook) = self.mid_publish.lock().unwrap().as_ref() {
            hook();
        }
        if is_paged(family) {
            let superseded = &mut snapshots.superseded;
            if paged && let Some(replaced) = replaced {
                superseded.push_back((replaced, Instant::now()));
            }
            self.expire(superseded);
            if superseded.len() > self.max_kept {
                // Config validation sizes the budget for the reader interval,
                // so this means sweeps land faster than their schedule.
                // Paging clients still finish: nothing unexpired is dropped.
                tracing::warn!(
                    family = ?family,
                    kept = superseded.len(),
                    budget = self.max_kept,
                    "more replaced snapshots within the keep time than budgeted"
                );
            }
        }
        drop(snapshots);
        self.record_attempt(
            family,
            LastAttempt {
                reader,
                failure: None,
                at: crate::clock::now(),
            },
        );
        self.ready.send_if_modified(|ready| ready.insert(family));
    }

    fn failed(&self, reader: Reader, family: Family, kind: FailureKind, error: &str) {
        // Kept snapshots expire on time while sweeps fail too.
        self.expire(&mut self.snapshots(family).superseded);
        self.record_attempt(
            family,
            LastAttempt {
                reader,
                failure: Some((kind, error.to_string())),
                at: crate::clock::now(),
            },
        );
    }
}

impl CensusView for SnapshotStore {
    fn machine_ids(&self) -> Option<BTreeSet<String>> {
        let mut ids = BTreeSet::new();
        for family in [Family::Host, Family::Dpu] {
            let snap = self.current(family)?;
            let units = snap.as_any().downcast_ref::<FamilySnapshot<UnitsData>>()?;
            ids.extend(units.data.units.iter().map(|unit| unit.id.clone()));
        }
        Some(ids)
    }
}

#[cfg(test)]
mod tests {
    use std::task::Poll;

    use carbide_site_health_probe_api::{Source, Stage, Unit, UnitType};

    use super::*;
    use crate::snapshot::SweepMeta;

    fn store(max_kept: usize) -> SnapshotStore {
        SnapshotStore::new(&Paging {
            keep_seconds: 300,
            max_kept_snapshots: max_kept,
        })
    }

    fn snap(seq: u64, ids: &[&str]) -> Arc<dyn AnySnapshot> {
        let units = ids
            .iter()
            .map(|id| Unit {
                id: id.to_string(),
                unit_type: UnitType::Host,
                source: Source::NicoApi,
                stage: Stage::Ready,
                done: true,
                state: "Ready".to_string(),
                reason: None,
                bmc_ip: None,
                host: None,
                dpu: None,
                instance: None,
                expected: None,
            })
            .collect();
        Arc::new(FamilySnapshot {
            meta: SweepMeta {
                process: 1,
                seq,
                reader: Reader::Devices,
                start: SystemTime::now(),
                end: SystemTime::now(),
            },
            data: UnitsData::new(units, 0),
        })
    }

    fn id(seq: u64) -> String {
        format!("1-devices-{seq}")
    }

    /// A client takes a cursor into the current snapshot of `family`.
    fn page(store: &SnapshotStore, family: Family) {
        let current = store.current(family).expect("a current snapshot");
        store.hold_for_paging(family, &current);
    }

    fn pending(future: &mut std::pin::Pin<Box<impl Future<Output = ()>>>) -> bool {
        let waker = std::task::Waker::noop();
        future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(waker))
            .is_pending()
    }

    #[tokio::test]
    async fn ready_only_after_first_complete_sweep() {
        let store = store(6);
        let mut ready = Box::pin(store.wait_all_ready(&[Family::Host, Family::Dpu]));
        assert!(pending(&mut ready));
        store.failed(Reader::Devices, Family::Host, FailureKind::Rpc, "down");
        store.publish(Family::Dpu, snap(1, &[]));
        assert!(pending(&mut ready), "the host family has no snapshot yet");
        store.publish(Family::Host, snap(1, &["h1"]));
        assert_eq!(
            ready
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop())),
            Poll::Ready(())
        );
    }

    #[tokio::test]
    async fn ready_stays_after_later_failures() {
        let store = store(6);
        store.publish(Family::Host, snap(1, &["h1"]));
        store.wait_all_ready(&[Family::Host]).await;
        store.failed(Reader::Devices, Family::Host, FailureKind::Timeout, "slow");
        let mut again = Box::pin(store.wait_all_ready(&[Family::Host]));
        assert!(!pending(&mut again), "ready stays ready");
        assert_eq!(
            store.last_attempt(Family::Host).unwrap().failure,
            Some((FailureKind::Timeout, "slow".to_string()))
        );
    }

    #[tokio::test]
    async fn incomplete_family_keeps_previous_snapshot() {
        let store = store(6);
        store.publish(Family::Host, snap(1, &["h1", "h2"]));
        store.failed(
            Reader::Devices,
            Family::Host,
            FailureKind::Unresolved,
            "1 host",
        );
        let current = store.current(Family::Host).expect("kept");
        assert_eq!(current.meta().seq, 1);
        assert_eq!(store.machine_ids(), None, "no DPU snapshot yet");
        store.publish(Family::Dpu, snap(1, &["d1"]));
        assert_eq!(
            store.machine_ids(),
            Some(BTreeSet::from([
                "h1".to_string(),
                "h2".to_string(),
                "d1".to_string()
            ]))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cursor_survives_three_publications_within_five_minutes() {
        let store = store(6);
        store.publish(Family::Host, snap(1, &["h1"]));
        for seq in 2..=4 {
            page(&store, Family::Host);
            tokio::time::sleep(Duration::from_secs(60)).await;
            store.publish(Family::Host, snap(seq, &["h1"]));
        }
        // Snapshot 1 was replaced 3 minutes ago.
        assert_eq!(store.by_id(Family::Host, &id(1)).unwrap().meta().seq, 1);
        assert_eq!(store.by_id(Family::Host, &id(4)).unwrap().meta().seq, 4);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_sweep_then_overdue_tick_keeps_snapshot() {
        let store = store(4);
        store.publish(Family::Host, snap(1, &["h1"]));
        page(&store, Family::Host);
        // A slow sweep lands, and the skipped tick's sweep right after it.
        tokio::time::sleep(Duration::from_secs(200)).await;
        store.publish(Family::Host, snap(2, &["h1"]));
        page(&store, Family::Host);
        store.publish(Family::Host, snap(3, &["h1"]));
        tokio::time::sleep(Duration::from_secs(250)).await;
        assert!(
            store.by_id(Family::Host, &id(2)).is_some(),
            "snapshot 2 was replaced 250 s ago"
        );
        assert!(store.by_id(Family::Host, &id(1)).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn superseded_snapshot_expires_after_five_minutes() {
        let store = store(6);
        store.publish(Family::Host, snap(1, &["h1"]));
        page(&store, Family::Host);
        store.publish(Family::Host, snap(2, &["h1"]));
        tokio::time::sleep(Duration::from_secs(299)).await;
        assert!(store.by_id(Family::Host, &id(1)).is_some());
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(store.by_id(Family::Host, &id(1)).is_none(), "expired");
        assert!(store.by_id(Family::Host, &id(2)).is_some(), "current");

        // Families nobody pages through keep no replaced snapshot.
        store.publish(Family::RestTotals, snap(1, &[]));
        page(&store, Family::RestTotals);
        store.publish(Family::RestTotals, snap(2, &[]));
        assert!(store.by_id(Family::RestTotals, &id(1)).is_none());
    }

    #[test]
    fn concurrent_lookup_never_misses_a_replaced_snapshot() {
        use std::sync::mpsc;

        #[derive(Debug, PartialEq)]
        enum Lookup {
            /// The lookup found the snapshot lock held and is waiting.
            Blocked,
            Done(bool),
        }

        let store = Arc::new(store(6));
        store.publish(Family::Host, snap(1, &["h1"]));
        page(&store, Family::Host);
        let (tx, rx) = mpsc::channel::<Lookup>();
        let rx = Arc::new(Mutex::new(rx));
        *store.lock_contended.lock().unwrap() = Some(Box::new({
            let tx = tx.clone();
            move || {
                let _ = tx.send(Lookup::Blocked);
            }
        }));
        // Inside the publication of snapshot 2, after it became current and
        // before snapshot 1 is kept, a lookup of snapshot 1 starts on
        // another thread. The publication goes on only once the lookup has
        // reported in: either blocked on the snapshot lock (the publication
        // holds it), or done, which would mean it ran inside the interval.
        let inside = Arc::new(Mutex::new(None));
        *store.mid_publish.lock().unwrap() = Some(Box::new({
            let (store, rx, inside) =
                (Arc::downgrade(&store), Arc::clone(&rx), Arc::clone(&inside));
            move || {
                let store = store.upgrade().expect("store");
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let found = store.by_id(Family::Host, &id(1)).is_some();
                    let _ = tx.send(Lookup::Done(found));
                });
                *inside.lock().unwrap() = Some(rx.lock().unwrap().recv().expect("lookup reports"));
            }
        }));
        store.publish(Family::Host, snap(2, &["h1"]));
        assert_eq!(
            inside.lock().unwrap().take(),
            Some(Lookup::Blocked),
            "the lookup waited for the publication's lock"
        );
        assert_eq!(
            rx.lock().unwrap().recv().expect("lookup finishes"),
            Lookup::Done(true),
            "a cursor on snapshot 1 expired while snapshot 2 was published"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unexpired_snapshot_never_evicted() {
        let store = store(2);
        for seq in 1..=10 {
            store.publish(Family::Host, snap(seq, &["h1"]));
            page(&store, Family::Host);
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        for seq in 1..=10 {
            assert!(
                store.by_id(Family::Host, &id(seq)).is_some(),
                "snapshot {seq} is within its keep time"
            );
        }
    }

    #[test]
    fn replaced_snapshot_without_cursor_dropped() {
        let store = store(6);
        let first = snap(1, &["h1"]);
        let first_weak = Arc::downgrade(&first);
        store.publish(Family::Host, first);
        store.publish(Family::Host, snap(2, &["h1"]));
        assert!(store.by_id(Family::Host, &id(1)).is_none(), "no cursor");
        assert_eq!(store.kept(Family::Host), 0);
        assert_eq!(first_weak.strong_count(), 0, "freed");

        page(&store, Family::Host);
        store.publish(Family::Host, snap(3, &["h1"]));
        assert!(store.by_id(Family::Host, &id(2)).is_some(), "a cursor");
        assert_eq!(store.kept(Family::Host), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cursor_on_snapshot_replaced_meanwhile_keeps_it() {
        let store = store(6);
        store.publish(Family::Host, snap(1, &["h1"]));
        // A listing loaded snapshot 1, and snapshot 2 lands before the
        // listing hands out its cursor.
        let listed = store.current(Family::Host).expect("current");
        store.publish(Family::Host, snap(2, &["h1"]));
        store.hold_for_paging(Family::Host, &listed);
        store.hold_for_paging(Family::Host, &listed);
        assert_eq!(store.kept(Family::Host), 1, "kept once");
        tokio::time::sleep(Duration::from_secs(299)).await;
        assert!(store.by_id(Family::Host, &id(1)).is_some());
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(store.by_id(Family::Host, &id(1)).is_none(), "expired");
    }

    #[tokio::test(start_paused = true)]
    async fn kept_snapshot_expires_while_sweeps_fail() {
        let store = store(6);
        store.publish(Family::Host, snap(1, &["h1"]));
        page(&store, Family::Host);
        store.publish(Family::Host, snap(2, &["h1"]));
        tokio::time::sleep(Duration::from_secs(300)).await;
        store.failed(Reader::Devices, Family::Host, FailureKind::Rpc, "down");
        assert_eq!(store.kept(Family::Host), 0);
    }
}
