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
//! `max_in_flight`, or when `[admission] max_in_flight_per_bmc` is set. It
//! holds the slot until the proxy has passed the BMC's response on to the
//! caller, or the exchange has failed, and for at most the bound its caller
//! gives from the grant on, after which the slot goes to the next waiting
//! request even if a caller that stopped reading still holds its response. A
//! request that finds no free slot waits in its class's queue at that BMC. A
//! freed slot goes to the highest-priority class that has a request waiting
//! and is under its own `max_in_flight`; classes of equal priority take
//! turns, and each class's requests go in the order they came. A request is
//! refused with `429` when its class's queue at that BMC is full of requests
//! still waiting, or when no slot frees before its deadline, and with `503`
//! when the proxy already tracks [`MAX_BMCS`] BMCs and this is another, or
//! when the proxy is shutting down. Limits are per proxy replica: with two
//! replicas, a BMC can receive twice a limit.
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
//! Runtime (max_in_flight_per_bmc)        one per BMC in use
//! └─ StrictPriority                      by class priority
//!    └─ BoundedConcurrency(class max_in_flight)
//!       └─ BoundedQueue(class max_queued)  first come, first served
//! ```

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::IpAddr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use bytes::Bytes;
use carbide_instrument::{Event, LabelValue, emit};
use http::StatusCode;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use nv_redfish_dispatcher::schedulers::{
    AdmissionContext, AdmissionDecision, AdmissionPolicy, BoundedConcurrency, BoundedQueue,
    BoundedQueueProducer, Fifo, StrictPriority,
};
use nv_redfish_dispatcher::{
    BoundedQueueBuilder, ClockConfig, EnqueueOutcome, FutureWork, Runtime, RuntimeConfig,
    RuntimeOutput, ScheduledWork, WithPriority,
};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::class::{ClassName, ClassTable, RequestClass};
use crate::config::AdmissionConfig;

type Work = FutureWork<(), Infallible>;
type ClassQueue = BoundedQueue<Work, Waiting, GaveUpFirst, Fifo>;
type ClassProducer = BoundedQueueProducer<Work, Waiting, GaveUpFirst, Fifo>;
type ClassNode = BoundedConcurrency<Work, ClassQueue>;
type Root = StrictPriority<Work, ClassNode>;
/// Work leaves the priority node tagged with its class's priority.
type Meta = WithPriority<Waiting>;

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
/// number of classes that take slots and their `max_queued`. Sized, like the
/// proxy's caches, far above any realistic fleet.
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
}

