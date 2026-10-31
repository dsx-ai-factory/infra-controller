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

//! Leak soaks of the state path, on tokio's paused clock, at 250 racks
//! (4,500 hosts with two DPUs each, 2,250 switches, 2,000 power shelves) and
//! at 1,000 racks (18,000 hosts, 9,000 switches, 8,000 shelves, an instance
//! per host). Each runs 500 device sweeps while the site churns: host states,
//! models and BMC versions change, a block of hosts is removed and comes back
//! with the same IDs, a tenth of the hosts have open firmware requests, reads
//! fail, a request names a gone host, sweeps are cancelled, clients page
//! across publications and abandon cursors, and every publication is scraped
//! and JSON-encoded while it lands. Only the API's REST reader flag is
//! toggled: no REST inventory reader runs here.
//!
//! Pass rule: every sweep ends as planned (complete, the planned families
//! failed, or cancelled). Every family keeps exactly its current and its kept
//! snapshots alive within budget, each family that should publish has this
//! sweep's snapshot with the site's counts and changes, and the others keep
//! their last one. After warm-up the retained heap has a least-squares slope
//! of at most 1 KiB per sweep. The final quiescent state (the starting
//! fixture, cursors expired) holds at most 1 MiB and 0.1% more allocations
//! than the same state measured right after warm-up, and no more label sets
//! and series: retired labels are gone. Every sweep that publishes hosts
//! shares most of its units with the last host snapshot, and no sweep's peak
//! heap goes above the bound of its scale.
//!
//! Heap is what the test thread holds above the fake site, counted by the
//! allocator below. It is not RSS. Slow, so ignored by default. Run alone in
//! a release build:
//! `cargo test --release -p carbide-site-health-probe soak -- --ignored --nocapture --test-threads=1`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::http::StatusCode;
use carbide_site_health_probe_api::{Family, Unit, UnitsResponse};
use prometheus::core::{Collector, Desc};
use prometheus::proto::MetricFamily;
use rpc::forge::TenantState;

use crate::api::{ApiState, router};
use crate::config::{ReaderConfig, StateConfig};
use crate::e2e_tests::get;
use crate::framework::{ObservationRecorder, Probe};
use crate::gate::{CallError, CallGate};
use crate::nico::fake::{self, FakeConnector, FakeNico, FakeSite};
use crate::readers::devices::{DEVICE_FAMILIES, DevicesReader};
use crate::snapshot::{AnySnapshot, FailureKind, Reader, SnapshotSink};
use crate::state::store::SnapshotStore;
use crate::state_metrics::{CountingSink, RETIRE_AFTER, StateCollector, units_of};
use crate::summary::SummaryState;

/// Counts the heap bytes and allocations each thread holds: what it
/// allocated minus what it freed.
struct CountingAllocator;

thread_local! {
    static LIVE: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
    static ALLOCATIONS: Cell<i64> = const { Cell::new(0) };
}

