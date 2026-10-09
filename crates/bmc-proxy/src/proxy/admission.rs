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

//! Per-BMC admission: how many requests the proxy sends to each BMC at a
//! time, and which waiting request goes next.
//!
//! A request takes a slot at its BMC before it is sent when its class sets
//! `max_in_flight` or has a breaker, or when `[admission]
//! max_in_flight_per_bmc` is set. It holds the slot until the proxy has
//! passed the BMC's response on to the caller, or the exchange has failed,
//! and for at most the bound its caller gives from the grant on, after which
//! the slot goes to the next waiting request even if a caller that stopped
//! reading still holds its response. A request that finds no free slot waits
//! in its class's queue at that BMC. A freed slot goes to the highest-priority
//! class that has a request waiting and is under its own `max_in_flight`;
//! classes of equal priority take turns, and each class's requests go in the
//! order they came. A request is refused with `429` when its class's queue at
//! that BMC is full of requests still waiting, or when no slot frees before
//! its deadline, and with `503` when the proxy already tracks [`MAX_BMCS`]
//! BMCs and this is another, when its class's breaker at the BMC is open, or
//! when the proxy is shutting down. Limits are per proxy replica: with two
//! replicas, a BMC can receive twice a limit.
//!
//! A class with a breaker has one at each BMC, the dispatcher's. An exchange
//! the BMC fails in a way the class's `trip_on` names counts against it; once
//! enough of the class's recent exchanges with the BMC failed, the breaker
//! opens: the proxy refuses the class's new requests to that BMC with `503`
//! for the cool-down, then lets one through, whose outcome closes the breaker
//! or opens it again. Other classes' requests to the BMC go on as before. The
//! breaker counts every freed slot as an exchange, so a request that stopped
//! waiting, or got its slot too late to use it, counts as one that succeeded:
//! such requests dilute the failures, and one can be the request let through.
//!
//! A class with a latency target holds back the classes without one: at
//! each BMC, every window of its answered requests that misses the target
//! cuts the slots the others may hold there, down to a floor, and every
//! window that meets it gives one back, up to the per-BMC limit; see
//! [`Share`]. A request's latency runs from its arrival at the proxy to the
//! BMC's response headers, or, without an answer, to its end; see
//! [`Latency`].
//!
//! Each BMC in use has its own `nv_redfish_dispatcher` runtime, driven by a
//! task of its own, as nico-api's admission drives one for its callers.
//! BMCs are independent, so no scheduling state is shared between them, and
//! scheduling a request touches only its own BMC's runtime. The dispatcher
//! never runs a request: for each request it runs a small grant that hands
//! the request its slot, then holds the BMC's capacity until the slot is
//! dropped or its time is up:
//!
//! ```text
//! Runtime                                    one per BMC in use
//! └─ BmcClasses(max_in_flight_per_bmc)       by class priority
//!    └─ CircuitBreaker                       one per class; never opens without a breaker
//!       └─ BoundedConcurrency(class max_in_flight)
//!          └─ BoundedQueue(class max_queued) first come, first served
//! ```

use std::collections::HashMap;
use std::net::IpAddr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use bytes::Bytes;
use carbide_instrument::{Event, LabelValue, emit};
use http::StatusCode;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use nv_redfish_dispatcher::schedulers::{
    AdmissionContext, AdmissionDecision, AdmissionPolicy, BoundedConcurrency, BoundedQueue,
    BoundedQueueProducer, BreakerState, CircuitBreaker, CircuitBreakerConfig, Fifo,
};
use nv_redfish_dispatcher::{
    BoundedQueueBuilder, ClockConfig, Completion, EnqueueOutcome, FutureWork, QueueEventSink,
    Readiness, Runtime, RuntimeConfig, RuntimeHandle, RuntimeOutput, ScheduledWork, Scheduler,
};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::class::{ClassName, ClassTable, RequestClass};
use crate::config::{AdmissionConfig, BreakerConfig, SloConfig, SloTarget};
use crate::proxy::slo::{Share, Verdict};

type Work = FutureWork<(), ExchangeFailed>;
type ClassQueue = BoundedQueue<Work, Waiting, GaveUpFirst, Fifo>;
type ClassProducer = BoundedQueueProducer<Work, Waiting, GaveUpFirst, Fifo>;
/// A class's breaker at one BMC, over the class's limit and queue there.
type ClassNode = CircuitBreaker<Work, BoundedConcurrency<Work, ClassQueue>>;

/// How long a BMC's runtime outlives its last request. A BMC polled every
/// few seconds keeps it; one left alone for a minute gives it up, and its
/// next request starts a new one.
const IDLE_BMC_TIMEOUT: Duration = Duration::from_secs(60);

/// How often idle BMCs' runtimes are stopped, and stopped ones collected.
/// With [`IDLE_BMC_TIMEOUT`], an unused runtime stops between one and one
/// and a half minutes after its last request.
const IDLE_BMC_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Most BMCs the proxy keeps runtimes for at a time. Only IPs nico-api has
/// credentials for get one, and each costs from about 6 to 20 KiB, by the
/// number of classes that take slots and their `max_queued`, and up to 1 KiB
/// more for each class's breaker `window`. Sized, like the proxy's caches,
/// far above any realistic fleet.
const MAX_BMCS: usize = 100_000;

/// Most places a class's queue keeps, beyond its `max_queued`, for requests
/// on their way to a free slot. Every request passes through its class's
/// queue, so without them a burst would be refused while slots were free.
const MAX_PASSING_THROUGH: usize = 128;

/// Why a request got no slot at its BMC; the `reason` on
/// `carbide_bmc_proxy_admission_refused_total`.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum Refused {
    #[error("too many requests of this class are waiting for this BMC")]
    QueueFull,
    #[error("no slot at this BMC came free within the request's upstream budget")]
    Timeout,
    #[error("the proxy is tracking too many BMCs to admit a request for another")]
    TooManyBmcs,
    #[error("the proxy is shutting down")]
    ShuttingDown,
    #[error(
        "this BMC failed too many recent requests of this class; the proxy is holding off on them"
    )]
    BreakerOpen,
}

impl Refused {
    /// What the caller is answered: `429` when the proxy's per-BMC limits
    /// turned the request away, `503` when the proxy could not take it or the
    /// class's breaker at the BMC is open. Either way, the BMC never received
    /// the request.
    pub(super) fn status(self) -> StatusCode {
        match self {
            Self::QueueFull | Self::Timeout => StatusCode::TOO_MANY_REQUESTS,
            Self::TooManyBmcs | Self::ShuttingDown | Self::BreakerOpen => {
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    }
}

/// A request got its slot at a BMC. Metric-only.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_admission_granted",
    metric_name = "carbide_bmc_proxy_admission_wait_milliseconds",
    component = "nico-bmc-proxy",
    log = off,
    metric = histogram,
    describe = "Time requests that got a slot at their BMC waited for it, by request class; only classes that take slots are observed, and requests refused or abandoned while waiting are not"
)]
struct AdmissionGranted {
    #[label]
    class: ClassName,
    #[observation]
    waited: Duration,
}

/// The proxy refused a request without sending it: for want of a slot at
/// its BMC, or because its class's breaker there is open. Metric-only:
/// refusals come as fast as callers retry, and the caller's answer names the
/// reason.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_admission_refused",
    metric_name = "carbide_bmc_proxy_admission_refused_total",
    component = "nico-bmc-proxy",
    log = off,
    metric = counter,
    describe = "Number of requests the proxy refused without sending them, for want of a slot at their BMC or because their class's breaker there was open, by request class and reason (queue_full, timeout, too_many_bmcs, breaker_open, shutting_down)"
)]
struct AdmissionRefused {
    #[label]
    class: ClassName,
    #[label]
    reason: Refused,
}

/// A class's circuit breaker at a BMC opened.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_breaker_opened",
    metric_name = "carbide_bmc_proxy_breaker_opened_total",
    component = "nico-bmc-proxy",
    log = warn,
    metric = counter,
    message = "BMC circuit breaker opened; refusing the class's requests to the BMC until a probe succeeds",
    describe = "Number of times a request class's circuit breaker at a BMC opened: the BMC failed too many of the class's recent exchanges, by request class"
)]
struct BreakerOpened {
    #[label]
    class: ClassName,
    #[context]
    bmc_ip_address: String,
}

/// A class with a latency target missed it at a BMC over its last window of
/// requests there, which cut the slots the classes without one may hold.
/// Metric-only: the counter shows how often a BMC falls behind.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_slo_missed",
    metric_name = "carbide_bmc_proxy_slo_missed_total",
    component = "nico-bmc-proxy",
    log = off,
    metric = counter,
    describe = "Number of windows of requests in which a class with a latency target missed it at a BMC, each cutting the slots classes without a target may hold there, by request class"
)]
struct SloMissed {
    #[label]
    class: ClassName,
}

