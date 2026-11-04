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

//! The share of a BMC's slots its classes without a latency target may hold:
//! cut while a class with a target misses it there, raised step by step
//! while the targets are met.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::config::{SloConfig, SloTarget};

/// What a class with a latency target made of its last window of requests
/// at a BMC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Met,
    Missed,
}

/// The slots one BMC's classes without a latency target may hold.
pub(super) struct Share {
    config: SloConfig,
    /// `max_in_flight_per_bmc`.
    ceiling: usize,
    /// The current limit, also read by the BMC's queues to size their room.
    limit: Arc<AtomicUsize>,
    /// Per class, in the order of the BMC's classes: the latencies of a
    /// class with a target since its last verdict.
    windows: Vec<Option<Window>>,
    measured_at: Option<Instant>,
}

struct Window {
    target: SloTarget,
    latencies: Vec<Duration>,
}

impl Share {
    /// The whole of `ceiling`, for classes with `targets`, in order.
    pub(super) fn new(
        config: SloConfig,
        ceiling: usize,
        targets: impl IntoIterator<Item = Option<SloTarget>>,
    ) -> Self {
        Self {
            config,
            ceiling,
            limit: Arc::new(AtomicUsize::new(ceiling)),
            windows: targets
                .into_iter()
                .map(|target| {
                    target.map(|target| Window {
                        target,
                        latencies: Vec::new(),
                    })
                })
                .collect(),
            measured_at: None,
        }
    }

    /// The current limit, as it changes.
    pub(super) fn published(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.limit)
    }

    /// Whether class `class` has a latency target.
    pub(super) fn has_target(&self, class: usize) -> bool {
        self.windows.get(class).is_some_and(Option::is_some)
    }

    /// Records that a request of class `class` was answered after `latency`,
    /// at `now`, and the verdict once the class's window is full. A class
    /// without a target records nothing.
    pub(super) fn record(
        &mut self,
        class: usize,
        latency: Duration,
        now: Instant,
    ) -> Option<Verdict> {
        let window = self.windows.get_mut(class)?.as_mut()?;
        self.measured_at = Some(now);
        window.latencies.push(latency);
        let window_size = usize::try_from(self.config.window.get()).unwrap_or(usize::MAX);
        if window.latencies.len() < window_size {
            return None;
        }
        let verdict = if percentile(&mut window.latencies, window.target.percentile)
            <= window.target.latency
        {
            Verdict::Met
        } else {
            Verdict::Missed
        };
        window.latencies.clear();
        let floor = usize::try_from(self.config.min_in_flight.get()).unwrap_or(usize::MAX);
        let limit = self.limit.load(Ordering::Relaxed);
        let limit = match verdict {
            Verdict::Met => limit
                .saturating_add(usize::try_from(self.config.increase.get()).unwrap_or(usize::MAX))
                .min(self.ceiling),
            Verdict::Missed => scaled(limit, self.config.decrease).max(floor),
        };
        self.limit.store(limit, Ordering::Relaxed);
        Some(verdict)
    }

    /// The slots the classes without a target may hold at `now`, while
    /// `targets_in_flight` requests of classes with a target are at the BMC:
    /// the whole of `ceiling` again once none has been answered for
    /// `reset_after` and none is waiting on the BMC, whose stall would
    /// otherwise look like quiet. A reset also drops partial windows.
    pub(super) fn limit(&mut self, now: Instant, targets_in_flight: usize) -> usize {
        if targets_in_flight == 0
            && self
                .measured_at
                .is_some_and(|at| now.saturating_duration_since(at) >= self.config.reset_after)
        {
            self.limit.store(self.ceiling, Ordering::Relaxed);
            self.measured_at = None;
            for window in self.windows.iter_mut().flatten() {
                window.latencies.clear();
            }
        }
        self.limit.load(Ordering::Relaxed)
    }
}

/// The nearest-rank `percentile` of `latencies`, which must not be empty.
fn percentile(latencies: &mut [Duration], percentile: f64) -> Duration {
    latencies.sort_unstable();
    let count = latencies.len();
    // Windows hold at most MAX_SLO_WINDOW latencies, so the conversions are
    // exact. The product of a decimal percentile and a count can land a hair
    // above a whole rank, as 0.07 * 100 does, which would round the rank up.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let rank = (percentile * count as f64 - 1e-9).ceil() as usize;
    latencies[rank.clamp(1, count) - 1]
}