fn count(bytes: i64, allocations: i64) {
    // Unavailable while the thread is torn down; those frees go uncounted.
    let _ = LIVE.try_with(|live| {
        let now = live.get() + bytes;
        live.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
    let _ = ALLOCATIONS.try_with(|live| live.set(live.get() + allocations));
}

/// Heap bytes the current thread holds.
fn live_bytes() -> i64 {
    LIVE.with(Cell::get)
}

/// Allocations the current thread holds.
fn live_allocations() -> i64 {
    ALLOCATIONS.with(Cell::get)
}

/// The most heap bytes the current thread held since the last call.
fn take_peak() -> i64 {
    PEAK.with(|peak| peak.replace(live_bytes()))
}

// SAFETY: Every allocation operation delegates to `System` without changing pointer,
// layout, size, or return-value semantics. The added bookkeeping touches only
// const-initialized thread-local `Cell`s with no destructor, so it neither allocates
// nor unwinds, and `System`'s `GlobalAlloc` guarantees remain intact.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller guarantees that `layout` has nonzero size as required by
        // `GlobalAlloc::alloc`; it is forwarded unchanged to `System`.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            count(layout.size() as i64, 1);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(-(layout.size() as i64), -1);
        // SAFETY: The caller guarantees that `ptr` is a live allocation from this
        // allocator and that `layout` matches it. Every allocation originates from
        // `System`, so forwarding both unchanged preserves provenance.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller guarantees that `layout` has nonzero size as required by
        // `GlobalAlloc::alloc_zeroed`; it is forwarded unchanged to `System`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            count(layout.size() as i64, 1);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The caller guarantees that `ptr` and `layout` describe a live
        // allocation from this allocator and that `new_size` satisfies
        // `GlobalAlloc::realloc`'s size and overflow requirements. Every allocation
        // originates from `System`, and all arguments are forwarded unchanged.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            count(new_size as i64 - layout.size() as i64, 0);
        }
        new
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// A soak's site: hosts (two DPUs each), switches, power shelves, racks,
/// and whether every host runs an instance.
#[derive(Clone, Copy)]
struct Scale {
    hosts: u32,
    switches: u32,
    shelves: u32,
    racks: u32,
    instances: bool,
    /// The most heap the soak's sweeps may hold at their peak, above the
    /// fake site. Measured, with room for allocator and ordering noise.
    max_peak: i64,
}

const RACKS_250: Scale = Scale {
    hosts: 4_500,
    switches: 2_250,
    shelves: 2_000,
    racks: 250,
    instances: false,
    max_peak: 45 << 20,
};

const RACKS_1000: Scale = Scale {
    hosts: 18_000,
    switches: 9_000,
    shelves: 8_000,
    racks: 1_000,
    instances: true,
    max_peak: 200 << 20,
};

const SWEEPS: u32 = 500;
/// Sweeps before the leak checks start: three retention windows and the
/// 15-minute label retirement, at least.
const WARMUP: u32 = 20;
/// Sweeps after the warm-up's quiescent check that refill the kept
/// snapshots and label sets before the slope is fitted.
const REWARM: u32 = 10;
const INTERVAL: Duration = Duration::from_secs(120);
/// Hosts that leave Ready each sweep, a different block every sweep.
const CHURN: u32 = 50;
/// Hosts removed each sweep. The block removed one sweep comes back the
/// next, with the same IDs.
const REMOVED: u32 = 20;
/// Hosts whose model and BMC version are new every sweep, so `hosts` label
/// sets appear and retire all the time.
const CANARIES: u32 = 5;
const PAGE: u32 = 1_000;
/// Every this many sweeps a switch read fails, which also fails the
/// expected family.
const FAIL_EVERY: u32 = 7;
/// Every this many sweeps the sweep is cancelled at its timeout.
const CANCEL_EVERY: u32 = 11;
/// Every this many sweeps a firmware request names a removed host, which
/// leaves the request family unresolved.
const UNJOINABLE_EVERY: u32 = 13;
/// One host in this many has an open firmware request, a different set
/// every sweep.
const REQUEST_ONE_IN: u32 = 10;
/// The retained heap may grow at most this much per sweep after warm-up.
const MAX_SLOPE: f64 = 1024.0;
/// What the final quiescent state may hold beyond the warm-up one.
const QUIESCENT_BYTES: i64 = 1 << 20;
const QUIESCENT_ALLOCATIONS: f64 = 0.001;
/// A host snapshot shares at least this many tenths of its units with the
/// last one. A sweep changes about a fifth of them: the open firmware
/// requests move to another tenth of the hosts every sweep.
const MIN_SHARED_TENTHS: usize = 7;

fn site(scale: Scale) -> FakeSite {
    let mut site = FakeSite::default();
    for i in 1..=scale.hosts {
        site.add_host(fake::host(i, "Ready", &[2 * i - 1, 2 * i]));
        site.expected_machines
            .push(fake::expected_machine(i, Some(i)));
        if scale.instances {
            site.add_instance(fake::instance(i, i, TenantState::Ready));
        }
    }
    for i in 1..=scale.switches {
        site.add_switch(fake::switch(i, "ready"));
        site.expected_switches
            .push(fake::expected_switch(i, Some(i)));
    }
    for i in 1..=scale.shelves {
        site.add_shelf(fake::shelf(i, "ready"));
        site.expected_shelves.push(fake::expected_shelf(i, Some(i)));
    }
    for i in 1..=scale.racks {
        site.add_rack(fake::rack(i, "ready"));
        site.expected_racks.push(fake::expected_rack(i));
        site.expected_rack_groups
            .push(fake::expected_rack_group(&format!("group-{i}")));
    }
    site
}

/// How a sweep is planned to end.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Outcome {
    Complete,
    /// The switch read fails, so switches and expected records keep their
    /// last snapshot. A request may also name a gone host.
    Failed {
        switches: bool,
        unjoinable: bool,
    },
    /// Cancelled at its timeout while still reading.
    Cancelled,
}

