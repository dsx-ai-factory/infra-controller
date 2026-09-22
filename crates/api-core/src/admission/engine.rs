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

//! Transport- and application-independent fair admission scheduling.
//!
//! The dispatcher owns only grant/completion bridges. Request tasks retain
//! their handler futures, so cancellation after a grant naturally cancels the
//! handler. Cancellation while queued drops the grant receiver; the canonical
//! queue entry remains until normal dequeue, where its first operation observes
//! the closed receiver and completes without invoking business logic.

use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use carbide_authn::middleware::ExternalUserInfo;
use hashbrown::Equivalent;
use hashbrown::hash_map::EntryRef;
use nv_redfish_dispatcher::schedulers::{
    BoundedConcurrency, BoundedQueue, BoundedQueueProducer, Fifo, RoundRobin, TailDrop,
};
use nv_redfish_dispatcher::{
    BoundedQueueBuilder, ClockConfig, EnqueueOutcome, FutureWork, Runtime, RuntimeConfig,
    RuntimeHandle, RuntimeOutput, ScheduledWork,
};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::limits::{AdmissionLimits, ClientLimits};
use super::peak_ewma::{PeakEwma, initial_duration};
use super::retry::{
    AdmissionPressure, RejectionScope, RetryAdvice, random_retry_jitter, retry_advice,
};

type Work = FutureWork<(), Infallible>;
type ClientQueue = BoundedQueue<Work, (), TailDrop, Fifo>;
type ClientProducer = BoundedQueueProducer<Work, (), TailDrop, Fifo>;
type SchedulerRoot = RoundRobin<Work, ()>;
type SchedulerHandle = RuntimeHandle<(), Infallible, ()>;

// This is a dispatcher-task lifecycle failsafe, not a handler execution limit.
// Keep it strictly below Kubernetes' current 30-second termination grace period
// so roughly ten seconds remain for the rest of process shutdown. These budgets
// should eventually come from one coordinated source; keep this internal until
// that shutdown-wide contract exists.
const DISPATCHER_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RejectionReason {
    QueueFull(RejectionScope),
    EstimatedQueueDelay(RejectionScope),
    QueueTimeout,
    ControllerUnavailable,
    ShuttingDown,
}

