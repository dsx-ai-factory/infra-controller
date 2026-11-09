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

//! Shutdown flush for the background workers that deliver sink queues.
//!
//! Sinks hand records to detached workers, and abandoned collectors keep their
//! sink references, so dropping a sink cannot trigger delivery. Instead each
//! sink registers its workers and backlog here, and [`SinkWorkers::flush`]
//! signals them all once collectors have drained.

use std::sync::{Arc, Mutex};

use futures::future::join_all;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Delivery progress of one sink's workers, in records.
///
/// A record is one queued entry: a health report, a log record, or a metric
/// observation, which can encode several OTLP data points. Records replaced
/// while waiting in a queue or a staged batch are never started, so they are
/// neither delivered nor dropped.
///
/// The counts change together under one lock, so a snapshot never sees a
/// record both pending and settled. An entry a worker has removed from its
/// queue but not yet started is in neither its queue nor these counts, so
/// totals read while workers still run are approximate for entries in transit.
#[derive(Clone, Default)]
pub(crate) struct DeliveryCounts(Arc<Mutex<Counts>>);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Counts {
    pending: usize,
    delivered: usize,
    failed: usize,
}

impl DeliveryCounts {
    /// Counts `count` records a worker removed from its queue for delivery.
    pub(crate) fn start(&self, count: usize) {
        self.lock().pending += count;
    }

    /// Settles `count` started records, of which `delivered` were acknowledged
    /// and `failed` were rejected or could not be sent.
    pub(crate) fn finish(&self, count: usize, delivered: usize, failed: usize) {
        let mut counts = self.lock();
        counts.pending -= count;
        counts.delivered += delivered;
        counts.failed += failed;
    }

    fn snapshot(&self) -> Counts {
        *self.lock()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.0.lock().expect("delivery counts mutex poisoned")
    }
}

struct RegisteredSink {
    sink_type: &'static str,
    endpoint_address: Option<String>,
    queued: Box<dyn Fn() -> usize + Send + Sync>,
    counts: DeliveryCounts,
    handles: Vec<JoinHandle<()>>,
}

/// Background workers of every configured sink, flushed once at shutdown.
#[derive(Default)]
pub(crate) struct SinkWorkers {
    flush_token: CancellationToken,
    sinks: Vec<RegisteredSink>,
}

impl SinkWorkers {
    /// Returns the signal that tells registered workers to deliver their
    /// backlog and return.
    pub(super) fn flush_token(&self) -> CancellationToken {
        self.flush_token.clone()
    }

    /// Tracks the workers of one sink, or of one target for a sink with
    /// several. `queued` reports records waiting in the sink's queues, and
    /// `counts` the records its workers have started and settled.
    pub(super) fn register(
        &mut self,
        sink_type: &'static str,
        endpoint_address: Option<String>,
        queued: impl Fn() -> usize + Send + Sync + 'static,
        counts: DeliveryCounts,
        handles: Vec<JoinHandle<()>>,
    ) {
        self.sinks.push(RegisteredSink {
            sink_type,
            endpoint_address,
            queued: Box::new(queued),
            counts,
            handles,
        });
    }

