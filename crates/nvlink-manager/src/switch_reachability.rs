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
//! Switch reachability monitor: debounced `SwitchUnreachable` and
//! `NmxtUnreachable` switch health alerts.
//!
//! The switch controller only talks to a switch while it changes state, so a
//! ready switch that drops off the network is not noticed by anything in
//! `nico-api`. The hardware health service can scrape NMX-T, but it is an
//! optional component. This periodic task fills the gap from inside the API:
//! each pass opens a TCP connection to every ready switch's NVOS management
//! address and, on primary switches, to the NMX-T telemetry port that only
//! they host.
//!
//! A single failed check is routine on a loaded site, so each service is
//! debounced separately: its alert is raised after
//! `unreachable_alert_after` consecutive failed checks and cleared on the
//! first check that connects. The stored report is read from the database
//! each pass rather than remembered here, so a restart or an operator removing
//! the report cannot leave the two out of step. Only passes without a stored
//! alert count towards the threshold, and writes are confirmed after they
//! succeed, so a failed write is retried on the next pass.
//!
//! The streak counts live in memory, so they are only meaningful if the same
//! replica sees every pass. One replica therefore owns the monitor: it takes
//! the work lock on its first pass and keeps it across passes, verifying with
//! a fenced no-op transaction at the start of each pass that it still holds
//! the lease. Other replicas find the lock taken and do nothing. When the
//! owner stops or its lease is taken over, the next owner starts its counts
//! from zero, so a takeover restarts the debounce; a stored alert is never
//! affected by that, since its state is read from the database.
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use carbide_utils::periodic_timer::PeriodicTimer;
use carbide_uuid::switch::SwitchId;
use chrono::{DateTime, Utc};
use db::work_lock_manager::{WorkLock, WorkLockManagerHandle};
use health_report::{HealthProbeAlert, HealthProbeId, HealthReport, HealthReportApplyMode};
use sqlx::PgPool;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::config::SwitchReachabilityConfig;
use crate::errors::{NvLinkManagerError, NvLinkManagerResult};

/// Source of the merge report the monitor stores on a switch.
const SWITCH_REACHABILITY_SOURCE: &str = "switch-reachability-monitor";

/// A switch service the monitor probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SwitchService {
    /// The NVOS management address, probed on every switch.
    Nvos,
    /// The NMX-T telemetry listener, probed on primary switches.
    NmxT,
}

impl SwitchService {
    const ALL: [Self; 2] = [Self::Nvos, Self::NmxT];

    fn probe_id(self) -> HealthProbeId {
        match self {
            Self::Nvos => HealthProbeId::switch_unreachable(),
            Self::NmxT => HealthProbeId::nmxt_unreachable(),
        }
    }

    /// The service behind a stored alert, or `None` for an unknown probe id.
    fn from_probe_id(id: &HealthProbeId) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|service| service.probe_id() == *id)
    }

    fn alert(self, target: String, reason: &str, since: DateTime<Utc>) -> HealthProbeAlert {
        match self {
            Self::Nvos => HealthProbeAlert::switch_unreachable(target, reason, since),
            Self::NmxT => HealthProbeAlert::nmxt_unreachable(target, reason, since),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Nvos => "nvos",
            Self::NmxT => "nmx-t",
        }
    }
}

/// Opens a connection to a switch service. Production uses TCP; tests script
/// outcomes.
#[async_trait]
trait SwitchProber: Send + Sync {
    /// Returns `Err(reason)` when `addr` did not accept a connection within
    /// `timeout`.
    async fn probe(&self, addr: SocketAddr, timeout: Duration) -> Result<(), String>;
}

/// Plain TCP connect: proves the listener accepts connections, nothing more.
struct TcpProber;

#[async_trait]
impl SwitchProber for TcpProber {
    async fn probe(&self, addr: SocketAddr, timeout: Duration) -> Result<(), String> {
        match tokio::time::timeout(timeout, TcpStream::connect(addr)).await {
            Ok(Ok(_stream)) => Ok(()),
            Ok(Err(error)) => Err(format!("connect failed: {error}")),
            Err(_) => Err(format!("connect timed out after {timeout:?}")),
        }
    }
}

/// What a pass decided for one switch service.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AlertAction {
    /// Leave the stored alert, if any, as it is.
    Keep,
    /// Write the alert: the service failed `threshold` checks in a row without
    /// a stored alert. Carries the last failure reason and when the run began.
    Set {
        reason: String,
        since: DateTime<Utc>,
    },
    /// Remove the stored alert: the service accepted a connection.
    Clear,
}

/// A run of consecutive failed checks with no stored alert.
#[derive(Debug, Clone, Copy)]
struct Streak {
    passes: u32,
    since: DateTime<Utc>,
}

type StreakKey = (SwitchId, SwitchService);

/// A pass's decision for one service of a switch, with the probed address.
type ServiceAction = (SwitchService, AlertAction, String);

/// Consecutive failed checks per switch service, held between passes.
#[derive(Debug, Default)]
struct UnreachableTracker {
    streaks: HashMap<StreakKey, Streak>,
    /// Switches whose report write failed, retried on the next pass.
    pending_writes: HashSet<SwitchId>,
}

impl UnreachableTracker {
    /// Records this pass's outcome for one switch service and decides the
    /// action. `failure` is the connection error, or `None` when the service
    /// accepted a connection. `alert_stored` is whether the switch currently
    /// carries this service's alert. Only passes without a stored alert count
    /// towards `threshold`, so an alert removed elsewhere during an ongoing
    /// outage is written again only after `threshold` further failures. The
    /// streak is kept until [`Self::confirm`] reports the write succeeded.
    fn observe(
        &mut self,
        key: &StreakKey,
        failure: Option<&str>,
        threshold: u32,
        alert_stored: bool,
    ) -> AlertAction {
        match failure {
            None => {
                self.streaks.remove(key);
                if alert_stored {
                    AlertAction::Clear
                } else {
                    AlertAction::Keep
                }
            }
            Some(_) if alert_stored => {
                self.streaks.remove(key);
                AlertAction::Keep
            }
            Some(reason) => {
                let streak = self.streaks.entry(*key).or_insert_with(|| Streak {
                    passes: 0,
                    since: Utc::now(),
                });
                streak.passes = streak.passes.saturating_add(1);
                if streak.passes >= threshold {
                    AlertAction::Set {
                        reason: reason.to_string(),
                        since: streak.since,
                    }
                } else {
                    AlertAction::Keep
                }
            }
        }
    }