impl Refused {
    /// What the caller is answered: `429` when the proxy's per-BMC limits
    /// turned the request away, `503` when the proxy itself could not take
    /// it. Either way, the BMC never received the request.
    pub(super) fn status(self) -> StatusCode {
        match self {
            Self::QueueFull | Self::Timeout => StatusCode::TOO_MANY_REQUESTS,
            Self::TooManyBmcs | Self::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
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

/// The proxy refused a request for want of a slot at its BMC. Metric-only:
/// refusals come as fast as callers retry, and the caller's answer names the
/// reason.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_admission_refused",
    metric_name = "carbide_bmc_proxy_admission_refused_total",
    component = "nico-bmc-proxy",
    log = off,
    metric = counter,
    describe = "Number of requests the proxy refused without sending them, for want of a slot at their BMC, by request class and reason (queue_full, timeout, too_many_bmcs, shutting_down)"
)]
struct AdmissionRefused {
    #[label]
    class: ClassName,
    #[label]
    reason: Refused,
}

/// A queued request.
struct Waiting {
    /// The request's claim to its place: gone once it stops waiting.
    claim: Weak<()>,
    /// How many requests the queue may hold for this one to join it.
    room: usize,
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
        if context.depth() < incoming.meta.room {
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
}

/// A BMC in use: its class queues, and the runtime serving them.
struct Bmc {
    /// One per class that takes slots, in the order of
    /// [`Admission::classes`].
    queues: Vec<ClassProducer>,
    /// Stops the BMC's runtime.
    stop: CancellationToken,
    last_used: Instant,
}

pub(super) struct Admission {
    /// Every class whose requests take slots.
    classes: Vec<SlotClass>,
    max_in_flight_per_bmc: NonZeroUsize,
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
            .filter(|class| per_bmc.is_some() || class.max_in_flight.is_some())
            .map(|class| SlotClass {
                name: class.name.clone(),
                priority: class.priority,
                max_in_flight: class.max_in_flight.unwrap_or(NonZeroU32::MAX),
                max_queued: class.max_queued,
            })
            .collect();
        let admission = Arc::new(Self {
            max_in_flight_per_bmc: per_bmc.map_or(NonZeroUsize::MAX, |max| {
                NonZeroUsize::try_from(max).expect("a u32 fits in a usize")
            }),
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

    /// A slot at `bmc` for a request of `class`, waiting for it until
    /// `deadline`, and held at most `hold_for` once granted. A class that
    /// takes no slots gets one at once.
    pub(super) async fn acquire(
        &self,
        bmc: IpAddr,
        class: &RequestClass,
        deadline: Instant,
        hold_for: Duration,
    ) -> Result<Slot, Refused> {
        let Some(index) = self
            .classes
            .iter()
            .position(|slot_class| slot_class.name == class.name)
        else {
            return Ok(Slot { _release: None });
        };
        let started = Instant::now();
        let granted = self.wait_for_slot(bmc, index, deadline, hold_for).await;
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
        deadline: Instant,
        hold_for: Duration,
    ) -> Result<Slot, Refused> {
        let waiting = Arc::new(());
        let (grant_tx, grant_rx) = oneshot::channel();
        self.enqueue(
            bmc,
            class,
            Arc::downgrade(&waiting),
            grant(grant_tx, hold_for),
        )?;
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
                    return Err(Refused::Timeout);
                }
                Ok(Slot {
                    _release: Some(release),
                })
            }
            () = tokio::time::sleep_until(deadline) => Err(Refused::Timeout),
        }
    }

    /// Queues `grant`, for the request that holds `claim`, in the queue of
    /// class `class` at `bmc`, starting the BMC's runtime on its first
    /// request.
    fn enqueue(
        &self,
        bmc: IpAddr,
        class: usize,
        claim: Weak<()>,
        grant: impl Future<Output = Result<Vec<()>, Infallible>> + Send + 'static,
    ) -> Result<(), Refused> {
        let mut bmcs = lock(&self.bmcs);
        if !bmcs.contains_key(&bmc) && bmcs.len() >= MAX_BMCS {
            return Err(Refused::TooManyBmcs);
        }
        let bmc = bmcs.entry(bmc).or_insert_with(|| self.start_bmc());
        bmc.last_used = Instant::now();
        // Requests queue one at a time under this lock. The runtime grants
        // and frees slots meanwhile, so the room can be off by a slot.
        let waiting = Waiting {
            claim,
            room: self.room(bmc, class),
        };
        match bmc.queues[class].try_push(ScheduledWork::new(waiting, Box::pin(grant))) {
            EnqueueOutcome::Admitted | EnqueueOutcome::Evicted { .. } => Ok(()),
            EnqueueOutcome::Rejected(_) => Err(Refused::QueueFull),
            EnqueueOutcome::Closed(_) => Err(Refused::ShuttingDown),
        }
    }

    /// How many requests class `class`'s queue at `bmc` may hold: its
    /// `max_queued`, and one for each slot free for the class now.
    fn room(&self, bmc: &Bmc, class: usize) -> usize {
        let in_flight: Vec<usize> = bmc
            .queues
            .iter()
            .map(|queue| queue.stats().in_flight)
            .collect();
        let max_in_flight =
            usize::try_from(self.classes[class].max_in_flight.get()).unwrap_or(usize::MAX);
        let free = max_in_flight.saturating_sub(in_flight[class]).min(
            self.max_in_flight_per_bmc
                .get()
                .saturating_sub(in_flight.iter().sum()),
        );
        self.classes[class].max_queued.get().saturating_add(free)
    }