    /// Signals every registered worker to deliver its backlog and waits for
    /// them until `deadline`.
    ///
    /// Workers still running at `deadline` are abandoned. Records delivered
    /// after the signal are logged as flushed. Records that failed after the
    /// signal, plus records still queued or being delivered at `deadline`, are
    /// logged as dropped, per sink and in the final totals.
    pub(crate) async fn flush(mut self, deadline: Instant) {
        let before: Vec<Counts> = self
            .sinks
            .iter()
            .map(|sink| sink.counts.snapshot())
            .collect();

        self.flush_token.cancel();

        let handles = self
            .sinks
            .iter_mut()
            .flat_map(|sink| sink.handles.iter_mut());

        let _ = tokio::time::timeout_at(deadline, join_all(handles)).await;

        let mut flushed_record_count = 0;
        let mut dropped_record_count = 0;
        let mut timed_out_sink_count = 0;

        for (sink, before) in self.sinks.iter().zip(before) {
            let after = sink.counts.snapshot();
            let remaining = (sink.queued)() + after.pending;
            let dropped = after.failed - before.failed + remaining;

            flushed_record_count += after.delivered - before.delivered;
            dropped_record_count += dropped;

            if !sink.handles.iter().all(JoinHandle::is_finished) {
                timed_out_sink_count += 1;
            }

            if dropped > 0 {
                tracing::warn!(
                    sink_type = sink.sink_type,
                    endpoint_address = sink.endpoint_address.as_deref(),
                    dropped_record_count = dropped,
                    "Sink dropped records at shutdown"
                );
            }
        }

        tracing::info!(
            flushed_record_count,
            dropped_record_count,
            timed_out_sink_count,
            "Sinks flushed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use carbide_instrument::testing::capture_logs_async;
    use futures::FutureExt;
    use futures::future::BoxFuture;
    use tokio::task::JoinError;

    use super::*;
    use crate::HealthError;
    use crate::discovery::{DiscoveryIteration, DiscoveryShutdown};
    use crate::sink::dedup_queue::DedupQueue;
    use crate::sink::report_worker::run_report_worker;

    type Discovery = BoxFuture<'static, Result<FinalRecordIteration, JoinError>>;
    type Listener = BoxFuture<'static, Result<Result<(), String>, JoinError>>;

    /// Discovery iteration whose collector shutdown takes `drain` and then
    /// queues a final record, as a draining collector would.
    struct FinalRecordIteration {
        queue: Arc<DedupQueue<&'static str, ()>>,
        shutdowns: Arc<AtomicUsize>,
        drain: Duration,
    }

    #[async_trait]
    impl DiscoveryIteration for FinalRecordIteration {
        async fn run_once(&mut self) -> Result<(), HealthError> {
            Ok(())
        }

        async fn shutdown(&mut self, _deadline: Instant) -> DiscoveryShutdown {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.drain).await;
            self.queue.save_latest("final", ());
            DiscoveryShutdown {
                closed: 1,
                timed_out: 0,
            }
        }
    }

    /// One registered report worker whose submissions wait for the flush
    /// signal, holding a record queued before shutdown.
    struct ServiceFixture {
        sink_workers: SinkWorkers,
        queue: Arc<DedupQueue<&'static str, ()>>,
        submitted: Arc<Mutex<Vec<&'static str>>>,
        shutdowns: Arc<AtomicUsize>,
        cancel_token: CancellationToken,
    }

    impl ServiceFixture {
        fn new(stall: bool) -> Self {
            let queue = Arc::new(DedupQueue::new());
            queue.save_latest("queued", ());

            let mut sink_workers = SinkWorkers::default();
            let counts = DeliveryCounts::default();
            let submitted = Arc::new(Mutex::new(Vec::new()));
            let flush_token = sink_workers.flush_token();

            let handle = tokio::spawn(run_report_worker(
                Arc::clone(&queue),
                counts.clone(),
                sink_workers.flush_token(),
                {
                    let submitted = Arc::clone(&submitted);
                    move |key, ()| {
                        let submitted = Arc::clone(&submitted);
                        let flush_token = flush_token.clone();

                        async move {
                            flush_token.cancelled().await;

                            if stall {
                                std::future::pending::<()>().await;
                            }

                            submitted.lock().unwrap().push(key);
                            true
                        }
                    }
                },
            ));

            let queued = Arc::clone(&queue);

            sink_workers.register(
                "test_sink",
                None,
                move || queued.len(),
                counts,
                vec![handle],
            );

            Self {
                sink_workers,
                queue,
                submitted,
                shutdowns: Arc::new(AtomicUsize::new(0)),
                cancel_token: CancellationToken::new(),
            }
        }

        fn iteration(&self, drain: Duration) -> FinalRecordIteration {
            FinalRecordIteration {
                queue: Arc::clone(&self.queue),
                shutdowns: Arc::clone(&self.shutdowns),
                drain,
            }
        }

        /// A discovery loop that returns its iteration once cancelled.
        fn discovery_returning_on_cancel(&self, drain: Duration) -> Discovery {
            let cancel_token = self.cancel_token.clone();
            let iteration = self.iteration(drain);

            async move {
                cancel_token.cancelled().await;
                Ok(iteration)
            }
            .boxed()
        }
    }

    async fn join_error() -> JoinError {
        let handle = tokio::spawn(std::future::pending::<()>());
        handle.abort();
        handle.await.unwrap_err()
    }

    #[test]
    fn finish_moves_records_from_pending_to_settled_in_one_snapshot() {
        let counts = DeliveryCounts::default();

        counts.start(3);
        counts.finish(2, 1, 1);

        assert_eq!(
            counts.snapshot(),
            Counts {
                pending: 1,
                delivered: 1,
                failed: 1,
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn flush_delivers_queued_reports_and_abandons_stalled_submissions() {
        #[derive(Clone, Copy)]
        enum Submission {
            Delivered,
            Failed,
            Stalled,
        }

        struct Case {
            scenario: &'static str,
            submission: Submission,
            submitted: usize,
            flushed: &'static str,
            dropped: Option<&'static str>,
            timed_out: &'static str,
        }

        for case in [
            Case {
                scenario: "queued reports are submitted after the flush signal",
                submission: Submission::Delivered,
                submitted: 3,
                flushed: "3",
                dropped: None,
                timed_out: "0",
            },
            Case {
                scenario: "failed submissions are dropped, not flushed",
                submission: Submission::Failed,
                submitted: 3,
                flushed: "0",
                dropped: Some("3"),
                timed_out: "0",
            },
            Case {
                scenario: "stalled submissions are abandoned at the deadline",
                submission: Submission::Stalled,
                submitted: 0,
                flushed: "0",
                dropped: Some("3"),
                timed_out: "1",
            },
        ] {
            let queue = Arc::new(DedupQueue::new());

            for key in ["a", "b", "c"] {
                queue.save_latest(key, ());
            }

            let mut sink_workers = SinkWorkers::default();
            let counts = DeliveryCounts::default();
            let submitted = Arc::new(Mutex::new(Vec::new()));
            let submission = case.submission;

            let handles = (0..2)
                .map(|_| {
                    let submitted = Arc::clone(&submitted);
                    tokio::spawn(run_report_worker(
                        Arc::clone(&queue),
                        counts.clone(),
                        sink_workers.flush_token(),
                        move |key, ()| {
                            let submitted = Arc::clone(&submitted);

                            async move {
                                if let Submission::Stalled = submission {
                                    std::future::pending::<()>().await;
                                }

                                submitted.lock().unwrap().push(key);
                                matches!(submission, Submission::Delivered)
                            }
                        },
                    ))
                })
                .collect();

            let queued = Arc::clone(&queue);
            sink_workers.register("test_sink", None, move || queued.len(), counts, handles);

            let deadline = Instant::now() + Duration::from_secs(20);
            let ((), logs) = capture_logs_async(sink_workers.flush(deadline)).await;

            assert_eq!(
                submitted.lock().unwrap().len(),
                case.submitted,
                "{}",
                case.scenario
            );

            let dropped = logs
                .iter()
                .find(|log| log.message == "Sink dropped records at shutdown");

            assert_eq!(
                dropped.and_then(|log| log.field("dropped_record_count")),
                case.dropped,
                "{}",
                case.scenario
            );

            if let Some(dropped) = dropped {
                assert_eq!(dropped.field("sink_type"), Some("test_sink"));
            }

            let summary = logs
                .iter()
                .find(|log| log.message == "Sinks flushed")
                .unwrap_or_else(|| panic!("{}: summary is logged", case.scenario));

            assert_eq!(
                summary.field("flushed_record_count"),
                Some(case.flushed),
                "{}",
                case.scenario
            );

            assert_eq!(
                summary.field("dropped_record_count"),
                Some(case.dropped.unwrap_or("0")),
                "{}",
                case.scenario
            );

            assert_eq!(
                summary.field("timed_out_sink_count"),
                Some(case.timed_out),
                "{}",
                case.scenario
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_service_exit_drains_collectors_and_flushes_sinks_once() {
        #[derive(Clone, Copy)]
        enum Exit {
            SignalFirst,
            ListenerCancelsWhileExiting,
            DiscoveryCancelsWhileReturning,
            DiscoveryJoinFailsWhileCancelling,
        }

        struct Case {
            scenario: &'static str,
            exit: Exit,
            shutdowns: usize,
            submitted: &'static [&'static str],
            discovery_log: &'static str,
        }

        for case in [
            Case {
                scenario: "signal observed first",
                exit: Exit::SignalFirst,
                shutdowns: 1,
                submitted: &["queued", "final"],
                discovery_log: "Collectors drained",
            },
            Case {
                scenario: "listener cancels the token while returning",
                exit: Exit::ListenerCancelsWhileExiting,
                shutdowns: 1,
                submitted: &["queued", "final"],
                discovery_log: "Collectors drained",
            },
            Case {
                scenario: "discovery cancels the token while returning its iteration",
                exit: Exit::DiscoveryCancelsWhileReturning,
                shutdowns: 1,
                submitted: &["queued", "final"],
                discovery_log: "Collectors drained",
            },
            Case {
                scenario: "discovery join fails while cancelling",
                exit: Exit::DiscoveryJoinFailsWhileCancelling,
                shutdowns: 0,
                submitted: &["queued"],
                discovery_log: "Discovery loop join error during shutdown",
            },
        ] {
            let fixture = ServiceFixture::new(false);
            let cancel_token = fixture.cancel_token.clone();
            let pending_listener: Listener = std::future::pending().boxed();

            // Async blocks panic if polled after completion, so a completed
            // discovery future polled again would fail the test.
            let (discovery, listener): (Discovery, Listener) = match case.exit {
                Exit::SignalFirst => {
                    cancel_token.cancel();
                    (
                        fixture.discovery_returning_on_cancel(Duration::ZERO),
                        pending_listener,
                    )
                }
                Exit::ListenerCancelsWhileExiting => (
                    fixture.discovery_returning_on_cancel(Duration::ZERO),
                    async move {
                        cancel_token.cancel();
                        Ok(Ok(()))
                    }
                    .boxed(),
                ),
                Exit::DiscoveryCancelsWhileReturning => {
                    let iteration = fixture.iteration(Duration::ZERO);

                    let discovery = async move {
                        cancel_token.cancel();
                        Ok(iteration)
                    };

                    (discovery.boxed(), pending_listener)
                }
                Exit::DiscoveryJoinFailsWhileCancelling => {
                    let error = join_error().await;

                    let discovery = async move {
                        cancel_token.cancel();
                        Err(error)
                    };

                    (discovery.boxed(), pending_listener)
                }
            };

            let ((), logs) = capture_logs_async(crate::wait_for_service_exit(
                discovery,
                listener,
                &fixture.cancel_token,
                fixture.sink_workers,
                Duration::from_secs(20),
            ))
            .await;

            assert_eq!(
                fixture.shutdowns.load(Ordering::SeqCst),
                case.shutdowns,
                "{}",
                case.scenario
            );

            assert_eq!(
                *fixture.submitted.lock().unwrap(),
                case.submitted,
                "{}",
                case.scenario
            );

            for message in [case.discovery_log, "Sinks flushed"] {
                assert_eq!(
                    logs.iter().filter(|log| log.message == message).count(),
                    1,
                    "{}: {message}",
                    case.scenario
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn uncancelled_service_exit_logs_the_failure_without_flushing_sinks() {
        #[derive(Clone, Copy)]
        enum Exit {
            ListenerShutdown,
            ListenerFails,
            ListenerJoinFails,
            DiscoveryEnds,
            DiscoveryJoinFails,
        }

        for (exit, level, message) in [
            (
                Exit::ListenerShutdown,
                tracing::Level::INFO,
                "Metrics listener shutdown",
            ),
            (
                Exit::ListenerFails,
                tracing::Level::ERROR,
                "Metrics listener failed",
            ),
            (
                Exit::ListenerJoinFails,
                tracing::Level::ERROR,
                "Metrics listener join error",
            ),
            (
                Exit::DiscoveryEnds,
                tracing::Level::ERROR,
                "Discovery loop ended unexpectedly",
            ),
            (
                Exit::DiscoveryJoinFails,
                tracing::Level::ERROR,
                "Discovery loop join error",
            ),
        ] {
            let fixture = ServiceFixture::new(false);
            let flush_token = fixture.sink_workers.flush_token();
            let pending_discovery: Discovery = std::future::pending().boxed();
            let pending_listener: Listener = std::future::pending().boxed();

            let (discovery, listener): (Discovery, Listener) = match exit {
                Exit::ListenerShutdown => (pending_discovery, async { Ok(Ok(())) }.boxed()),
                Exit::ListenerFails => (
                    pending_discovery,
                    async { Ok(Err("bind failed".to_string())) }.boxed(),
                ),
                Exit::ListenerJoinFails => {
                    let error = join_error().await;
                    (pending_discovery, async { Err(error) }.boxed())
                }
                Exit::DiscoveryEnds => {
                    let iteration = fixture.iteration(Duration::ZERO);
                    (async { Ok(iteration) }.boxed(), pending_listener)
                }
                Exit::DiscoveryJoinFails => {
                    let error = join_error().await;
                    (async { Err(error) }.boxed(), pending_listener)
                }
            };

            let ((), logs) = capture_logs_async(crate::wait_for_service_exit(
                discovery,
                listener,
                &fixture.cancel_token,
                fixture.sink_workers,
                Duration::from_secs(20),
            ))
            .await;

            assert!(
                logs.iter()
                    .any(|log| log.level == level && log.message == message),
                "{message}"
            );

            assert!(
                !logs.iter().any(|log| log.message == "Sinks flushed"
                    || log.message == "Shutdown signal received; draining collectors"),
                "{message}"
            );

            assert_eq!(fixture.shutdowns.load(Ordering::SeqCst), 0, "{message}");
            assert!(!flush_token.is_cancelled(), "{message}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_phases_share_one_deadline_from_the_observed_signal() {
        struct Case {
            scenario: &'static str,
            drain: Duration,
            flush_signalled_after: Duration,
        }

        for case in [
            Case {
                scenario: "stalled sink gets the budget left after draining",
                drain: Duration::from_secs(5),
                flush_signalled_after: Duration::from_secs(7),
            },
            Case {
                scenario: "collectors exhaust the budget",
                drain: Duration::from_secs(15),
                flush_signalled_after: Duration::from_secs(10),
            },
        ] {
            let fixture = ServiceFixture::new(true);
            let flush_token = fixture.sink_workers.flush_token();

            let flush_signalled = tokio::spawn(async move {
                flush_token.cancelled().await;
                Instant::now()
            });

            let iteration = fixture.iteration(case.drain);

            let discovery = async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok(iteration)
            };

            fixture.cancel_token.cancel();
            let start = Instant::now();

            let ((), logs) = capture_logs_async(crate::wait_for_service_exit(
                discovery,
                std::future::pending::<Result<Result<(), String>, JoinError>>(),
                &fixture.cancel_token,
                fixture.sink_workers,
                Duration::from_secs(10),
            ))
            .await;

            assert_eq!(
                start.elapsed(),
                Duration::from_secs(10),
                "{}",
                case.scenario
            );

            assert_eq!(
                flush_signalled.await.unwrap() - start,
                case.flush_signalled_after,
                "{}",
                case.scenario
            );

            let summary = logs
                .iter()
                .find(|log| log.message == "Sinks flushed")
                .unwrap_or_else(|| panic!("{}: summary is logged", case.scenario));

            assert_eq!(
                summary.field("timed_out_sink_count"),
                Some("1"),
                "{}",
                case.scenario
            );
        }
    }
}