impl RejectionReason {
    pub(super) const fn scope(self) -> Option<RejectionScope> {
        match self {
            Self::QueueFull(scope) | Self::EstimatedQueueDelay(scope) => Some(scope),
            Self::QueueTimeout | Self::ControllerUnavailable | Self::ShuttingDown => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AdmissionRejection {
    pub(super) reason: RejectionReason,
    pub(super) retry: RetryAdvice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClientKey {
    /// An authenticated user, grouped by its complete certificate identity.
    ExternalUser(ExternalUserInfo),
    /// A SPIFFE service identifier.
    ServiceId(String),
    /// A SPIFFE machine identifier.
    MachineId(String),
    /// A request that has no recognized client identity.
    Default,
}

impl Hash for ClientKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // ClientKeyRef and ClientKey must hash the same: Construct a ref first and then hash it.
        self.as_ref().hash(state);
    }
}

impl ClientKey {
    fn as_ref(&self) -> ClientKeyRef<'_> {
        match self {
            ClientKey::Default => ClientKeyRef::Default,
            ClientKey::ExternalUser(u) => ClientKeyRef::ExternalUser(u),
            ClientKey::ServiceId(id) => ClientKeyRef::ServiceId(id),
            ClientKey::MachineId(id) => ClientKeyRef::MachineId(id),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(super) enum ClientKeyRef<'a> {
    ExternalUser(&'a ExternalUserInfo),
    ServiceId(&'a str),
    MachineId(&'a str),
    Default,
}

impl ClientKeyRef<'_> {
    fn to_client_key(self) -> ClientKey {
        match self {
            ClientKeyRef::Default => ClientKey::Default,
            ClientKeyRef::ExternalUser(u) => ClientKey::ExternalUser(u.to_owned()),
            ClientKeyRef::ServiceId(id) => ClientKey::ServiceId(id.to_owned()),
            ClientKeyRef::MachineId(id) => ClientKey::MachineId(id.to_owned()),
        }
    }
}

impl Equivalent<ClientKey> for ClientKeyRef<'_> {
    fn equivalent(&self, key: &ClientKey) -> bool {
        self == &key.as_ref()
    }
}

pub(super) trait AdmissionObserver: Send + Sync + 'static {
    fn admitted(&self) {}

    fn pending_finished(&self, _duration: Duration) {}

    fn execution_finished(&self, _duration: Duration) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AdmissionSnapshot {
    pub(super) work_in_flight: usize,
    pub(super) pending: usize,
}

struct ClientState {
    child_id: u32,
    producer: ClientProducer,
    limits: ClientLimits,
    execution_ewma: Arc<PeakEwma>,
    last_activity: Instant,
}

struct EngineState {
    clients: hashbrown::HashMap<ClientKey, ClientState>,
}

pub(super) struct FairAdmission {
    limits: AdmissionLimits,
    state: Mutex<EngineState>,
    runtime_handle: SchedulerHandle,
    pending: Arc<AtomicUsize>,
    global_execution_ewma: Arc<PeakEwma>,
    shutdown: CancellationToken,
    observer: Arc<dyn AdmissionObserver>,
}

impl FairAdmission {
    pub(super) fn start(
        limits: AdmissionLimits,
        shutdown: CancellationToken,
        observer: Arc<dyn AdmissionObserver>,
        join_set: &mut JoinSet<()>,
    ) -> Arc<Self> {
        let root = SchedulerRoot::new();
        let runtime = Runtime::new(
            RuntimeConfig {
                global_max_in_flight: limits.max_work_in_flight,
                clock: ClockConfig::Wallclock,
            },
            root,
        );
        let runtime_handle = runtime.handle();
        let initial_global_duration = initial_duration(
            limits.default_client().pending_timeout(),
            limits.max_work_in_flight(),
            limits.max_pending(),
        );
        let engine = Arc::new(Self {
            limits,
            state: Mutex::new(EngineState {
                clients: hashbrown::HashMap::new(),
            }),
            runtime_handle: runtime_handle.clone(),
            pending: Arc::new(AtomicUsize::new(0)),
            global_execution_ewma: Arc::new(PeakEwma::new(initial_global_duration)),
            shutdown: shutdown.clone(),
            observer,
        });

        join_set
            .build_task()
            .name("API admission dispatcher")
            .spawn(run_dispatcher(runtime, runtime_handle, shutdown.clone()))
            .expect("spawning API admission dispatcher must succeed");

        let cleanup_engine = Arc::clone(&engine);
        join_set
            .build_task()
            .name("API admission client cleanup")
            .spawn(async move { cleanup_engine.run_cleanup(shutdown).await })
            .expect("spawning API admission cleanup must succeed");

        engine
    }

    /// Enqueues a request in the fair per-client scheduler and waits for permission
    /// to execute its handler.
    ///
    /// The dispatcher never owns or polls the handler itself. It schedules a small
    /// grant bridge that connects scheduler capacity to an RAII execution permit:
    ///
    /// ```text
    /// Original request task                    Dispatcher runtime
    /// ─────────────────────                    ──────────────────
    ///
    /// create grant channel
    ///        │
    ///        │  enqueue GrantBridge
    ///        ├───────────────────────────────► per-client FIFO
    ///        │                                  │
    ///        │                                  │ round-robin selection
    ///        │                                  │ + client/global concurrency
    ///        │                                  ▼
    ///        │                               start GrantBridge
    ///        │                                  │
    ///        │  send completion sender          │
    ///        ◄──────────────────────────────────┤
    ///        │                                  │
    /// return ExecutionPermit                    │ wait for completion
    ///        │                                  │
    /// execute original handler                  │
    ///        │                                  │
    /// drop ExecutionPermit                      │
    ///        ├──── signal completion ──────────►│
    ///        │                                  ▼
    ///        │                             release capacity
    /// ```
    ///
    /// Admission applies global and per-client hard queue bounds before enqueueing.
    /// It also rejects requests whose estimated wait in their per-client FIFO
    /// exceeds their pending timeout.
    /// Every rejection carries retry guidance derived from the stricter of client
    /// and global pressure.
    ///
    /// If the request is cancelled or times out while queued, the grant receiver is
    /// dropped. Its canonical queue entry remains until normal dequeue. When the
    /// dispatcher eventually selects it, `GrantBridge` observes the closed receiver
    /// and completes without executing business logic.
    ///
    /// The state mutex protects client scheduler creation and the enqueue decision.
    /// It is released before this method waits for a grant.
    pub(super) async fn acquire(
        &self,
        client_key: ClientKeyRef<'_>,
        client_limits: ClientLimits,
    ) -> Result<ExecutionPermit, AdmissionRejection> {
        // Phase 1: Fast preflight and grant-channel setup.
        //
        // Avoid taking the client-state lock once shutdown is already visible.
        // The check is repeated under the lock below to close the race between
        // this observation and the atomic admission decision.
        if self.shutdown.is_cancelled() {
            return Err(self.rejection(RejectionReason::ShuttingDown, &client_key, client_limits));
        }

        let (grant_tx, grant_rx) = oneshot::channel();

        // Phase 2: Make one atomic admission decision under the state lock.
        //
        // Client creation, pressure sampling, limit checks, and enqueueing stay
        // in the same critical section so concurrent requests cannot make
        // decisions from mutually inconsistent client state.
        {
            let mut state = lock(&self.state);
            if self.shutdown.is_cancelled() {
                let retry = self.retry_advice(&state, &client_key, client_limits);
                return Err(AdmissionRejection {
                    reason: RejectionReason::ShuttingDown,
                    retry,
                });
            }

            // Reject at the global hard boundary before creating client state:
            // a request that cannot enter the global queue must not allocate a
            // new scheduler subtree merely to be rejected.
            if self.pending.load(Ordering::Relaxed) >= self.limits.max_pending() {
                let retry = self.retry_advice(&state, &client_key, client_limits);
                return Err(AdmissionRejection {
                    reason: RejectionReason::QueueFull(RejectionScope::Global),
                    retry,
                });
            }

            // Resolve (or lazily create) the client's bounded scheduler, then
            // capture client and global pressure from the same admission turn.
            // These snapshots are reused for all rejection advice below.
            let client = match self.client(&mut state, &client_key, client_limits) {
                Ok(client) => client,
                Err(reason) => {
                    let retry = self.retry_advice(&state, &client_key, client_limits);
                    return Err(AdmissionRejection { reason, retry });
                }
            };
            let client_stats = client.producer.stats();
            let global_stats = self.runtime_handle.stats();
            let global_pending = self.pending.load(Ordering::Relaxed);
            let client_pressure = AdmissionPressure {
                work_in_flight: client_stats.in_flight,
                pending: client_stats.depth,
                max_work_in_flight: client.limits.max_work_in_flight(),
                execution_duration_ewma: client.execution_ewma.duration(),
            };
            let global_pressure = AdmissionPressure {
                work_in_flight: global_stats.in_flight as usize,
                pending: global_pending,
                max_work_in_flight: self.limits.max_work_in_flight(),
                execution_duration_ewma: self.global_execution_ewma.duration(),
            };
            // Phase 3: Apply client hard capacity and predictive backpressure.
            //
            // The hard bound protects memory. The per-client FIFO provides a
            // meaningful queue position, so its EWMA can reject earlier when
            // the request is not expected to receive a grant before its pending
            // timeout. Global pending depth cannot provide the same prediction:
            // the root scheduler serves runnable client queues round-robin and
            // skips queues blocked by their per-client concurrency limits.
            if client_stats.depth >= client.limits.max_pending() {
                return Err(AdmissionRejection {
                    reason: RejectionReason::QueueFull(RejectionScope::Client),
                    retry: retry_advice(client_pressure, global_pressure, random_retry_jitter()),
                });
            }

            if client_pressure.estimated_wait() > client.limits.pending_timeout() {
                return Err(AdmissionRejection {
                    reason: RejectionReason::EstimatedQueueDelay(RejectionScope::Client),
                    retry: retry_advice(client_pressure, global_pressure, random_retry_jitter()),
                });
            }

            // Phase 4: Enqueue the grant bridge, not the business handler.
            //
            // `PendingEntry` accounts for queue residence until the dispatcher
            // starts the bridge. Once started, the bridge grants execution to
            // this request task and holds scheduler capacity until the returned
            // `ExecutionPermit` signals completion.
            let pending = PendingEntry {
                global_pending: Arc::clone(&self.pending),
                started: Instant::now(),
                observer: Arc::clone(&self.observer),
            };
            self.pending.fetch_add(1, Ordering::Relaxed);
            let work: Work = Box::pin(dispatch_grant(grant_tx, pending));
            match client.producer.try_push(ScheduledWork::new((), work)) {
                EnqueueOutcome::Admitted => client.last_activity = Instant::now(),
                EnqueueOutcome::Rejected(_work) => {
                    return Err(AdmissionRejection {
                        reason: RejectionReason::QueueFull(RejectionScope::Client),
                        retry: retry_advice(
                            client_pressure,
                            global_pressure,
                            random_retry_jitter(),
                        ),
                    });
                }
                EnqueueOutcome::Closed(_work) => {
                    return Err(AdmissionRejection {
                        reason: RejectionReason::ControllerUnavailable,
                        retry: retry_advice(
                            client_pressure,
                            global_pressure,
                            random_retry_jitter(),
                        ),
                    });
                }
                EnqueueOutcome::Evicted { .. } => {
                    unreachable!("tail-drop admission never evicts queued work")
                }
            }
        }

        // Phase 5: Wait for a grant without holding the client-state lock.
        //
        // A timeout or cancellation drops `grant_rx`. The queued bridge remains
        // canonical and later observes that closed receiver when dequeued. The
        // biased select gives shutdown and timeout precedence over a simultaneous
        // grant, preventing new handler work after either deadline is visible.
        let timeout = tokio::time::sleep(client_limits.pending_timeout());
        tokio::pin!(timeout);
        let completion = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => {
                return Err(self.rejection(
                    RejectionReason::ShuttingDown,
                    &client_key,
                    client_limits,
                ));
            }
            () = &mut timeout => {
                return Err(self.rejection(
                    RejectionReason::QueueTimeout,
                    &client_key,
                    client_limits,
                ));
            }
            result = grant_rx => match result {
                Ok(completion) => completion,
                Err(_) => {
                    return Err(self.rejection(
                        RejectionReason::ControllerUnavailable,
                        &client_key,
                        client_limits,
                    ));
                }
            },
        };

        // Phase 6: Materialize the RAII execution lease.
        //
        // The permit owns the bridge's completion sender. Its `Drop` releases
        // scheduler capacity and records the observed handler duration in both
        // the client and global execution EWMAs.
        let client_execution_ewma = match self.client_ewma(&client_key) {
            Ok(ewma) => ewma,
            Err(reason) => return Err(self.rejection(reason, &client_key, client_limits)),
        };
        self.observer.admitted();
        Ok(ExecutionPermit {
            completion: Some(completion),
            started: Instant::now(),
            observer: Arc::clone(&self.observer),
            client_execution_ewma,
            global_execution_ewma: Arc::clone(&self.global_execution_ewma),
        })
    }

    fn rejection(
        &self,
        reason: RejectionReason,
        client_key: &ClientKeyRef,
        client_limits: ClientLimits,
    ) -> AdmissionRejection {
        let state = lock(&self.state);
        AdmissionRejection {
            reason,
            retry: self.retry_advice(&state, client_key, client_limits),
        }
    }

    fn retry_advice(
        &self,
        state: &EngineState,
        client_key: &ClientKeyRef,
        client_limits: ClientLimits,
    ) -> RetryAdvice {
        let client_pressure = state.clients.get(client_key).map_or_else(
            || AdmissionPressure {
                work_in_flight: 0,
                pending: 0,
                max_work_in_flight: client_limits.max_work_in_flight(),
                execution_duration_ewma: initial_duration(
                    client_limits.pending_timeout(),
                    client_limits.max_work_in_flight(),
                    client_limits.max_pending(),
                ),
            },
            |client| {
                let stats = client.producer.stats();
                AdmissionPressure {
                    work_in_flight: stats.in_flight,
                    pending: stats.depth,
                    max_work_in_flight: client.limits.max_work_in_flight(),
                    execution_duration_ewma: client.execution_ewma.duration(),
                }
            },
        );
        let global_stats = self.runtime_handle.stats();
        let global_pressure = AdmissionPressure {
            work_in_flight: global_stats.in_flight as usize,
            pending: self.pending.load(Ordering::Relaxed),
            max_work_in_flight: self.limits.max_work_in_flight(),
            execution_duration_ewma: self.global_execution_ewma.duration(),
        };
        retry_advice(client_pressure, global_pressure, random_retry_jitter())
    }

    fn client<'a>(
        &self,
        state: &'a mut EngineState,
        client_key: &ClientKeyRef,
        limits: ClientLimits,
    ) -> Result<&'a mut ClientState, RejectionReason> {
        let client = match state.clients.entry_ref(client_key) {
            EntryRef::Occupied(entry) => entry.into_mut(),
            EntryRef::Vacant(entry) => {
                let (queue, producer): (ClientQueue, ClientProducer) =
                    BoundedQueueBuilder::new(limits.max_pending).fifo().build();
                let child = BoundedConcurrency::new(limits.max_work_in_flight, queue);
                let child_id = self
                    .runtime_handle
                    .with_root_mut::<SchedulerRoot, _>(|mut root| root.add_child(child))
                    .ok_or(RejectionReason::ControllerUnavailable)?;
                let execution_ewma = Arc::new(PeakEwma::new(initial_duration(
                    limits.pending_timeout(),
                    limits.max_work_in_flight(),
                    limits.max_pending(),
                )));
                entry.insert_with_key(
                    client_key.to_client_key(),
                    ClientState {
                        child_id,
                        producer,
                        limits,
                        execution_ewma,
                        last_activity: Instant::now(),
                    },
                )
            }
        };
        Ok(client)
    }

    fn client_ewma(&self, client_key: &ClientKeyRef) -> Result<Arc<PeakEwma>, RejectionReason> {
        lock(&self.state)
            .clients
            .get(client_key)
            .map(|client| Arc::clone(&client.execution_ewma))
            .ok_or(RejectionReason::ControllerUnavailable)
    }

    pub(super) fn snapshot(&self) -> AdmissionSnapshot {
        AdmissionSnapshot {
            work_in_flight: self.runtime_handle.stats().in_flight as usize,
            pending: self.pending.load(Ordering::Relaxed),
        }
    }

    async fn run_cleanup(&self, shutdown: CancellationToken) {
        let cadence = self
            .limits
            .client_idle_timeout()
            .min(Duration::from_secs(30));
        let mut interval = tokio::time::interval(cadence);
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                _ = interval.tick() => self.cleanup_idle_clients(Instant::now()),
            }
        }
    }

    fn cleanup_idle_clients(&self, now: Instant) {
        let removed = {
            let mut state = lock(&self.state);
            let idle_timeout = self.limits.client_idle_timeout();
            let keys: Vec<_> = state
                .clients
                .iter()
                .filter_map(|(key, client)| {
                    let stats = client.producer.stats();
                    (stats.depth == 0
                        && stats.in_flight == 0
                        && now.saturating_duration_since(client.last_activity) >= idle_timeout)
                        .then(|| key.clone())
                })
                .collect();
            keys.into_iter()
                .filter_map(|key| state.clients.remove(&key))
                .collect::<Vec<_>>()
        };

        for client in removed {
            self.runtime_handle
                .with_root_mut::<SchedulerRoot, _>(|mut root| root.remove_child(client.child_id));
        }
    }
}