    /// Records whether the switch's report write succeeded. Confirmed `Set`s
    /// end their streaks so the next alert needs `threshold` passes again; a
    /// failed write is remembered so the switch is written again next pass.
    fn confirm(
        &mut self,
        switch_id: &SwitchId,
        actions: &[(SwitchService, AlertAction)],
        written: bool,
    ) {
        if written {
            self.pending_writes.remove(switch_id);
            for (service, action) in actions {
                if matches!(action, AlertAction::Set { .. }) {
                    self.streaks.remove(&(*switch_id, *service));
                }
            }
        } else {
            self.pending_writes.insert(*switch_id);
        }
    }

    /// Forgets everything: this replica no longer owns the monitor, so its
    /// counts no longer describe consecutive passes.
    fn reset(&mut self) {
        self.streaks.clear();
        self.pending_writes.clear();
    }

    /// Drops streaks of switch services no longer probed, such as NMX-T on a
    /// switch that stopped being primary, and pending writes of switches no
    /// longer probed at all.
    fn retain(&mut self, keep: impl Fn(&StreakKey) -> bool) {
        self.streaks.retain(|key, _| keep(key));
        self.pending_writes
            .retain(|switch_id| SwitchService::ALL.iter().any(|s| keep(&(*switch_id, *s))));
    }
}

/// Builds the alerts a switch's report should hold after a pass: stored
/// alerts stay unless cleared, and `Set` replaces the alert for its service.
/// Returns `None` when nothing changed, so no write is needed.
fn desired_alerts(
    stored: &[HealthProbeAlert],
    actions: &[ServiceAction],
) -> Option<Vec<HealthProbeAlert>> {
    let mut alerts = stored.to_vec();
    let mut changed = false;
    for (service, action, target) in actions {
        let id = service.probe_id();
        match action {
            AlertAction::Keep => {}
            AlertAction::Set { reason, since } => {
                alerts.retain(|alert| alert.id != id);
                alerts.push(service.alert(target.clone(), reason, *since));
                changed = true;
            }
            AlertAction::Clear => {
                let before = alerts.len();
                alerts.retain(|alert| alert.id != id);
                changed |= alerts.len() != before;
            }
        }
    }
    changed.then_some(alerts)
}

/// Counters for one pass, recorded on the iteration span.
#[derive(Debug, Default)]
struct PassMetrics {
    switches: usize,
    switches_without_nvos_ip: usize,
    probes: usize,
    probe_failures: usize,
    alerts_set: usize,
    alerts_cleared: usize,
    write_failures: usize,
    duration: Duration,
}

impl fmt::Display for PassMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{ switches: {}, switches_without_nvos_ip: {}, probes: {}, probe_failures: {}, \
             alerts_set: {}, alerts_cleared: {}, write_failures: {}, duration_ms: {} }}",
            self.switches,
            self.switches_without_nvos_ip,
            self.probes,
            self.probe_failures,
            self.alerts_set,
            self.alerts_cleared,
            self.write_failures,
            self.duration.as_millis()
        )
    }
}

/// One switch's probe results for a pass.
struct SwitchOutcome {
    switch_id: SwitchId,
    /// Per service: the probed address and the failure, if any.
    results: Vec<(SwitchService, SocketAddr, Option<String>)>,
}

/// Periodic task that probes ready switches and maintains their reachability
/// alerts. Runs in every `nico-api` replica, but only the replica holding the
/// work lock does any work, and it keeps the lock across passes so its
/// in-memory streaks see every pass.
pub(super) struct SwitchReachabilityMonitor {
    db_pool: PgPool,
    config: SwitchReachabilityConfig,
    work_lock_manager_handle: WorkLockManagerHandle,
    prober: Arc<dyn SwitchProber>,
    tracker: Mutex<UnreachableTracker>,
    /// The work lock while this replica owns the monitor. Released on drop,
    /// so a stopped replica hands over within the lease timeout.
    lock: tokio::sync::Mutex<Option<WorkLock>>,
}

impl SwitchReachabilityMonitor {
    const ITERATION_WORK_KEY: &'static str = "SwitchReachabilityMonitor::run_single_iteration";

    pub(super) fn new(
        db_pool: PgPool,
        config: SwitchReachabilityConfig,
        work_lock_manager_handle: WorkLockManagerHandle,
    ) -> Self {
        Self::with_prober(
            db_pool,
            config,
            work_lock_manager_handle,
            Arc::new(TcpProber),
        )
    }

    /// Builds a monitor with a custom prober, for tests that script outcomes.
    fn with_prober(
        db_pool: PgPool,
        config: SwitchReachabilityConfig,
        work_lock_manager_handle: WorkLockManagerHandle,
        prober: Arc<dyn SwitchProber>,
    ) -> Self {
        Self {
            db_pool,
            config,
            work_lock_manager_handle,
            prober,
            tracker: Mutex::new(UnreachableTracker::default()),
            lock: tokio::sync::Mutex::new(None),
        }
    }

    /// Spawns the monitor. The caller decides whether it runs; see
    /// [`crate::config::NvLinkConfig::switch_reachability_enabled`].
    pub(super) fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> std::io::Result<()> {
        join_set
            .build_task()
            .name("switch-reachability-monitor")
            .spawn(async move { self.run(cancel_token).await })?;
        Ok(())
    }