/// The failure of an exchange the BMC failed, as its grant reports it to the
/// BMC's circuit breaker.
struct ExchangeFailed;

/// A queued request.
struct Waiting {
    /// The request's claim to its place: gone once it stops waiting.
    claim: Weak<()>,
    /// How many requests the queue may hold for this one to join it.
    room: usize,
    /// How long the request took, for a class with a latency target.
    latency: Option<Arc<Latency>>,
}

/// How long a request of a class with a latency target took, from its
/// arrival at the proxy: until the BMC answered, or until the request ended
/// without an answer. Unset when the request could tell nothing of the BMC.
/// Set once, before the request's grant ends, and read by the BMC's runtime
/// when it does.
struct Latency {
    arrived: Instant,
    taken: OnceLock<Option<Duration>>,
}

impl Latency {
    fn new(arrived: Instant) -> Self {
        Self {
            arrived,
            taken: OnceLock::new(),
        }
    }

    fn measure(&self) {
        self.taken.set(Some(self.arrived.elapsed())).ok();
    }

    fn leave_unmeasured(&self) {
        self.taken.set(None).ok();
    }

    fn taken(&self) -> Option<Duration> {
        self.taken.get().copied().flatten()
    }
}

/// Measures a request that stops waiting for its slot. Dropped before the
/// request's grant can end, so the BMC's runtime finds the latency set.
struct Unanswered(Option<Arc<Latency>>);

impl Drop for Unanswered {
    fn drop(&mut self) {
        if let Some(latency) = &self.0 {
            latency.measure();
        }
    }
}

/// Admits a request while its class's queue holds fewer than the request's
/// `room`. Past that, it makes room by evicting a request that has stopped
/// waiting, and refuses the newcomer only when every queued request still
/// waits.
struct GaveUpFirst;

impl AdmissionPolicy<Work, Waiting> for GaveUpFirst {
    fn decide(
        &mut self,
        context: AdmissionContext<'_, Work, Waiting>,
        incoming: &ScheduledWork<Work, Waiting>,
    ) -> AdmissionDecision {
        if context.depth() < incoming.meta.room.min(context.capacity()) {
            return AdmissionDecision::Admit;
        }
        context
            .entries()
            .find(|entry| entry.work().meta.claim.strong_count() == 0)
            .map_or(AdmissionDecision::Reject, |entry| {
                AdmissionDecision::EvictAndAdmit { id: entry.id() }
            })
    }
}

/// The limits of a class whose requests take slots.
struct SlotClass {
    name: ClassName,
    priority: u8,
    max_in_flight: NonZeroU32,
    max_queued: NonZeroUsize,
    breaker: Option<CircuitBreakerConfig>,
    slo: Option<SloTarget>,
}

/// A BMC in use: its class queues, and the runtime serving them.
struct Bmc {
    /// One per class that takes slots, in the order of
    /// [`Admission::classes`].
    queues: Vec<ClassProducer>,
    runtime: RuntimeHandle<(), ExchangeFailed, Waiting>,
    /// Stops the BMC's runtime.
    stop: CancellationToken,
    last_used: Instant,
    /// The slots the classes without a latency target may hold, as the
    /// runtime last set it, when some class has a target.
    others_limit: Option<Arc<AtomicUsize>>,
}

impl Bmc {
    fn breaker(&self, class: usize) -> BreakerState {
        self.runtime
            .with_root(|root: &BmcClasses| root.classes[class].node.state())
            .expect("a BMC's runtime root is its classes")
    }

    fn breaker_open(&self) -> bool {
        self.runtime
            .with_root(|root: &BmcClasses| {
                root.classes
                    .iter()
                    .any(|class| matches!(class.node.state(), BreakerState::Open { .. }))
            })
            .expect("a BMC's runtime root is its classes")
    }

    /// Whether class `class`'s breaker at this BMC lets a new request in:
    /// always when closed, and when half-open only as its probe, once none of
    /// the class's requests is queued at or sent to the BMC.
    fn breaker_admits(&self, class: usize) -> bool {
        match self.breaker(class) {
            BreakerState::Closed => true,
            BreakerState::Open { .. } | BreakerState::HalfOpen { probing: true } => false,
            BreakerState::HalfOpen { probing: false } => {
                let stats = self.queues[class].stats();
                stats.depth == 0 && stats.in_flight == 0
            }
        }
    }
}

pub(super) struct Admission {
    /// Every class whose requests take slots.
    classes: Vec<SlotClass>,
    max_in_flight_per_bmc: NonZeroUsize,
    slo: SloConfig,
    bmcs: Mutex<HashMap<IpAddr, Bmc>>,
    /// The BMCs' runtimes.
    runtimes: Mutex<JoinSet<()>>,
    shutdown: CancellationToken,
}

impl Admission {
    /// The admission for `classes` under `config`. When some class takes
    /// slots, the sweep of idle BMCs runs on `join_set` until `shutdown`,
    /// then waits for the BMCs' runtimes to stop. `shutdown` also refuses
    /// every request waiting for a slot, and every request after it.
    pub(super) fn start(
        classes: &ClassTable,
        config: &AdmissionConfig,
        shutdown: CancellationToken,
        join_set: &mut JoinSet<()>,
    ) -> Arc<Self> {
        let per_bmc = config.max_in_flight_per_bmc;
        let classes: Vec<SlotClass> = classes
            .iter()
            .filter(|class| {
                per_bmc.is_some() || class.breaker.is_some() || class.max_in_flight.is_some()
            })
            .map(|class| SlotClass {
                name: class.name.clone(),
                priority: class.priority,
                max_in_flight: class.max_in_flight.unwrap_or(NonZeroU32::MAX),
                max_queued: class.max_queued,
                breaker: class.breaker.as_ref().map(CircuitBreakerConfig::from),
                slo: class.slo,
            })
            .collect();
        let admission = Arc::new(Self {
            max_in_flight_per_bmc: per_bmc.map_or(NonZeroUsize::MAX, |max| {
                NonZeroUsize::try_from(max).expect("a u32 fits in a usize")
            }),
            slo: config.slo,
            bmcs: Mutex::new(HashMap::new()),
            runtimes: Mutex::new(JoinSet::new()),
            shutdown: shutdown.clone(),
            classes,
        });
        if !admission.classes.is_empty() {
            let sweeping = Arc::clone(&admission);
            join_set
                .build_task()
                .name("bmc admission idle sweep")
                .spawn(async move { sweeping.sweep_idle(shutdown).await })
                .expect("spawning the bmc admission sweep must succeed");
        }
        admission
    }

    /// A slot at `bmc` for a request of `class` that `arrived` at the proxy,
    /// waiting for it until `deadline`, and held at most `hold_for` once
    /// granted. A class that takes no slots gets one at once.
    pub(super) async fn acquire(
        &self,
        bmc: IpAddr,
        class: &RequestClass,
        arrived: Instant,
        deadline: Instant,
        hold_for: Duration,
    ) -> Result<Slot, Refused> {
        let Some(index) = self
            .classes
            .iter()
            .position(|slot_class| slot_class.name == class.name)
        else {
            return Ok(Slot::free());
        };
        let started = Instant::now();
        let granted = self
            .wait_for_slot(bmc, index, arrived, deadline, hold_for)
            .await;
        match &granted {
            Ok(_) => emit(AdmissionGranted {
                class: class.name.clone(),
                waited: started.elapsed(),
            }),
            Err(reason) => emit(AdmissionRefused {
                class: class.name.clone(),
                reason: *reason,
            }),
        }
        granted
    }

