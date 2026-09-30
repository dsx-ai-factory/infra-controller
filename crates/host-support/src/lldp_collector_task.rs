// SPDX-FileCopyrightText: Copyright (c) 2025-2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Periodically collects LLDP neighbors.
//!
//! [`start_lldp_collector`] owns the whole arrangement: it starts the task, runs
//! the collection, and hands back a [`LatestLldpCollection`] the consumer
//! reads without awaiting anything.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::lldp_collector::{LldpCollectorResult, LldpNeighbor};

/// The most recent completed LLDP collection, shared from the collector task to
/// a service's control loop.
///
/// Every finished attempt lands here as itself: a failure as `Err`,
/// and a host that genuinely sees no neighbors as `Ok(vec![])`.
///
/// Only the newest attempt is kept. An older one is overwritten rather than
/// queued.
#[derive(Clone, Default)]
pub struct LatestLldpCollection(Arc<ArcSwapOption<LldpCollectorResult<Vec<LldpNeighbor>>>>);

impl LatestLldpCollection {
    /// The last completed collection.
    /// Reading does not consume it.
    /// `None` only before the collector has finished its first attempt.
    pub fn latest(&self) -> Option<LldpCollectorResult<Vec<LldpNeighbor>>> {
        self.0.load_full().map(|collected| (*collected).clone())
    }

    /// Replace whatever the slot holds with the attempt that just finished.
    fn publish(&self, collected: LldpCollectorResult<Vec<LldpNeighbor>>) {
        self.0.store(Some(Arc::new(collected)));
    }
}

/// Start collecting LLDP neighbors with `collect` every `interval` on a task of
/// its own, and return the slot each completed attempt is published to.
///
/// The task stops once `cancel_token` is cancelled.
pub fn start_lldp_collector<Collect, CollectFuture>(
    collect: Collect,
    interval: Duration,
    join_set: &mut JoinSet<()>,
    cancel_token: CancellationToken,
) -> LatestLldpCollection
where
    Collect: FnMut() -> CollectFuture + Send + 'static,
    CollectFuture: Future<Output = LldpCollectorResult<Vec<LldpNeighbor>>> + Send + 'static,
{
    let latest = LatestLldpCollection::default();
    join_set.spawn(run_lldp_collector(
        collect,
        latest.clone(),
        interval,
        cancel_token,
    ));
    latest
}