async fn dispatch_grant(
    grant: oneshot::Sender<oneshot::Sender<()>>,
    pending: PendingEntry,
) -> Result<Vec<()>, Infallible> {
    drop(pending);
    let (completion, completed) = oneshot::channel();
    if grant.send(completion).is_ok() {
        completed.await.ok();
    }
    Ok(Vec::new())
}

async fn run_dispatcher(
    mut runtime: Runtime<(), Infallible, ()>,
    handle: SchedulerHandle,
    shutdown: CancellationToken,
) {
    // Phase 1: drive normal scheduling until API shutdown is requested. A
    // SleepUntil hint is interruptible so a distant scheduler wake-up cannot
    // postpone the transition into shutdown.
    loop {
        let output = tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                break;
            }
            output = runtime.next() => output,
        };
        if !handle_runtime_output(output, Some(&shutdown)).await {
            break;
        }
    }

    // Phase 2: stop dispatching queued work and give already admitted bridges
    // a bounded opportunity to observe their ExecutionPermit completion.
    handle.graceful_shutdown();
    let drain = async {
        loop {
            if !handle_runtime_output(runtime.next().await, None).await {
                return;
            }
        }
    };
    let drain_deadline = tokio::time::Instant::now() + DISPATCHER_SHUTDOWN_DRAIN_TIMEOUT;
    if tokio::time::timeout_at(drain_deadline, drain)
        .await
        .is_err()
    {
        let stats = handle.stats();
        tracing::warn!(
            shutdown_drain_timeout_seconds = DISPATCHER_SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
            remaining_work_in_flight = stats.in_flight,
            queued_runtime_outputs = stats.output_queue.queued,
            "API admission dispatcher shutdown drain timed out"
        );
    }

    // Dropping the runtime after the deadline is safe: it drops only queued or
    // running grant bridges owned by the scheduler. Request tasks own their
    // handler futures, so admitted business logic is not preempted; a later
    // ExecutionPermit completion simply observes that its bridge is gone.
}

