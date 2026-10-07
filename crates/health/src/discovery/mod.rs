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

mod cleanup;
mod context;
mod identity;
mod iteration;
mod reachability;
mod spawn;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use carbide_utils::managed_loop::{self, LoopManager};
pub use context::DiscoveryLoopContext;
pub(crate) use context::load_nmxc_schema_override;
pub use iteration::run_discovery_iteration;
use tokio::sync::Notify;

use crate::HealthError;
use crate::collectors::{BackoffConfig, ExponentialBackoff};

#[derive(Debug, Clone)]
pub struct DiscoveryIterationStats {
    pub discovered_endpoints: usize,
    pub sharded_endpoints: usize,
    pub active_monitors: usize,
}

/// One pass of the endpoint discovery loop: the unit of work
/// [`run_discovery_loop`] repeats, counts, and retries.
#[async_trait]
pub(crate) trait DiscoveryIteration: Send {
    async fn run_once(&mut self) -> Result<(), HealthError>;
}

/// Drives `iteration` forever on the discovery cadence. Every pass is
/// counted by the shared managed-loop event; a failed pass writes that
/// event's WARN line and schedules a capped exponential backoff instead of
/// ending the loop, and the next success resets the backoff and returns the
/// cadence to `interval`. Collector mode transitions wake the loop before the
/// scheduled delay expires.
pub(crate) async fn run_discovery_loop(
    interval: Duration,
    backoff_config: BackoffConfig,
    collector_transition_notify: Arc<Notify>,
    mut iteration: impl DiscoveryIteration,
) {
    let mut backoff = ExponentialBackoff::new(&backoff_config);
    loop {
        let result = iteration.run_once().await;
        managed_loop::record_iteration(LoopManager::HealthDiscovery, &result);
        let delay = match result {
            Ok(()) => {
                backoff.reset();
                interval
            }
            Err(_) => backoff.next_delay(),
        };

        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = collector_transition_notify.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::MetricsCapture;

    use super::*;

    /// A scripted pass: succeeds or fails per the script and reports when
    /// each pass started.
    struct ScriptedIteration {
        script: Vec<bool>,
        calls: usize,
        times: tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>,
    }

    #[async_trait]
    impl DiscoveryIteration for ScriptedIteration {
        async fn run_once(&mut self) -> Result<(), HealthError> {
            let ok = self.script[self.calls.min(self.script.len() - 1)];
            self.calls += 1;
            self.times
                .send(tokio::time::Instant::now())
                .expect("test holds the receiver");
            if ok {
                Ok(())
            } else {
                Err(HealthError::GenericError("iteration failed".to_string()))
            }
        }
    }

    /// A failing iteration keeps the loop alive on a doubling retry cadence
    /// (instead of the historical first-error exit), and the next success
    /// resets the backoff so a later failure retries from the initial delay
    /// again. Iteration start times come out exact under the paused clock;
    /// only the backoff's random jitter (less than one doubling) widens the
    /// asserted windows.
    #[tokio::test(start_paused = true)]
    async fn discovery_loop_retries_failures_and_resets_backoff() {
        let metrics = MetricsCapture::start();
        let (times_tx, mut times_rx) = tokio::sync::mpsc::unbounded_channel();
        let collector_transition_notify = Arc::new(Notify::new());

        let loop_task = tokio::spawn(run_discovery_loop(
            Duration::from_secs(300),
            BackoffConfig {
                initial: Duration::from_secs(1),
                max: Duration::from_secs(8),
            },
            collector_transition_notify,
            ScriptedIteration {
                // Two failures (retry, then doubled retry), a success
                // (normal cadence), then a failure that must retry from the
                // initial delay again.
                script: vec![false, false, true, false, true],
                calls: 0,
                times: times_tx,
            },
        ));

        let mut times = Vec::new();
        for _ in 0..5 {
            times.push(times_rx.recv().await.expect("loop keeps iterating"));
        }
        loop_task.abort();

        let gaps: Vec<Duration> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let within = |gap: Duration, low: u64, high: u64| {
            gap >= Duration::from_secs(low) && gap < Duration::from_secs(high)
        };

        // The first failure retries within [initial, 2 * initial): the loop
        // survived the error.
        assert!(within(gaps[0], 1, 2), "first retry: {gaps:?}");
        // A second consecutive failure doubles the delay.
        assert!(within(gaps[1], 2, 4), "doubled retry: {gaps:?}");
        // A success sleeps the full discovery interval, exactly.
        assert_eq!(
            gaps[2],
            Duration::from_secs(300),
            "normal cadence: {gaps:?}"
        );
        // The success also reset the backoff: the next failure retries from
        // the initial delay again, not from the doubled one.
        assert!(within(gaps[3], 1, 2), "retry after reset: {gaps:?}");

        // Every pass counted under its outcome: three failures, two successes.
        assert_eq!(
            metrics.counter_delta(
                "carbide_managed_loop_iterations_total",
                &[("manager", "health_discovery"), ("outcome", "error")],
            ),
            3.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_managed_loop_iterations_total",
                &[("manager", "health_discovery"), ("outcome", "ok")],
            ),
            2.0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn collector_transition_wakes_discovery_before_the_normal_interval() {
        let _metrics = MetricsCapture::start();
        let (times_tx, mut times_rx) = tokio::sync::mpsc::unbounded_channel();
        let collector_transition_notify = Arc::new(Notify::new());

        let loop_task = tokio::spawn(run_discovery_loop(
            Duration::from_secs(300),
            BackoffConfig::default(),
            collector_transition_notify.clone(),
            ScriptedIteration {
                script: vec![true],
                calls: 0,
                times: times_tx,
            },
        ));

        let first_iteration = times_rx.recv().await.expect("first iteration should run");
        collector_transition_notify.notify_one();

        let second_iteration = times_rx
            .recv()
            .await
            .expect("transition should wake the next iteration");

        loop_task.abort();

        assert_eq!(second_iteration, first_iteration);
    }
}