/// Collect every `interval`, publishing each completed attempt to `latest`,
/// until `cancel_token` is cancelled.
///
/// A failure is published rather than swallowed (the failure is the report).
async fn run_lldp_collector<Collect, CollectFuture>(
    mut collect: Collect,
    latest: LatestLldpCollection,
    interval: Duration,
    cancel_token: CancellationToken,
) where
    Collect: FnMut() -> CollectFuture,
    CollectFuture: Future<Output = LldpCollectorResult<Vec<LldpNeighbor>>>,
{
    let mut ticker = tokio::time::interval(interval);
    // A collection that overran the interval must not be followed by a burst of catch-up attempts.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // The first tick completes immediately, so a freshly started service does
    // not wait a full interval for its first collection.
    while cancel_token
        .run_until_cancelled(ticker.tick())
        .await
        .is_some()
    {
        // Cancellable mid-collection as well: shutdown must not wait for an
        // in-flight `lldpcli` to finish.
        let Some(collected) = cancel_token.run_until_cancelled(collect()).await else {
            return;
        };
        if let Err(error) = &collected {
            tracing::warn!(%error, "Could not collect LLDP neighbors");
        }
        latest.publish(collected);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use ::rpc::machine_discovery::LldpSwitchData;

    use super::*;
    use crate::lldp_collector::LldpCollectorError;

    /// The loop really waits this out between attempts, but under
    /// `start_paused` the runtime jumps the clock instead of sleeping.
    const TEST_INTERVAL: Duration = Duration::from_secs(60);

    /// One neighbor on one port, the simplest snapshot the collector can yield.
    fn one_neighbor() -> Vec<LldpNeighbor> {
        vec![LldpNeighbor {
            local_mac: "aa:bb:cc:dd:ee:ff".to_string(),
            switch: LldpSwitchData {
                local_port: "p0".to_string(),
                remote_port_type: "ifname".to_string(),
                remote_port_value: "p0".to_string(),
                ..Default::default()
            },
        }]
    }

    fn failure(message: &str) -> LldpCollectorResult<Vec<LldpNeighbor>> {
        Err(LldpCollectorError::Lldp(message.to_string()))
    }

    /// Run the loop over a queue of outcomes, stopping it once they are all
    /// consumed. `start_paused` lets the runtime jump the interval, so the test
    /// does not actually wait.
    async fn run_over(
        outcomes: Vec<LldpCollectorResult<Vec<LldpNeighbor>>>,
    ) -> LatestLldpCollection {
        let outcomes = Mutex::new(VecDeque::from(outcomes));
        let latest = LatestLldpCollection::default();
        let cancel_token = CancellationToken::new();

        run_lldp_collector(
            || {
                let outcome = outcomes
                    .lock()
                    .expect("outcomes are never poisoned")
                    .pop_front();
                let cancel_token = cancel_token.clone();
                async move {
                    match outcome {
                        Some(outcome) => outcome,
                        // Out of scripted outcomes. Cancel the way a shutdown
                        // does, and never resolve, so the loop stops here
                        // without publishing anything the test did not script.
                        None => {
                            cancel_token.cancel();
                            std::future::pending().await
                        }
                    }
                }
            },
            latest.clone(),
            TEST_INTERVAL,
            cancel_token.clone(),
        )
        .await;

        latest
    }

    // The consumer must always see the newest attempt and nothing older.
    // An attempt it never read is overwritten.
    #[tokio::test(start_paused = true)]
    async fn latest_yields_only_the_newest_attempt() {
        let latest = run_over(vec![Ok(one_neighbor()), failure("lldpcli went away")]).await;
        assert!(
            matches!(latest.latest(), Some(Err(LldpCollectorError::Lldp(message))) if message == "lldpcli went away"),
            "a failure after a success must replace it"
        );

        let latest = run_over(vec![failure("lldpcli went away"), Ok(one_neighbor())]).await;
        assert!(
            matches!(latest.latest(), Some(Ok(neighbors)) if neighbors.len() == 1),
            "a recovery must replace the failure before it"
        );
    }

    // A host that sees no neighbors is an valid observation, not an absent one.
    #[tokio::test(start_paused = true)]
    async fn an_empty_collection_is_published_as_an_observation() {
        let latest = run_over(vec![Ok(one_neighbor()), Ok(vec![])]).await;

        assert!(
            matches!(latest.latest(), Some(Ok(neighbors)) if neighbors.is_empty()),
            "a host that stopped seeing neighbors must not read as an absent collection"
        );
    }

    // Reading is not consuming. A control loop iterating faster than the
    // collector must keep seeing the last collection instead of alternating
    // between it and nothing, which would make the loop's own cadence visible in
    // what it reports.
    #[tokio::test(start_paused = true)]
    async fn a_collection_stays_readable_until_a_newer_one_replaces_it() {
        let latest = run_over(vec![Ok(one_neighbor())]).await;

        assert!(latest.latest().is_some());
        assert!(
            matches!(latest.latest(), Some(Ok(neighbors)) if neighbors.len() == 1),
            "reading must leave the collection in place"
        );
    }

    // Nothing has been collected before the task's first attempt finishes, and
    // that is the one case the consumer must not treat as an observation.
    #[test]
    fn a_slot_with_no_completed_attempt_reads_as_absent() {
        assert!(LatestLldpCollection::default().latest().is_none());
    }

    // `start_paused` is load-bearing: the first `tick()` is already due, but on a
    // live clock it still round-trips through the timer driver, so one
    // `yield_now` is not enough for the task to reach its first publish.
    #[tokio::test(start_paused = true)]
    async fn the_started_task_publishes_and_then_stops_on_cancel() {
        let neighbors = one_neighbor();
        let mut join_set = JoinSet::new();
        let cancel_token = CancellationToken::new();

        let latest = start_lldp_collector(
            move || {
                let neighbors = neighbors.clone();
                async move { Ok(neighbors) }
            },
            TEST_INTERVAL,
            &mut join_set,
            cancel_token.clone(),
        );
        tokio::task::yield_now().await;

        assert!(
            matches!(latest.latest(), Some(Ok(neighbors)) if neighbors.len() == 1),
            "the caller must get a filled slot without awaiting the task"
        );

        cancel_token.cancel();
        // Hangs the test if cancellation does not actually stop the loop.
        join_set.join_all().await;
    }
}
