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

//! A soak of the state path at the size of a 250-rack site, on tokio's
//! paused clock: device sweeps of 4,500 hosts with two DPUs each, 2,250
//! switches, 2,000 power shelves and 250 racks, with host states and
//! versions changing between sweeps, clients paging between and across
//! sweeps, and the registry scraped. Retained state must plateau: kept
//! snapshots, snapshots still alive anywhere, the label sets the collector
//! tracks, the series count, and the heap bytes the test thread holds.
//!
//! Slow in a debug build, so ignored by default. Bytes are counted per
//! thread, and the test runs on its own thread, so other tests running
//! meanwhile do not change its count:
//! `cargo test -p carbide-site-health-probe soak -- --ignored --nocapture`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::http::StatusCode;
use carbide_site_health_probe_api::{Family, UnitsResponse};
use prometheus::core::{Collector, Desc};
use prometheus::proto::MetricFamily;

use crate::api::{ApiState, router};
use crate::config::{ReaderConfig, StateConfig};
use crate::e2e_tests::get;
use crate::framework::{ObservationRecorder, Probe};
use crate::gate::CallGate;
use crate::nico::fake::{self, FakeConnector, FakeNico, FakeSite};
use crate::readers::devices::{DEVICE_FAMILIES, DevicesReader};
use crate::snapshot::{AnySnapshot, FailureKind, Reader, SnapshotSink};
use crate::state::store::SnapshotStore;
use crate::state_metrics::{CountingSink, StateCollector};

/// Counts the heap bytes each thread holds: what it allocated minus what
/// it freed.
struct CountingAllocator;

thread_local! {
    static LIVE: Cell<i64> = const { Cell::new(0) };
}

fn count(delta: i64) {
    // Unavailable while the thread is torn down; those frees go uncounted.
    let _ = LIVE.try_with(|live| live.set(live.get() + delta));
}

/// Heap bytes the current thread holds.
fn live_bytes() -> i64 {
    LIVE.with(Cell::get)
}

// SAFETY: Every allocation operation delegates to `System` without changing pointer,
// layout, size, or return-value semantics. The added bookkeeping touches only a
// const-initialized thread-local `Cell` with no destructor, so it neither allocates
// nor unwinds, and `System`'s `GlobalAlloc` guarantees remain intact.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller guarantees that `layout` has nonzero size as required by
        // `GlobalAlloc::alloc`; it is forwarded unchanged to `System`.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            count(layout.size() as i64);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(-(layout.size() as i64));
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
            count(layout.size() as i64);
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
            count(new_size as i64 - layout.size() as i64);
        }
        new
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

const HOSTS: u32 = 4_500;
const SWITCHES: u32 = 2_250;
const SHELVES: u32 = 2_000;
const RACKS: u32 = 250;
const SWEEPS: u32 = 30;
const INTERVAL: Duration = Duration::from_secs(120);
/// Sweeps that fill the 15-minute retirement window, then the sweeps whose
/// retained state must not exceed theirs.
const WARM: std::ops::RangeInclusive<u32> = 9..=15;
const STEADY: std::ops::RangeInclusive<u32> = 16..=SWEEPS;
/// Hosts that leave Ready each sweep, a different block every sweep.
const CHURN: u32 = 50;
/// Hosts whose BMC version is new every sweep, so a `hosts` label set
/// appears and retires every sweep.
const CANARIES: u32 = 5;
const PAGE: u32 = 1_000;

fn site() -> FakeSite {
    let mut site = FakeSite::default();
    for i in 1..=HOSTS {
        site.add_host(fake::host(i, "Ready", &[2 * i - 1, 2 * i]));
        site.expected_machines
            .push(fake::expected_machine(i, Some(i)));
    }
    for i in 1..=SWITCHES {
        site.add_switch(fake::switch(i, "ready"));
        site.expected_switches
            .push(fake::expected_switch(i, Some(i)));
    }
    for i in 1..=SHELVES {
        site.add_shelf(fake::shelf(i, "ready"));
        site.expected_shelves.push(fake::expected_shelf(i, Some(i)));
    }
    for i in 1..=RACKS {
        site.add_rack(fake::rack(i, "ready"));
        site.expected_racks.push(fake::expected_rack(i));
        site.expected_rack_groups
            .push(fake::expected_rack_group(&format!("group-{i}")));
    }
    site
}