fn outcome(k: u32) -> Outcome {
    if k.is_multiple_of(CANCEL_EVERY) {
        return Outcome::Cancelled;
    }
    let (switches, unjoinable) = (
        k.is_multiple_of(FAIL_EVERY),
        k.is_multiple_of(UNJOINABLE_EVERY),
    );
    if switches || unjoinable {
        Outcome::Failed {
            switches,
            unjoinable,
        }
    } else {
        Outcome::Complete
    }
}

/// The families a sweep ending as `outcome` leaves with their last snapshot.
fn kept_back(outcome: Outcome) -> BTreeSet<Family> {
    match outcome {
        Outcome::Complete => BTreeSet::new(),
        Outcome::Cancelled => DEVICE_FAMILIES.into_iter().collect(),
        Outcome::Failed {
            switches,
            unjoinable,
        } => {
            let mut families = BTreeSet::new();
            if switches {
                families.extend([Family::Switch, Family::Expected]);
            }
            if unjoinable {
                families.insert(Family::HostFirmwareRequests);
            }
            families
        }
    }
}

/// The hosts block `size` of sweep `k` covers.
fn block(scale: Scale, size: u32, k: u32) -> std::ops::RangeInclusive<u32> {
    let at = (k % (scale.hosts / size)) * size;
    at + 1..=at + size
}

/// Moves the site to sweep `k`, or back to its starting fixture for `None`:
/// one block of hosts is not Ready, one block is gone, the canaries report a
/// model and a BMC version that change every sweep, and a tenth of the hosts
/// have open firmware requests. With `unjoinable` one request names a gone
/// host.
fn set_sweep(site: &mut FakeSite, scale: Scale, k: Option<u32>, unjoinable: bool) {
    let block = |size: u32| k.map(|k| block(scale, size, k));
    let (churned, removed) = (block(CHURN), block(REMOVED));
    for i in 1..=scale.hosts {
        let id = fake::host_id(i).to_string();
        if removed.as_ref().is_some_and(|r| r.contains(&i)) {
            if let Some(host) = site.hosts.remove(&id) {
                for dpu in host.dpu_ids {
                    site.dpus.remove(&dpu.to_string());
                }
            }
            continue;
        }
        let host = match site.hosts.get_mut(&id) {
            Some(host) => host,
            None => {
                site.add_host(fake::host(i, "Ready", &[2 * i - 1, 2 * i]));
                site.hosts.get_mut(&id).expect("host")
            }
        };
        host.state = if churned.as_ref().is_some_and(|c| c.contains(&i)) {
            "HostInitializing/WaitingForDiscovery".to_string()
        } else {
            "Ready".to_string()
        };
        if i <= CANARIES {
            let fresh = fake::host(i, "Ready", &[]);
            host.model = k.map_or(fresh.model, |k| Some(format!("model-{}", k % 60)));
            host.bmc_version = k.map_or(fresh.bmc_version, |k| Some(format!("canary-{k}")));
        }
    }
    site.firmware_requests.clear();
    if let Some(k) = k {
        site.firmware_requests.extend(
            (1..=scale.hosts)
                .filter(|i| (i + k).is_multiple_of(REQUEST_ONE_IN))
                .filter(|i| removed.as_ref().is_none_or(|r| !r.contains(i)))
                .map(|i| {
                    fake::firmware_request(i, if i % 2 == 0 { "Automated" } else { "AdminCli" })
                }),
        );
        if unjoinable && let Some(gone) = removed.as_ref().map(|r| *r.start()) {
            site.firmware_requests
                .push(fake::firmware_request(gone, "AdminCli"));
        }
    }
}

