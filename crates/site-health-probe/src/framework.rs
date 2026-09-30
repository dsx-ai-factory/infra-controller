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

//! Schedules probes and moves their results through a channel pipeline:
//!
//! ```text
//! probe loops ── one task per probe, per-probe interval/timeout ──▶ mpsc<ProbeResult>
//! collect     ── single consumer ──▶ Sink (metrics today; more consumers later)
//! ```
//!
//! Probes are pure measurement: they record Observations and never touch
//! metrics, so new result consumers (dashboards, aggregators) attach at the
//! [`Sink`] seam without touching any probe.
//!
//! TODO(#5360-followup): site-stats probe family — the same pipeline is meant
//! to carry richer Observations later (host-ingestion progress, instance
//! creation, firmware-update tracking), with consumers aggregating p50/p95/p99
//! from results rather than probes computing anything themselves.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use eyre::eyre;
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinSet};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// Marks a run that ignored cancellation and never returned. The scheduler
/// keeps reporting it every interval so the outage stays visible on the
/// timeout counter without stacking more blocked tasks.
const WEDGED_ERROR: &str = "probe wedged: run did not return after twice its timeout";

/// One timed operation inside a probe run (e.g. a single RPC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Observation {
    pub operation: &'static str,
    pub duration: Duration,
}

/// Classifies a completed probe run. Timeout is separate from failure because
/// at fleet scale a slow-but-alive API and a down API are different incidents.
/// A run cancelled by shutdown has no variant: its future is dropped before a
/// result exists, so nothing reaches the sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Success,
    Failure,
    Timeout,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Timeout => "timeout",
        }
    }
}

/// One probe run's outcome, emitted on the pipeline channel.
#[derive(Debug)]
pub(crate) struct ProbeResult {
    pub probe: &'static str,
    /// The surface probed (e.g. "nico-api", "nico-rest-api") — the aggregation
    /// axis for per-API dashboards.
    pub api: &'static str,
    pub outcome: Outcome,
    pub observations: Vec<Observation>,
    /// The failure detail; kept for logs and tests, the sink only uses outcome.
    pub error: Option<String>,
    /// Marks a probe-implementation bug (recovered, never fatal).
    pub panicked: bool,
    /// Completion time of an actual probe run. `None` on synthetic watchdog
    /// results (a wedged run never completed), so the last-run-timestamp metric
    /// goes stale during a wedge instead of being refreshed by markers that
    /// measured nothing.
    pub at: Option<SystemTime>,
}

/// Collects the observations of one probe run. It is shared with the spawned
/// run so observations recorded before a timeout survive the dropped future —
/// partial observations before a failure are still valid measurements.
#[derive(Clone, Default)]
pub(crate) struct ObservationRecorder {
    observations: Arc<Mutex<Vec<Observation>>>,
}