/// Moves the site to sweep `k`: one block of hosts is not Ready, and the
/// canaries report a BMC version no earlier sweep saw.
fn churn(site: &mut FakeSite, k: u32) {
    let blocks = HOSTS / CHURN;
    let block = |k: u32| (k % blocks) * CHURN + 1..=(k % blocks) * CHURN + CHURN;
    for i in 1..=HOSTS {
        let host = site
            .hosts
            .get_mut(&fake::host_id(i).to_string())
            .expect("host");
        host.state = if block(k).contains(&i) {
            "HostInitializing/WaitingForDiscovery".to_string()
        } else {
            "Ready".to_string()
        };
        if i <= CANARIES {
            host.bmc_version = Some(format!("canary-{k}"));
        }
    }
}

/// Stores every delivery and remembers each published snapshot weakly, so
/// the test sees whether a replaced snapshot is still alive anywhere.
struct Tracking {
    inner: CountingSink,
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
        self.inner.publish(family, snap);
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

/// What one sweep left behind.
#[derive(Debug, Clone, Copy)]
struct Retained {
    kept: usize,
    label_sets: usize,
    series: usize,
    bytes: i64,
}

#[tokio::test(start_paused = true)]
#[ignore = "slow: 30 sweeps at 250-rack size; run alone with --ignored"]
async fn soak_retained_state_plateaus() {
    let t0 = crate::clock::now();
    let started = tokio::time::Instant::now();
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
        published: Mutex::new(Vec::new()),
    });

    let nico = FakeNico::new(site());
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
    let app = router(ApiState::new(Arc::clone(&store), &cfg));
    let budget = cfg.paging.needed_for(INTERVAL);

    let mut retained: BTreeMap<u32, Retained> = BTreeMap::new();
    for k in 1..=SWEEPS {
        tokio::time::sleep_until(started + INTERVAL * (k - 1)).await;
        churn(&mut nico.site.lock().unwrap(), k);
        // A client starts paging hosts on the current snapshot and finishes
        // after the next one replaced it.
        let across = if k > 1 {
            Some(first_page(&app, "host").await)
        } else {
            None
        };

        reader
            .run(&ObservationRecorder::default())
            .await
            .expect("complete sweep");

        if let Some(cursor) = across {
            assert_eq!(
                page_all(&app, "host", Some(cursor)).await,
                (HOSTS - PAGE) as usize,
                "sweep {k}: paging across a publication"
            );
        }
        // A client pages every host of the new snapshot, another abandons
        // a DPU listing after its first page.
        assert_eq!(page_all(&app, "host", None).await, HOSTS as usize);
        let _abandoned = first_page(&app, "dpu").await;
        let mut series = 0;
        for _ in 0..2 {
            series = setup
                .registry
                .gather()
                .iter()
                .map(|family| family.get_metric().len())
                .sum();
        }
        nico.clear_calls();

        let alive = sink.alive();
        let mut kept = 0;
        for family in DEVICE_FAMILIES {
            let family_kept = store.kept(family);
            assert!(
                family_kept <= budget,
                "sweep {k}: {family:?} keeps {family_kept} replaced snapshots, budget {budget}"
            );
            // Only hosts and DPUs get cursors; replaced snapshots of the
            // other families are dropped at once.
            if !matches!(family, Family::Host | Family::Dpu) {
                assert_eq!(
                    family_kept, 0,
                    "sweep {k}: {family:?} keeps snapshots no cursor points into"
                );
            }
            assert_eq!(
                alive.get(&family).copied().unwrap_or_default(),
                1 + family_kept,
                "sweep {k}: {family:?} snapshots alive beyond the current and the kept ones"
            );
            kept += family_kept;
        }
        let now = Retained {
            kept,
            label_sets: collector.tracked_label_sets(),
            series,
            bytes: live_bytes(),
        };
        eprintln!(
            "sweep {k:>2}: kept {:>2} snapshots, {:>4} label sets, {:>4} series, {:>6.1} MiB",
            now.kept,
            now.label_sets,
            now.series,
            now.bytes as f64 / f64::from(1 << 20)
        );
        retained.insert(k, now);
    }

    type Measure = fn(&Retained) -> i64;
    let most = |range: std::ops::RangeInclusive<u32>, of: Measure| {
        range.map(|k| of(&retained[&k])).max().expect("sweeps")
    };
    let checks: [(&str, Measure, i64); 4] = [
        ("kept snapshots", |r| r.kept as i64, 0),
        ("label sets", |r| r.label_sets as i64, 0),
        ("series", |r| r.series as i64, 0),
        // String lengths change with the canary versions; a leaked
        // snapshot is megabytes.
        ("heap bytes", |r| r.bytes, 1 << 20),
    ];
    for (what, of, slack) in checks {
        let warm = most(WARM, of);
        let steady = most(STEADY, of);
        assert!(
            steady <= warm + slack,
            "{what} grew after the warm-up: {steady} after sweep {}, at most {warm} before",
            WARM.end()
        );
    }
}