/// The open requests and present hosts of sweep `k`.
fn requests(scale: Scale, k: u32) -> usize {
    let removed = block(scale, REMOVED, k);
    (1..=scale.hosts)
        .filter(|i| (i + k).is_multiple_of(REQUEST_ONE_IN) && !removed.contains(i))
        .count()
}

/// Stores every delivery and remembers each published snapshot weakly, so
/// the test sees whether a replaced snapshot is still alive anywhere. Every
/// publication is scraped and its first page JSON-encoded while it lands.
struct Tracking {
    inner: CountingSink,
    registry: prometheus::Registry,
    published: Mutex<Vec<(Family, Weak<dyn AnySnapshot>)>>,
}

impl Tracking {
    /// Published snapshots still alive, per family. Forgets the dead ones.
    fn alive(&self) -> BTreeMap<Family, usize> {
        let mut published = self.published.lock().unwrap();
        published.retain(|(_, snap)| snap.strong_count() > 0);
        let mut alive = BTreeMap::new();
        for (family, _) in published.iter() {
            *alive.entry(*family).or_default() += 1;
        }
        alive
    }
}

impl SnapshotSink for Tracking {
    fn publish(&self, family: Family, snap: Arc<dyn AnySnapshot>) {
        self.published
            .lock()
            .unwrap()
            .push((family, Arc::downgrade(&snap)));
        if let Some(data) = units_of(snap.as_ref()) {
            let page: Vec<&Unit> = data
                .units
                .iter()
                .take(PAGE as usize)
                .map(|u| &**u)
                .collect();
            drop(serde_json::to_vec(&page).expect("encode a page"));
        }
        self.inner.publish(family, snap);
        drop(self.registry.gather());
    }

    fn failed(&self, reader: Reader, family: Family, kind: FailureKind, error: &str) {
        self.inner.failed(reader, family, kind, error);
    }
}

/// Registers the collector while the test keeps a handle to it.
struct Shared(Arc<StateCollector>);

impl Collector for Shared {
    fn desc(&self) -> Vec<&Desc> {
        self.0.desc()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        self.0.collect()
    }
}

/// Pages through `family` from `cursor` (or the start), and returns the
/// units read.
async fn page_all(app: &axum::Router, kind: &str, mut cursor: Option<String>) -> usize {
    let mut read = 0;
    loop {
        let uri = match &cursor {
            Some(cursor) => format!("/v1/units?type={kind}&limit={PAGE}&cursor={cursor}"),
            None => format!("/v1/units?type={kind}&limit={PAGE}"),
        };
        let (status, reply) = get::<UnitsResponse>(app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        let reply = reply.expect("units reply");
        read += reply.units.len();
        cursor = reply.next_cursor;
        if cursor.is_none() {
            return read;
        }
    }
}

/// The first page of `kind`, and its cursor.
async fn first_page(app: &axum::Router, kind: &str) -> String {
    let (status, reply) =
        get::<UnitsResponse>(app, &format!("/v1/units?type={kind}&limit={PAGE}")).await;
    assert_eq!(status, StatusCode::OK);
    reply.expect("units reply").next_cursor.expect("more pages")
}

/// What one sweep left behind, above the fake site.
#[derive(Debug, Clone, Copy)]
struct Retained {
    bytes: i64,
    /// The most heap held during the sweep, its paging and scrapes.
    peak: i64,
}

/// The least-squares slope of `points`, per step.
fn slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
    let (mx, my) = (sx / n, sy / n);
    let (num, den) = points.iter().fold((0.0, 0.0), |(num, den), (x, y)| {
        (num + (x - mx) * (y - my), den + (x - mx) * (x - mx))
    });
    num / den
}

