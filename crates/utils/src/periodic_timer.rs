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

use std::time::{Duration, Instant};

/// PeriodicTimer is a timer for periodic tasks, which tries to maintain
/// a consistent cycle time in between iterations.
///
/// This is achieved by a simple subtraction of the elapsed iteration time
/// from the run interval, so that the total cycle time stays as close to
/// interval as possible.
///
/// If an iteration takes longer than the interval, the next iteration
/// starts immediately (sleep time saturates to zero).
///
/// Example usage looks like:
///
/// ```ignore
/// let timer = PeriodicTimer::new(Duration::from_secs(30));
/// loop {
///     let tick = timer.tick();
///     ..do your work here.
///     tick.sleep().await;
/// }
/// ```
#[derive(Debug, Clone)]
pub struct PeriodicTimer {
    interval: Duration,
}

/// Tick tracks a single tick of a PeriodicTimer, and is created
/// by PeriodicTimer::tick. It records when the tick started, so that
/// the Tick::sleep routine can subtract the elapsed time from the
/// configured interval.
#[derive(Debug)]
pub struct Tick {
    interval: Duration,
    started_at: Instant,
}

impl PeriodicTimer {
    pub fn new(interval: Duration) -> Self {
        Self { interval }
    }

    /// tick() begins a new tick. This is intended to be called
    /// before running the iteration work, and is used to track
    /// the time of the iteration.
    pub fn tick(&self) -> Tick {
        Tick {
            interval: self.interval,
            started_at: Instant::now(),
        }
    }
}

impl Tick {
    /// remaining returns how long to sleep so that the total cycle time
    /// (iteration + sleep) is as close to the configured run interval as
    /// possible.
    pub fn remaining(&self) -> Duration {
        self.interval.saturating_sub(self.started_at.elapsed())
    }

    /// sleep sleeps for the remaining time in this Tick.
    pub async fn sleep(self) {
        tokio::time::sleep(self.remaining()).await;
    }

    /// set_interval override the interval for this Tick only.
    /// Used in adaptive situations (e.g. ib_fabric_monitor) which may
    /// want a shorter interval under certain conditions (e.g. when changes
    /// were detected) while still subtracting elapsed time.
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_subtracts_elapsed_time() {
        let tick = Tick {
            interval: Duration::from_secs(30),
            started_at: Instant::now() - Duration::from_secs(10),
        };
        let remaining = tick.remaining();
        // Should be roughly 20s.
        assert!(remaining <= Duration::from_secs(21));
        assert!(remaining >= Duration::from_secs(19));
    }

    #[test]
    fn remaining_saturates_to_zero_when_iteration_exceeds_interval() {
        let tick = Tick {
            interval: Duration::from_secs(30),
            started_at: Instant::now() - Duration::from_secs(45),
        };
        assert_eq!(tick.remaining(), Duration::ZERO);
    }

    #[test]
    fn remaining_is_close_to_interval_when_iteration_is_fast() {
        let tick = Tick {
            interval: Duration::from_secs(30),
            started_at: Instant::now(),
        };
        let remaining = tick.remaining();
        assert!(remaining >= Duration::from_millis(29_900));
        assert!(remaining <= Duration::from_secs(30));
    }

    #[test]
    fn set_interval_overrides_for_single_tick() {
        let timer = PeriodicTimer::new(Duration::from_secs(30));
        let mut tick = timer.tick();
        tick.set_interval(Duration::from_secs(1));
        let remaining = tick.remaining();
        assert!(remaining <= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn sleep_completes_quickly_when_iteration_exceeds_interval() {
        let tick = Tick {
            interval: Duration::from_millis(10),
            started_at: Instant::now() - Duration::from_millis(100),
        };
        let before = Instant::now();
        tick.sleep().await;
        // Should return ~immediately.
        assert!(before.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn sleep_waits_for_remaining_time() {
        let timer = PeriodicTimer::new(Duration::from_millis(100));
        let tick = timer.tick();
        let before = Instant::now();
        tick.sleep().await;
        let elapsed = before.elapsed();
        assert!(elapsed >= Duration::from_millis(80));
        assert!(elapsed < Duration::from_millis(200));
    }
}