    /// A BMC's queues, and its runtime started on its own task.
    fn start_bmc(&self) -> Bmc {
        let mut root = Root::new();
        let queues = self
            .classes
            .iter()
            .map(|class| {
                let passing_through = usize::try_from(class.max_in_flight.get())
                    .unwrap_or(usize::MAX)
                    .min(self.max_in_flight_per_bmc.get())
                    .min(MAX_PASSING_THROUGH);
                let (queue, producer): (ClassQueue, ClassProducer) =
                    BoundedQueueBuilder::new(class.max_queued.saturating_add(passing_through))
                        .admission_policy(GaveUpFirst)
                        .fifo()
                        .build();
                root.add_child(
                    BoundedConcurrency::new(class.max_in_flight, queue),
                    class.priority,
                );
                producer
            })
            .collect();
        let runtime = Runtime::new(
            RuntimeConfig {
                global_max_in_flight: self.max_in_flight_per_bmc,
                clock: ClockConfig::Wallclock,
            },
            root,
        );
        let stop = self.shutdown.child_token();
        lock(&self.runtimes)
            .build_task()
            .name("bmc admission runtime")
            .spawn(drive(runtime, stop.clone()))
            .expect("spawning a bmc admission runtime must succeed");
        Bmc {
            queues,
            stop,
            last_used: Instant::now(),
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
    /// [`IDLE_BMC_TIMEOUT`] and that no request holds a slot at or waits
    /// for now.
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

/// The work the dispatcher runs for a request: hands the request its slot,
/// then holds the BMC's capacity until the request drops the slot, or for
/// `hold_for` at most, after which the slot's exchange must have ended.
async fn grant(
    slot: oneshot::Sender<oneshot::Sender<()>>,
    hold_for: Duration,
) -> Result<Vec<()>, Infallible> {
    let (release, released) = oneshot::channel();
    if slot.send(release).is_ok() {
        // Ends, as an error, once the request drops its slot.
        let _released_or_expired = tokio::time::timeout(hold_for, released).await;
    }
    Ok(Vec::new())
}

/// Drives a BMC's runtime until `stop`. Requests that got their slot keep
/// it; see [`Admission::start`] for the requests still waiting.
async fn drive(mut runtime: Runtime<(), Infallible, Meta>, stop: CancellationToken) {
    loop {
        let output = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            output = runtime.next() => output,
        };
        match output {
            RuntimeOutput::SleepUntil(deadline) => {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => return,
                    () = tokio::time::sleep_until(Instant::from_std(deadline)) => {}
                }
            }
            RuntimeOutput::Work { result, .. } => match result {
                Ok(_) => {}
                Err(never) => match never {},
            },
            RuntimeOutput::Runtime(event) => match event {},
            RuntimeOutput::Shutdown => return,
        }
    }
}

/// A request's slot at its BMC. Dropping it frees the slot for the next
/// waiting request.
#[must_use]
pub(super) struct Slot {
    /// Dropping this ends the grant that holds the slot; `None` for a class
    /// that takes no slots.
    _release: Option<oneshot::Sender<()>>,
}

impl Slot {
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
    use std::pin::pin;
    use std::time::Duration;

    use axum::body::Body;
    use carbide_instrument::testing::MetricsCapture;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, Check, check_cases_async, check_values};
    use futures::FutureExt;
    use hyper::body::Body as HttpBody;
    use tokio::task::JoinSet;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use super::{
        Admission, Bmc, IDLE_BMC_SWEEP_INTERVAL, IDLE_BMC_TIMEOUT, MAX_BMCS, Refused, Slot,
    };
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
        let deadline = Instant::now() + class.upstream_timeout;
        state.admission.acquire(at, class, deadline, hold_for).await
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
        {
            let mut bmcs = state.admission.bmcs.lock().unwrap();
            for n in 0..MAX_BMCS - 1 {
                let octets = (u32::try_from(n).unwrap() + (10 << 24)).to_be_bytes();
                bmcs.insert(
                    IpAddr::from(octets),
                    Bmc {
                        queues: Vec::new(),
                        stop: CancellationToken::new(),
                        last_used: Instant::now(),
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
    /// request away, and `503` when the proxy itself could not take it.
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
            ],
            |refused| refused.status().as_u16(),
        );
    }

    /// A body keeps its length and its end when it holds a slot, so the
    /// caller's response is framed as it would be without one.
    #[test]
    fn a_held_body_is_framed_as_before() {
        let held = |body| Slot { _release: None }.hold_until_sent(body);
        assert_eq!(held(Body::from("abc")).size_hint().exact(), Some(3));
        assert!(held(Body::empty()).is_end_stream());
    }
}