    async fn wait_for_slot(
        &self,
        bmc: IpAddr,
        class: usize,
        arrived: Instant,
        deadline: Instant,
        hold_for: Duration,
    ) -> Result<Slot, Refused> {
        let waiting = Arc::new(());
        let latency = self.classes[class]
            .slo
            .is_some()
            .then(|| Arc::new(Latency::new(arrived)));
        let (grant_tx, grant_rx) = oneshot::channel();
        self.enqueue(
            bmc,
            class,
            Waiting {
                claim: Arc::downgrade(&waiting),
                room: 0,
                latency: latency.clone(),
            },
            grant(grant_tx, hold_for),
        )?;
        // Declared after `grant_rx`, so dropped before it.
        let unanswered = Unanswered(latency);
        // A request that gives up drops `waiting`, and its queued grant,
        // dequeued later or evicted to make room, finds nobody to hand the
        // slot to. Shutdown comes first, so a request queued after it is
        // refused at once.
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => Err(Refused::ShuttingDown),
            granted = grant_rx => {
                // The grant is dropped unsent only with the BMC's runtime.
                let release = granted.map_err(|_stopped| Refused::ShuttingDown)?;
                // A slot that comes as the budget runs out leaves the
                // exchange no time; refusing it tells the caller why.
                if Instant::now() >= deadline {
                    drop(unanswered);
                    return Err(Refused::Timeout);
                }
                let mut unanswered = unanswered;
                Ok(Slot {
                    release: Some(release),
                    failed: false,
                    latency: unanswered.0.take(),
                })
            }
            () = tokio::time::sleep_until(deadline) => Err(Refused::Timeout),
        }
    }

    /// Queues `grant`, for the request `waiting`, in the queue of class
    /// `class` at `bmc`, starting the BMC's runtime on its first request.
    fn enqueue(
        &self,
        bmc: IpAddr,
        class: usize,
        mut waiting: Waiting,
        grant: impl Future<Output = Result<Vec<()>, ExchangeFailed>> + Send + 'static,
    ) -> Result<(), Refused> {
        let mut bmcs = lock(&self.bmcs);
        if !bmcs.contains_key(&bmc) && bmcs.len() >= MAX_BMCS {
            return Err(Refused::TooManyBmcs);
        }
        let bmc = bmcs.entry(bmc).or_insert_with(|| self.start_bmc(bmc));
        bmc.last_used = Instant::now();
        if self.classes[class].breaker.is_some() && !bmc.breaker_admits(class) {
            return Err(Refused::BreakerOpen);
        }
        // Requests queue one at a time under this lock. The runtime grants
        // and frees slots meanwhile, so the room can be off by a slot.
        waiting.room = self.room(bmc, class);
        match bmc.queues[class].try_push(ScheduledWork::new(waiting, Box::pin(grant))) {
            EnqueueOutcome::Admitted | EnqueueOutcome::Evicted { .. } => Ok(()),
            EnqueueOutcome::Rejected(_) => Err(Refused::QueueFull),
            EnqueueOutcome::Closed(_) => Err(Refused::ShuttingDown),
        }
    }

    /// How many requests class `class`'s queue at `bmc` may hold: its
    /// `max_queued`, and one for each slot free for the class now, within the
    /// share of the classes without a latency target when it has none.
    fn room(&self, bmc: &Bmc, class: usize) -> usize {
        let in_flight: Vec<usize> = bmc
            .queues
            .iter()
            .map(|queue| queue.stats().in_flight)
            .collect();
        let max_in_flight =
            usize::try_from(self.classes[class].max_in_flight.get()).unwrap_or(usize::MAX);
        let mut free = max_in_flight.saturating_sub(in_flight[class]).min(
            self.max_in_flight_per_bmc
                .get()
                .saturating_sub(in_flight.iter().sum()),
        );
        if let Some(others_limit) = &bmc.others_limit
            && self.classes[class].slo.is_none()
        {
            let others_in_flight: usize = self
                .classes
                .iter()
                .zip(&in_flight)
                .filter(|(class, _)| class.slo.is_none())
                .map(|(_, in_flight)| in_flight)
                .sum();
            free = free.min(
                others_limit
                    .load(Ordering::Relaxed)
                    .saturating_sub(others_in_flight),
            );
        }
        self.classes[class].max_queued.get().saturating_add(free)
    }

    /// The queues of the BMC at `address`, and its runtime started on its
    /// own task.
    fn start_bmc(&self, address: IpAddr) -> Bmc {
        let (root, queues) =
            BmcClasses::new(address, &self.classes, self.max_in_flight_per_bmc, self.slo);
        let others_limit = root.share.as_ref().map(Share::published);
        let runtime = Runtime::new(
            RuntimeConfig {
                global_max_in_flight: NonZeroUsize::MAX,
                clock: ClockConfig::Wallclock,
            },
            root,
        );
        let handle = runtime.handle();
        let stop = self.shutdown.child_token();
        lock(&self.runtimes)
            .build_task()
            .name("bmc admission runtime")
            .spawn(drive(runtime, stop.clone()))
            .expect("spawning a bmc admission runtime must succeed");
        Bmc {
            queues,
            runtime: handle,
            stop,
            last_used: Instant::now(),
            others_limit,
        }
    }

    async fn sweep_idle(&self, shutdown: CancellationToken) {
        let mut interval = tokio::time::interval(IDLE_BMC_SWEEP_INTERVAL);
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    self.stop_idle_bmcs(Instant::now());
                    let stopped: Vec<_> = {
                        let mut runtimes = lock(&self.runtimes);
                        std::iter::from_fn(|| runtimes.try_join_next()).collect()
                    };
                    // A BMC whose runtime panicked could never be served
                    // again; the panic takes the proxy down instead.
                    for stopped in stopped {
                        if let Err(error) = stopped
                            && error.is_panic()
                        {
                            std::panic::resume_unwind(error.into_panic());
                        }
                    }
                }
            }
        }
        // Every runtime stops with `shutdown`, whose child tokens stop them.
        let runtimes = std::mem::take(&mut *lock(&self.runtimes));
        runtimes.join_all().await;
    }

    /// Stops the runtime of every BMC that has had no request for
    /// [`IDLE_BMC_TIMEOUT`], that no request holds a slot at or waits for
    /// now, and none of whose breakers is open. A breaker past its cool-down
    /// stays half-open until a request comes, so it goes with its idle BMC,
    /// whose next request starts with a closed one.
    fn stop_idle_bmcs(&self, now: Instant) {
        let stopped: Vec<Bmc> = {
            let mut bmcs = lock(&self.bmcs);
            let idle: Vec<IpAddr> = bmcs
                .iter()
                .filter(|(_, bmc)| {
                    now.saturating_duration_since(bmc.last_used) >= IDLE_BMC_TIMEOUT
                        && bmc.queues.iter().all(|queue| {
                            let stats = queue.stats();
                            stats.depth == 0 && stats.in_flight == 0
                        })
                        && !bmc.breaker_open()
                })
                .map(|(ip, _)| *ip)
                .collect();
            idle.iter().filter_map(|ip| bmcs.remove(ip)).collect()
        };
        for bmc in stopped {
            bmc.stop.cancel();
        }
    }
}

/// The breaker of a class without one: it keeps no outcomes.
const NEVER_OPENS: CircuitBreakerConfig = CircuitBreakerConfig {
    failure_threshold: 1.0,
    sample_window: 0,
    min_samples: 1,
    cool_down: Duration::ZERO,
};

impl From<&BreakerConfig> for CircuitBreakerConfig {
    fn from(config: &BreakerConfig) -> Self {
        Self {
            failure_threshold: config.failure_threshold,
            sample_window: config.window,
            min_samples: config.min_samples,
            cool_down: config.cool_down,
        }
    }
}

/// The work the dispatcher runs for a request: hands the request its slot,
/// then holds the BMC's capacity until the request drops the slot, or for
/// `hold_for` at most, after which the slot's exchange must have ended. It
/// fails when the request reports the BMC failed its exchange.
async fn grant(
    slot: oneshot::Sender<oneshot::Sender<bool>>,
    hold_for: Duration,
) -> Result<Vec<()>, ExchangeFailed> {
    let (release, released) = oneshot::channel();
    if slot.send(release).is_ok()
        && let Ok(Ok(true)) = tokio::time::timeout(hold_for, released).await
    {
        return Err(ExchangeFailed);
    }
    Ok(Vec::new())
}

/// Drives a BMC's runtime until `stop`. Requests that got their slot keep
/// it; see [`Admission::start`] for the requests still waiting.
async fn drive(mut runtime: Runtime<(), ExchangeFailed, Waiting>, stop: CancellationToken) {
    // When an open breaker's cool-down ends. The runtime keeps serving the
    // other classes meanwhile, and runs the grants, so it is polled on.
    let mut deadline = None;
    loop {
        let output = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            output = runtime.next() => output,
            () = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)),
                if deadline.is_some() =>
            {
                deadline = None;
                continue;
            }
        };
        match output {
            RuntimeOutput::SleepUntil(at) => deadline = Some(Instant::from_std(at)),
            // The class's breaker has counted the outcome.
            RuntimeOutput::Work { .. } => {}
            RuntimeOutput::Runtime(event) => match event {},
            RuntimeOutput::Shutdown => return,
        }
    }
}