/// What a quiescent state holds above the fake site.
#[derive(Debug, Clone, Copy)]
struct Quiescent {
    bytes: i64,
    allocations: i64,
    label_sets: usize,
    series: usize,
}

/// How many units of `new` are the same allocation as the unit of `old` with
/// the same ID. Both are sorted by ID, so it allocates nothing.
fn shared_units(old: &[Arc<Unit>], new: &[Arc<Unit>]) -> usize {
    let (mut i, mut j, mut shared) = (0, 0, 0);
    while i < old.len() && j < new.len() {
        match old[i].id.cmp(&new[j].id) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                shared += usize::from(Arc::ptr_eq(&old[i], &new[j]));
                i += 1;
                j += 1;
            }
        }
    }
    shared
}

/// The soak's moving parts.
struct Soak {
    scale: Scale,
    store: Arc<SnapshotStore>,
    sink: Arc<Tracking>,
    collector: Arc<StateCollector>,
    registry: prometheus::Registry,
    nico: Arc<FakeNico>,
    reader: DevicesReader,
    app: axum::Router,
    rest_off: axum::Router,
    /// Reader runs so far: the sequence number of the newest sweep.
    runs: Cell<u64>,
    /// Calls the last complete sweep made.
    calls: Cell<usize>,
    /// The units of the last host snapshot a sweep checked, to count what
    /// the next one shares. Empty after a quiescent sweep.
    prior_hosts: RefCell<Vec<Arc<Unit>>>,
    /// Heap the fake site and the setup hold before the first sweep.
    base: (i64, i64),
}

impl Soak {
    fn new(scale: Scale) -> Self {
        let t0 = crate::clock::now();
        let cfg = StateConfig::default();
        let setup = metrics_endpoint::new_metrics_setup("site-health-probe", "test", false)
            .expect("metrics setup");
        let store = Arc::new(SnapshotStore::new(&cfg.paging));
        let collector = Arc::new(StateCollector::new(
            Arc::clone(&store),
            t0,
            cfg.nico_api.host_read,
        ));
        setup
            .registry
            .register(Box::new(Shared(Arc::clone(&collector))))
            .expect("register");
        let sink = Arc::new(Tracking {
            inner: CountingSink::new(Arc::clone(&store), &setup.meter),
            registry: setup.registry.clone(),
            published: Mutex::new(Vec::new()),
        });
        let nico = FakeNico::new(site(scale));
        let reader = DevicesReader::new(
            ReaderConfig {
                enabled: true,
                interval: INTERVAL,
                timeout: Duration::from_secs(600),
            },
            cfg.nico_api.batch_size,
            Arc::new(FakeConnector(Arc::clone(&nico))),
            Arc::new(CallGate::new(
                "nico-api",
                cfg.caps.calls_per_second,
                cfg.caps.call_timeout,
                cfg.caps.max_backoff,
            )),
            Arc::clone(&sink) as Arc<dyn SnapshotSink>,
            t0,
        );
        let app = router(ApiState::new(
            Arc::clone(&store),
            &cfg,
            SummaryState::unused(&cfg.metrics_store),
        ));
        let mut off = cfg;
        off.readers.rest_inventory.enabled = false;
        let rest_off = router(ApiState::new(
            Arc::clone(&store),
            &off,
            SummaryState::unused(&off.metrics_store),
        ));
        // The meter's instruments are created lazily; one gather sets them up
        // before the baseline, which also holds the whole starting fixture.
        drop(setup.registry.gather());
        set_sweep(&mut nico.site.lock().unwrap(), scale, None, false);
        Self {
            scale,
            store,
            sink,
            collector,
            registry: setup.registry,
            nico,
            reader,
            app,
            rest_off,
            runs: Cell::new(0),
            calls: Cell::new(0),
            prior_hosts: RefCell::new(Vec::new()),
            base: (live_bytes(), live_allocations()),
        }
    }

