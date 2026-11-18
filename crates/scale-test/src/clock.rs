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

//! One clock for wall time and monotonic time.
//!
//! Production wall time is the system clock, the same clock the probe reads,
//! so a request's `end` is never ahead of the probe over a long run. A
//! test clock made with [`TokioClock::starting_at`] is a fixed origin plus the
//! tokio monotonic time elapsed since it, so
//! `#[tokio::test(start_paused = true)]` drives both.

use chrono::{DateTime, TimeDelta, Utc};
use tokio::time::Instant;

pub trait Clock: Send + Sync {
    /// Wall-clock time, as recorded in the journal and the result.
    fn now(&self) -> DateTime<Utc>;
    /// Monotonic time, for timeouts and the lock's renewal age.
    fn instant(&self) -> Instant;
}

/// The production clock, and with [`starting_at`](Self::starting_at) a test
/// clock on paused tokio time.
#[derive(Debug, Clone)]
pub struct TokioClock {
    /// None follows the system clock. Some is a simulated wall time.
    wall_origin: Option<DateTime<Utc>>,
    origin: Instant,
}

impl TokioClock {
    /// Wall time is the system clock.
    pub fn new() -> Self {
        Self {
            wall_origin: None,
            origin: Instant::now(),
        }
    }

    /// A clock whose wall time reads `wall` right now and then moves with
    /// tokio time, paused or not.
    pub fn starting_at(wall: DateTime<Utc>) -> Self {
        Self {
            wall_origin: Some(wall),
            origin: Instant::now(),
        }
    }
}

impl Default for TokioClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TokioClock {
    fn now(&self) -> DateTime<Utc> {
        let Some(wall_origin) = self.wall_origin else {
            return Utc::now();
        };
        let elapsed = Instant::now().duration_since(self.origin);
        wall_origin
            .checked_add_signed(TimeDelta::from_std(elapsed).unwrap_or(TimeDelta::MAX))
            .unwrap_or(DateTime::<Utc>::MAX_UTC)
    }

    fn instant(&self) -> Instant {
        Instant::now()
    }
}

/// A wall clock that moves only when told to. For tests that compare replies
/// made at different real times.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct FixedClock {
    now: std::sync::Mutex<DateTime<Utc>>,
}

#[cfg(any(test, feature = "test-support"))]
impl FixedClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: std::sync::Mutex::new(now),
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.lock().unwrap_or_else(|e| e.into_inner()) = now;
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn instant(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn wall_time_follows_paused_tokio_time() {
        let origin = DateTime::parse_from_rfc3339("2026-10-23T10:00:00Z")
            .unwrap()
            .to_utc();
        let clock = TokioClock::starting_at(origin);
        tokio::time::sleep(Duration::from_secs(90)).await;
        assert_eq!(clock.now(), origin + TimeDelta::seconds(90));
    }

    #[tokio::test(start_paused = true)]
    async fn production_wall_time_is_the_system_clock() {
        let clock = TokioClock::new();
        // Tokio time ran an hour ahead of the system clock.
        tokio::time::advance(Duration::from_secs(3600)).await;
        let off = (clock.now() - Utc::now()).num_seconds().abs();
        assert!(off < 5, "{off} s from the system clock");
    }
}
