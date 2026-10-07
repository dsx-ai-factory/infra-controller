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

mod inventory_source;

use std::sync::Arc;
use std::time::Instant;

use futures::future::join_all;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use self::inventory_source::{LogSource, PollOutcome, PolledInventorySource, SourceFailure};
use crate::config::InventoryConfig;
use crate::inventory::{InventoryLease, InventoryProvider, InventorySnapshot};
use crate::metrics::Metrics;
use crate::state::Fabric;

const MAX_CONCURRENT_INVENTORY_REQUESTS: usize = 8;

pub(crate) fn start(
    fabric: Fabric,
    metrics: Metrics,
    config: InventoryConfig,
    local_provider: Option<Arc<dyn InventoryProvider>>,
    cancellation: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run(fabric, metrics, config, local_provider, cancellation).await {
            tracing::error!(error = %error, "UFM inventory reconciliation stopped");
        }
    })
}

async fn run(
    fabric: Fabric,
    metrics: Metrics,
    config: InventoryConfig,
    local_provider: Option<Arc<dyn InventoryProvider>>,
    cancellation: CancellationToken,
) -> eyre::Result<()> {
    let InventoryConfig {
        poll_interval,
        request_timeout,
        failure_grace_period,
        failure_action,
        static_sources,
    } = config;
    let mut next_lease = InventoryLease::FIRST_REMOTE;
    let mut sources = static_sources
        .into_iter()
        .map(|config_source| {
            let lease = next_lease;
            next_lease = next_lease.checked_next().expect("inventory lease overflow");
            PolledInventorySource::new(config_source, request_timeout, lease)
        })
        .collect::<Vec<_>>();

    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let request_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_INVENTORY_REQUESTS));

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            _ = interval.tick() => {}
        }

        if let Some(provider) = local_provider.as_ref() {
            let snapshot = provider.inventory_snapshot();
            record_reconciliation(&fabric, &metrics, snapshot, InventoryLease::LOCAL);
        }

        let requests = sources
            .drain(..)
            .map(|source| source.poll(Arc::clone(&request_semaphore)));
        let polled_sources = join_all(requests).await;

        for (source, outcome) in polled_sources {
            match outcome {
                PollOutcome::ClientUnavailable => {}
                PollOutcome::Succeeded {
                    snapshot,
                    previous_inventory_id,
                } => {
                    if let Some(previous_id) = previous_inventory_id.as_ref() {
                        fabric.source_failed(
                            previous_id,
                            source.lease(),
                            crate::config::FailureAction::Remove,
                        );
                    }
                    record_reconciliation(&fabric, &metrics, snapshot, source.lease());
                }
                PollOutcome::Failed(error) => {
                    tracing::warn!(source = %LogSource(&source), error, "Could not poll machine-a-tron inventory");
                    metrics.record_reconciliation("poll_failed");
                }
            }
            sources.push(source);
        }

        let now = Instant::now();
        for source in &mut sources {
            match source.take_failure(now, failure_grace_period) {
                Some(SourceFailure::ClientConfiguration(error)) => {
                    tracing::warn!(source = %LogSource(source), error, "Could not configure inventory HTTP client");
                    metrics.record_reconciliation("client_configuration_failed");
                }
                Some(SourceFailure::Inventory {
                    inventory_id,
                    lease,
                }) => {
                    fabric.source_failed(inventory_id, lease, failure_action.clone());
                    metrics.record_reconciliation(match failure_action {
                        crate::config::FailureAction::MarkDown => "source_marked_down",
                        crate::config::FailureAction::Remove => "source_removed",
                    });
                }
                None => {}
            }
        }
    }
}

fn record_reconciliation(
    fabric: &Fabric,
    metrics: &Metrics,
    snapshot: InventorySnapshot,
    lease: InventoryLease,
) {
    match fabric.reconcile(snapshot, lease) {
        Ok(outcome) => metrics.record_reconciliation(outcome.metric_label()),
        Err(error) => {
            tracing::warn!(error = %error, "Rejected machine-a-tron inventory snapshot");
            metrics.record_reconciliation("invalid_snapshot");
        }
    }
}