    /// One reader run.
    async fn run(&self) -> eyre::Result<()> {
        self.runs.set(self.runs.get() + 1);
        self.reader.run(&ObservationRecorder::default()).await
    }

    /// Hosts in the current host snapshot.
    fn hosts_published(&self) -> usize {
        let current = self.store.current(Family::Host).expect("a host snapshot");
        units_of(current.as_ref()).expect("units").units.len()
    }

    /// One sweep `k` with its clients, failures and cancellations.
    async fn sweep(&self, k: u32) {
        let planned = outcome(k);
        let unjoinable = matches!(
            planned,
            Outcome::Failed {
                unjoinable: true,
                ..
            }
        );
        set_sweep(
            &mut self.nico.site.lock().unwrap(),
            self.scale,
            Some(k),
            unjoinable,
        );
        // A client starts paging hosts on the current snapshot and finishes
        // after the next one replaced it.
        let across = if k > 1 {
            let before = self.hosts_published();
            Some((first_page(&self.app, "host").await, before))
        } else {
            None
        };
        if matches!(planned, Outcome::Failed { switches: true, .. }) {
            self.nico.fail_next(
                "find_switches_by_ids",
                CallError::Status("gRPC Internal".to_string()),
            );
        }
        if planned == Outcome::Cancelled {
            // The scheduler drops a sweep at its timeout and records it.
            tokio::select! {
                _ = self.run() => panic!("sweep {k} ended before its timeout"),
                () = tokio::time::sleep(Duration::from_secs(5)) => self.reader.timed_out(),
            }
        } else {
            let result = self.run().await;
            assert_eq!(
                result.is_ok(),
                planned == Outcome::Complete,
                "sweep {k}: {result:?}"
            );
            if planned == Outcome::Complete {
                self.calls.set(self.nico.starts().len());
            }
        }
        self.check(k, planned);
        // The cursor still pages the snapshot it was taken on, whatever
        // replaced it since.
        if let Some((cursor, before)) = across {
            assert_eq!(
                page_all(&self.app, "host", Some(cursor)).await,
                before - PAGE as usize,
                "sweep {k}: paging across a publication"
            );
        }
        assert_eq!(
            page_all(&self.app, "host", None).await,
            self.hosts_published(),
            "sweep {k}: a full host listing"
        );
        // Abandoned listings.
        let _dpus = first_page(&self.app, "dpu").await;
        if self.scale.instances {
            let _instances = first_page(&self.app, "instance").await;
        }
        // Only the API's REST reader flag: no REST reader runs here.
        let (on, _) = get::<serde_json::Value>(&self.app, "/v1/config").await;
        assert_eq!(on, StatusCode::SERVICE_UNAVAILABLE, "no REST reader runs");
        let (off, _) = get::<serde_json::Value>(&self.rest_off, "/v1/config").await;
        assert_eq!(off, StatusCode::OK);
        drop(self.registry.gather());
        self.nico.clear_calls();
    }

