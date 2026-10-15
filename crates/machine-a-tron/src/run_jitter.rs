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

//! Random offsets that keep a simulated fleet from calling nico-api in lockstep.

use std::time::Duration;

use rand::RngExt;

/// Delay before a simulator's first run, uniform in `[0, interval)`.
pub(crate) fn first_run_offset(interval: Duration) -> Duration {
    let interval_nanos = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);
    if interval_nanos == 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(rand::rng().random_range(0..interval_nanos))
}

/// Vary an interval by up to 10 percent either way.
pub(crate) fn jitter_interval(interval: Duration) -> Duration {
    let max_jitter_nanos = i64::try_from((interval / 10).as_nanos()).unwrap_or(i64::MAX);
    let jitter_nanos = rand::rng().random_range(-max_jitter_nanos..=max_jitter_nanos);
    let jitter = Duration::from_nanos(jitter_nanos.unsigned_abs());
    if jitter_nanos < 0 {
        interval.saturating_sub(jitter)
    } else {
        interval.saturating_add(jitter)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    const SAMPLES: usize = 1_000;

    #[derive(Debug, PartialEq)]
    struct Spread {
        within_bounds: bool,
        varies: bool,
    }

    fn spread(sample: impl Fn() -> Duration, in_bounds: impl Fn(Duration) -> bool) -> Spread {
        let samples: Vec<Duration> = (0..SAMPLES).map(|_| sample()).collect();
        Spread {
            within_bounds: samples.iter().all(|sample| in_bounds(*sample)),
            varies: samples.iter().min() != samples.iter().max(),
        }
    }

    fn first_run_offsets(interval: Duration) -> Spread {
        spread(
            || first_run_offset(interval),
            |offset| offset < interval || offset.is_zero(),
        )
    }

    fn jittered_intervals(interval: Duration) -> Spread {
        let lower = interval - interval / 10;
        let upper = interval.saturating_add(interval / 10);
        spread(
            || jitter_interval(interval),
            |jittered| (lower..=upper).contains(&jittered),
        )
    }

    #[test]
    fn first_run_offset_spreads_over_the_interval() {
        value_scenarios!(first_run_offsets:
            "zero interval stays zero" {
                Duration::ZERO => Spread { within_bounds: true, varies: false },
            }
            "offset is below the interval" {
                Duration::from_millis(100) => Spread { within_bounds: true, varies: true },
                Duration::from_secs(30) => Spread { within_bounds: true, varies: true },
            }
        );
    }

    #[test]
    fn jitter_interval_stays_within_ten_percent() {
        value_scenarios!(jittered_intervals:
            "zero interval stays zero" {
                Duration::ZERO => Spread { within_bounds: true, varies: false },
            }
            "jitter stays within ten percent" {
                Duration::from_millis(100) => Spread { within_bounds: true, varies: true },
                Duration::from_secs(1) => Spread { within_bounds: true, varies: true },
                Duration::from_secs(30) => Spread { within_bounds: true, varies: true },
            }
            "Duration::MAX saturates instead of overflowing" {
                Duration::MAX => Spread { within_bounds: true, varies: true },
            }
        );
    }
}