/// `limit` scaled by `factor`, below 1, and rounded down, so a cut always
/// lowers a limit above the floor.
fn scaled(limit: usize, factor: f32) -> usize {
    // Limits are at most a u32's.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let scaled = (limit as f64 * f64::from(factor)).floor() as usize;
    scaled.min(limit.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::{Duration, Instant};

    use carbide_test_support::value_scenarios;

    use super::{Share, percentile};
    use crate::config::{SloConfig, SloTarget};

    #[derive(Clone, Copy)]
    enum Step {
        /// A request of class 0, which has a 100ms target, answered after
        /// this many milliseconds.
        Target(u64),
        /// A request of class 1, which has none, answered after this many
        /// milliseconds.
        Other(u64),
        Wait(Duration),
        /// Time passing while a request of class 0 waits on the BMC.
        WaitStalled(Duration),
    }
    use Step::{Other, Target, Wait, WaitStalled};

    /// The share of a BMC with a limit of 8 after each step, with windows of
    /// two requests, a floor of 2, and a reset after 10 seconds.
    fn shares(steps: &[Step]) -> Vec<usize> {
        let config = SloConfig {
            window: NonZeroU32::new(2).unwrap(),
            min_in_flight: NonZeroU32::new(2).unwrap(),
            reset_after: Duration::from_secs(10),
            ..SloConfig::default()
        };
        let target = SloTarget {
            latency: Duration::from_millis(100),
            percentile: 0.5,
        };
        let mut share = Share::new(config, 8, [Some(target), None]);
        let mut now = Instant::now();
        steps
            .iter()
            .map(|step| {
                let mut targets_in_flight = 0;
                match *step {
                    Target(ms) => {
                        share.record(0, Duration::from_millis(ms), now);
                    }
                    Other(ms) => {
                        share.record(1, Duration::from_millis(ms), now);
                    }
                    Wait(time) => now += time,
                    WaitStalled(time) => {
                        now += time;
                        targets_in_flight = 1;
                    }
                }
                share.limit(now, targets_in_flight)
            })
            .collect()
    }

    /// Each full window of a class with a target halves the share when it
    /// misses, down to the floor, and raises it by one when it meets the
    /// target, up to the limit. Half a window, and a class without a target,
    /// decide nothing; a target left unanswered gives back the whole limit,
    /// unless a request of its class is still waiting on the BMC.
    #[test]
    fn the_share_follows_the_targets() {
        value_scenarios!(
            run = |steps: &[Step]| shares(steps);
            "cut" {
                &[Target(200), Target(200), Target(200), Target(200), Target(200), Target(200)][..]
                    => vec![8, 4, 4, 2, 2, 2],
            }

            "raised" {
                &[Target(200), Target(200), Target(50), Target(50), Target(50), Target(50)][..]
                    => vec![8, 4, 4, 5, 5, 6],
                &[Target(50), Target(50)][..] => vec![8, 8],
            }

            "undecided" {
                &[Target(200)][..] => vec![8],
                &[Other(200), Other(200)][..] => vec![8, 8],
            }

            "reset" {
                &[Target(200), Target(200), Wait(Duration::from_secs(9))][..] => vec![8, 4, 4],
                &[Target(200), Target(200), Wait(Duration::from_secs(10))][..] => vec![8, 4, 8],
                &[Target(200), Target(200), WaitStalled(Duration::from_secs(10))][..] => vec![8, 4, 4],
            }
        );
    }

    /// A window's verdict weighs its nearest-rank percentile.
    #[test]
    fn percentile_takes_the_nearest_rank() {
        value_scenarios!(
            run = |(p, count): (f64, u64)| {
                let mut latencies: Vec<Duration> =
                    (1..=count).rev().map(Duration::from_millis).collect();
                percentile(&mut latencies, p).as_millis()
            };
            "ranks" {
                (0.05, 10) => 1,
                (0.9, 10) => 9,
                (0.91, 10) => 10,
                (0.99, 100) => 99,
                (0.07, 100) => 7,
            }
        );
    }
}
