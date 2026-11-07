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

//! Sweep freshness and stage targets. Pure, used by the probe client.

use std::fmt;

pub use carbide_site_health_probe_api::Direction;
use carbide_site_health_probe_api::{CountResponse, SweepInfo};
use chrono::{DateTime, Utc};

/// A quantile in basis points, 1 to 10,000. Integer math keeps targets exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fraction(u16);

/// The quantiles every stage reports: 0.5, 0.9, 0.95, 0.99, 0.995 and 1.
pub const DEFAULT_QS: [Fraction; 6] = [
    Fraction(5000),
    Fraction(9000),
    Fraction(9500),
    Fraction(9900),
    Fraction(9950),
    Fraction(10000),
];

impl Fraction {
    pub const ONE: Fraction = Fraction(10000);

    pub const fn new(basis_points: u16) -> Option<Self> {
        if basis_points == 0 || basis_points > 10000 {
            None
        } else {
            Some(Self(basis_points))
        }
    }

    pub const fn basis_points(self) -> u16 {
        self.0
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 10000.0
    }
}

/// The shortest decimal form: `0.5`, `0.995`, `1`.
impl fmt::Display for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.0 / 10000;
        let frac = self.0 % 10000;
        if frac == 0 {
            return write!(f, "{whole}");
        }
        let digits = format!("{frac:04}");
        write!(f, "{whole}.{}", digits.trim_end_matches('0'))
    }
}

/// One fresh sweep as the observer saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepPoint {
    pub sweep_start: DateTime<Utc>,
    /// Sweep end. Crossings use this time.
    pub read_at: DateTime<Utc>,
    pub count: u64,
}

/// True when the sweep is at most two of its own intervals old. Only a fresh
/// reply can mark units done.
pub fn sweep_info_fresh(sweep: &SweepInfo) -> bool {
    let interval = sweep.sweep_interval_seconds;
    let age = sweep.sweep_age_seconds;
    interval > 0.0 && age >= 0.0 && age <= 2.0 * interval
}

/// [`sweep_info_fresh`] of a count reply's sweep.
pub fn sweep_fresh(reply: &CountResponse) -> bool {
    sweep_info_fresh(&reply.sweep)
}

/// `base + ceil(q * n)` going up, `base - ceil(q * n)` going down (not
/// below 0). Integer math.
pub fn target(base: u64, n: u64, q: Fraction, dir: Direction) -> u64 {
    let step = (u128::from(q.0) * u128::from(n)).div_ceil(10000);
    let step = u64::try_from(step).unwrap_or(u64::MAX);
    match dir {
        Direction::Up => base.saturating_add(step),
        Direction::Down => base.saturating_sub(step),
    }
}

/// True when `count` is at or past `target` in `dir`.
pub fn reached(count: u64, target: u64, dir: Direction) -> bool {
    match dir {
        Direction::Up => count >= target,
        Direction::Down => count <= target,
    }
}

#[cfg(test)]
mod tests {
    use carbide_site_health_probe_api::Family;
    use carbide_test_support::{Check, check_values};

    use super::*;

    /// Table rows as labeled checks.
    fn rows<I, T>(rows: impl IntoIterator<Item = (&'static str, I, T)>) -> Vec<Check<I, T>> {
        rows.into_iter()
            .map(|(scenario, input, expect)| Check {
                scenario,
                input,
                expect,
            })
            .collect()
    }

    fn reply(age: f64, interval: f64) -> CountResponse {
        let at = DateTime::parse_from_rfc3339("2026-10-23T10:00:00Z")
            .unwrap()
            .to_utc();
        CountResponse {
            count: 10,
            sweep: SweepInfo {
                snapshot_id: "s1".to_string(),
                family: Family::Host,
                sweep_start: at,
                read_at: at,
                sweep_age_seconds: age,
                sweep_interval_seconds: interval,
            },
        }
    }

    #[test]
    fn sweep_fresh_uses_reply_interval() {
        check_values(
            rows([
                ("20 s interval, 40 s old", (40.0, 20.0), true),
                ("20 s interval, 40.1 s old", (40.1, 20.0), false),
                ("60 s interval, 100 s old", (100.0, 60.0), true),
                ("60 s interval, 121 s old", (121.0, 60.0), false),
                ("brand new sweep", (0.0, 20.0), true),
                ("zero interval", (0.0, 0.0), false),
                ("negative age", (-1.0, 20.0), false),
                ("NaN age", (f64::NAN, 20.0), false),
            ]),
            |(age, interval)| sweep_fresh(&reply(age, interval)),
        );
    }

    #[test]
    fn fraction_prints_shortest_decimal() {
        let printed: Vec<String> = DEFAULT_QS.iter().map(|q| q.to_string()).collect();
        assert_eq!(printed, ["0.5", "0.9", "0.95", "0.99", "0.995", "1"]);
        assert_eq!(Fraction::new(1).unwrap().to_string(), "0.0001");
        assert_eq!(Fraction::new(0), None);
        assert_eq!(Fraction::new(10001), None);
        assert_eq!(Fraction::ONE.as_f64(), 1.0);
    }

    fn q(basis_points: u16) -> Fraction {
        Fraction::new(basis_points).unwrap()
    }

    #[test]
    fn target_rounds_up_both_directions() {
        check_values(
            rows([
                ("N 4500 at 0.995", (0, 4500, q(9950), Direction::Up), 4478),
                ("N 4500 at 0.5", (0, 4500, q(5000), Direction::Up), 2250),
                ("half of 7 rounds up", (100, 7, q(5000), Direction::Up), 104),
                ("all", (100, 7, Fraction::ONE, Direction::Up), 107),
                (
                    "down, N 4500 at 0.995",
                    (4500, 4500, q(9950), Direction::Down),
                    22,
                ),
                (
                    "down, half of 3 rounds up",
                    (10, 3, q(5000), Direction::Down),
                    8,
                ),
                (
                    "down never below 0",
                    (5, 10, Fraction::ONE, Direction::Down),
                    0,
                ),
                ("one basis point of 1", (0, 1, q(1), Direction::Up), 1),
            ]),
            |(base, n, q, dir)| target(base, n, q, dir),
        );
    }
}