    async fn run(&self, cancel_token: CancellationToken) {
        let timer = PeriodicTimer::new(self.config.run_interval);
        loop {
            let tick = timer.tick();
            // A pass can take up to `probe_timeout` per switch service, so
            // shutdown interrupts an in-flight pass instead of waiting for it.
            // Dropping the pass abandons its probes and any uncommitted write;
            // the held work lock is released with the monitor.
            tokio::select! {
                result = self.run_single_iteration() => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "SwitchReachabilityMonitor iteration failed");
                    }
                }
                _ = cancel_token.cancelled() => {
                    tracing::info!("SwitchReachabilityMonitor stop was requested");
                    return;
                }
            }
            tokio::select! {
                _ = tick.sleep() => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("SwitchReachabilityMonitor stop was requested");
                    return;
                }
            }
        }
    }

    /// Runs one pass. Returns the number of switches whose report was written.
    async fn run_single_iteration(&self) -> NvLinkManagerResult<usize> {
        let pass_id: String = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));
        let span = tracing::span!(
            parent: None,
            tracing::Level::INFO,
            "switch_reachability_monitor",
            pass_id,
            otel.status_code = tracing::field::Empty,
            otel.status_message = tracing::field::Empty,
            metrics = tracing::field::Empty,
        );
        let mut metrics = PassMetrics::default();
        let started = Instant::now();
        let result = self
            .run_single_iteration_inner(&mut metrics)
            .instrument(span.clone())
            .await;
        metrics.duration = started.elapsed();
        span.record(
            "otel.status_code",
            if result.is_ok() { "ok" } else { "error" },
        );
        if let Err(error) = &result {
            span.record("otel.status_message", format!("{error:?}"));
        }
        span.record("metrics", metrics.to_string());
        result
    }

    async fn run_single_iteration_inner(
        &self,
        metrics: &mut PassMetrics,
    ) -> NvLinkManagerResult<usize> {
        let mut owned = self.lock.lock().await;
        if owned.is_none() {
            match self
                .work_lock_manager_handle
                .try_acquire_lock(Self::ITERATION_WORK_KEY.into())
                .await
            {
                Ok(lock) => {
                    tracing::info!("SwitchReachabilityMonitor: this replica now owns the monitor");
                    *owned = Some(lock);
                }
                Err(error) => {
                    tracing::debug!(
                        %error,
                        "SwitchReachabilityMonitor: another replica owns the monitor"
                    );
                    return Ok(0);
                }
            }
        }
        let lock = owned.as_ref().expect("lock was just acquired or kept");
        if !self.still_owns(lock).await? {
            tracing::warn!(
                "SwitchReachabilityMonitor lease was taken over; dropping local streaks"
            );
            self.release(&mut owned);
            return Ok(0);
        }
        match self.run_owned_pass(lock, metrics).await {
            Ok(written) => Ok(written),
            Err(PassFailure::LockLost) => {
                self.release(&mut owned);
                Err(NvLinkManagerError::internal(
                    "work lock was lost during the pass; remaining writes abandoned".to_string(),
                ))
            }
            Err(PassFailure::Other(error)) => Err(error),
        }
    }

    /// Verifies the held lock still names this replica. A takeover changes the
    /// lock's worker, which the fence detects as a failed precondition.
    async fn still_owns(&self, lock: &WorkLock) -> NvLinkManagerResult<bool> {
        let mut txn = self.db_pool.begin().await.map_err(|e| {
            NvLinkManagerError::internal(format!("Failed to begin transaction: {e}"))
        })?;
        let owns = match lock.fence_transaction(&mut txn).await {
            Ok(()) => true,
            Err(db::DatabaseError::FailedPrecondition(_)) => false,
            Err(error) => return Err(error.into()),
        };
        txn.rollback().await.ok();
        Ok(owns)
    }

    /// Gives up ownership: drops the lock, which releases it, and forgets the
    /// streaks, which only described this replica's consecutive passes.
    fn release(&self, owned: &mut Option<WorkLock>) {
        *owned = None;
        self.tracker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reset();
    }

    async fn run_owned_pass(
        &self,
        lock: &WorkLock,
        metrics: &mut PassMetrics,
    ) -> Result<usize, PassFailure> {
        let (targets, stored) = {
            let mut conn = self.acquire().await?;
            let targets = db::switch::find_reachability_targets(conn.as_mut()).await?;
            let stored = db::switch::find_health_reports_by_merge_source(
                conn.as_mut(),
                SWITCH_REACHABILITY_SOURCE,
            )
            .await?;
            (targets, stored)
        };
        metrics.switches = targets.len();

        let endpoints = {
            let switch_ids: Vec<SwitchId> = targets.iter().map(|t| t.switch_id).collect();
            let mut conn = self.acquire().await?;
            db::switch::find_switch_endpoints_by_ids(conn.as_mut(), &switch_ids).await?
        };
        let nvos_ips: HashMap<SwitchId, std::net::IpAddr> = endpoints
            .into_iter()
            .filter_map(|row| row.nvos_ip.map(|ip| (row.switch_id, ip)))
            .collect();

        // Probe every service of every switch concurrently; each probe is
        // bounded by `probe_timeout`.
        // Admission is bounded before spawning, so a fleet-sized pass holds at
        // most `max_concurrent_switches` connection attempts and tasks at once.
        let admission = Arc::new(Semaphore::new(self.config.max_concurrent_switches.get()));
        let mut probes = JoinSet::new();
        let mut probed: HashSet<SwitchId> = HashSet::new();
        let mut probed_services: HashSet<StreakKey> = HashSet::new();
        for target in &targets {
            let Some(ip) = nvos_ips.get(&target.switch_id) else {
                metrics.switches_without_nvos_ip += 1;
                tracing::debug!(
                    switch_id = %target.switch_id,
                    rack_id = target.rack_id.as_ref().map(tracing::field::display),
                    "Switch has no NVOS IP yet; reachability not checked"
                );
                continue;
            };
            probed.insert(target.switch_id);
            let mut services = vec![(SwitchService::Nvos, self.config.nvos_port)];
            if target.is_primary {
                services.push((SwitchService::NmxT, self.config.nmx_t_port));
            }
            probed_services.extend(services.iter().map(|(s, _)| (target.switch_id, *s)));
            let switch_id = target.switch_id;
            let ip = *ip;
            let prober = Arc::clone(&self.prober);
            let timeout = self.config.probe_timeout;
            let permit = Arc::clone(&admission)
                .acquire_owned()
                .await
                .map_err(|_| NvLinkManagerError::internal("probe semaphore closed".into()))?;
            probes.spawn(async move {
                let _permit = permit;
                let mut results = Vec::with_capacity(services.len());
                for (service, port) in services {
                    let addr = SocketAddr::new(ip, port);
                    let failure = prober.probe(addr, timeout).await.err();
                    results.push((service, addr, failure));
                }
                SwitchOutcome { switch_id, results }
            });
        }

        let mut outcomes = Vec::with_capacity(probed.len());
        while let Some(joined) = probes.join_next().await {
            match joined {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => tracing::warn!(%error, "Switch reachability probe task failed"),
            }
        }
        outcomes.sort_by_key(|outcome| outcome.switch_id);

        let stored: HashMap<SwitchId, HealthReport> = stored.into_iter().collect();
        let threshold = self.config.unreachable_alert_after.get();

        // Decide every switch's actions under the tracker lock, then write
        // without holding it.
        let mut plans: Vec<(SwitchId, Vec<ServiceAction>)> = Vec::new();
        {
            let mut tracker = self.tracker.lock().unwrap_or_else(PoisonError::into_inner);
            tracker.retain(|key| probed_services.contains(key));
            for outcome in &outcomes {
                let stored_alerts = stored
                    .get(&outcome.switch_id)
                    .map(|report| report.alerts.as_slice())
                    .unwrap_or_default();
                let mut actions = Vec::with_capacity(outcome.results.len());
                for (service, addr, failure) in &outcome.results {
                    metrics.probes += 1;
                    if failure.is_some() {
                        metrics.probe_failures += 1;
                    }
                    let alert_stored = stored_alerts
                        .iter()
                        .any(|alert| alert.id == service.probe_id());
                    let action = tracker.observe(
                        &(outcome.switch_id, *service),
                        failure.as_deref(),
                        threshold,
                        alert_stored,
                    );
                    actions.push((*service, action, addr.to_string()));
                }
                // A stored alert for a service this switch is no longer eligible
                // for, such as NMX-T after it stopped being primary, is cleared.
                for alert in stored_alerts {
                    let Some(service) = SwitchService::from_probe_id(&alert.id) else {
                        continue;
                    };
                    if !probed_services.contains(&(outcome.switch_id, service)) {
                        actions.push((
                            service,
                            AlertAction::Clear,
                            alert.target.clone().unwrap_or_default(),
                        ));
                    }
                }
                plans.push((outcome.switch_id, actions));
            }
        }

        let mut written = 0;
        for (switch_id, actions) in &plans {
            let stored_report = stored.get(switch_id);
            let stored_alerts = stored_report
                .map(|report| report.alerts.as_slice())
                .unwrap_or_default();
            let Some(alerts) = desired_alerts(stored_alerts, actions) else {
                continue;
            };
            for (service, action, target) in actions {
                match action {
                    AlertAction::Set { reason, since } => {
                        metrics.alerts_set += 1;
                        tracing::warn!(
                            switch_id = %switch_id,
                            service = service.label(),
                            target,
                            since = %since,
                            reason,
                            "Switch service unreachable; raising alert"
                        );
                    }
                    AlertAction::Clear => {
                        metrics.alerts_cleared += 1;
                        tracing::info!(
                            switch_id = %switch_id,
                            service = service.label(),
                            target,
                            "Switch service reachable again; clearing alert"
                        );
                    }
                    AlertAction::Keep => {}
                }
            }
            let outcome = self
                .write_report(lock, switch_id, alerts, stored_report.is_some())
                .await;
            let ok = outcome.is_ok();
            if ok {
                written += 1;
            } else {
                metrics.write_failures += 1;
            }
            let confirmed: Vec<(SwitchService, AlertAction)> = actions
                .iter()
                .map(|(service, action, _)| (*service, action.clone()))
                .collect();
            self.tracker
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .confirm(switch_id, &confirmed, ok);
            if let Err(WriteFailure::LockLost) = outcome {
                return Err(PassFailure::LockLost);
            }
        }

        // A switch that left the probed set (deleted, or no longer ready) keeps
        // no reachability report; its state is owned by its controller now.
        for switch_id in stored.keys() {
            if probed.contains(switch_id) {
                continue;
            }
            tracing::info!(
                switch_id = %switch_id,
                "Switch is no longer probed; removing its reachability report"
            );
            match self.write_report(lock, switch_id, Vec::new(), true).await {
                Ok(()) => written += 1,
                Err(WriteFailure::LockLost) => {
                    metrics.write_failures += 1;
                    return Err(PassFailure::LockLost);
                }
                Err(WriteFailure::Other) => metrics.write_failures += 1,
            }
        }

        Ok(written)
    }

    async fn acquire(&self) -> NvLinkManagerResult<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        self.db_pool
            .acquire()
            .await
            .map_err(|e| NvLinkManagerError::internal(format!("Failed to acquire connection: {e}")))
    }

    /// Stores `alerts` as the switch's merge report, or removes the report when
    /// `alerts` is empty. Each write is one short transaction fenced on the
    /// pass's work lock, so a pass whose lease another replica took over
    /// cannot overwrite that replica's newer report with stale results.
    async fn write_report(
        &self,
        lock: &WorkLock,
        switch_id: &SwitchId,
        alerts: Vec<HealthProbeAlert>,
        stored: bool,
    ) -> Result<(), WriteFailure> {
        let result = async {
            let mut txn = self.db_pool.begin().await.map_err(|e| {
                NvLinkManagerError::internal(format!("Failed to begin transaction: {e}"))
            })?;
            lock.fence_transaction(&mut txn).await?;
            if alerts.is_empty() {
                if stored {
                    db::switch::remove_health_report(
                        txn.as_mut(),
                        switch_id,
                        HealthReportApplyMode::Merge,
                        SWITCH_REACHABILITY_SOURCE,
                    )
                    .await?;
                }
            } else {
                let report = HealthReport {
                    source: SWITCH_REACHABILITY_SOURCE.to_string(),
                    triggered_by: None,
                    observed_at: Some(Utc::now()),
                    successes: vec![],
                    alerts,
                };
                db::switch::insert_health_report(
                    txn.as_mut(),
                    switch_id,
                    HealthReportApplyMode::Merge,
                    &report,
                )
                .await?;
            }
            txn.commit().await.map_err(|e| {
                NvLinkManagerError::internal(format!("Failed to commit transaction: {e}"))
            })?;
            Ok::<(), NvLinkManagerError>(())
        }
        .await;
        match result {
            Ok(()) => Ok(()),
            Err(NvLinkManagerError::DatabaseError(db::DatabaseError::FailedPrecondition(
                message,
            ))) => {
                tracing::error!(
                    switch_id = %switch_id,
                    message,
                    "Switch reachability report not written: work lock lost"
                );
                Err(WriteFailure::LockLost)
            }
            Err(error) => {
                tracing::warn!(
                    switch_id = %switch_id,
                    %error,
                    "Failed to write switch reachability report; will retry next pass"
                );
                Err(WriteFailure::Other)
            }
        }
    }
}