/// Handles one runtime output and returns whether the caller should continue
/// driving the runtime.
async fn handle_runtime_output(
    output: RuntimeOutput<(), Infallible>,
    shutdown: Option<&CancellationToken>,
) -> bool {
    match output {
        RuntimeOutput::SleepUntil(deadline) => {
            let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
            if let Some(shutdown) = shutdown {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return false,
                    () = sleep => {}
                }
            } else {
                sleep.await;
            }
        }
        RuntimeOutput::Work { result, .. } => match result {
            Ok(_) => {}
            Err(error) => match error {},
        },
        RuntimeOutput::Runtime(event) => match event {},
        RuntimeOutput::Shutdown => return false,
    }
    true
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("admission state mutex must not be poisoned")
}

pub(super) struct ExecutionPermit {
    completion: Option<oneshot::Sender<()>>,
    started: Instant,
    observer: Arc<dyn AdmissionObserver>,
    client_execution_ewma: Arc<PeakEwma>,
    global_execution_ewma: Arc<PeakEwma>,
}

impl std::fmt::Debug for ExecutionPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ExecutionPermit")
    }
}

impl Drop for ExecutionPermit {
    fn drop(&mut self) {
        self.completion
            .take()
            .and_then(|sender| sender.send(()).ok());
        let duration = self.started.elapsed();
        self.client_execution_ewma.observe(duration);
        self.global_execution_ewma.observe(duration);
        self.observer.execution_finished(duration);
    }
}

