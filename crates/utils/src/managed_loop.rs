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

//! The shared heartbeat for managed maintenance loops: every iteration
//! counts once in `carbide_managed_loop_iterations_total`, by manager and
//! outcome, so a single query shows which loops are running, how often, and
//! which of them are failing -- across binaries.

use carbide_instrument::Outcome;

/// The managed loop an iteration belongs to: the `manager` label on
/// [`ManagedLoopIteration`].
///
/// The measured boot metrics collector is deliberately absent: its
/// iterations are already counted, split by the same outcome label, by the
/// `_count` series of
/// `carbide_measured_boot_collector_iteration_latency_milliseconds`, and a
/// second per-iteration counter would duplicate that series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
pub enum LoopManager {
    /// The machine update manager's update-initiation pass (`nico-api`).
    MachineUpdateManager,
    /// The machine validation manager's reconciliation pass (`nico-api`).
    MachineValidationManager,
    /// The managed-host-state republisher's MQTT sweep (`nico-api`).
    ManagedHostStateRepublisher,
    /// The BMC endpoint discovery pass (`nico-hardware-health`).
    HealthDiscovery,
}

/// One pass of a managed maintenance loop completed. Each manager's rate
/// is its heartbeat -- a series that stops moving means the loop is stuck or
/// gone -- and the `outcome = error` split is its failure rate. A pass that
/// yields because another instance holds its work lock still counts as `ok`:
/// the loop itself is alive. A pass that cannot even ask for the lock
/// (database down or overloaded) counts as `error`: nothing is doing the
/// work.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "managed_loop_iteration",
    metric_name = "carbide_managed_loop_iterations_total",
    component = "managed-loop",
    log = dynamic,
    metric = counter,
    message = "managed loop iteration failed",
    describe = "Number of managed loop iterations, by manager and outcome; the measured boot metrics collector's iterations are counted by its latency histogram instead"
)]
pub struct ManagedLoopIteration {
    #[label]
    pub manager: LoopManager,
    #[label]
    pub outcome: Outcome,
    /// The iteration error's text; empty on success (the line only renders
    /// on failure).
    #[context]
    pub error: String,
}

/// Every iteration is counted; only the failures write the WARN line these
/// loops have always logged.
impl carbide_instrument::DynamicLog for ManagedLoopIteration {
    fn log_at(&self) -> carbide_instrument::LogAt {
        match self.outcome {
            Outcome::Ok => carbide_instrument::LogAt::Off,
            Outcome::Error => carbide_instrument::LogAt::Level(tracing::Level::WARN),
        }
    }
}

/// Counts one loop iteration from its result: an ok iteration counts
/// silently, a failed one also writes the event's WARN line with the error
/// text.
pub fn record_iteration<T, E: std::fmt::Display>(manager: LoopManager, result: &Result<T, E>) {
    carbide_instrument::emit(ManagedLoopIteration {
        manager,
        outcome: Outcome::from(result),
        error: match result {
            Ok(_) => String::new(),
            Err(error) => error.to_string(),
        },
    });
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{CapturedLog, MetricsCapture, capture_logs};

    use super::*;

    fn field<'a>(log: &'a CapturedLog, name: &str) -> Option<&'a str> {
        log.fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// One `record_iteration` call moves the counter under the site's
    /// manager and the result's outcome; an ok iteration builds no log line
    /// at all, a failed one writes exactly the WARN line with the error text.
    #[test]
    fn record_iteration_counts_every_outcome_and_logs_only_failures() {
        struct Case {
            scenario: &'static str,
            manager: LoopManager,
            result: Result<(), &'static str>,
            expect_manager: &'static str,
            expect_outcome: &'static str,
            expect_warn_error: Option<&'static str>,
        }

        let cases = [
            Case {
                scenario: "ok iteration counts silently",
                manager: LoopManager::MachineUpdateManager,
                result: Ok(()),
                expect_manager: "machine_update_manager",
                expect_outcome: "ok",
                expect_warn_error: None,
            },
            Case {
                scenario: "failed iteration counts and warns",
                manager: LoopManager::HealthDiscovery,
                result: Err("connection refused"),
                expect_manager: "health_discovery",
                expect_outcome: "error",
                expect_warn_error: Some("connection refused"),
            },
        ];

        for case in cases {
            let metrics = MetricsCapture::start();
            let logs = capture_logs(|| record_iteration(case.manager, &case.result));

            match case.expect_warn_error {
                None => {
                    assert!(
                        logs.is_empty(),
                        "{}: expected no log line: {logs:?}",
                        case.scenario
                    );
                }
                Some(error) => {
                    assert_eq!(logs.len(), 1, "{}", case.scenario);
                    assert_eq!(logs[0].level, tracing::Level::WARN, "{}", case.scenario);
                    assert_eq!(
                        logs[0].message, "managed loop iteration failed",
                        "{}",
                        case.scenario
                    );
                    assert_eq!(
                        field(&logs[0], "manager"),
                        Some(case.expect_manager),
                        "{}",
                        case.scenario
                    );
                    assert_eq!(field(&logs[0], "error"), Some(error), "{}", case.scenario);
                }
            }

            assert_eq!(
                metrics.counter_delta(
                    "carbide_managed_loop_iterations_total",
                    &[
                        ("manager", case.expect_manager),
                        ("outcome", case.expect_outcome),
                    ],
                ),
                1.0,
                "{}",
                case.scenario
            );
        }
    }

    /// The manager label separates the loops into independent series: two
    /// managers reporting the same outcome each move their own counter once.
    #[test]
    fn record_iteration_counts_each_manager_separately() {
        let metrics = MetricsCapture::start();
        capture_logs(|| {
            for manager in [
                LoopManager::MachineValidationManager,
                LoopManager::ManagedHostStateRepublisher,
            ] {
                record_iteration::<(), &str>(manager, &Ok(()));
            }
        });

        for manager in [
            "machine_validation_manager",
            "managed_host_state_republisher",
        ] {
            assert_eq!(
                metrics.counter_delta(
                    "carbide_managed_loop_iterations_total",
                    &[("manager", manager), ("outcome", "ok")],
                ),
                1.0,
                "manager {manager}"
            );
        }
    }
}
