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
//! BMC self-reset simulation (machine-a-tron epic #3796, issue 4).
//!
//! Real BMCs go OFFLINE for a platform-specific window after a self-reset
//! (`Manager.Reset` via Redfish, `ipmitool mc reset cold` via IPMI): every
//! management request fails until the controller finishes rebooting, while
//! the SERVER's power state is unaffected. The nico machine-controller
//! issues such resets in five automated scenarios; without this simulation
//! its retry/recovery behavior during the outage window is never exercised.
//!
//! Model: a per-mock [`BmcAvailabilityState`] holding `Online` or
//! `Resetting { until }`. A reset trigger stamps `until = now + reset
//! duration` (from the machine's resolved [`LifecycleTimings::bmc_reset`]);
//! the request middleware answers `503 Service Unavailable` while the
//! window is open and lazily flips back to `Online` on the first request
//! after it expires — exactly how a real BMC "comes back": silently, by
//! answering again. No timers, no tasks.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
enum BmcAvailability {
    Online,
    Resetting { until: Instant },
}

/// Shared availability state for one mock BMC.
#[derive(Debug)]
pub struct BmcAvailabilityState {
    /// How long the BMC stays offline after a self-reset
    /// (resolved per machine from its platform timing profile).
    reset_duration: Duration,
    state: Mutex<BmcAvailability>,
}

impl BmcAvailabilityState {
    pub fn new(reset_duration: Duration) -> Self {
        Self {
            reset_duration,
            state: Mutex::new(BmcAvailability::Online),
        }
    }

    /// Lock the state, recovering from a poisoned mutex instead of
    /// panicking: the enum inside is a `Copy` value only ever replaced
    /// whole, so a poison (another thread panicked while holding the
    /// lock) leaves at worst a stale-but-valid state. That other panic
    /// is the bug to chase — log loudly here, but keep this mock
    /// answering rather than cascading the failure.
    fn lock_state(&self) -> MutexGuard<'_, BmcAvailability> {
        self.state.lock().unwrap_or_else(|poisoned| {
            tracing::error!(
                "BMC availability mutex poisoned by a panic elsewhere; \
                 continuing with the last recorded state"
            );
            poisoned.into_inner()
        })
    }

    /// Enter the offline window (a self-reset was triggered).
    /// Returns the window duration for logging.
    pub fn begin_reset(&self) -> Duration {
        let mut state = self.lock_state();
        *state = BmcAvailability::Resetting {
            until: Instant::now() + self.reset_duration,
        };
        self.reset_duration
    }

    /// True while the BMC is inside its offline window. Lazily recovers:
    /// the first call after the window expires flips the state back to
    /// `Online`.
    pub fn is_offline(&self) -> bool {
        let mut state = self.lock_state();
        match *state {
            BmcAvailability::Online => false,
            BmcAvailability::Resetting { until } => {
                if Instant::now() >= until {
                    *state = BmcAvailability::Online;
                    false
                } else {
                    true
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_online() {
        let a = BmcAvailabilityState::new(Duration::from_secs(60));
        assert!(!a.is_offline());
    }

    #[test]
    fn reset_opens_the_offline_window() {
        let a = BmcAvailabilityState::new(Duration::from_secs(60));
        assert_eq!(a.begin_reset(), Duration::from_secs(60));
        assert!(a.is_offline());
        // still offline on repeated checks inside the window
        assert!(a.is_offline());
    }

    #[test]
    fn recovers_lazily_after_the_window() {
        let a = BmcAvailabilityState::new(Duration::from_millis(10));
        a.begin_reset();
        assert!(a.is_offline());
        std::thread::sleep(Duration::from_millis(20));
        // first check after expiry flips back to Online
        assert!(!a.is_offline());
        assert!(!a.is_offline());
    }

    #[test]
    fn survives_a_poisoned_mutex() {
        use std::sync::Arc;

        let a = Arc::new(BmcAvailabilityState::new(Duration::from_secs(60)));
        let poisoner = Arc::clone(&a);
        // poison the mutex: panic in another thread while holding the lock
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.state.lock().unwrap();
            panic!("poison the availability mutex");
        })
        .join();
        assert!(a.state.lock().is_err(), "mutex should be poisoned");

        // no panic, state still fully usable
        assert!(!a.is_offline());
        a.begin_reset();
        assert!(a.is_offline());
    }

    #[test]
    fn second_reset_reopens_the_window() {
        let a = BmcAvailabilityState::new(Duration::from_millis(10));
        a.begin_reset();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!a.is_offline());
        a.begin_reset();
        assert!(a.is_offline());
    }
}
