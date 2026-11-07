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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::join_all;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Delivery progress of one sink's workers, in records.
///
/// Records replaced while waiting in a queue or a staged batch are never
/// started, so they are neither delivered nor dropped.
#[derive(Clone, Default)]
pub(crate) struct DeliveryCounts(Arc<Counts>);

#[derive(Default)]
struct Counts {
    pending: AtomicUsize,
    delivered: AtomicUsize,
    failed: AtomicUsize,
}

impl DeliveryCounts {
    /// Counts `count` records a worker removed from its queue for delivery.
    pub(crate) fn start(&self, count: usize) {
        self.0.pending.fetch_add(count, Ordering::SeqCst);
    }

    /// Settles `count` started records, of which `delivered` were acknowledged
    /// and `failed` were rejected or could not be sent.
    pub(crate) fn finish(&self, count: usize, delivered: usize, failed: usize) {
        self.0.delivered.fetch_add(delivered, Ordering::SeqCst);
        self.0.failed.fetch_add(failed, Ordering::SeqCst);
        self.0.pending.fetch_sub(count, Ordering::SeqCst);
    }

    fn settled(&self) -> (usize, usize) {
        (
            self.0.delivered.load(Ordering::SeqCst),
            self.0.failed.load(Ordering::SeqCst),
        )
    }

    fn pending(&self) -> usize {
        self.0.pending.load(Ordering::SeqCst)
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
        let settled: Vec<(usize, usize)> = self
            .sinks
            .iter()
            .map(|sink| sink.counts.settled())
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

        for (sink, (delivered_before, failed_before)) in self.sinks.iter().zip(settled) {
            let (delivered, failed) = sink.counts.settled();
            let remaining = (sink.queued)() + sink.counts.pending();
            let dropped = failed - failed_before + remaining;

            flushed_record_count += delivered - delivered_before;
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
    use std::time::Duration;

    use carbide_instrument::testing::capture_logs_async;

    use super::*;
    use crate::sink::dedup_queue::DedupQueue;
    use crate::sink::report_worker::run_report_worker;

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
}
