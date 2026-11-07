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

use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// LogLimiter is a concurrent datastructure for reducing the amount of logs that
/// are emitted for a certain objects and events.
///
/// LogLimiter tracks events for objects of type `K` independently.
///
/// It's `should_log` can be called to determine whether a log entry for the
/// object should be emitted or should be suppressed.
///
/// The suppression period is configurable
pub(crate) struct LogLimiter<K> {
    objects: DashMap<K, LimitData>,
    suppress_period: Duration,
    clean_unused_keys_period: Duration,
    start: Instant,
    next_cleanup: AtomicU64,
}

impl<K: Hash + PartialEq + Eq + Clone> Default for LogLimiter<K> {
    fn default() -> Self {
        LogLimiter::new(
            std::time::Duration::from_secs(5 * 60),
            std::time::Duration::from_secs(60 * 60),
        )
    }
}

impl<K: Hash + PartialEq + Eq + Clone> LogLimiter<K> {
    pub(crate) fn new(suppress_period: Duration, clean_unused_keys_period: Duration) -> Self {
        assert!(
            clean_unused_keys_period >= Duration::from_secs(1),
            "Minimum clean_unused_keys_period is 1s"
        );
        let start = Instant::now();
        Self {
            objects: Default::default(),
            suppress_period,
            clean_unused_keys_period,
            start,
            next_cleanup: AtomicU64::new(clean_unused_keys_period.as_secs()),
        }
    }

    /// Returns whether a log message which can be summarized as `log_summary`
    /// should be emitted (`true`) or suppressed (`false`).
    pub(crate) fn should_log(&self, key: &K, log_summary: &str) -> bool {
        let now = Instant::now();

        // The main path will only insert and look up the passed `key` inside the map.
        // Keys that have been submitted by different callers would however not be
        // monitored and cleaned.
        // To avoid them from taking up infinite memory, the next section of code
        // will scan the entire key set periodically and clean up any entries
        // that are not currently used for suppressing logs
        let elapsed_s = now.saturating_duration_since(self.start).as_secs();
        let old_next_cleanup = self.next_cleanup.load(Ordering::SeqCst);
        if elapsed_s >= old_next_cleanup {
            // We might need to clean up stale keys
            let next_cleanup = (now + self.clean_unused_keys_period)
                .saturating_duration_since(self.start)
                .as_secs();

            match self.next_cleanup.compare_exchange(
                old_next_cleanup,
                next_cleanup,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    // Perform cleanup
                    self.objects.retain(|_key, limit_data| {
                        now < limit_data.last_logged + self.suppress_period
                    });
                }
                Err(_) => {
                    // We lost the race
                    // Another thread will perform the cleanup
                }
            }
        }

        match self.objects.get_mut(key) {
            Some(mut limit_data) => {
                if log_summary != limit_data.log_summary
                    || now >= limit_data.last_logged + self.suppress_period
                {
                    limit_data.last_logged = now;
                    limit_data.log_summary = log_summary.to_string();
                    true
                } else {
                    false
                }
            }
            None => {
                // Key has never been seen. Allow to log
                // If a concurrent call happens at exactly the same time, both
                // logs might be allowed. For this use-case this is however not
                // a problem.
                let data = LimitData {
                    last_logged: now,
                    log_summary: log_summary.to_string(),
                };
                self.objects.insert(key.clone(), data);
                true
            }
        }
    }
}

struct LimitData {
    last_logged: Instant,
    log_summary: String,
}

impl Default for LimitData {
    fn default() -> Self {
        Self {
            last_logged: Instant::now(),
            log_summary: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPPRESS_PERIOD: Duration = Duration::from_millis(200);
    const LONG_CLEANUP_PERIOD: Duration = Duration::MAX;
    /// Test period to actually suppport cleanup
    /// This needs to be at least 1 second to support the mechanism
    const SHORT_CLEANUP_PERIOD: Duration = Duration::from_millis(1000);

    #[test]
    fn limit_idential_logs() {
        let limiter = LogLimiter::<String>::new(SUPPRESS_PERIOD, LONG_CLEANUP_PERIOD);
        assert!(limiter.should_log(&"A".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"A".to_string(), "Warning A"));
        assert!(limiter.should_log(&"B".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"B".to_string(), "Warning A"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!limiter.should_log(&"A".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"B".to_string(), "Warning A"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(limiter.should_log(&"A".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"A".to_string(), "Warning A"));
        assert!(limiter.should_log(&"B".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"B".to_string(), "Warning A"));
    }

    #[test]
    fn log_latest_log_if_changed() {
        let limiter = LogLimiter::<String>::new(SUPPRESS_PERIOD, LONG_CLEANUP_PERIOD);
        assert!(limiter.should_log(&"A".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"A".to_string(), "Warning A"));
        // The latest log message changed. Therefore log the latter
        assert!(limiter.should_log(&"A".to_string(), "Warning B"));
        assert!(!limiter.should_log(&"A".to_string(), "Warning B"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!limiter.should_log(&"A".to_string(), "Warning B"));
        std::thread::sleep(Duration::from_millis(100));
        assert!(limiter.should_log(&"A".to_string(), "Warning B"));
        assert!(limiter.should_log(&"A".to_string(), "Warning A"));
    }

    #[test]
    fn cleanup_unreferenced_keys() {
        let limiter = LogLimiter::<String>::new(SUPPRESS_PERIOD, SHORT_CLEANUP_PERIOD);
        assert!(limiter.should_log(&"A".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"A".to_string(), "Warning A"));
        assert!(limiter.should_log(&"B".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"B".to_string(), "Warning A"));
        assert!(limiter.should_log(&"C".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"C".to_string(), "Warning A"));
        std::thread::sleep(Duration::from_millis(900));
        // All 3 limits are now expired. Now renew one limit to still apply till 1.1s
        assert!(limiter.should_log(&"C".to_string(), "Warning A"));
        assert!(!limiter.should_log(&"C".to_string(), "Warning A"));
        // Now try to log something else. After cleanup, entry C should still be in the map.
        // Everything else should be gone.
        std::thread::sleep(Duration::from_millis(100));
        assert!(limiter.should_log(&"D".to_string(), "Warning A"));
        assert_eq!(limiter.objects.len(), 2);
        assert!(!limiter.should_log(&"C".to_string(), "Warning A"));
    }
}
