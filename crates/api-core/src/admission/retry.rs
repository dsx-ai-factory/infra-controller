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

use std::time::Duration;

const MIN_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const RETRY_JITTER_BASIS_POINTS: i32 = 2_000;
const BASIS_POINTS: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RejectionScope {
    Global,
    Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RetryAdvice {
    delay_seconds: u64,
    pressure_scope: RejectionScope,
}

impl RetryAdvice {
    pub(super) const fn delay_seconds(self) -> u64 {
        self.delay_seconds
    }

    pub(super) const fn pressure_scope(self) -> RejectionScope {
        self.pressure_scope
    }

    #[cfg(test)]
    pub(super) const fn for_test(delay_seconds: u64, pressure_scope: RejectionScope) -> Self {
        Self {
            delay_seconds,
            pressure_scope,
        }
    }
}

/// Point-in-time inputs to the retry backpressure estimator.
///
/// The backlog is normalized into execution batches because `concurrency`
/// requests can finish during one average service interval. Treating every
/// request as a serial delay would overstate pressure whenever concurrency is
/// greater than one. The execution EWMA converts those batches into time and
/// adapts the guidance to the actual cost of admitted work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AdmissionPressure {
    pub(super) work_in_flight: usize,
    pub(super) pending: usize,
    pub(super) max_work_in_flight: usize,
    pub(super) execution_duration_ewma: Duration,
}

impl AdmissionPressure {
    /// Estimates how long newly admitted work waits behind complete batches.
    ///
    /// A partial batch can begin immediately, so it does not add a full service
    /// interval to the admission decision.
    pub(super) fn estimated_wait(self) -> Duration {
        let complete_batches =
            self.work_in_flight.saturating_add(self.pending) / self.max_work_in_flight;
        self.execution_duration_ewma
            .saturating_mul(u32::try_from(complete_batches).unwrap_or(u32::MAX))
    }

    /// Estimates retry backpressure, including the current partial batch.
    fn estimated_delay(self) -> Duration {
        let backlog = self.work_in_flight.saturating_add(self.pending);
        let batches = backlog.saturating_add(self.max_work_in_flight.saturating_sub(1))
            / self.max_work_in_flight;
        self.execution_duration_ewma
            .saturating_mul(u32::try_from(batches).unwrap_or(u32::MAX))
    }
}

/// Produces bounded, jittered retry guidance from client and global pressure.
///
/// `Retry-After` is a backpressure signal, not a prediction of the first permit
/// release. A client retrying at the earliest possible slot would immediately
/// compete with the existing backlog and amplify overload. We instead select
/// the stricter of the client and global estimates: a request cannot make
/// progress faster than either limiting scope. The one-to-thirty-second bounds
/// keep empty or very fast estimates actionable and prevent stale or
/// pathological EWMA samples from asking clients to disappear for too long.
/// Jitter spreads retry attempts across time so a shared rejection does not
/// become a synchronized retry wave. The model assumes recent service time and
/// configured concurrency approximate near-future throughput; scale tests may
/// justify tuning the EWMA response, bounds, or jitter range if workloads are
/// strongly bursty or service times are multimodal.
pub(super) fn retry_advice(
    client: AdmissionPressure,
    global: AdmissionPressure,
    jitter_basis_points: i32,
) -> RetryAdvice {
    // Keep this boundary defensive even though the production generator already
    // returns values in range. Callers cannot make the multiplier negative or
    // amplify jitter beyond the documented ±20% policy.
    let jitter_basis_points =
        jitter_basis_points.clamp(-RETRY_JITTER_BASIS_POINTS, RETRY_JITTER_BASIS_POINTS);

    let client_delay = client.estimated_delay();
    let global_delay = global.estimated_delay();
    let (estimated_delay, pressure_scope) = if client_delay > global_delay {
        (client_delay, RejectionScope::Client)
    } else {
        (global_delay, RejectionScope::Global)
    };
    let bounded = estimated_delay.clamp(MIN_RETRY_DELAY, MAX_RETRY_DELAY);
    let multiplier = BASIS_POINTS.saturating_add_signed(jitter_basis_points);
    let jittered_nanos =
        bounded.as_nanos().saturating_mul(u128::from(multiplier)) / u128::from(BASIS_POINTS);
    let jittered = Duration::from_nanos(u64::try_from(jittered_nanos).unwrap_or(u64::MAX))
        .clamp(MIN_RETRY_DELAY, MAX_RETRY_DELAY);

    RetryAdvice {
        // HTTP Retry-After has whole-second resolution. Rounding up avoids
        // weakening the calculated backpressure interval. The same normalized
        // value is sent to gRPC clients in milliseconds.
        delay_seconds: jittered
            .as_secs()
            .saturating_add(u64::from(jittered.subsec_nanos() != 0)),
        pressure_scope,
    }
}

