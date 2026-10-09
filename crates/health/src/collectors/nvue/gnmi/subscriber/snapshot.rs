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

//! Reads and applies periodic snapshots beside synchronized gNMI streams.

use std::future::{Future, poll_fn};
use std::task::Poll;
use std::time::Duration;

use futures::stream;
use rand::RngExt;
use tokio_stream::{Stream, StreamExt};

use super::super::on_change_processor::{EventSnapshot, GnmiOnChangeProcessor};
use super::super::proto;
use super::super::reconciliation::{MetricReconciler, MetricSnapshot};
use super::{
    GnmiClientProvider, GnmiStreamMetrics, SubscriptionResponses, subscribe_with_cached_credentials,
};
use crate::HealthError;

enum ReconciliationSnapshot {
    Events(EventSnapshot),
    Metrics(MetricSnapshot),
}

// Events retain whole rows; other streams retain individual metric source paths.
pub(super) enum ReconciliationProcessor<'a> {
    Events(&'a GnmiOnChangeProcessor, &'a GnmiStreamMetrics),
    Metrics(&'a MetricReconciler),
}

/// Runs snapshot reconciliation only while its live subscription is synchronized.
pub(super) struct Reconciliation<'a> {
    /// Presence cache that owns the readings for the live stream.
    pub(super) processor: ReconciliationProcessor<'a>,

    /// Snapshot cadence; zero disables reconciliation.
    pub(super) interval: Duration,

    /// ONCE request covering the same paths as the live stream.
    pub(super) request: proto::SubscribeRequest,

    /// Stream identity used in reconciliation logs.
    pub(super) stream_name: &'a str,
}

impl Reconciliation<'_> {
    /// Polls snapshots beside live updates and discards unfinished presence on exit.
    pub(super) async fn consume<F>(
        &self,
        live: F,
        client_provider: &GnmiClientProvider,
    ) -> Result<(), tonic::Status>
    where
        F: Future<Output = Result<(), tonic::Status>>,
    {
        let result = if self.interval.is_zero() {
            live.await
        } else {
            let snapshots = reconciliation_snapshots(
                client_provider,
                &self.processor,
                self.interval,
                self.request.clone(),
            );

            self.consume_snapshots(live, snapshots, &client_provider.switch_id)
                .await
        };

        // The losing snapshot future releases its RPC before pending presence is discarded.
        self.processor.finish(None);

        result
    }

    /// Applies completed snapshots only after ready live updates have protected their sources.
    async fn consume_snapshots<F, S>(
        &self,
        live: F,
        snapshots: S,
        switch_id: &str,
    ) -> Result<(), tonic::Status>
    where
        F: Future<Output = Result<(), tonic::Status>>,
        S: Stream<Item = Result<ReconciliationSnapshot, HealthError>>,
    {
        tokio::pin!(live, snapshots);

        loop {
            let snapshot = tokio::select! {
                result = &mut live => return result,
                snapshot = snapshots.next() => snapshot,
            };

            let Some(snapshot) = snapshot else {
                return Ok(());
            };

            // Snapshot completion can be polled before an already-ready live response.
            // Poll the live consumer again before committing any removals.
            if let Poll::Ready(result) = poll_fn(|cx| Poll::Ready(live.as_mut().poll(cx))).await {
                return result;
            }

            let snapshot = snapshot.and_then(|snapshot| {
                // A cooperative yield does not prove the live RPC was drained.
                // Preserve candidates and retry next interval if the task budget ran out.
                if !tokio::task::coop::has_budget_remaining() {
                    return Err(HealthError::GnmiError(
                        "gNMI cleanup deferred while live updates are backlogged".to_string(),
                    ));
                }

                Ok(snapshot)
            });

            match snapshot {
                Ok(snapshot) => {
                    let removed = self.processor.finish(Some(snapshot));

                    if removed > 0 {
                        tracing::info!(
                            switch_id,
                            stream = self.stream_name,
                            removed_metric_count = removed,
                            "Reconciled cached gNMI metrics"
                        );
                    }
                }
                Err(error) => {
                    self.processor.finish(None);
                    tracing::warn!(switch_id, stream = self.stream_name, %error, "Failed to reconcile cached gNMI metrics");
                }
            }
        }
    }
}

impl ReconciliationProcessor<'_> {
    fn begin(&self, request: &proto::SubscribeRequest) -> Option<ReconciliationSnapshot> {
        match self {
            Self::Events(processor, _) => processor
                .begin_reconciliation()
                .map(ReconciliationSnapshot::Events),
            Self::Metrics(processor) => processor
                .begin(request)
                .map(ReconciliationSnapshot::Metrics),
        }
    }

    fn finish(&self, snapshot: Option<ReconciliationSnapshot>) -> usize {
        match (self, snapshot) {
            (Self::Events(processor, metrics), Some(ReconciliationSnapshot::Events(snapshot))) => {
                processor.finish_reconciliation(Some(snapshot), metrics)
            }
            (Self::Metrics(processor), Some(ReconciliationSnapshot::Metrics(snapshot))) => {
                processor.finish(Some(snapshot))
            }
            (Self::Events(processor, metrics), _) => processor.finish_reconciliation(None, metrics),
            (Self::Metrics(processor), _) => processor.finish(None),
        }
    }
}