impl ObservationRecorder {
    /// Records one timed operation.
    pub(crate) fn record(&self, operation: &'static str, duration: Duration) {
        self.observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Observation {
                operation,
                duration,
            });
    }

    /// Removes and returns everything recorded so far. The scheduler harvests
    /// with this after a run ends; probe tests use it to assert partials.
    pub(crate) fn take(&self) -> Vec<Observation> {
        std::mem::take(
            &mut *self
                .observations
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

/// One synthetic check. `run` records its timed observations on the recorder
/// as it makes them; the future may be dropped at the probe's timeout or on
/// shutdown, so nothing after an `.await` is guaranteed to execute.
#[async_trait::async_trait]
pub(crate) trait Probe: Send + Sync {
    fn name(&self) -> &'static str;
    /// The surface this probe exercises ("nico-api", "nico-rest-api").
    fn api(&self) -> &'static str;
    fn interval(&self) -> Duration;
    fn timeout(&self) -> Duration;
    async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()>;
}

/// Consumes results. Delivery is serialized by [`collect`]'s single consumer.
pub(crate) trait Sink: Send + Sync {
    fn record(&self, result: &ProbeResult);
}

/// Starts one task per probe and returns the result channel plus the set of
/// probe-loop tasks. The channel closes after the last loop ends, so a
/// consumer can receive to a clean drain; await [`join_loops`] on the returned
/// set to observe that (and any scheduler bug) after cancelling the token.
pub(crate) fn start(
    probes: Vec<Arc<dyn Probe>>,
    cancel: CancellationToken,
) -> (mpsc::Receiver<ProbeResult>, JoinSet<()>) {
    let (results, receiver) = mpsc::channel(4 * probes.len().max(1) + 1);
    let mut loops = JoinSet::new();
    for probe in probes {
        loops.spawn(probe_loop(probe, results.clone(), cancel.clone()));
    }
    (receiver, loops)
}

/// Waits for every probe loop to end. A panicked loop is a scheduler bug, not
/// a probe failure; it is surfaced as an error so the process exits instead of
/// silently running with a dead probe.
pub(crate) async fn join_loops(mut loops: JoinSet<()>) -> eyre::Result<()> {
    while let Some(joined) = loops.join_next().await {
        if let Err(err) = joined
            && err.is_panic()
        {
            return Err(eyre!("probe loop panicked: {}", panic_message(err)));
        }
    }
    Ok(())
}

/// Consumes results until the channel closes, forwarding each to the sink.
/// Returns after a clean drain. Shutdown-cancelled runs never produce a
/// result, so no filtering is needed here.
pub(crate) async fn collect(mut results: mpsc::Receiver<ProbeResult>, sink: Arc<dyn Sink>) {
    while let Some(result) = results.recv().await {
        sink.record(&result);
    }
}

/// A run task's join output: completed-in-time (with the probe's result) or
/// elapsed at the probe's timeout.
type RunHandle = tokio::task::JoinHandle<Result<eyre::Result<()>, tokio::time::error::Elapsed>>;

/// How one scheduled run ended, from the scheduler's point of view.
enum Disposition {
    /// The run completed (successfully or not) within twice its timeout.
    Completed(ProbeResult),
    /// The run ignored cancellation past twice its timeout; a synthetic
    /// timeout result was produced and the still-outstanding task is kept as
    /// a marker so the loop re-reports instead of stacking blocked runs.
    Wedged(ProbeResult, RunHandle),
    /// Shutdown arrived mid-run; the run was aborted and measured nothing.
    Cancelled,
}

/// Drives one probe: fire immediately (a fresh pod reports within seconds),
/// then on every interval tick until shutdown.
async fn probe_loop(
    probe: Arc<dyn Probe>,
    results: mpsc::Sender<ProbeResult>,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(probe.interval());
    // Like Go's time.Ticker, a tick that arrives while a slow run is still in
    // flight must not queue a burst of make-up runs.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // A run that outlived its watchdog; None when no run is stuck.
    let mut wedged: Option<RunHandle> = None;
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            _ = ticker.tick() => {}
        }
        // A wedged run that finally returned is simply dropped here (its
        // watchdog result was already reported); one still stuck is re-reported
        // instead of spawning another blocked run on top of it.
        if let Some(outstanding) = wedged.take()
            && !outstanding.is_finished()
        {
            wedged = Some(outstanding);
            emit(&results, &cancel, wedged_result(probe.as_ref())).await;
            continue;
        }
        match run_once(&probe, &cancel).await {
            Disposition::Completed(result) => emit(&results, &cancel, result).await,
            Disposition::Wedged(result, outstanding) => {
                wedged = Some(outstanding);
                emit(&results, &cancel, result).await;
            }
            Disposition::Cancelled => return,
        }
    }
}

/// Delivers a result without deadlocking on shutdown: the channel is buffered,
/// and if the consumer is gone the cancellation branch releases the producer.
async fn emit(results: &mpsc::Sender<ProbeResult>, cancel: &CancellationToken, r: ProbeResult) {
    tokio::select! {
        biased;
        _ = results.send(r) => {}
        () = cancel.cancelled() => {}
    }
}