/// The root of a BMC's runtime: its classes, under `max_in_flight_per_bmc`.
/// A freed slot goes to the highest-priority class that lets a request
/// through, and classes of equal priority take turns. The dispatcher's
/// `BoundedConcurrency` over `StrictPriority` would do the same, but stops
/// polling the classes at the limit, and a breaker opens for its cool-down
/// from the time it was last polled; it also hides the classes' breakers.
struct BmcClasses {
    address: IpAddr,
    /// In the order of [`Admission::classes`].
    classes: Vec<ClassAtBmc>,
    /// `classes` by priority, highest first.
    tiers: Vec<Tier>,
    max_in_flight: usize,
    in_flight: usize,
    /// The slots classes without a latency target may hold, when some class
    /// has one.
    share: Option<Share>,
    /// Slots held by classes without a latency target.
    others_in_flight: usize,
    /// When the runtime last polled the root.
    now: std::time::Instant,
}

struct ClassAtBmc {
    name: ClassName,
    node: ClassNode,
}

/// The classes of one priority, which take turns.
struct Tier {
    /// Indices into [`BmcClasses::classes`].
    classes: Vec<usize>,
    /// The position in `classes` of the class whose turn is next.
    next: usize,
}

impl BmcClasses {
    /// The root of the runtime of the BMC at `address`, and the producers of
    /// its class queues, in the order of `classes`.
    fn new(
        address: IpAddr,
        classes: &[SlotClass],
        max_in_flight: NonZeroUsize,
        slo: SloConfig,
    ) -> (Self, Vec<ClassProducer>) {
        let (nodes, queues) = classes
            .iter()
            .map(|class| {
                let passing_through = usize::try_from(class.max_in_flight.get())
                    .unwrap_or(usize::MAX)
                    .min(max_in_flight.get())
                    .min(MAX_PASSING_THROUGH);
                let (queue, producer): (ClassQueue, ClassProducer) =
                    BoundedQueueBuilder::new(class.max_queued.saturating_add(passing_through))
                        .admission_policy(GaveUpFirst)
                        .fifo()
                        .build();
                let node = CircuitBreaker::new(
                    class.breaker.unwrap_or(NEVER_OPENS),
                    BoundedConcurrency::new(class.max_in_flight, queue),
                );
                let class = ClassAtBmc {
                    name: class.name.clone(),
                    node,
                };
                (class, producer)
            })
            .unzip();
        let mut priorities: Vec<u8> = classes.iter().map(|class| class.priority).collect();
        priorities.sort_unstable_by(|a, b| b.cmp(a));
        priorities.dedup();
        let tiers = priorities
            .into_iter()
            .map(|priority| Tier {
                classes: (0..classes.len())
                    .filter(|&index| classes[index].priority == priority)
                    .collect(),
                next: 0,
            })
            .collect();
        let share = classes.iter().any(|class| class.slo.is_some()).then(|| {
            Share::new(
                slo,
                max_in_flight.get(),
                classes.iter().map(|class| class.slo),
            )
        });
        let root = Self {
            address,
            classes: nodes,
            tiers,
            max_in_flight: max_in_flight.get(),
            in_flight: 0,
            share,
            others_in_flight: 0,
            now: std::time::Instant::now(),
        };
        (root, queues)
    }
}

/// Whether class `class` may take another slot at `now`, as far as `share`,
/// of which the classes without a latency target hold `others_in_flight`,
/// goes. Free of the root, so it can run while its tiers are borrowed.
fn within_share(
    share: &mut Option<Share>,
    in_flight: usize,
    others_in_flight: usize,
    now: std::time::Instant,
    class: usize,
) -> bool {
    match share {
        Some(share) if !share.has_target(class) => {
            others_in_flight < share.limit(now, in_flight - others_in_flight)
        }
        _ => true,
    }
}

impl Scheduler<Work> for BmcClasses {
    type Meta = Waiting;

    fn update_ready(&mut self, now: std::time::Instant) -> Readiness {
        self.now = now;
        let mut ready = false;
        let mut next_update_at = None;
        for index in 0..self.classes.len() {
            let readiness = self.classes[index].node.update_ready(now);
            ready |= readiness.ready
                && within_share(
                    &mut self.share,
                    self.in_flight,
                    self.others_in_flight,
                    now,
                    index,
                );
            next_update_at = next_update_at
                .into_iter()
                .chain(readiness.next_update_at)
                .min();
        }
        Readiness {
            ready: ready && self.in_flight < self.max_in_flight,
            next_update_at,
            next_cost: None,
        }
    }

    fn take_next(&mut self) -> Option<ScheduledWork<Work, Waiting>> {
        if self.in_flight >= self.max_in_flight {
            return None;
        }
        for tier in &mut self.tiers {
            let turns = tier.classes.len();
            for turn in 0..turns {
                let at = (tier.next + turn) % turns;
                let index = tier.classes[at];
                if !within_share(
                    &mut self.share,
                    self.in_flight,
                    self.others_in_flight,
                    self.now,
                    index,
                ) {
                    continue;
                }
                if let Some(mut work) = self.classes[index].node.take_next() {
                    tier.next = (at + 1) % turns;
                    work.routing
                        .push(u32::try_from(index).expect("a BMC has fewer than 2^32 classes"));
                    self.in_flight += 1;
                    if self
                        .share
                        .as_ref()
                        .is_some_and(|share| !share.has_target(index))
                    {
                        self.others_in_flight += 1;
                    }
                    return Some(work);
                }
            }
        }
        None
    }

    fn on_complete(&mut self, mut completion: Completion<Waiting>) {
        let Some(index) = completion
            .routing
            .pop()
            .and_then(|index| usize::try_from(index).ok())
            .filter(|&index| index < self.classes.len())
        else {
            return;
        };
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(share) = &mut self.share {
            if share.has_target(index) {
                let latency = completion.meta.latency.as_ref().and_then(|at| at.taken());
                if let Some(latency) = latency
                    && share.record(index, latency, self.now) == Some(Verdict::Missed)
                {
                    emit(SloMissed {
                        class: self.classes[index].name.clone(),
                    });
                }
            } else {
                self.others_in_flight = self.others_in_flight.saturating_sub(1);
            }
        }
        let class = &mut self.classes[index];
        let was_open = matches!(class.node.state(), BreakerState::Open { .. });
        class.node.on_complete(completion);
        if !was_open && matches!(class.node.state(), BreakerState::Open { .. }) {
            emit(BreakerOpened {
                class: class.name.clone(),
                bmc_ip_address: self.address.to_string(),
            });
        }
    }

    fn register_queue_event_sink(&mut self, sink: QueueEventSink) {
        for class in &mut self.classes {
            class.node.register_queue_event_sink(sink.clone());
        }
    }
}

/// A request's slot at its BMC. Dropping it frees the slot for the next
/// waiting request.
#[must_use]
pub(super) struct Slot {
    /// Sending on this, or dropping it, ends the grant that holds the slot;
    /// `None` for a class that takes no slots.
    release: Option<oneshot::Sender<bool>>,
    /// Whether the BMC failed the exchange.
    failed: bool,
    /// How long the request took, for a class with a latency target:
    /// measured when the slot is freed unless reported before.
    latency: Option<Arc<Latency>>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(latency) = &self.latency {
            latency.measure();
        }
        if let Some(release) = self.release.take() {
            release.send(self.failed).ok();
        }
    }
}

impl Slot {
    fn free() -> Self {
        Self {
            release: None,
            failed: false,
            latency: None,
        }
    }

    /// Reports that the BMC answered now. Only the first report counts.
    pub(super) fn answered(&self) {
        if let Some(latency) = &self.latency {
            latency.measure();
        }
    }

    /// Reports that the exchange told nothing of the BMC's latency: the
    /// proxy could not reach the BMC, or failed on its own.
    pub(super) fn unmeasured(&self) {
        if let Some(latency) = &self.latency {
            latency.leave_unmeasured();
        }
    }

    /// Reports that the BMC failed the exchange. Its breaker counts it once
    /// the slot is freed.
    pub(super) fn bmc_failed(&mut self) {
        self.failed = true;
    }

    /// `body`, holding this slot until the body has been sent or dropped.
    pub(super) fn hold_until_sent(self, body: Body) -> Body {
        Body::new(HeldBody { body, _slot: self })
    }
}

struct HeldBody {
    body: Body,
    _slot: Slot,
}

