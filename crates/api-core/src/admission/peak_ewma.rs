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

//! Conservative execution-duration estimator for admission backpressure.
//!
//! An exponentially weighted moving average (EWMA) is a rolling average that
//! favors recent observations without retaining every historical sample. This
//! peak-biased variant incorporates a slow sample immediately and lets the
//! estimate decay as the service demonstrates faster execution.
//!
//! Queue pressure is measured in concurrency batches, so admission needs an
//! estimate of how long an admitted batch will take. A simple average can turn
//! optimistic immediately after a slow handler and admit work that is unlikely
//! to begin before its pending deadline. This deliberately favors protecting
//! the queue from overload over admitting work based on a stale, low-latency
//! estimate.
//!
//! Each client and the global scheduler maintain an independent estimator.
//! Per-client estimates drive predictive queue-delay rejection because each
//! client has a FIFO queue. Both estimates drive bounded, jittered retry advice;
//! they do not impose execution limits themselves.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const EWMA_NEW_SAMPLE_WEIGHT: u64 = 1;
const EWMA_EXISTING_WEIGHT: u64 = 4;

pub(super) struct PeakEwma {
    duration_nanos: AtomicU64,
}

impl PeakEwma {
    pub(super) fn new(initial: Duration) -> Self {
        Self {
            duration_nanos: AtomicU64::new(duration_to_nanos(initial)),
        }
    }

    pub(super) fn observe(&self, sample: Duration) {
        let sample = duration_to_nanos(sample);
        let mut current = self.duration_nanos.load(Ordering::Relaxed);
        loop {
            let next = if sample >= current {
                sample
            } else {
                current
                    .saturating_mul(EWMA_EXISTING_WEIGHT)
                    .saturating_add(sample.saturating_mul(EWMA_NEW_SAMPLE_WEIGHT))
                    / (EWMA_EXISTING_WEIGHT + EWMA_NEW_SAMPLE_WEIGHT)
            };
            match self.duration_nanos.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    pub(super) fn duration(&self) -> Duration {
        Duration::from_nanos(self.duration_nanos.load(Ordering::Relaxed))
    }
}

fn duration_to_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

pub(super) fn initial_duration(timeout: Duration, concurrency: usize, pending: usize) -> Duration {
    timeout.saturating_mul(u32::try_from(concurrency).unwrap_or(u32::MAX))
        / u32::try_from(pending).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rises_immediately_and_decays_gradually() {
        let ewma = PeakEwma::new(Duration::from_millis(100));
        ewma.observe(Duration::from_millis(500));
        assert_eq!(ewma.duration(), Duration::from_millis(500));
        ewma.observe(Duration::ZERO);
        assert_eq!(ewma.duration(), Duration::from_millis(400));
    }
}