/// Executes a single probe run with timeout and panic isolation. The timeout
/// drops the run's future; a watchdog backstops a run that ignores even that
/// (a blocked poll — e.g. a synchronous read on a wedged kubelet volume): if
/// the task has not ended after twice the probe's timeout, a synthetic timeout
/// result is returned together with the still-outstanding task, so the
/// scheduler stays live and the outage stays visible instead of the probe
/// silently never reporting again. Shutdown mid-run aborts the task and
/// produces nothing — an interrupted run is not a measurement.
///
/// The watchdog is best-effort: a poll blocked inside a runtime worker can
/// also strand tokio's time driver, in which case no timer — this one
/// included — fires until some external event unparks a worker. The hard
/// backstop for that state is the liveness probe: the process stops answering
/// /health and the kubelet restarts it.
async fn run_once(probe: &Arc<dyn Probe>, cancel: &CancellationToken) -> Disposition {
    let recorder = ObservationRecorder::default();
    let mut run = spawn_run(Arc::clone(probe), recorder.clone());
    tokio::select! {
        biased;
        joined = &mut run => {
            Disposition::Completed(classify(probe.as_ref(), joined, recorder.take()))
        }
        () = cancel.cancelled() => {
            run.abort();
            Disposition::Cancelled
        }
        () = tokio::time::sleep(probe.timeout() * 2) => {
            tracing::error!(probe = probe.name(), timeout = ?probe.timeout(), "{WEDGED_ERROR}");
            // Best effort: an abort only lands once the blocked poll returns,
            // at which point the task ends and the loop stops re-reporting.
            run.abort();
            Disposition::Wedged(wedged_result(probe.as_ref()), run)
        }
    }
}

fn spawn_run(probe: Arc<dyn Probe>, recorder: ObservationRecorder) -> RunHandle {
    let timeout = probe.timeout();
    tokio::spawn(async move { tokio::time::timeout(timeout, probe.run(&recorder)).await })
}

fn wedged_result(probe: &dyn Probe) -> ProbeResult {
    ProbeResult {
        probe: probe.name(),
        api: probe.api(),
        outcome: Outcome::Timeout,
        observations: Vec::new(),
        error: Some(WEDGED_ERROR.to_string()),
        panicked: false,
        at: None,
    }
}

/// Maps a finished run task to its result. Partial observations recorded
/// before a failure or timeout are kept — they are still valid measurements.
fn classify(
    probe: &dyn Probe,
    joined: Result<Result<eyre::Result<()>, tokio::time::error::Elapsed>, JoinError>,
    observations: Vec<Observation>,
) -> ProbeResult {
    let mut result = ProbeResult {
        probe: probe.name(),
        api: probe.api(),
        outcome: Outcome::Success,
        observations,
        error: None,
        panicked: false,
        at: Some(SystemTime::now()),
    };
    match joined {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(err))) => {
            result.outcome = Outcome::Failure;
            result.error = Some(format!("{err:#}"));
            tracing::warn!(probe = probe.name(), error = %err, "probe failed");
        }
        Ok(Err(_elapsed)) => {
            result.outcome = Outcome::Timeout;
            result.error = Some("probe timed out".to_string());
            tracing::warn!(probe = probe.name(), timeout = ?probe.timeout(), "probe timed out");
        }
        Err(join_error) => {
            // A panicking probe is a probe bug: record it and keep the
            // scheduler alive for the remaining probes.
            result.outcome = Outcome::Failure;
            result.panicked = join_error.is_panic();
            let message = panic_message(join_error);
            result.error = Some(message.clone());
            tracing::warn!(probe = probe.name(), error = %message, "probe failed");
        }
    }
    result
}