    /// Checks every family after sweep `k` ended as `planned`: exactly its
    /// current and kept snapshots alive within budget, this sweep's snapshot
    /// with the site's counts where it should publish, its last snapshot and
    /// the planned failure where it should not.
    fn check(&self, k: u32, planned: Outcome) {
        let budget = StateConfig::default().paging.needed_for(INTERVAL);
        let alive = self.sink.alive();
        let failing = kept_back(planned);
        let present = (self.scale.hosts - REMOVED) as usize;
        for family in DEVICE_FAMILIES {
            let kept = self.store.kept(family);
            assert!(
                kept <= budget,
                "sweep {k}: {family:?} keeps {kept} replaced snapshots, budget {budget}"
            );
            if !matches!(family, Family::Host | Family::Dpu | Family::Instance) {
                assert_eq!(kept, 0, "sweep {k}: {family:?} keeps unpaged snapshots");
            }
            assert_eq!(
                alive.get(&family).copied().unwrap_or_default(),
                kept + 1,
                "sweep {k}: {family:?} snapshots alive besides the current and the kept ones"
            );
            let current = self.store.current(family).expect("a current snapshot");
            let fresh = current.meta().seq == self.runs.get();
            let attempt = self.store.last_attempt(family).expect("an attempt");
            if failing.contains(&family) {
                assert!(
                    !fresh,
                    "sweep {k}: {family:?} published although planned to fail"
                );
                let kind = attempt.failure.as_ref().map(|(kind, _)| *kind);
                let want = match (planned, family) {
                    (Outcome::Cancelled, _) => FailureKind::Timeout,
                    (_, Family::HostFirmwareRequests) => FailureKind::Unresolved,
                    _ => FailureKind::Rpc,
                };
                assert_eq!(kind, Some(want), "sweep {k}: {family:?}");
                continue;
            }
            assert!(fresh, "sweep {k}: {family:?} did not publish this sweep");
            assert!(
                attempt.failure.is_none(),
                "sweep {k}: {family:?} {attempt:?}"
            );
            let units = units_of(current.as_ref()).expect("units").units.as_slice();
            let want = match family {
                Family::Host => present,
                Family::Dpu => 2 * present,
                Family::Switch => self.scale.switches as usize,
                Family::PowerShelf => self.scale.shelves as usize,
                Family::Rack => self.scale.racks as usize,
                Family::Instance if self.scale.instances => self.scale.hosts as usize,
                Family::Instance => 0,
                Family::Expected => {
                    (self.scale.hosts
                        + self.scale.switches
                        + self.scale.shelves
                        + 2 * self.scale.racks) as usize
                }
                Family::HostFirmwareRequests => requests(self.scale, k),
                _ => units.len(),
            };
            assert_eq!(units.len(), want, "sweep {k}: {family:?} count");
            if family == Family::Host {
                // The changes of this sweep, not an older one.
                let unit = |i: u32| {
                    let id = fake::host_id(i).to_string();
                    units.iter().find(|u| u.id == id)
                };
                if let Some(canary) = unit(1) {
                    let model = canary.host.as_ref().and_then(|h| h.model.clone());
                    assert_eq!(model, Some(format!("model-{}", k % 60)), "sweep {k}");
                }
                let churned = *block(self.scale, CHURN, k).start();
                if let Some(unit) = unit(churned) {
                    assert_eq!(unit.state, "HostInitializing", "sweep {k}: host {churned}");
                }
                // Unchanged hosts are the last snapshot's allocations. Without
                // sharing every kept snapshot is a full copy.
                let mut prior = self.prior_hosts.borrow_mut();
                if !prior.is_empty() {
                    let shared = shared_units(&prior, units);
                    assert!(
                        shared * 10 >= units.len() * MIN_SHARED_TENTHS,
                        "sweep {k}: {shared} of {} host units shared with the last snapshot",
                        units.len()
                    );
                }
                prior.clear();
                prior.extend(units.iter().cloned());
            }
        }
    }

    /// Heap bytes and allocations in the starting fixture's quiescent state:
    /// one clean sweep of it, every cursor and retired label expired, the
    /// lazy expiry triggered and the registry gathered.
    async fn quiescent(&self) -> Quiescent {
        self.prior_hosts.borrow_mut().clear();
        set_sweep(&mut self.nico.site.lock().unwrap(), self.scale, None, false);
        self.run().await.expect("clean sweep");
        let keep = Duration::from_secs(StateConfig::default().paging.keep_seconds);
        tokio::time::sleep(keep.max(RETIRE_AFTER) + Duration::from_secs(1)).await;
        for family in DEVICE_FAMILIES {
            let _ = self.store.by_id(family, "expire-now");
        }
        drop(self.registry.gather());
        let series = self
            .registry
            .gather()
            .iter()
            .map(|family| family.get_metric().len())
            .sum();
        self.nico.clear_calls();
        Quiescent {
            bytes: live_bytes() - self.base.0,
            allocations: live_allocations() - self.base.1,
            label_sets: self.collector.tracked_label_sets(),
            series,
        }
    }
}