pub(super) fn random_retry_jitter() -> i32 {
    rand::random_range(-RETRY_JITTER_BASIS_POINTS..=RETRY_JITTER_BASIS_POINTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressure(
        work_in_flight: usize,
        pending: usize,
        max_work_in_flight: usize,
        execution_duration_ewma: Duration,
    ) -> AdmissionPressure {
        AdmissionPressure {
            work_in_flight,
            pending,
            max_work_in_flight,
            execution_duration_ewma,
        }
    }

    #[test]
    fn estimated_wait_counts_complete_batches_ahead() {
        let execution_duration_ewma = Duration::from_millis(100);
        assert_eq!(
            pressure(3, 0, 4, execution_duration_ewma).estimated_wait(),
            Duration::ZERO
        );
        assert_eq!(
            pressure(4, 0, 4, execution_duration_ewma).estimated_wait(),
            Duration::from_millis(100)
        );
        assert_eq!(
            pressure(4, 5, 4, execution_duration_ewma).estimated_wait(),
            Duration::from_millis(200)
        );
    }

    #[test]
    fn retry_delay_uses_batched_client_and_global_pressure() {
        struct Case {
            name: &'static str,
            client: AdmissionPressure,
            global: AdmissionPressure,
            jitter_basis_points: i32,
            expected_seconds: u64,
            expected_scope: RejectionScope,
        }

        let cases = [
            Case {
                name: "empty is bounded to minimum",
                client: pressure(0, 0, 4, Duration::from_secs(2)),
                global: pressure(0, 0, 8, Duration::from_secs(1)),
                jitter_basis_points: 0,
                expected_seconds: 1,
                expected_scope: RejectionScope::Global,
            },
            Case {
                name: "partial batch counts as one batch",
                client: pressure(1, 0, 4, Duration::from_secs(2)),
                global: pressure(1, 0, 8, Duration::from_secs(1)),
                jitter_basis_points: 0,
                expected_seconds: 2,
                expected_scope: RejectionScope::Client,
            },
            Case {
                name: "saturated batch counts as one batch",
                client: pressure(4, 0, 4, Duration::from_secs(2)),
                global: pressure(4, 0, 8, Duration::from_secs(1)),
                jitter_basis_points: 0,
                expected_seconds: 2,
                expected_scope: RejectionScope::Client,
            },
            Case {
                name: "client pressure is stricter",
                client: pressure(4, 5, 2, Duration::from_secs(2)),
                global: pressure(4, 5, 10, Duration::from_secs(1)),
                jitter_basis_points: 0,
                expected_seconds: 10,
                expected_scope: RejectionScope::Client,
            },
            Case {
                name: "global pressure is stricter",
                client: pressure(1, 0, 4, Duration::from_secs(1)),
                global: pressure(8, 9, 8, Duration::from_secs(3)),
                jitter_basis_points: 0,
                expected_seconds: 9,
                expected_scope: RejectionScope::Global,
            },
            Case {
                name: "upper bound applies after positive jitter",
                client: pressure(1, 0, 1, Duration::from_secs(40)),
                global: pressure(0, 0, 1, Duration::from_secs(1)),
                jitter_basis_points: RETRY_JITTER_BASIS_POINTS,
                expected_seconds: 30,
                expected_scope: RejectionScope::Client,
            },
            Case {
                name: "lower bound applies after negative jitter",
                client: pressure(0, 0, 1, Duration::from_secs(1)),
                global: pressure(0, 0, 1, Duration::from_secs(1)),
                jitter_basis_points: -RETRY_JITTER_BASIS_POINTS,
                expected_seconds: 1,
                expected_scope: RejectionScope::Global,
            },
            Case {
                name: "positive jitter",
                client: pressure(1, 0, 1, Duration::from_secs(10)),
                global: pressure(0, 0, 1, Duration::from_secs(1)),
                jitter_basis_points: RETRY_JITTER_BASIS_POINTS,
                expected_seconds: 12,
                expected_scope: RejectionScope::Client,
            },
            Case {
                name: "negative jitter",
                client: pressure(1, 0, 1, Duration::from_secs(10)),
                global: pressure(0, 0, 1, Duration::from_secs(1)),
                jitter_basis_points: -RETRY_JITTER_BASIS_POINTS,
                expected_seconds: 8,
                expected_scope: RejectionScope::Client,
            },
            Case {
                name: "excessive positive jitter is clamped",
                client: pressure(1, 0, 1, Duration::from_secs(10)),
                global: pressure(0, 0, 1, Duration::from_secs(1)),
                jitter_basis_points: i32::MAX,
                expected_seconds: 12,
                expected_scope: RejectionScope::Client,
            },
            Case {
                name: "excessive negative jitter is clamped",
                client: pressure(1, 0, 1, Duration::from_secs(10)),
                global: pressure(0, 0, 1, Duration::from_secs(1)),
                jitter_basis_points: i32::MIN,
                expected_seconds: 8,
                expected_scope: RejectionScope::Client,
            },
        ];

        for case in cases {
            let actual = retry_advice(case.client, case.global, case.jitter_basis_points);
            assert_eq!(
                actual.delay_seconds(),
                case.expected_seconds,
                "{}",
                case.name
            );
            assert_eq!(
                actual.pressure_scope(),
                case.expected_scope,
                "{}",
                case.name
            );
        }
    }
}
