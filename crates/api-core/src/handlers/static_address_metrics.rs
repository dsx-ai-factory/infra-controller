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

use carbide_instrument::{Event, LabelValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum AssignmentOutcome {
    Assigned,
    ReplacedStatic,
    ReplacedDhcp,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum RemovalOutcome {
    Removed,
    NotFound,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum PreallocationOutcome {
    Assigned,
    Skipped,
    Created,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreallocationSuccess {
    Assigned,
    Skipped,
    Created,
}

#[derive(Event)]
#[event(
    event_name = "static_address_assignment_completed",
    metric_name = "carbide_static_address_assignments_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of static address assignment attempts, by outcome.",
    labels(outcome: AssignmentOutcome),
)]
pub(super) enum StaticAddressAssignmentCompleted {
    #[event(labels(outcome = Assigned))]
    Assigned {},
    #[event(labels(outcome = ReplacedStatic))]
    ReplacedStatic {},
    #[event(labels(outcome = ReplacedDhcp))]
    ReplacedDhcp {},
    #[event(
        labels(outcome = Error),
        log = warn,
        message = "Static address assignment completed"
    )]
    Error {
        #[context]
        error: String,
    },
}

#[derive(Event)]
#[event(
    event_name = "static_address_removal_completed",
    metric_name = "carbide_static_address_removals_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of static address removal attempts, by outcome.",
    labels(outcome: RemovalOutcome),
)]
pub(super) enum StaticAddressRemovalCompleted {
    #[event(labels(outcome = Removed))]
    Removed {},
    #[event(labels(outcome = NotFound))]
    NotFound {},
    #[event(
        labels(outcome = Error),
        log = warn,
        message = "Static address removal completed"
    )]
    Error {
        #[context]
        error: String,
    },
}

#[derive(Event)]
#[event(
    event_name = "static_address_preallocation_completed",
    metric_name = "carbide_static_address_preallocations_total",
    component = "nico-api",
    log = off,
    metric = counter,
    describe = "Number of static address preallocation outcomes recorded, by outcome; successful outcomes are counted only after commit.",
    labels(outcome: PreallocationOutcome),
)]
pub(super) enum StaticAddressPreallocationCompleted {
    #[event(labels(outcome = Assigned))]
    Assigned {},
    #[event(labels(outcome = Skipped))]
    Skipped {},
    #[event(labels(outcome = Created))]
    Created {},
    #[event(
        labels(outcome = Error),
        log = warn,
        message = "Static address preallocation completed"
    )]
    Error {
        #[context]
        error: String,
    },
}

impl From<PreallocationSuccess> for StaticAddressPreallocationCompleted {
    fn from(outcome: PreallocationSuccess) -> Self {
        match outcome {
            PreallocationSuccess::Assigned => Self::Assigned {},
            PreallocationSuccess::Skipped => Self::Skipped {},
            PreallocationSuccess::Created => Self::Created {},
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};

    use super::*;

    #[test]
    fn preallocation_success_maps_to_expected_event() {
        assert!(matches!(
            StaticAddressPreallocationCompleted::from(PreallocationSuccess::Assigned),
            StaticAddressPreallocationCompleted::Assigned {}
        ));
        assert!(matches!(
            StaticAddressPreallocationCompleted::from(PreallocationSuccess::Skipped),
            StaticAddressPreallocationCompleted::Skipped {}
        ));
        assert!(matches!(
            StaticAddressPreallocationCompleted::from(PreallocationSuccess::Created),
            StaticAddressPreallocationCompleted::Created {}
        ));
    }

    #[test]
    fn events_count_all_outcomes_and_log_only_errors() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(StaticAddressAssignmentCompleted::Assigned {});
            emit(StaticAddressAssignmentCompleted::ReplacedStatic {});
            emit(StaticAddressAssignmentCompleted::ReplacedDhcp {});
            emit(StaticAddressAssignmentCompleted::Error {
                error: "operation failed".to_string(),
            });

            emit(StaticAddressRemovalCompleted::Removed {});
            emit(StaticAddressRemovalCompleted::NotFound {});
            emit(StaticAddressRemovalCompleted::Error {
                error: "operation failed".to_string(),
            });

            emit(StaticAddressPreallocationCompleted::from(
                PreallocationSuccess::Assigned,
            ));
            emit(StaticAddressPreallocationCompleted::from(
                PreallocationSuccess::Skipped,
            ));
            emit(StaticAddressPreallocationCompleted::from(
                PreallocationSuccess::Created,
            ));
            emit(StaticAddressPreallocationCompleted::Error {
                error: "operation failed".to_string(),
            });
        });

        assert_eq!(logs.len(), 3);
        assert!(logs.iter().all(|log| log.level == tracing::Level::WARN));

        assert_outcomes(
            &metrics,
            "carbide_static_address_assignments_total",
            &["assigned", "replaced_static", "replaced_dhcp", "error"],
        );
        assert_outcomes(
            &metrics,
            "carbide_static_address_removals_total",
            &["removed", "not_found", "error"],
        );
        // Product-path tests share the global metrics registry and can emit these
        // outcomes concurrently, so only assert that this test emitted each one.
        assert_outcomes_at_least(
            &metrics,
            "carbide_static_address_preallocations_total",
            &["assigned", "skipped", "created", "error"],
        );
    }

    fn assert_outcomes(metrics: &MetricsCapture, name: &str, outcomes: &[&str]) {
        for outcome in outcomes {
            assert_eq!(metrics.counter_delta(name, &[("outcome", outcome)]), 1.0);
        }
    }

    fn assert_outcomes_at_least(metrics: &MetricsCapture, name: &str, outcomes: &[&str]) {
        for outcome in outcomes {
            assert!(metrics.counter_delta(name, &[("outcome", outcome)]) >= 1.0);
        }
    }
}