fn panic_message(err: JoinError) -> String {
    if !err.is_panic() {
        return "probe task cancelled".to_string();
    }
    let payload = err.into_panic();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".to_string());
    format!("probe panicked: {message}")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// What a [`FakeProbe`] run does.
    enum Behavior {
        /// Record these observations and succeed.
        Succeed(Vec<(&'static str, Duration)>),
        /// Record one observation, then fail.
        FailAfterPartial,
        /// Record one observation, then sleep past any test timeout.
        SleepPastTimeout,
        /// Panic (a probe bug).
        Panic,
        /// Rendezvous with the sibling probe — completes only if both runs are
        /// in flight at the same time.
        Rendezvous(Arc<tokio::sync::Barrier>),
        /// Await forever without any timer, so only cancellation ends the run.
        PendForever,
        /// Block the polling thread on a std channel until the test releases
        /// it — a run that ignores even the dropped-future timeout.
        BlockOnStdChannel(Arc<Mutex<std::sync::mpsc::Receiver<()>>>),
    }

    struct FakeProbe {
        name: &'static str,
        interval: Duration,
        timeout: Duration,
        runs: Arc<AtomicU32>,
        behavior: Behavior,
    }

    #[async_trait::async_trait]
    impl Probe for FakeProbe {
        fn name(&self) -> &'static str {
            self.name
        }
        fn api(&self) -> &'static str {
            "test-api"
        }
        fn interval(&self) -> Duration {
            self.interval
        }
        fn timeout(&self) -> Duration {
            self.timeout
        }
        async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            match &self.behavior {
                Behavior::Succeed(observations) => {
                    for (operation, duration) in observations {
                        recorder.record(operation, *duration);
                    }
                    Ok(())
                }
                Behavior::FailAfterPartial => {
                    recorder.record("partial", Duration::from_millis(1));
                    Err(eyre!("boom"))
                }
                Behavior::SleepPastTimeout => {
                    recorder.record("before_sleep", Duration::from_millis(1));
                    tokio::time::sleep(self.timeout * 10).await;
                    Ok(())
                }
                Behavior::Panic => panic!("kaboom"),
                Behavior::Rendezvous(barrier) => {
                    barrier.wait().await;
                    Ok(())
                }
                Behavior::PendForever => {
                    std::future::pending::<()>().await;
                    Ok(())
                }
                Behavior::BlockOnStdChannel(receiver) => {
                    let receiver = receiver.lock().unwrap_or_else(PoisonError::into_inner);
                    let _ = receiver.recv();
                    Ok(())
                }
            }
        }
    }

    fn fake(name: &'static str, behavior: Behavior) -> (Arc<FakeProbe>, Arc<AtomicU32>) {
        fake_with_timing(
            name,
            behavior,
            Duration::from_secs(10),
            Duration::from_secs(1),
        )
    }

    fn fake_with_timing(
        name: &'static str,
        behavior: Behavior,
        interval: Duration,
        timeout: Duration,
    ) -> (Arc<FakeProbe>, Arc<AtomicU32>) {
        let runs = Arc::new(AtomicU32::new(0));
        let probe = Arc::new(FakeProbe {
            name,
            interval,
            timeout,
            runs: Arc::clone(&runs),
            behavior,
        });
        (probe, runs)
    }

    /// A sink capturing every recorded result, for tests that go through
    /// [`collect`].
    #[derive(Default)]
    struct CaptureSink {
        results: Mutex<Vec<(&'static str, Outcome)>>,
    }

    impl Sink for CaptureSink {
        fn record(&self, result: &ProbeResult) {
            self.results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((result.probe, result.outcome));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pipeline_delivers_success_and_observations() {
        let (probe, runs) = fake(
            "ok_probe",
            Behavior::Succeed(vec![
                ("first_op", Duration::from_millis(12)),
                ("second_op", Duration::from_millis(30)),
            ]),
        );
        let cancel = CancellationToken::new();
        let (mut results, loops) = start(vec![probe], cancel.clone());

        // The immediate first fire plus one interval tick.
        for _ in 0..2 {
            let result = results.recv().await.expect("result");
            assert_eq!(result.probe, "ok_probe");
            assert_eq!(result.api, "test-api");
            assert_eq!(result.outcome, Outcome::Success);
            assert_eq!(
                result
                    .observations
                    .iter()
                    .map(|o| o.operation)
                    .collect::<Vec<_>>(),
                vec!["first_op", "second_op"],
                "observations arrive in recording order"
            );
            assert!(
                result.at.is_some(),
                "a real run carries its completion time"
            );
        }
        assert!(runs.load(Ordering::SeqCst) >= 2);

        cancel.cancel();
        join_loops(loops).await.expect("loops end cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn pipeline_delivers_failure_with_partial_observations() {
        let (probe, _) = fake("failing_probe", Behavior::FailAfterPartial);
        let cancel = CancellationToken::new();
        let (mut results, _loops) = start(vec![probe], cancel.clone());

        let result = results.recv().await.expect("result");
        assert_eq!(result.outcome, Outcome::Failure);
        assert!(!result.panicked);
        assert!(
            result.error.as_deref().expect("error").contains("boom"),
            "failure carries the probe error"
        );
        assert_eq!(
            result.observations.len(),
            1,
            "observations before the failure are kept"
        );
        cancel.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn pipeline_delivers_timeout_with_partial_observations() {
        let (probe, _) = fake("slow_probe", Behavior::SleepPastTimeout);
        let cancel = CancellationToken::new();
        let (mut results, _loops) = start(vec![probe], cancel.clone());

        let result = results.recv().await.expect("result");
        assert_eq!(result.outcome, Outcome::Timeout);
        assert_eq!(
            result.observations.len(),
            1,
            "observations before the timeout survive the dropped future"
        );
        assert!(
            result.at.is_some(),
            "a cooperative timeout is a completed run"
        );
        cancel.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn pipeline_survives_panicking_probe() {
        let (panicker, panicker_runs) = fake("panicking_probe", Behavior::Panic);
        let (healthy, _) = fake("healthy_probe", Behavior::Succeed(vec![]));
        let cancel = CancellationToken::new();
        let (mut results, loops) = start(vec![panicker, healthy], cancel.clone());

        let mut panicked = 0;
        let mut healthy_ok = 0;
        while panicked < 2 || healthy_ok < 2 {
            let result = results.recv().await.expect("result");
            match result.probe {
                "panicking_probe" => {
                    assert_eq!(result.outcome, Outcome::Failure);
                    assert!(result.panicked);
                    assert!(
                        result.error.as_deref().expect("error").contains("kaboom"),
                        "panic payload is reported"
                    );
                    panicked += 1;
                }
                "healthy_probe" => {
                    assert_eq!(result.outcome, Outcome::Success);
                    healthy_ok += 1;
                }
                other => panic!("unexpected probe {other}"),
            }
        }
        assert!(
            panicker_runs.load(Ordering::SeqCst) >= 2,
            "a panicking probe keeps being scheduled"
        );

        cancel.cancel();
        join_loops(loops)
            .await
            .expect("a probe panic is contained; loops stay healthy");
    }

    #[tokio::test(start_paused = true)]
    async fn probes_run_concurrently() {
        // Each run blocks on a two-party barrier, so a result can only exist
        // if both probes' runs were in flight at the same time. If the
        // scheduler were serial, the runs would time out instead.
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (a, _) = fake("probe_a", Behavior::Rendezvous(Arc::clone(&barrier)));
        let (b, _) = fake("probe_b", Behavior::Rendezvous(barrier));
        let cancel = CancellationToken::new();
        let (mut results, _loops) = start(vec![a, b], cancel.clone());

        for _ in 0..2 {
            let result = results.recv().await.expect("result");
            assert_eq!(
                result.outcome,
                Outcome::Success,
                "{}: rendezvous requires true concurrency",
                result.probe
            );
        }
        cancel.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_stops_and_drains_pipeline() {
        let (probe, _) = fake("ok_probe", Behavior::Succeed(vec![]));
        let cancel = CancellationToken::new();
        let (mut results, loops) = start(vec![probe], cancel.clone());

        let _ = results.recv().await.expect("first result");
        cancel.cancel();
        while results.recv().await.is_some() {
            // Buffered results drain; the channel then closes.
        }
        join_loops(loops).await.expect("loops end cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_cancelled_run_is_not_recorded() {
        // The run never completes on its own; shutdown must not record a
        // spurious failure for it — every rollout would otherwise log one.
        let (probe, runs) = fake_with_timing(
            "hanging_probe",
            Behavior::PendForever,
            Duration::from_secs(7200),
            Duration::from_secs(3600),
        );
        let cancel = CancellationToken::new();
        let (mut results, loops) = start(vec![probe], cancel.clone());

        // Let the first fire enter its run before shutting down. Yielding
        // (not sleeping) keeps the paused clock from reaching the timeout.
        while runs.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        cancel.cancel();

        let mut recorded = 0;
        while results.recv().await.is_some() {
            recorded += 1;
        }
        assert_eq!(recorded, 0, "an interrupted run is not a measurement");
        join_loops(loops).await.expect("loops end cleanly");
    }

    /// The one real-time test: a blocked poll cannot be dropped or advanced by
    /// the paused clock, which is exactly the failure mode the watchdog exists
    /// for. Waits are event-driven (channel receives) with generous deadlines.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn watchdog_reports_wedged_probe() {
        // A blocked poll can also strand tokio's time driver: with every other
        // worker parked on a timer, nothing polls the driver and no timer —
        // the watchdog's or this test's deadlines — ever fires (in production
        // the liveness probe restarts a process in that state). A std-thread
        // heartbeat keeps unparking the runtime so a free worker keeps picking
        // the driver up and the watchdog under test can do its job.
        let beating = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (beat, mut beats) = tokio::sync::mpsc::unbounded_channel::<()>();
        let heart = std::thread::spawn({
            let beating = Arc::clone(&beating);
            move || {
                while beating.load(Ordering::SeqCst) && beat.send(()).is_ok() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        let pulse = tokio::spawn(async move { while beats.recv().await.is_some() {} });

        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let (probe, runs) = fake_with_timing(
            "wedged_probe",
            Behavior::BlockOnStdChannel(Arc::new(Mutex::new(blocked))),
            Duration::from_millis(200),
            Duration::from_millis(50),
        );
        let cancel = CancellationToken::new();
        let (mut results, loops) = start(vec![probe], cancel.clone());

        // The watchdog fires at 2× timeout, then every tick re-reports while
        // the run stays stuck.
        for nth in 0..2 {
            let result = tokio::time::timeout(Duration::from_secs(10), results.recv())
                .await
                .expect("watchdog result within deadline")
                .expect("result");
            assert_eq!(result.outcome, Outcome::Timeout, "wedged report #{nth}");
            assert!(
                result.error.as_deref().expect("error").contains("wedged"),
                "wedged report #{nth} names the wedge"
            );
            assert!(
                result.at.is_none(),
                "a wedged run completed nothing, so its timestamp must go stale"
            );
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "re-reports do not stack more blocked runs"
        );

        cancel.cancel();
        // Release the blocked poll so the stuck run (whose result is dropped
        // late) and the runtime can end.
        drop(release);
        while tokio::time::timeout(Duration::from_secs(10), results.recv())
            .await
            .expect("drain within deadline")
            .is_some()
        {}
        join_loops(loops).await.expect("loops end cleanly");

        beating.store(false, Ordering::SeqCst);
        heart.join().expect("heartbeat thread ends");
        pulse.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn collect_forwards_to_sink_until_drained() {
        let (probe, _) = fake("ok_probe", Behavior::Succeed(vec![]));
        let cancel = CancellationToken::new();
        let (results, _loops) = start(vec![probe], cancel.clone());
        let sink = Arc::new(CaptureSink::default());
        let collector = tokio::spawn(collect(results, Arc::clone(&sink) as Arc<dyn Sink>));

        while sink
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
        collector.await.expect("collector drains and returns");

        let recorded = sink
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert!(recorded.contains(&("ok_probe", Outcome::Success)));
    }
}