/// Why an owned pass stopped early.
enum PassFailure {
    /// The lease was taken over by another replica mid-pass, so the remaining
    /// results are stale and the pass is abandoned.
    LockLost,
    Other(NvLinkManagerError),
}

impl From<NvLinkManagerError> for PassFailure {
    fn from(error: NvLinkManagerError) -> Self {
        Self::Other(error)
    }
}

impl From<db::DatabaseError> for PassFailure {
    fn from(error: db::DatabaseError) -> Self {
        Self::Other(error.into())
    }
}

/// Why a report write did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteFailure {
    /// The fence found the pass's work lock held by another worker.
    LockLost,
    /// Any other database error; the write is retried next pass.
    Other,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    fn switch() -> SwitchId {
        SwitchId::from(uuid::Uuid::from_u128(
            0x5eac_4ab1_1e00_0000_0000_0000_0000_0001,
        ))
    }

    /// One pass as the tracker sees it: the failure if any, and whether the
    /// alert is stored before the pass.
    type Pass = (Option<&'static str>, bool);

    /// Actions without their timestamps and reasons, which vary per run.
    #[derive(Debug, PartialEq, Eq)]
    enum Action {
        Keep,
        Set,
        Clear,
    }

    /// Runs `passes` for one switch service against `threshold`, confirming
    /// every write as successful, and returns each action.
    fn actions(threshold: u32, passes: &[Pass]) -> Vec<Action> {
        let mut tracker = UnreachableTracker::default();
        let key = (switch(), SwitchService::Nvos);
        passes
            .iter()
            .map(|(failure, stored)| {
                let action = tracker.observe(&key, *failure, threshold, *stored);
                tracker.confirm(&key.0, &[(key.1, action.clone())], true);
                match action {
                    AlertAction::Keep => Action::Keep,
                    AlertAction::Set { .. } => Action::Set,
                    AlertAction::Clear => Action::Clear,
                }
            })
            .collect()
    }

    #[test]
    fn alerts_are_debounced_and_cleared_on_first_success() {
        use Action::*;
        let down = Some("connect failed");
        check_values(
            [
                Check {
                    scenario: "raised only at the threshold, then not rewritten",
                    input: (
                        3,
                        vec![(down, false), (down, false), (down, false), (down, true)],
                    ),
                    expect: vec![Keep, Keep, Set, Keep],
                },
                Check {
                    scenario: "a success resets the streak",
                    input: (
                        2,
                        vec![(down, false), (None, false), (down, false), (down, false)],
                    ),
                    expect: vec![Keep, Keep, Keep, Set],
                },
                Check {
                    scenario: "a stored alert is cleared on the first success",
                    input: (3, vec![(down, true), (None, true), (None, false)]),
                    expect: vec![Keep, Clear, Keep],
                },
                Check {
                    scenario: "an alert removed elsewhere needs the full threshold again",
                    input: (2, vec![(down, true), (down, false), (down, false)]),
                    expect: vec![Keep, Keep, Set],
                },
                Check {
                    scenario: "a confirmed set starts the count over",
                    input: (1, vec![(down, false), (down, false)]),
                    expect: vec![Set, Set],
                },
            ],
            |(threshold, passes)| actions(threshold, &passes),
        );
    }

    #[test]
    fn set_reports_when_the_run_of_failures_began_and_failed_writes_keep_the_streak() {
        let mut tracker = UnreachableTracker::default();
        let key = (switch(), SwitchService::NmxT);
        let before = Utc::now();
        assert_eq!(
            tracker.observe(&key, Some("down"), 2, false),
            AlertAction::Keep
        );
        let AlertAction::Set { since, reason } =
            tracker.observe(&key, Some("still down"), 2, false)
        else {
            panic!("second failure should set");
        };
        assert!(since >= before && since <= Utc::now());
        assert_eq!(reason, "still down");

        // The write failed: the streak survives, so the next failing pass sets
        // again without waiting for the threshold anew.
        tracker.confirm(
            &key.0,
            &[(key.1, AlertAction::Set { since, reason })],
            false,
        );
        assert!(tracker.pending_writes.contains(&key.0));
        assert!(matches!(
            tracker.observe(&key, Some("down"), 2, false),
            AlertAction::Set { .. }
        ));
    }

    #[test]
    fn desired_alerts_merges_services_and_reports_no_change() {
        let since = Utc::now();
        let nvos = HealthProbeAlert::switch_unreachable("10.0.0.1:443".into(), "refused", since);
        let nmxt = HealthProbeAlert::nmxt_unreachable("10.0.0.1:9352".into(), "refused", since);
        let set = |reason: &str| AlertAction::Set {
            reason: reason.to_string(),
            since,
        };

        // Setting NMX-T keeps the stored NVOS alert.
        let alerts = desired_alerts(
            std::slice::from_ref(&nvos),
            &[
                (
                    SwitchService::Nvos,
                    AlertAction::Keep,
                    "10.0.0.1:443".into(),
                ),
                (
                    SwitchService::NmxT,
                    set("timed out"),
                    "10.0.0.1:9352".into(),
                ),
            ],
        )
        .expect("set changes the report");
        assert_eq!(
            alerts.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["SwitchUnreachable", "NmxtUnreachable"]
        );
        assert!(alerts[1].message.contains("timed out"));

        // Clearing one of two leaves the other; clearing the last empties it.
        let both = vec![nvos.clone(), nmxt];
        let alerts = desired_alerts(
            &both,
            &[(
                SwitchService::NmxT,
                AlertAction::Clear,
                "10.0.0.1:9352".into(),
            )],
        )
        .expect("clear changes the report");
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].id, HealthProbeId::switch_unreachable());
        assert_eq!(
            desired_alerts(
                std::slice::from_ref(&nvos),
                &[(
                    SwitchService::Nvos,
                    AlertAction::Clear,
                    "10.0.0.1:443".into()
                )]
            ),
            Some(Vec::new())
        );

        // Keep everywhere, or clearing an absent alert, needs no write.
        assert_eq!(
            desired_alerts(
                &both,
                &[
                    (SwitchService::Nvos, AlertAction::Keep, String::new()),
                    (SwitchService::NmxT, AlertAction::Keep, String::new()),
                ]
            ),
            None
        );
        assert_eq!(
            desired_alerts(
                &[],
                &[(SwitchService::Nvos, AlertAction::Clear, String::new())]
            ),
            None
        );
    }

    /// Prober whose outcome per service is scripted by the test.
    #[derive(Default)]
    struct ScriptedProber {
        /// Failure reason per port; ports absent here are reachable.
        failing_ports: Mutex<HashMap<u16, String>>,
    }

    impl ScriptedProber {
        fn set(&self, port: u16, failure: Option<&str>) {
            let mut failing = self.failing_ports.lock().unwrap();
            match failure {
                Some(reason) => {
                    failing.insert(port, reason.to_string());
                }
                None => {
                    failing.remove(&port);
                }
            }
        }
    }

    #[async_trait]
    impl SwitchProber for ScriptedProber {
        async fn probe(&self, addr: SocketAddr, _timeout: Duration) -> Result<(), String> {
            match self.failing_ports.lock().unwrap().get(&addr.port()) {
                Some(reason) => Err(reason.clone()),
                None => Ok(()),
            }
        }
    }

    /// Alert ids and targets stored on the switch under this monitor's source.
    async fn stored_alerts(
        pool: &PgPool,
        switch_id: &SwitchId,
    ) -> Result<Option<Vec<(String, Option<String>)>>, Box<dyn std::error::Error>> {
        let mut conn = pool.acquire().await?;
        let reports = db::switch::find_health_reports_by_merge_source(
            conn.as_mut(),
            SWITCH_REACHABILITY_SOURCE,
        )
        .await?;
        Ok(reports
            .into_iter()
            .find(|(id, _)| id == switch_id)
            .map(|(_, report)| {
                report
                    .alerts
                    .iter()
                    .map(|alert| (alert.id.to_string(), alert.target.clone()))
                    .collect()
            }))
    }

    async fn set_controller_state(
        pool: &PgPool,
        switch_id: &SwitchId,
        state: &model::switch::SwitchControllerState,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let fresh = db::switch::find_by_id(txn.as_mut(), switch_id)
            .await?
            .expect("switch exists");
        assert_eq!(
            db::switch::try_update_controller_state(
                txn.as_mut(),
                *switch_id,
                fresh.controller_state.version,
                fresh.controller_state.version.increment(),
                state,
            )
            .await?,
            db::ConditionalWrite::Applied(())
        );
        txn.commit().await?;
        Ok(())
    }

    #[carbide_macros::sqlx_test]
    async fn passes_raise_and_clear_switch_alerts_through_the_database(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::switch::SwitchControllerState;

        // A ready primary switch with a seeded NVOS IP.
        let mut txn = pool.begin().await?;
        let switch =
            db::test_support::switch::create_seeded_discovered(txn.as_mut(), 1, "Switch1").await?;
        txn.commit().await?;
        set_controller_state(&pool, &switch.id, &SwitchControllerState::Ready).await?;
        sqlx::query("UPDATE switches SET is_primary = true WHERE id = $1")
            .bind(switch.id)
            .execute(&pool)
            .await?;
        let nvos_ip = {
            let mut conn = pool.acquire().await?;
            db::switch::find_switch_endpoints_by_ids(conn.as_mut(), &[switch.id])
                .await?
                .pop()
                .and_then(|row| row.nvos_ip)
                .expect("seeded switch has an NVOS IP")
        };

        let prober = Arc::new(ScriptedProber::default());
        let config = SwitchReachabilityConfig {
            unreachable_alert_after: std::num::NonZeroU32::new(2).expect("non-zero"),
            ..SwitchReachabilityConfig::default()
        };
        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let monitor = SwitchReachabilityMonitor::with_prober(
            pool.clone(),
            config.clone(),
            work_lock_manager,
            prober.clone(),
        );
        let nmxt_target = Some(SocketAddr::new(nvos_ip, config.nmx_t_port).to_string());
        let nvos_target = Some(SocketAddr::new(nvos_ip, config.nvos_port).to_string());

        // Both services reachable: nothing stored.
        assert_eq!(monitor.run_single_iteration().await?, 0);
        assert_eq!(stored_alerts(&pool, &switch.id).await?, None);

        // NMX-T down: the first failure is debounced, the second raises only
        // the NMX-T alert.
        prober.set(config.nmx_t_port, Some("connect failed: refused"));
        assert_eq!(monitor.run_single_iteration().await?, 0);
        assert_eq!(stored_alerts(&pool, &switch.id).await?, None);
        assert_eq!(monitor.run_single_iteration().await?, 1);
        assert_eq!(
            stored_alerts(&pool, &switch.id).await?,
            Some(vec![("NmxtUnreachable".to_string(), nmxt_target.clone())])
        );

        // Still down: the stored alert is not rewritten.
        assert_eq!(monitor.run_single_iteration().await?, 0);

        // NVOS goes down too: after its threshold both alerts are stored.
        prober.set(config.nvos_port, Some("connect timed out"));
        assert_eq!(monitor.run_single_iteration().await?, 0);
        assert_eq!(monitor.run_single_iteration().await?, 1);
        assert_eq!(
            stored_alerts(&pool, &switch.id).await?,
            Some(vec![
                ("NmxtUnreachable".to_string(), nmxt_target.clone()),
                ("SwitchUnreachable".to_string(), nvos_target),
            ])
        );

        // NVOS recovers first: its alert clears on the first success while
        // NMX-T's stays.
        prober.set(config.nvos_port, None);
        assert_eq!(monitor.run_single_iteration().await?, 1);
        assert_eq!(
            stored_alerts(&pool, &switch.id).await?,
            Some(vec![("NmxtUnreachable".to_string(), nmxt_target)])
        );

        // NMX-T recovers: the report is removed.
        prober.set(config.nmx_t_port, None);
        assert_eq!(monitor.run_single_iteration().await?, 1);
        assert_eq!(stored_alerts(&pool, &switch.id).await?, None);

        // NMX-T down on a primary, which is then demoted: the NMX-T alert is
        // cleared although NVOS is still probed, and its streak is dropped, so
        // being promoted again starts the count over.
        prober.set(config.nmx_t_port, Some("connect failed: refused"));
        monitor.run_single_iteration().await?;
        assert_eq!(monitor.run_single_iteration().await?, 1);
        sqlx::query("UPDATE switches SET is_primary = false WHERE id = $1")
            .bind(switch.id)
            .execute(&pool)
            .await?;
        assert_eq!(monitor.run_single_iteration().await?, 1);
        assert_eq!(stored_alerts(&pool, &switch.id).await?, None);
        sqlx::query("UPDATE switches SET is_primary = true WHERE id = $1")
            .bind(switch.id)
            .execute(&pool)
            .await?;
        assert_eq!(monitor.run_single_iteration().await?, 0);
        assert_eq!(stored_alerts(&pool, &switch.id).await?, None);

        // A stored report on a switch that is no longer ready is removed.
        assert_eq!(monitor.run_single_iteration().await?, 1);
        set_controller_state(&pool, &switch.id, &SwitchControllerState::FetchInfo).await?;
        assert_eq!(monitor.run_single_iteration().await?, 1);
        assert_eq!(stored_alerts(&pool, &switch.id).await?, None);

        join_set.shutdown().await;
        Ok(())
    }

    /// Prober that records how many probes were in flight at once.
    #[derive(Default)]
    struct ConcurrencyProber {
        in_flight: std::sync::atomic::AtomicUsize,
        max_in_flight: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl SwitchProber for ConcurrencyProber {
        async fn probe(&self, _addr: SocketAddr, _timeout: Duration) -> Result<(), String> {
            use std::sync::atomic::Ordering;
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[carbide_macros::sqlx_test]
    async fn probes_are_bounded_by_max_concurrent_switches(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::switch::SwitchControllerState;

        for seed in 1..=4u8 {
            let mut txn = pool.begin().await?;
            let switch = db::test_support::switch::create_seeded_discovered(
                txn.as_mut(),
                seed,
                &format!("Switch{seed}"),
            )
            .await?;
            txn.commit().await?;
            set_controller_state(&pool, &switch.id, &SwitchControllerState::Ready).await?;
        }

        let prober = Arc::new(ConcurrencyProber::default());
        let config = SwitchReachabilityConfig {
            max_concurrent_switches: std::num::NonZeroUsize::MIN,
            ..SwitchReachabilityConfig::default()
        };
        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let monitor = SwitchReachabilityMonitor::with_prober(
            pool.clone(),
            config,
            work_lock_manager,
            prober.clone(),
        );

        monitor.run_single_iteration().await?;

        assert_eq!(
            prober
                .max_in_flight
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "four reachable switches were probed one at a time"
        );
        join_set.shutdown().await;
        Ok(())
    }

    #[carbide_macros::sqlx_test]
    async fn one_replica_owns_the_monitor_across_passes(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use db::work_lock_manager::KeepaliveConfig;
        use model::switch::SwitchControllerState;

        let mut txn = pool.begin().await?;
        let switch =
            db::test_support::switch::create_seeded_discovered(txn.as_mut(), 1, "Switch1").await?;
        txn.commit().await?;
        set_controller_state(&pool, &switch.id, &SwitchControllerState::Ready).await?;

        let config = SwitchReachabilityConfig {
            unreachable_alert_after: std::num::NonZeroU32::new(3).expect("non-zero"),
            ..SwitchReachabilityConfig::default()
        };
        let prober = Arc::new(ScriptedProber::default());
        prober.set(config.nvos_port, Some("connect failed: refused"));

        // Replica A takes the lock on its first pass and keeps it, so its
        // streak counts every pass.
        let mut a_manager = JoinSet::new();
        let a_handle =
            db::work_lock_manager::start(&mut a_manager, pool.clone(), Default::default()).await?;
        let a = SwitchReachabilityMonitor::with_prober(
            pool.clone(),
            config.clone(),
            a_handle,
            prober.clone(),
        );
        // Replica B treats a lease without keepalives as expired quickly.
        let mut b_manager = JoinSet::new();
        let b_handle = db::work_lock_manager::start(
            &mut b_manager,
            pool.clone(),
            KeepaliveConfig {
                interval: Duration::from_secs(3600),
                timeout: Duration::from_millis(50),
            },
        )
        .await?;
        let b = SwitchReachabilityMonitor::with_prober(
            pool.clone(),
            config.clone(),
            b_handle,
            prober.clone(),
        );

        assert_eq!(a.run_single_iteration().await?, 0);
        assert!(
            a.lock.lock().await.is_some(),
            "A keeps the lock between passes"
        );
        // B's passes do nothing while A owns the monitor, and do not count.
        assert_eq!(b.run_single_iteration().await?, 0);
        assert_eq!(b.run_single_iteration().await?, 0);
        assert!(b.lock.lock().await.is_none());
        assert!(b.tracker.lock().unwrap().streaks.is_empty());
        // A's third consecutive pass raises the alert: B's passes in between
        // did not dilute the count.
        assert_eq!(a.run_single_iteration().await?, 0);
        assert_eq!(a.run_single_iteration().await?, 1);
        assert_eq!(
            stored_alerts(&pool, &switch.id).await?.map(|a| a.len()),
            Some(1)
        );

        // A's manager stops sending keepalives; B takes the lease over and
        // becomes the owner. A notices on its next pass and forgets its state.
        a_manager.shutdown().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        prober.set(config.nvos_port, None);
        assert_eq!(b.run_single_iteration().await?, 1, "B clears the alert");
        assert!(b.lock.lock().await.is_some());
        assert_eq!(a.run_single_iteration().await?, 0);
        assert!(a.lock.lock().await.is_none(), "A gave up the lock");
        assert!(a.tracker.lock().unwrap().streaks.is_empty());
        assert_eq!(stored_alerts(&pool, &switch.id).await?, None);

        b_manager.shutdown().await;
        Ok(())
    }

    #[carbide_macros::sqlx_test]
    async fn writes_are_fenced_on_the_pass_lock(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use db::work_lock_manager::KeepaliveConfig;

        let mut txn = pool.begin().await?;
        let switch =
            db::test_support::switch::create_seeded_discovered(txn.as_mut(), 1, "Switch1").await?;
        txn.commit().await?;

        // The monitor's manager holds the pass lock, then stops sending
        // keepalives, as a replica that stalled would.
        let mut stalled = JoinSet::new();
        let stalled_handle =
            db::work_lock_manager::start(&mut stalled, pool.clone(), Default::default()).await?;
        let monitor = SwitchReachabilityMonitor::with_prober(
            pool.clone(),
            SwitchReachabilityConfig::default(),
            stalled_handle.clone(),
            Arc::new(ScriptedProber::default()),
        );
        let stale_lock = stalled_handle
            .try_acquire_lock(SwitchReachabilityMonitor::ITERATION_WORK_KEY.into())
            .await?;
        let alert = |reason: &str| {
            vec![HealthProbeAlert::switch_unreachable(
                "10.0.0.1:443".into(),
                reason,
                Utc::now(),
            )]
        };

        // While the lease is held, the fenced write succeeds.
        assert_eq!(
            monitor
                .write_report(&stale_lock, &switch.id, alert("first"), false)
                .await,
            Ok(())
        );
        stalled.shutdown().await;

        // Another replica treats the lease as expired and takes the key.
        let mut replica = JoinSet::new();
        let replica_handle = db::work_lock_manager::start(
            &mut replica,
            pool.clone(),
            KeepaliveConfig {
                interval: Duration::from_secs(3600),
                timeout: Duration::from_millis(1),
            },
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _fresh_lock = replica_handle
            .try_acquire_lock(SwitchReachabilityMonitor::ITERATION_WORK_KEY.into())
            .await?;

        // The stalled pass's write is refused and the stored report is intact.
        assert_eq!(
            monitor
                .write_report(&stale_lock, &switch.id, alert("stale"), true)
                .await,
            Err(WriteFailure::LockLost)
        );
        let stored = stored_alerts(&pool, &switch.id)
            .await?
            .expect("report kept");
        assert_eq!(stored.len(), 1);
        let mut conn = pool.acquire().await?;
        let report = db::switch::find_health_reports_by_merge_source(
            conn.as_mut(),
            SWITCH_REACHABILITY_SOURCE,
        )
        .await?
        .pop()
        .expect("report")
        .1;
        assert!(report.alerts[0].message.contains("(first)"));

        drop(stale_lock);
        replica.shutdown().await;
        Ok(())
    }

    /// Prober whose connections never complete, as a blackholed port's would
    /// until `probe_timeout`.
    struct HangingProber;

    #[async_trait]
    impl SwitchProber for HangingProber {
        async fn probe(&self, _addr: SocketAddr, _timeout: Duration) -> Result<(), String> {
            std::future::pending().await
        }
    }

    #[carbide_macros::sqlx_test]
    async fn shutdown_interrupts_an_in_flight_pass(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::switch::SwitchControllerState;

        let mut txn = pool.begin().await?;
        let switch =
            db::test_support::switch::create_seeded_discovered(txn.as_mut(), 1, "Switch1").await?;
        txn.commit().await?;
        set_controller_state(&pool, &switch.id, &SwitchControllerState::Ready).await?;

        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let mut monitor_tasks = JoinSet::new();
        let cancel = CancellationToken::new();
        SwitchReachabilityMonitor::with_prober(
            pool.clone(),
            SwitchReachabilityConfig {
                probe_timeout: Duration::from_secs(3600),
                ..SwitchReachabilityConfig::default()
            },
            work_lock_manager,
            Arc::new(HangingProber),
        )
        .start(&mut monitor_tasks, cancel.clone())?;

        // Let the first pass reach its probe, then stop the monitor; it must
        // return without waiting for the probe.
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), monitor_tasks.join_next())
            .await
            .expect("monitor stopped while its pass was probing")
            .expect("monitor task present")?;

        join_set.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn tcp_prober_distinguishes_listening_from_closed_ports() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let open = listener.local_addr().expect("addr");
        // Bind and drop a second listener so the port is known to be closed.
        let closed = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind")
            .local_addr()
            .expect("addr");

        assert_eq!(TcpProber.probe(open, Duration::from_secs(5)).await, Ok(()));
        let failure = TcpProber
            .probe(closed, Duration::from_secs(5))
            .await
            .expect_err("closed port should fail");
        assert!(failure.starts_with("connect failed"), "{failure}");
    }
}