/// Reads one complete snapshot and releases its RPC before refreshing rejected credentials.
async fn collect_snapshot(
    client_provider: &GnmiClientProvider,
    mut snapshot: ReconciliationSnapshot,
    request: proto::SubscribeRequest,
) -> Result<ReconciliationSnapshot, HealthError> {
    let (stream, generation) =
        match subscribe_with_cached_credentials(client_provider, request).await {
            Ok(opened) => opened,
            Err(error) => {
                if let Some(generation) = error.credential_generation {
                    client_provider
                        .refresh_auth_if_needed(&error.error, generation)
                        .await;
                }

                return Err(error.error);
            }
        };

    let mut stream = SubscriptionResponses::new(stream, client_provider.request_timeout);

    let result = async {
        loop {
            let response = stream
                .next()
                .await
                .transpose()
                .map_err(HealthError::GnmiStatus)?
                .flatten()
                .ok_or_else(|| {
                    HealthError::GnmiError(
                        "gNMI snapshot closed before synchronization".to_string(),
                    )
                })?;

            let complete = match &mut snapshot {
                ReconciliationSnapshot::Events(snapshot) => snapshot.process_response(&response),
                ReconciliationSnapshot::Metrics(snapshot) => snapshot.process_response(&response),
            }
            .map_err(HealthError::GnmiStatus)?;

            if complete {
                return Ok(snapshot);
            }
        }
    }
    .await;

    drop(stream);

    if let Err(error) = &result {
        client_provider
            .refresh_auth_if_needed(error, generation)
            .await;
    }

    result
}

/// Reads snapshots beside live updates; only the consumer commits removals.
fn reconciliation_snapshots<'a>(
    client_provider: &'a GnmiClientProvider,
    processor: &'a ReconciliationProcessor<'a>,
    interval: Duration,
    request: proto::SubscribeRequest,
) -> impl Stream<Item = Result<ReconciliationSnapshot, HealthError>> + 'a {
    let phase = rand::rng().random_range(Duration::ZERO..interval);
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + phase, interval);

    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    stream::unfold(timer, move |mut timer| {
        let request = request.clone();

        async move {
            loop {
                timer.tick().await;

                let Some(snapshot) = processor.begin(&request) else {
                    continue;
                };

                let attempt = tokio::time::timeout(
                    client_provider.request_timeout,
                    collect_snapshot(client_provider, snapshot, request.clone()),
                );

                tokio::pin!(attempt);

                let result = loop {
                    tokio::select! {
                        result = &mut attempt => break result,
                        _ = timer.tick() => {}
                    }
                }
                .unwrap_or_else(|_| {
                    Err(HealthError::GnmiError(
                        "gNMI snapshot exceeded request_timeout".to_string(),
                    ))
                });

                return Some((result, timer));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::super::super::client::{build_sample_subscribe_request, build_snapshot_request};
    use super::*;
    use crate::endpoint::BmcAddr;
    use crate::sink::{EventContext, MetricSample};

    #[tokio::test]
    async fn completed_snapshots_preserve_ready_live_updates_and_retry_after_backlogs() {
        for (name, backlogged, expected_retained) in
            [("ready update", false, 1), ("live backlog", true, 2)]
        {
            let context = EventContext {
                endpoint_key: "switch".into(),
                addr: BmcAddr {
                    ip: "127.0.0.1".parse().unwrap(),
                    port: None,
                    mac: None,
                },
                collector_type: "nvue_gnmi",
                metadata: None,
                rack_id: None,
                labels: Default::default(),
            };

            let processor = MetricReconciler::new(None, context);

            let request = build_snapshot_request(build_sample_subscribe_request(
                &[proto::Path::default()],
                1,
            ));

            let mut sample = MetricSample {
                key: "live".into(),
                name: "nvue_gnmi".into(),
                metric_type: "reading".into(),
                unit: "count".into(),
                value: 1.0,
                labels: Vec::new(),
                context: None,
            };

            let live_path = proto::PathElem {
                name: "live".into(),
                ..Default::default()
            };

            let stale_path = proto::PathElem {
                name: "stale".into(),
                ..Default::default()
            };

            processor.record(&sample, &[&live_path]);

            sample.key = "stale".into();
            processor.record(&sample, &[&stale_path]);

            let reconciliation = Reconciliation {
                processor: ReconciliationProcessor::Metrics(&processor),
                interval: Duration::from_secs(1800),
                request,
                stream_name: "test",
            };

            let snapshot = reconciliation
                .processor
                .begin(&reconciliation.request)
                .unwrap();

            let ready = AtomicBool::new(false);

            let snapshots = stream::once(async {
                // Make a live response ready in the same poll as snapshot completion.
                ready.store(true, Ordering::SeqCst);
                Ok(snapshot)
            });

            let live = async {
                poll_fn(|_| {
                    if ready.load(Ordering::SeqCst) {
                        processor.touch(&[&live_path]);
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;

                if backlogged {
                    loop {
                        tokio::task::consume_budget().await;
                    }
                }

                pending::<Result<(), tonic::Status>>().await
            };

            tokio::time::timeout(
                Duration::from_secs(1),
                reconciliation.consume_snapshots(live, snapshots, "switch"),
            )
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .unwrap();

            // A later quiet snapshot can remove every reading retained by this attempt.
            let retry = reconciliation
                .processor
                .begin(&reconciliation.request)
                .unwrap_or_else(|| panic!("{name}: cleanup removed a buffered live source"));

            assert_eq!(
                reconciliation.processor.finish(Some(retry)),
                expected_retained,
                "{name}"
            );
        }
    }
}