/// Runs the soak at `scale` and checks the pass rule. Returns each sweep's
/// retained heap and peak.
async fn soak(name: &str, scale: Scale) -> Vec<Retained> {
    // Allocated before the baseline, so the measurements never change it.
    let mut retained: Vec<Retained> = Vec::with_capacity(SWEEPS as usize);
    let soak = Soak::new(scale);
    let mib = |bytes: i64| bytes as f64 / f64::from(1 << 20);
    eprintln!("{name}: fake site and setup {:.1} MiB", mib(soak.base.0));
    let mut warm = None;
    // Like the scheduler, sweeps start at least an interval apart.
    let mut next = tokio::time::Instant::now();
    for k in 1..=SWEEPS {
        tokio::time::sleep_until(next).await;
        next = tokio::time::Instant::now() + INTERVAL;
        take_peak();
        soak.sweep(k).await;
        let now = Retained {
            bytes: live_bytes() - soak.base.0,
            peak: take_peak() - soak.base.0,
        };
        if k == 1 || k.is_multiple_of(50) || k == WARMUP {
            eprintln!(
                "{name} sweep {k:>3}: {:>6.1} MiB retained, peak {:>6.1} MiB, {} label sets, \
                 {} calls in a complete sweep",
                mib(now.bytes),
                mib(now.peak),
                soak.collector.tracked_label_sets(),
                soak.calls.get(),
            );
        }
        retained.push(now);
        if k == WARMUP {
            warm = Some(soak.quiescent().await);
            next = tokio::time::Instant::now();
        }
    }
    let warm = warm.expect("warm-up baseline");
    let last = soak.quiescent().await;
    let peak = retained.iter().map(|r| r.peak).max().expect("sweeps");
    let steady: Vec<(f64, f64)> = (WARMUP + REWARM..=SWEEPS)
        .map(|k| (f64::from(k), retained[k as usize - 1].bytes as f64))
        .collect();
    let slope = slope(&steady);
    eprintln!(
        "{name}: peak {:.1} MiB, slope {slope:.1} bytes per sweep",
        mib(peak)
    );
    for (when, q) in [("after warm-up", warm), ("at the end", last)] {
        eprintln!(
            "{name}: quiescent {when}: {:.1} MiB, {} allocations, {} label sets, {} series",
            mib(q.bytes),
            q.allocations,
            q.label_sets,
            q.series
        );
    }
    assert!(
        slope <= MAX_SLOPE,
        "{name}: retained heap grows {slope:.1} bytes per sweep after warm-up"
    );
    assert!(
        last.bytes <= warm.bytes + QUIESCENT_BYTES,
        "{name}: quiescent heap {} bytes at the end, {} after warm-up",
        last.bytes,
        warm.bytes
    );
    assert!(
        last.allocations as f64 <= warm.allocations as f64 * (1.0 + QUIESCENT_ALLOCATIONS),
        "{name}: {} quiescent allocations at the end, {} after warm-up",
        last.allocations,
        warm.allocations
    );
    assert!(warm.series > 0, "{name}: no series to compare");
    assert!(
        last.label_sets <= warm.label_sets,
        "{name}: {} label sets at the end, {} after warm-up",
        last.label_sets,
        warm.label_sets
    );
    assert!(
        last.series <= warm.series,
        "{name}: {} series at the end, {} after warm-up",
        last.series,
        warm.series
    );
    assert!(
        peak <= scale.max_peak,
        "{name}: peak heap {peak} bytes, bound {}",
        scale.max_peak
    );
    retained
}

#[tokio::test(start_paused = true)]
#[ignore = "slow: 500 sweeps at 250 racks; run alone in release with --ignored"]
async fn soak_250_racks_full_read() {
    soak("250 racks, full read", RACKS_250).await;
}

#[tokio::test(start_paused = true)]
#[ignore = "slow: 500 sweeps at 1,000 racks; run alone in release with --ignored"]
async fn soak_1000_racks_full_read() {
    soak("1,000 racks, full read", RACKS_1000).await;
}
