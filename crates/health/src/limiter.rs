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

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rand::RngExt;

const TOKEN_SCALE: u64 = 1_000_000;

/// Token bucket rate limiter
#[derive(Debug, Clone)]
pub struct BucketLimiter {
    /// Current number of available tokens (tokens * `TOKEN_SCALE`)
    tokens: Arc<AtomicU64>,
    /// Maximum tokens available at once (burst capacity, not concurrency limit)
    capacity: u64,
    /// How often to add tokens back
    replenish_interval: Duration,
    /// Last time tokens were replenished (nanoseconds since reference epoch)
    last_replenish: Arc<AtomicU64>,
    /// Reference epoch for time tracking
    epoch: Instant,
    /// Maximum jitter duration to add to exploration intervals
    max_jitter: Duration,
}

pub trait RateLimiter: Send + Sync {
    fn acquire<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

impl RateLimiter for BucketLimiter {
    fn acquire<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            BucketLimiter::acquire(self).await;
        })
    }
}

#[derive(Debug, Clone)]
pub struct NoopLimiter;

impl RateLimiter for NoopLimiter {
    fn acquire<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}

impl BucketLimiter {
    /// Create a new token bucket limiter
    ///
    /// # Arguments
    ///
    /// * `capacity` - Burst size: max tokens accumulated (e.g., 32)
    /// * `replenish_interval` - How often to add tokens (milliseconds)
    /// * `max_jitter` - Maximum random delay to spread requests
    pub fn new(capacity: usize, replenish_interval: Duration, max_jitter: Duration) -> Self {
        let epoch = Instant::now();
        let capacity = (capacity as u64) * TOKEN_SCALE;

        Self {
            tokens: Arc::new(AtomicU64::new(capacity)),
            capacity,
            replenish_interval,
            last_replenish: Arc::new(AtomicU64::new(0)),
            epoch,
            max_jitter,
        }
    }

    /// Acquire a token for an exploration
    /// Waits if no tokens available, then returns when one becomes available
    pub async fn acquire(&self) {
        loop {
            self.replenish_tokens();
            // Prevent running `replenish_tokens` in contention scenario
            loop {
                let current = self.tokens.load(Ordering::Relaxed);

                if current >= TOKEN_SCALE {
                    match self.tokens.compare_exchange(
                        current,
                        current - TOKEN_SCALE,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            if !self.max_jitter.is_zero() {
                                tokio::time::sleep(self.jitter()).await;
                            }
                            return;
                        }
                        Err(_) => {
                            continue;
                        }
                    }
                } else {
                    break;
                }
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn replenish_tokens(&self) {
        let now = Instant::now();
        let duration = now.duration_since(self.epoch);
        let now_nanos = duration.as_nanos().min(u64::MAX as u128) as u64;

        let last_nanos = self.last_replenish.load(Ordering::Relaxed);
        let elapsed = Duration::from_nanos(now_nanos.saturating_sub(last_nanos));

        if elapsed >= self.replenish_interval {
            let intervals_passed = elapsed.as_secs_f64() / self.replenish_interval.as_secs_f64();
            let tokens_to_add_fixed = ((TOKEN_SCALE as f64) * intervals_passed) as u64;

            if self
                .last_replenish
                .compare_exchange(last_nanos, now_nanos, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                self.tokens
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                        Some((current + tokens_to_add_fixed).min(self.capacity))
                    })
                    .ok();
            }
        }
    }

    fn jitter(&self) -> Duration {
        let max_jitter_ms = self.max_jitter.as_millis() as u64;

        if max_jitter_ms == 0 {
            return Duration::ZERO;
        }

        let mut rng = rand::rng();
        let jitter_ms = rng.random_range(0..max_jitter_ms);
        Duration::from_millis(jitter_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jitter_range() {
        let limiter = BucketLimiter::new(10, Duration::from_millis(100), Duration::from_secs(1));

        for _ in 0..100 {
            let jitter = limiter.jitter();
            assert!(jitter <= Duration::from_secs(1));
            assert!(jitter >= Duration::from_millis(0));
        }
    }
}