impl HttpBody for HeldBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("an admission mutex must not be poisoned")
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::IpAddr;
    use std::num::NonZeroUsize;
    use std::pin::pin;
    use std::time::Duration;

    use axum::body::Body;
    use carbide_instrument::testing::MetricsCapture;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, Check, check_cases_async, check_values};
    use futures::FutureExt;
    use hyper::body::Body as HttpBody;
    use nv_redfish_dispatcher::schedulers::CircuitBreakerConfig;
    use nv_redfish_dispatcher::{ClockConfig, Runtime, RuntimeConfig};
    use tokio::task::JoinSet;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use super::{
        Admission, Bmc, BmcClasses, IDLE_BMC_SWEEP_INTERVAL, IDLE_BMC_TIMEOUT, MAX_BMCS, Refused,
        Slot,
    };
    use crate::config::SloConfig;
    use crate::proxy::BmcProxyState;
    use crate::proxy::test_support::test_state_with_config;

    /// How long a request that is not granted a slot is watched. On the
    /// paused clock, time moves only once every task, the BMCs' runtimes
    /// included, has nothing left to do: a request still waiting after this
    /// was not granted a slot.
    const WATCHED_FOR: Duration = Duration::from_millis(100);

    const METRICS: &str = "/redfish/v1/Chassis/Chassis_0/EnvironmentMetrics";

    fn bmc(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }

    /// The config that adds `admission` to the bare minimum.
    fn config_with(admission: &str) -> String {
        format!(
            r#"
            [tls]
            identity_pemfile_path = ""
            identity_keyfile_path = ""
            root_cafile_path = ""
            admin_root_cafile_path = ""

            [auth]

            {admission}
            "#
        )
    }

    fn proxy_with(admission: &str) -> BmcProxyState {
        test_state_with_config(&config_with(admission))
    }

    /// A slot at `at` for a `method` request on `path`, held at most
    /// `hold_for`, waiting for it within the class's budget.
    async fn slot_held_for(
        state: &BmcProxyState,
        at: IpAddr,
        method: http::Method,
        path: &str,
        hold_for: Duration,
    ) -> Result<Slot, Refused> {
        let class = state.config.classes.classify(&method, path, &[]);
        let arrived = Instant::now();
        let deadline = arrived + class.upstream_timeout;
        state
            .admission
            .acquire(at, class, arrived, deadline, hold_for)
            .await
    }

    /// A slot as the proxy asks for one for a request without a body.
    async fn slot(
        state: &BmcProxyState,
        at: IpAddr,
        method: http::Method,
        path: &str,
    ) -> Result<Slot, Refused> {
        let budget = state
            .config
            .classes
            .classify(&method, path, &[])
            .upstream_timeout;
        slot_held_for(state, at, method, path, budget.saturating_mul(2)).await
    }

    /// Whether `slot` is still waiting after [`WATCHED_FOR`].
    async fn waits(slot: &mut (impl Future<Output = Result<Slot, Refused>> + Unpin)) -> bool {
        tokio::time::timeout(WATCHED_FOR, slot).await.is_err()
    }

    async fn granted(slot: impl Future<Output = Result<Slot, Refused>>) -> Slot {
        tokio::time::timeout(WATCHED_FOR, slot)
            .await
            .expect("a slot is granted")
            .expect("the request is admitted")
    }

    const METRICS_ONE_AT_A_TIME: &str = r#"
        [[class]]
        name = "metrics"
        match = ["GET /redfish/v1/**/EnvironmentMetrics"]
        max_in_flight = 1
        upstream_timeout = "30s"
    "#;

    /// A class's waiting requests get its slot in the order they came.
    #[tokio::test(start_paused = true)]
    async fn a_class_serves_its_requests_in_order() {
        let state = proxy_with(METRICS_ONE_AT_A_TIME);
        let held = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        let mut first = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        assert!(waits(&mut first).await, "the first waits");
        let mut second = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        assert!(waits(&mut second).await, "the second waits");

        drop(held);
        tokio::time::sleep(WATCHED_FOR).await;
        let first = (&mut first).now_or_never();
        let second = (&mut second).now_or_never();
        assert_eq!((first.is_some(), second.is_some()), (true, false));
    }

    /// A limit applies per BMC, and a request outside every limited class
    /// takes no slot, and is not observed.
    #[tokio::test(start_paused = true)]
    async fn other_bmcs_and_classes_do_not_wait() {
        let metrics = MetricsCapture::start();
        // A class name no other test uses, so their grants are not counted.
        let state = proxy_with(&format!(
            r#"
            {METRICS_ONE_AT_A_TIME}

            [[class]]
            name = "unlimited"
            match = ["PATCH /redfish/v1/**/EnvironmentMetrics"]
            "#
        ));
        let _held = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        drop(granted(slot(&state, bmc(2), http::Method::GET, METRICS)).await);
        drop(granted(slot(&state, bmc(1), http::Method::PATCH, METRICS)).await);
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_bmc_proxy_admission_wait_milliseconds",
                &[("class", "unlimited")],
            ),
            0,
        );
    }

    /// A request that finds its class's queue full of requests waiting at
    /// its BMC is refused at once; one that waits out its class's budget is
    /// refused then. Each refusal is counted by reason, and each grant's wait
    /// observed.
    #[tokio::test(start_paused = true)]
    async fn a_request_without_a_slot_is_refused() {
        let metrics = MetricsCapture::start();
        let state = proxy_with(
            r#"
            [[class]]
            name = "refusals"
            match = ["GET /redfish/v1/**/EnvironmentMetrics"]
            max_in_flight = 1
            max_queued = 1
            upstream_timeout = "2s"
            "#,
        );
        let _held = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        let mut waiting = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        let queued_at = Instant::now();
        assert!(waits(&mut waiting).await, "the second waits");

        let refused_at = Instant::now();
        assert_eq!(
            slot(&state, bmc(1), http::Method::GET, METRICS).await.err(),
            Some(Refused::QueueFull),
        );
        assert_eq!(refused_at.elapsed(), Duration::ZERO, "refused at once");

        assert_eq!(waiting.await.err(), Some(Refused::Timeout));
        assert_eq!(
            queued_at.elapsed(),
            Duration::from_secs(2),
            "after its budget"
        );

        let refused = |reason| {
            metrics.counter_delta(
                "carbide_bmc_proxy_admission_refused_total",
                &[("class", "refusals"), ("reason", reason)],
            )
        };
        assert_eq!((refused("queue_full"), refused("timeout")), (1.0, 1.0));
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_bmc_proxy_admission_wait_milliseconds",
                &[("class", "refusals")],
            ),
            1,
        );
    }

    /// A burst of a class's requests is not refused while it has free slots:
    /// its queue has room for those on their way to one, beyond `max_queued`.
    #[tokio::test(start_paused = true)]
    async fn a_burst_with_free_slots_is_not_refused() {
        let state = proxy_with(
            r#"
            [[class]]
            name = "metrics"
            match = ["GET /redfish/v1/**/EnvironmentMetrics"]
            max_in_flight = 4
            max_queued = 1
            "#,
        );
        // Each request is queued on its first poll, before the BMC's runtime
        // hands any of them a slot.
        let mut burst: Vec<_> = (0..5)
            .map(|_| Box::pin(slot(&state, bmc(1), http::Method::GET, METRICS)))
            .collect();
        for request in &mut burst {
            assert!(request.as_mut().now_or_never().is_none(), "queued");
        }
        tokio::time::sleep(WATCHED_FOR).await;
        let outcomes: Vec<_> = burst
            .iter_mut()
            .map(|request| match request.as_mut().now_or_never() {
                Some(Ok(_slot)) => "granted",
                Some(Err(_)) => "refused",
                None => "waiting",
            })
            .collect();
        assert_eq!(
            outcomes,
            ["granted", "granted", "granted", "granted", "waiting"]
        );
    }

    /// Under the per-BMC limit, a slot another class holds is not free: the
    /// queue makes no room beyond `max_queued` for it.
    #[tokio::test(start_paused = true)]
    async fn a_slot_another_class_holds_is_not_free() {
        let state = proxy_with(
            r#"
            [admission]
            max_in_flight_per_bmc = 1

            [[class]]
            name = "metrics"
            match = ["GET /redfish/v1/**/EnvironmentMetrics"]
            max_queued = 1
            "#,
        );
        let _held = granted(slot(&state, bmc(1), http::Method::PATCH, METRICS)).await;
        let mut waiting = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        assert!(waits(&mut waiting).await, "the first read waits");
        assert_eq!(
            slot(&state, bmc(1), http::Method::GET, METRICS).await.err(),
            Some(Refused::QueueFull),
        );
    }

    #[derive(Clone, Copy)]
    enum Held {
        /// A read holds the slot, of the class listed first.
        Read,
        /// A write holds the slot, of the class listed second.
        Write,
    }

    /// Which of a waiting read and a later waiting write gets the next slot
    /// under the per-BMC limit, when the write's class has `priority` and
    /// `held` frees the slot.
    async fn next_granted((priority, held): (u8, Held)) -> &'static str {
        let state = proxy_with(&format!(
            r#"
            [admission]
            max_in_flight_per_bmc = 1

            [[class]]
            name = "metrics"
            match = ["GET /redfish/v1/**/EnvironmentMetrics"]

            [[class]]
            name = "power"
            match = ["PATCH /redfish/v1/**/EnvironmentMetrics"]
            priority = {priority}
            "#
        ));
        let held_method = match held {
            Held::Read => http::Method::GET,
            Held::Write => http::Method::PATCH,
        };
        let held = granted(slot(&state, bmc(1), held_method, METRICS)).await;
        let mut read = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        assert!(waits(&mut read).await, "the read waits");
        let mut write = pin!(slot(&state, bmc(1), http::Method::PATCH, METRICS));
        assert!(waits(&mut write).await, "the write waits");

        drop(held);
        tokio::time::sleep(WATCHED_FOR).await;
        // Granted slots stay held until both have been looked at.
        let read = (&mut read).now_or_never();
        let write = (&mut write).now_or_never();
        match (read.is_some(), write.is_some()) {
            (false, true) => "the write",
            (true, false) => "the read",
            _ => "both or neither",
        }
    }

    /// Under the per-BMC limit, a freed slot goes to the higher-priority
    /// class. Classes of equal priority take turns: the class whose request
    /// held the slot goes after the other, whichever is listed first.
    #[tokio::test(start_paused = true)]
    async fn waiting_requests_go_by_priority_then_in_turn() {
        check_cases_async(
            [
                Case {
                    scenario: "a higher-priority class goes first",
                    input: (1, Held::Write),
                    expect: Yields("the write"),
                },
                Case {
                    scenario: "after a write, the read's turn",
                    input: (0, Held::Write),
                    expect: Yields("the read"),
                },
                Case {
                    scenario: "after a read, the write's turn",
                    input: (0, Held::Read),
                    expect: Yields("the write"),
                },
            ],
            |input| async move { Ok::<_, Infallible>(next_granted(input).await) },
        )
        .await;
    }

    /// A request that stops waiting gives up its place in the queue, so the
    /// next one can wait instead of being refused.
    #[tokio::test(start_paused = true)]
    async fn a_request_that_gives_up_frees_its_place() {
        let state = proxy_with(
            r#"
            [[class]]
            name = "metrics"
            match = ["GET /redfish/v1/**/EnvironmentMetrics"]
            max_in_flight = 1
            max_queued = 1
            "#,
        );
        let held = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        {
            let mut abandoned = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
            assert!(waits(&mut abandoned).await, "it waits, then gives up");
        }
        let mut next = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        assert!(
            waits(&mut next).await,
            "the next waits instead of being refused"
        );
        drop(held);
        drop(granted(next).await);
    }

    /// A slot whose holder outlives its exchange's bound goes to the next
    /// waiting request, and the late holder's release frees nothing more.
    #[tokio::test(start_paused = true)]
    async fn a_slot_held_past_its_exchange_is_reclaimed() {
        let state = proxy_with(METRICS_ONE_AT_A_TIME);
        let stalled = granted(slot_held_for(
            &state,
            bmc(1),
            http::Method::GET,
            METRICS,
            Duration::from_secs(1),
        ))
        .await;
        let waiting_from = Instant::now();
        let reclaimed = slot(&state, bmc(1), http::Method::GET, METRICS)
            .await
            .expect("the slot is reclaimed");
        assert_eq!(waiting_from.elapsed(), Duration::from_secs(1));

        drop(stalled);
        let mut third = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        assert!(waits(&mut third).await, "the reclaimed slot is still held");
        drop(reclaimed);
        drop(granted(third).await);
    }

    /// A slot granted once the request's deadline has passed is refused, even
    /// when the request learns of the grant before its deadline's timer: it
    /// would leave the exchange no time.
    #[tokio::test(start_paused = true)]
    async fn a_slot_granted_past_the_deadline_is_refused() {
        let state = proxy_with(
            r#"
            [[class]]
            name = "metrics"
            match = ["GET /redfish/v1/**/EnvironmentMetrics"]
            max_in_flight = 1
            upstream_timeout = "1s"
            "#,
        );
        let _expiring = granted(slot_held_for(
            &state,
            bmc(1),
            http::Method::GET,
            METRICS,
            Duration::from_secs(2),
        ))
        .await;
        let mut late = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        assert!((&mut late).now_or_never().is_none(), "it waits");
        // The held slot is reclaimed and granted to the request a second
        // after its deadline, and only then is the request looked at again.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(late.await.err(), Some(Refused::Timeout));
    }

    /// Past its limit of BMCs, the proxy refuses requests for another BMC,
    /// and keeps serving the ones it tracks.
    #[tokio::test(start_paused = true)]
    async fn too_many_bmcs_are_refused() {
        let state = proxy_with(METRICS_ONE_AT_A_TIME);
        drop(granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await);
        // Placeholders that share one runtime nobody drives.
        let runtime = Runtime::new(
            RuntimeConfig {
                global_max_in_flight: NonZeroUsize::MIN,
                clock: ClockConfig::Wallclock,
            },
            BmcClasses::new(bmc(0), &[], NonZeroUsize::MIN, SloConfig::default()).0,
        );
        {
            let mut bmcs = state.admission.bmcs.lock().unwrap();
            for n in 0..MAX_BMCS - 1 {
                let octets = (u32::try_from(n).unwrap() + (10 << 24)).to_be_bytes();
                bmcs.insert(
                    IpAddr::from(octets),
                    Bmc {
                        queues: Vec::new(),
                        runtime: runtime.handle(),
                        stop: CancellationToken::new(),
                        last_used: Instant::now(),
                        others_limit: None,
                    },
                );
            }
        }
        drop(granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await);
        assert_eq!(
            slot(&state, bmc(2), http::Method::GET, METRICS).await.err(),
            Some(Refused::TooManyBmcs),
        );
    }

    /// Shutdown refuses the requests waiting for a slot, and every request
    /// after it, and stops the BMCs' runtimes.
    #[tokio::test(start_paused = true)]
    async fn shutdown_refuses_waiting_requests() {
        let config =
            crate::Config::parse(&config_with(METRICS_ONE_AT_A_TIME)).expect("the config parses");
        let shutdown = CancellationToken::new();
        let mut tasks = JoinSet::new();
        let admission = Admission::start(
            &config.classes,
            &config.admission,
            shutdown.clone(),
            &mut tasks,
        );
        let class = config.classes.classify(&http::Method::GET, METRICS, &[]);
        let acquire = || {
            admission.acquire(
                bmc(1),
                class,
                Instant::now(),
                Instant::now() + Duration::from_secs(30),
                Duration::from_secs(60),
            )
        };

        let _held = granted(acquire()).await;
        let mut waiting = pin!(acquire());
        assert!(waits(&mut waiting).await, "it waits");
        shutdown.cancel();

        assert_eq!(waiting.await.err(), Some(Refused::ShuttingDown));
        assert_eq!(acquire().await.err(), Some(Refused::ShuttingDown));
        tokio::time::timeout(WATCHED_FOR, tasks.join_all())
            .await
            .expect("the admission's tasks stop");
    }

    /// The sweep stops the runtime of an idle BMC and collects it, and the
    /// BMC's next request starts a new one. A BMC used recently, or with a
    /// request holding a slot, keeps its runtime.
    #[tokio::test(start_paused = true)]
    async fn idle_bmcs_stop_their_runtime() {
        // A budget long enough that the slot held throughout is not
        // reclaimed.
        let state = proxy_with(
            r#"
            [[class]]
            name = "metrics"
            match = ["GET /redfish/v1/**/EnvironmentMetrics"]
            max_in_flight = 1
            upstream_timeout = "30m"
            "#,
        );
        let bmcs = || {
            let mut bmcs: Vec<IpAddr> = state
                .admission
                .bmcs
                .lock()
                .unwrap()
                .keys()
                .copied()
                .collect();
            bmcs.sort();
            bmcs
        };
        let running = || state.admission.runtimes.lock().unwrap().len();
        drop(granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await);
        let _held = granted(slot(&state, bmc(2), http::Method::GET, METRICS)).await;

        tokio::time::sleep(IDLE_BMC_TIMEOUT / 2).await;
        drop(granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await);
        tokio::time::sleep(IDLE_BMC_TIMEOUT / 2 + IDLE_BMC_SWEEP_INTERVAL / 2).await;
        assert_eq!(
            bmcs(),
            [bmc(1), bmc(2)],
            "bmc 1 was used under a timeout ago"
        );

        tokio::time::sleep(IDLE_BMC_TIMEOUT / 2 + IDLE_BMC_SWEEP_INTERVAL).await;
        assert_eq!((bmcs(), running()), (vec![bmc(2)], 1), "bmc 1 went idle");

        drop(granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await);
        assert_eq!(running(), 2);
    }

    /// The caller is answered `429` when the proxy's per-BMC limits turned its
    /// request away, and `503` when the proxy could not take it or its class's
    /// breaker at the BMC is open.
    #[test]
    fn a_refusal_answers_by_its_cause() {
        check_values(
            [
                Check {
                    scenario: "the queue is full",
                    input: Refused::QueueFull,
                    expect: 429,
                },
                Check {
                    scenario: "no slot within the budget",
                    input: Refused::Timeout,
                    expect: 429,
                },
                Check {
                    scenario: "the proxy tracks too many BMCs",
                    input: Refused::TooManyBmcs,
                    expect: 503,
                },
                Check {
                    scenario: "the proxy is shutting down",
                    input: Refused::ShuttingDown,
                    expect: 503,
                },
                Check {
                    scenario: "the class's breaker at the BMC is open",
                    input: Refused::BreakerOpen,
                    expect: 503,
                },
            ],
            |refused| refused.status().as_u16(),
        );
    }

    /// How long the breakers in these tests stay open. Breaker tests run on
    /// real time: the dispatcher's clock is the wall clock, which a paused
    /// tokio clock does not move, so under one a breaker never leaves open.
    const COOL_DOWN: Duration = Duration::from_millis(200);

    /// Long enough for a BMC's runtime to see a freed slot's grant end, and
    /// count its outcome.
    const SETTLE: Duration = Duration::from_millis(20);

    /// Breakers that open on the second failure in a row.
    const BREAKER: &str = r#"
        [admission.breaker]
        window = 4
        min_samples = 2
        cool_down = "200ms"
    "#;

    /// Has a request of the default class at `at` fail its exchange.
    async fn fail_an_exchange(state: &BmcProxyState, at: IpAddr) {
        let mut slot = granted(slot(state, at, http::Method::GET, METRICS)).await;
        slot.bmc_failed();
        drop(slot);
        tokio::time::sleep(SETTLE).await;
    }

    /// A class besides the default, which takes slots whenever the default
    /// class does.
    const POWER: &str = r#"
        [[class]]
        name = "power"
        match = ["PATCH /redfish/v1/**/EnvironmentMetrics"]
    "#;

    /// What a request of the default class at `at` meets at once: `None` when
    /// it is granted a slot.
    async fn met_at_once(state: &BmcProxyState, at: IpAddr) -> Option<Refused> {
        tokio::time::timeout(WATCHED_FOR, slot(state, at, http::Method::GET, METRICS))
            .await
            .expect("the request is answered at once")
            .err()
    }

    /// Whether a `method` request at `at` is granted a slot at once.
    async fn granted_at_once(state: &BmcProxyState, at: IpAddr, method: http::Method) -> bool {
        tokio::time::timeout(WATCHED_FOR, slot(state, at, method, METRICS))
            .await
            .is_ok_and(|granted| granted.is_ok())
    }

    /// What `failures` failed exchanges of the default class at one BMC under
    /// `admission` do, while an earlier exchange of the class there outlasts
    /// them: (what the class's next request to the BMC meets, whether the
    /// BMC's requests of another class and the class's requests to another
    /// BMC are granted, breaker openings reported once the earlier exchange
    /// has ended too, refusals for an open breaker counted).
    async fn after_failures(
        (admission, failures): (&'static str, usize),
    ) -> (Option<Refused>, bool, f64, f64) {
        let metrics = MetricsCapture::start();
        let state = proxy_with(&format!("{admission}\n{POWER}"));
        let earlier = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        for _ in 0..failures {
            fail_an_exchange(&state, bmc(1)).await;
        }
        let next = met_at_once(&state, bmc(1)).await;
        let again = slot(&state, bmc(1), http::Method::GET, METRICS).await.err();
        assert_eq!(next, again, "an open breaker refuses every request");
        let others = granted_at_once(&state, bmc(1), http::Method::PATCH).await
            && granted_at_once(&state, bmc(2), http::Method::GET).await;
        drop(earlier);
        tokio::time::sleep(SETTLE).await;
        (
            next,
            others,
            metrics.counter_delta(
                "carbide_bmc_proxy_breaker_opened_total",
                &[("class", "default")],
            ),
            metrics.counter_delta(
                "carbide_bmc_proxy_admission_refused_total",
                &[("class", "default"), ("reason", "breaker_open")],
            ),
        )
    }

    /// Failed exchanges open their class's breaker at their BMC, which
    /// refuses the class's requests to that BMC at once and reports the
    /// opening once; other classes and BMCs are served. Without a breaker,
    /// failures refuse nothing, even as many as would open the dispatcher's
    /// default breaker.
    #[tokio::test]
    async fn failures_open_their_class_breaker_at_their_bmc() {
        check_cases_async(
            [
                Case {
                    scenario: "with a breaker",
                    input: (BREAKER, 2),
                    expect: Yields((Some(Refused::BreakerOpen), true, 1.0, 2.0)),
                },
                Case {
                    scenario: "without one",
                    input: ("[admission]\nmax_in_flight_per_bmc = 4", 5),
                    expect: Yields((None, true, 0.0, 0.0)),
                },
            ],
            |input| async move { Ok::<_, Infallible>(after_failures(input).await) },
        )
        .await;
    }

    /// What a BMC's request meets once its open breaker cooled down, waited
    /// for an exchange that outlasted the cool-down, and let a probe through,
    /// which failed if `probe_fails`; and the breaker's openings reported.
    async fn after_a_probe(probe_fails: bool) -> (Option<Refused>, f64) {
        let metrics = MetricsCapture::start();
        let state = proxy_with(BREAKER);
        let earlier = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        fail_an_exchange(&state, bmc(1)).await;
        fail_an_exchange(&state, bmc(1)).await;
        assert_eq!(
            slot(&state, bmc(1), http::Method::GET, METRICS).await.err(),
            Some(Refused::BreakerOpen),
            "the breaker opened"
        );
        tokio::time::sleep(COOL_DOWN + SETTLE).await;
        assert_eq!(
            met_at_once(&state, bmc(1)).await,
            Some(Refused::BreakerOpen),
            "the probe waits for the earlier exchange"
        );
        drop(earlier);
        tokio::time::sleep(SETTLE).await;
        let mut probe = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        assert_eq!(
            slot(&state, bmc(1), http::Method::GET, METRICS).await.err(),
            Some(Refused::BreakerOpen),
            "the probe's verdict is awaited"
        );
        if probe_fails {
            probe.bmc_failed();
        }
        drop(probe);
        tokio::time::sleep(SETTLE).await;
        let next = met_at_once(&state, bmc(1)).await;
        (
            next,
            metrics.counter_delta(
                "carbide_bmc_proxy_breaker_opened_total",
                &[("class", "default")],
            ),
        )
    }

    /// After its cool-down, and once the class's earlier exchanges with the
    /// BMC ended, an open breaker lets one request through, refusing the
    /// others meanwhile and until its outcome closes the breaker or opens it
    /// again, which is reported as another opening.
    #[tokio::test]
    async fn a_probe_decides_whether_the_breaker_closes() {
        check_cases_async(
            [
                Case {
                    scenario: "the probe succeeds",
                    input: false,
                    expect: Yields((None, 1.0)),
                },
                Case {
                    scenario: "the probe fails",
                    input: true,
                    expect: Yields((Some(Refused::BreakerOpen), 2.0)),
                },
            ],
            |probe_fails| async move { Ok::<_, Infallible>(after_a_probe(probe_fails).await) },
        )
        .await;
    }

    /// A breaker opens for its whole cool-down even when the BMC's last
    /// exchange held its only slot for longer than that: the per-BMC limit
    /// does not stop the breaker's clock.
    #[tokio::test]
    async fn a_breaker_under_the_per_bmc_limit_opens_for_its_cool_down() {
        let _metrics = MetricsCapture::start();
        let state = proxy_with(&format!(
            "[admission]\nmax_in_flight_per_bmc = 1\n{BREAKER}"
        ));
        for _ in 0..2 {
            let mut held = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
            tokio::time::sleep(COOL_DOWN + COOL_DOWN / 2).await;
            held.bmc_failed();
            drop(held);
            tokio::time::sleep(SETTLE).await;
        }
        assert_eq!(
            slot(&state, bmc(1), http::Method::GET, METRICS).await.err(),
            Some(Refused::BreakerOpen),
        );
    }

    /// The sweep keeps an idle BMC while a breaker there is open, so the
    /// breaker stays open for its cool-down, and stops it once the cool-down
    /// has passed.
    #[tokio::test]
    async fn the_sweep_keeps_a_bmc_through_its_breakers_cool_down() {
        let _metrics = MetricsCapture::start();
        let state = proxy_with(BREAKER);
        fail_an_exchange(&state, bmc(1)).await;
        fail_an_exchange(&state, bmc(1)).await;
        let tracked_after_a_sweep = || {
            state
                .admission
                .stop_idle_bmcs(Instant::now() + IDLE_BMC_TIMEOUT);
            state.admission.bmcs.lock().unwrap().contains_key(&bmc(1))
        };
        assert!(tracked_after_a_sweep(), "kept while the breaker is open");
        assert_eq!(
            slot(&state, bmc(1), http::Method::GET, METRICS).await.err(),
            Some(Refused::BreakerOpen),
        );
        tokio::time::sleep(COOL_DOWN + SETTLE).await;
        assert!(
            !tracked_after_a_sweep(),
            "stopped once the cool-down passed"
        );
    }

    /// A full queue makes room for a newcomer by evicting a request that
    /// stopped waiting, however many slots its class has.
    #[tokio::test(start_paused = true)]
    async fn a_full_queue_evicts_requests_that_stopped_waiting() {
        let state = proxy_with(
            r#"
            [[class]]
            name = "metrics"
            match = ["GET /redfish/v1/**/EnvironmentMetrics"]
            max_in_flight = 200
            max_queued = 1
            "#,
        );
        // Each request is queued on its first poll, before the BMC's runtime
        // takes any, and then stops waiting.
        for _ in 0..200 {
            assert!(
                Box::pin(slot(&state, bmc(1), http::Method::GET, METRICS))
                    .as_mut()
                    .now_or_never()
                    .is_none(),
                "queued"
            );
        }
        assert!(
            Box::pin(slot(&state, bmc(1), http::Method::GET, METRICS))
                .as_mut()
                .now_or_never()
                .is_none(),
            "the newcomer is queued, not refused"
        );
    }

    /// Each setting of a class's breaker reaches the dispatcher's breaker.
    #[test]
    fn breaker_settings_reach_the_dispatcher() {
        let config = crate::Config::parse(&config_with(
            "[admission.breaker]\nfailure_threshold = 0.25\nwindow = 7\nmin_samples = 3\ncool_down = \"42s\"",
        ))
        .expect("the config parses");
        let class = config.classes.classify(&http::Method::GET, METRICS, &[]);
        let breaker = CircuitBreakerConfig::from(class.breaker.as_ref().expect("set"));
        assert_eq!(
            (
                breaker.failure_threshold,
                breaker.sample_window,
                breaker.min_samples,
                breaker.cool_down,
            ),
            (0.25, 7, 3, Duration::from_secs(42)),
        );
    }

    /// A class with a one-second latency target, judged on each answer,
    /// under a per-BMC limit of 2, which a miss halves for the other classes.
    const SLO: &str = r#"
        [admission]
        max_in_flight_per_bmc = 2

        [admission.slo]
        window = 1

        [[class]]
        name = "power"
        match = ["PATCH /redfish/v1/**/EnvironmentMetrics"]
        slo = { latency = "1s" }
    "#;

    /// How many requests of the default class get a slot at a BMC at once,
    /// after a request of a class with a latency target there was answered
    /// after `latency`; and the misses counted.
    async fn others_granted_after(latency: Duration) -> (usize, f64) {
        let metrics = MetricsCapture::start();
        let state = proxy_with(SLO);
        let power = granted(slot(&state, bmc(1), http::Method::PATCH, METRICS)).await;
        tokio::time::sleep(latency).await;
        power.answered();
        drop(power);
        // The runtime weighs the answer once it sees the slot freed, after
        // any grant it hands out in the same poll.
        tokio::time::sleep(WATCHED_FOR).await;
        let _first = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        let mut second = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        let others = if waits(&mut second).await { 1 } else { 2 };
        (
            others,
            metrics.counter_delta("carbide_bmc_proxy_slo_missed_total", &[("class", "power")]),
        )
    }

    /// A class that misses its latency target at a BMC halves the slots the
    /// other classes may hold there, and the miss is counted; one that meets
    /// it leaves them the whole limit.
    #[tokio::test(start_paused = true)]
    async fn a_missed_target_holds_back_the_other_classes() {
        check_cases_async(
            [
                Case {
                    scenario: "the target is met",
                    input: Duration::from_millis(100),
                    expect: Yields((2, 0.0)),
                },
                Case {
                    scenario: "the target is missed",
                    input: Duration::from_secs(2),
                    expect: Yields((1, 1.0)),
                },
            ],
            |latency| async move { Ok::<_, Infallible>(others_granted_after(latency).await) },
        )
        .await;
    }

    /// After a missed target halves the slots of the classes without one,
    /// their queues hold only `max_queued` beyond the slots still free to
    /// them, and refuse the next request at once.
    #[tokio::test(start_paused = true)]
    async fn a_cut_share_shrinks_the_queues_too() {
        // Its miss is counted, so it holds the capture other tests count in.
        let _metrics = MetricsCapture::start();
        let state = proxy_with(&format!(
            r#"
            {SLO}

            [[class]]
            name = "default"
            max_queued = 1
            "#
        ));
        let power = granted(slot(&state, bmc(1), http::Method::PATCH, METRICS)).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        drop(power);
        tokio::time::sleep(WATCHED_FOR).await;
        let _held = granted(slot(&state, bmc(1), http::Method::GET, METRICS)).await;
        let mut queued = pin!(slot(&state, bmc(1), http::Method::GET, METRICS));
        assert!(waits(&mut queued).await, "one waits");
        assert_eq!(
            slot(&state, bmc(1), http::Method::GET, METRICS).await.err(),
            Some(Refused::QueueFull),
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum Ending {
        /// The request waited for a slot until its budget ran out.
        RefusedForWant,
        /// The request's caller went away while it held its slot.
        Abandoned,
        /// The proxy could not reach the BMC.
        Unreachable,
    }

    /// The misses counted for a class with a one-second target and a
    /// two-second budget, one request at a time, after a request that ended
    /// as `ending`, two seconds after it arrived.
    async fn misses_after(ending: Ending) -> f64 {
        let metrics = MetricsCapture::start();
        let state = proxy_with(
            r#"
            [admission]
            max_in_flight_per_bmc = 2

            [admission.slo]
            window = 1

            [[class]]
            name = "power"
            match = ["PATCH /redfish/v1/**/EnvironmentMetrics"]
            max_in_flight = 1
            upstream_timeout = "2s"
            slo = { latency = "1s" }
            "#,
        );
        let request = || slot(&state, bmc(1), http::Method::PATCH, METRICS);
        match ending {
            Ending::RefusedForWant => {
                let held = granted(request()).await;
                held.answered();
                assert_eq!(request().await.err(), Some(Refused::Timeout));
                drop(held);
            }
            Ending::Abandoned => {
                let held = granted(request()).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
                drop(held);
            }
            Ending::Unreachable => {
                let held = granted(request()).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
                held.unmeasured();
                drop(held);
            }
        }
        tokio::time::sleep(WATCHED_FOR).await;
        metrics.counter_delta("carbide_bmc_proxy_slo_missed_total", &[("class", "power")])
    }

    /// A request of a class with a latency target that got no answer counts
    /// as taking as long as it lasted, whether it waited out its budget for
    /// a slot or its caller went away; one that could not reach the BMC does
    /// not count.
    #[tokio::test(start_paused = true)]
    async fn a_request_without_an_answer_counts_as_late() {
        check_cases_async(
            [
                Case {
                    scenario: "it waited out its budget for a slot",
                    input: Ending::RefusedForWant,
                    expect: Yields(1.0),
                },
                Case {
                    scenario: "its caller went away",
                    input: Ending::Abandoned,
                    expect: Yields(1.0),
                },
                Case {
                    scenario: "the BMC was unreachable",
                    input: Ending::Unreachable,
                    expect: Yields(0.0),
                },
            ],
            |ending| async move { Ok::<_, Infallible>(misses_after(ending).await) },
        )
        .await;
    }

    /// A body keeps its length and its end when it holds a slot, so the
    /// caller's response is framed as it would be without one.
    #[test]
    fn a_held_body_is_framed_as_before() {
        let held = |body| Slot::free().hold_until_sent(body);
        assert_eq!(held(Body::from("abc")).size_hint().exact(), Some(3));
        assert!(held(Body::empty()).is_end_stream());
    }
}