struct PendingEntry {
    global_pending: Arc<AtomicUsize>,
    started: Instant,
    observer: Arc<dyn AdmissionObserver>,
}

impl Drop for PendingEntry {
    fn drop(&mut self) {
        self.global_pending.fetch_sub(1, Ordering::Relaxed);
        self.observer.pending_finished(self.started.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroUsize};

    use futures::poll;
    use nv_redfish_dispatcher::{Completion, QueueEventSink, Readiness, Scheduler};
    use tokio::sync::Notify;
    use tokio::task::JoinHandle;

    use super::*;

    struct NoopObserver;

    impl AdmissionObserver for NoopObserver {}

    async fn started_bridge() -> (CancellationToken, oneshot::Sender<()>, JoinHandle<()>) {
        let (queue, producer): (ClientQueue, ClientProducer) = BoundedQueueBuilder::new(
            NonZeroUsize::new(1).expect("test queue capacity is non-zero"),
        )
        .fifo()
        .build();
        let child = BoundedConcurrency::new(
            NonZeroU32::new(1).expect("test concurrency is non-zero"),
            queue,
        );
        let mut root = SchedulerRoot::new();
        let _child_id = root.add_child(child);
        let runtime = Runtime::new(
            RuntimeConfig {
                global_max_in_flight: NonZeroUsize::new(1)
                    .expect("test global concurrency is non-zero"),
                clock: ClockConfig::Wallclock,
            },
            root,
        );
        let handle = runtime.handle();
        let shutdown = CancellationToken::new();
        let (grant_tx, grant_rx) = oneshot::channel();
        let pending = PendingEntry {
            global_pending: Arc::new(AtomicUsize::new(1)),
            started: Instant::now(),
            observer: Arc::new(NoopObserver),
        };
        let work: Work = Box::pin(dispatch_grant(grant_tx, pending));
        assert!(matches!(
            producer.try_push(ScheduledWork::new((), work)),
            EnqueueOutcome::Admitted
        ));
        let dispatcher = tokio::spawn(run_dispatcher(runtime, handle, shutdown.clone()));
        let completion = grant_rx.await.expect("dispatcher starts the grant bridge");
        (shutdown, completion, dispatcher)
    }

    #[tokio::test(start_paused = true)]
    async fn dispatcher_shutdown_gracefully_drains_completed_bridge() {
        let (shutdown, completion, dispatcher) = started_bridge().await;

        shutdown.cancel();
        drop(completion);

        tokio::time::timeout(Duration::from_secs(1), dispatcher)
            .await
            .expect("completed bridge drains before the shutdown deadline")
            .expect("dispatcher task exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn dispatcher_shutdown_bounds_outstanding_bridge() {
        let (shutdown, completion, dispatcher) = started_bridge().await;

        shutdown.cancel();
        tokio::task::yield_now().await;
        assert!(!dispatcher.is_finished());

        tokio::time::advance(DISPATCHER_SHUTDOWN_DRAIN_TIMEOUT - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(!dispatcher.is_finished());

        tokio::time::advance(Duration::from_millis(1)).await;
        dispatcher
            .await
            .expect("dispatcher exits when the shutdown drain deadline expires");
        assert!(
            completion.send(()).is_err(),
            "timed-out dispatcher drops the outstanding grant bridge"
        );
    }

    struct LongSleepScheduler {
        deadline: Instant,
        sleep_observed: Arc<Notify>,
    }

    impl Scheduler<Work> for LongSleepScheduler {
        type Meta = ();

        fn update_ready(&mut self, _now: Instant) -> Readiness {
            self.sleep_observed.notify_one();
            Readiness::not_ready(Some(self.deadline))
        }

        fn take_next(&mut self) -> Option<ScheduledWork<Work, Self::Meta>> {
            None
        }

        fn on_complete(&mut self, _completion: Completion<Self::Meta>) {}

        fn register_queue_event_sink(&mut self, _sink: QueueEventSink) {}
    }

    #[tokio::test(start_paused = true)]
    async fn dispatcher_cancellation_interrupts_long_sleep_until() {
        let sleep_observed = Arc::new(Notify::new());
        let mut root = SchedulerRoot::new();
        let _child_id = root.add_child(LongSleepScheduler {
            deadline: Instant::now() + Duration::from_secs(60 * 60),
            sleep_observed: Arc::clone(&sleep_observed),
        });
        let runtime = Runtime::new(
            RuntimeConfig {
                global_max_in_flight: NonZeroUsize::new(1)
                    .expect("test global concurrency is non-zero"),
                clock: ClockConfig::Wallclock,
            },
            root,
        );
        let handle = runtime.handle();
        let shutdown = CancellationToken::new();
        let dispatcher = tokio::spawn(run_dispatcher(runtime, handle, shutdown.clone()));
        sleep_observed.notified().await;

        shutdown.cancel();

        tokio::time::timeout(Duration::from_secs(1), dispatcher)
            .await
            .expect("shutdown interrupts the distant SleepUntil deadline")
            .expect("dispatcher task exits cleanly");
    }

    #[tokio::test]
    async fn non_runnable_backlog_does_not_reject_independent_client_at_global_saturation() {
        let pending_timeout = Duration::from_secs(5);
        let limits = AdmissionLimits::new(4, 16, 1, 4, pending_timeout, Duration::from_secs(60))
            .expect("test admission limits are valid");
        let client_limits = limits.default_client();
        let unused_global_capacity =
            limits.max_work_in_flight() - client_limits.max_work_in_flight();
        let shutdown = CancellationToken::new();
        let mut join_set = JoinSet::new();
        let engine = FairAdmission::start(limits, shutdown, Arc::new(NoopObserver), &mut join_set);

        let mut slow_sample = engine
            .acquire(ClientKeyRef::ServiceId("slow-client"), client_limits)
            .await
            .expect("the first request is admitted");
        // Record a historical slow handler without making this unit test wait
        // for the wall-clock duration.
        slow_sample.started = slow_sample
            .started
            .checked_sub(pending_timeout + Duration::from_secs(1))
            .expect("test execution start remains representable");
        drop(slow_sample);

        let backlogged_request = engine
            .acquire(ClientKeyRef::ServiceId("backlogged-client"), client_limits)
            .await
            .expect("the backlogged client's first request is admitted");
        let mut backlog = Vec::new();
        for _ in 0..unused_global_capacity {
            let mut request = Box::pin(
                engine.acquire(ClientKeyRef::ServiceId("backlogged-client"), client_limits),
            );
            assert!(poll!(request.as_mut()).is_pending());
            backlog.push(request);
        }

        let mut other_active_requests = Vec::new();
        for client in ["active-client-a", "active-client-b", "active-client-c"] {
            other_active_requests.push(
                engine
                    .acquire(ClientKeyRef::ServiceId(client), client_limits)
                    .await
                    .expect("request is admitted while global capacity remains"),
            );
        }
        assert_eq!(
            engine.snapshot(),
            AdmissionSnapshot {
                work_in_flight: limits.max_work_in_flight(),
                pending: unused_global_capacity,
            }
        );

        let mut idle_client_request =
            Box::pin(engine.acquire(ClientKeyRef::ServiceId("idle-client"), client_limits));
        assert!(poll!(idle_client_request.as_mut()).is_pending());
        assert_eq!(
            engine.snapshot(),
            AdmissionSnapshot {
                work_in_flight: limits.max_work_in_flight(),
                pending: unused_global_capacity + 1,
            }
        );

        drop(
            other_active_requests
                .pop()
                .expect("one active request releases a global execution slot"),
        );
        let idle_client_request = idle_client_request
            .await
            .expect("an independent client receives the next global execution slot");
        drop(idle_client_request);
        drop(other_active_requests);
        drop(backlogged_request);
        drop(backlog);
    }
}
