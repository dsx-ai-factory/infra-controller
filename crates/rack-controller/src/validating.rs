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

//! Handler for RackState::Validating, plus rack-validation helper types.

use std::collections::HashMap;

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_uuid::rack::RackId;
use model::machine::StableHostMachine;
use model::metadata::Metadata;
use model::rack::{MachineRvLabels, Rack, RackState, RackValidationState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;

//------------------------------------------------------------------------------
// Helper types

/// Removes all `rv.*` labels from the given metadata in-place.
///
/// Returns `true` if any labels were removed, `false` if the metadata was
/// already clean.
pub(super) fn strip_rv_labels(metadata: &mut Metadata) -> bool {
    let before = metadata.labels.len();
    metadata.labels.retain(|k, _| !k.starts_with("rv."));
    let after = metadata.labels.len();

    after != before
}

/// Aggregated summary of all partition validation statuses in a rack.
/// Used by the state handler to determine state transitions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RackPartitionSummary {
    /// Total number of partitions in the rack
    total_partitions: usize,
    /// Number of partitions that haven't started validation
    pending: usize,
    /// Number of partitions currently being validated
    in_progress: usize,
    /// Number of partitions that passed validation
    validated: usize,
    /// Number of partitions that failed validation
    failed: usize,
}

/// Per-machine rack-validation state, derived from machine metadata labels.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MachineRvState {
    Idle,
    Inp,
    Pass,
    Fail(String),
}

impl TryFrom<Metadata> for MachineRvState {
    type Error = StateHandlerError;

    fn try_from(metadata: Metadata) -> Result<Self, Self::Error> {
        let st_label = MachineRvLabels::State.as_str();
        let fail_label = MachineRvLabels::FailDesc.as_str();

        let st = metadata
            .labels
            .get(st_label)
            .ok_or_else(|| {
                StateHandlerError::InvalidState(format!("missing required label '{}'", st_label))
            })?
            .as_str();

        match st {
            "idle" => Ok(MachineRvState::Idle),
            "inp" => Ok(MachineRvState::Inp),
            "pass" => Ok(MachineRvState::Pass),
            "fail" => {
                let desc = metadata.labels.get(fail_label).cloned().unwrap_or_default();
                Ok(MachineRvState::Fail(desc))
            }
            other => Err(StateHandlerError::InvalidState(format!(
                "unknown '{}' value: '{}'",
                st_label, other
            ))),
        }
    }
}

/// Partition grouping: maps partition ID -> per-node validation states.
///
/// Only machines that carry the `rv.part-id` label are considered
/// validation participants. Machines without it are silently skipped.
/// Machines whose `rv.run-id` label is missing or doesn't match the
/// provided `run_id` are also skipped (stale labels from previous runs).
struct RvPartitions {
    inner: HashMap<String, Vec<MachineRvState>>,
}

impl RvPartitions {
    /// Build from a vec of machines, filtering by run ID.
    fn from_machines(
        machines: Vec<StableHostMachine>,
        run_id: &str,
    ) -> Result<Self, StateHandlerError> {
        Self::from_meta_iter(machines.into_iter().map(|m| m.metadata), run_id)
    }

    /// Core grouping logic over any iterator of Metadata.
    /// Extracted so unit tests can feed plain metadata without constructing
    /// full Machine values.
    fn from_meta_iter(
        iter: impl Iterator<Item = Metadata>,
        run_id: &str,
    ) -> Result<Self, StateHandlerError> {
        let mut inner: HashMap<String, Vec<MachineRvState>> = HashMap::new();
        let part_label = MachineRvLabels::PartitionId.as_str();
        let run_label = MachineRvLabels::RunId.as_str();

        for mut meta in iter {
            // Skip machines that aren't part of rack validation
            let Some(part_id) = meta.labels.remove(part_label) else {
                continue;
            };

            // Skip machines whose run ID doesn't match the current run
            let machine_run_id = meta.labels.remove(run_label);
            let Some(fetched) = machine_run_id else {
                continue;
            };

            if run_id != fetched {
                continue;
            }

            let rv_state = meta.try_into()?;
            inner.entry(part_id).or_default().push(rv_state);
        }

        Ok(RvPartitions { inner })
    }

    /// Aggregate per-node states into a [`RackPartitionSummary`].
    ///
    /// For each partition, the aggregate status is:
    /// - Validated   if all nodes are `Pass`
    /// - Failed      else if any node is `Fail`
    /// - InProgress  else if any node is `Inp`
    /// - Pending     otherwise (all `Idle`, or a mix of `Idle`/`Pass`)
    fn summarize(&self) -> RackPartitionSummary {
        let mut summary = RackPartitionSummary {
            total_partitions: self.inner.len(),
            ..Default::default()
        };

        for states in self.inner.values() {
            if states.iter().all(|s| *s == MachineRvState::Pass) {
                summary.validated += 1;
            } else if states.iter().any(|s| matches!(s, MachineRvState::Fail(_))) {
                summary.failed += 1;
            } else if states.contains(&MachineRvState::Inp) {
                summary.in_progress += 1;
            } else {
                summary.pending += 1;
            }
        }

        summary
    }
}

//------------------------------------------------------------------------------
// Validation helpers

/// Loads the aggregated partition validation summary for a rack.
///
/// Queries all machines belonging to the rack, reads their validation metadata
/// labels, and aggregates the status by partition.
async fn load_partition_summary(
    rack_id: &RackId,
    rack: &Rack,
    run_id: &str,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<RackPartitionSummary, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;
    let machines = super::get_machines_from_rack(rack, &mut txn).await?;
    txn.commit().await?;

    tracing::debug!(
        rack_id = %rack_id,
        machine_count = machines.len(),
        "Rack has machines",
    );

    let partitions = RvPartitions::from_machines(machines, run_id)?;
    Ok(partitions.summarize())
}

/// Scans the rack's machines for an `rv.run-id` label set by RVS.
/// Returns the first run ID found, or `None` if RVS has not started a run yet.
async fn find_rv_run_id(
    rack_id: &RackId,
    rack: &Rack,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<Option<String>, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;
    let machines = super::get_machines_from_rack(rack, &mut txn).await?;
    txn.commit().await?;

    let run_label = MachineRvLabels::RunId.as_str();
    let found = machines
        .into_iter()
        .find_map(|mut m| m.metadata.labels.remove(run_label));

    tracing::debug!(
        rack_id = %rack_id,
        found_run_id = ?found,
        "Rack rv.run-id scan",
    );

    Ok(found)
}

/// Computes the next validation sub-state based on current sub-state and
/// partition summary.
///
/// Pure function encoding the validation state machine transitions.
/// Returns `None` if no transition should occur.
fn compute_validation_transition(
    current: &RackValidationState,
    summary: &RackPartitionSummary,
) -> Option<RackValidationState> {
    match current {
        RackValidationState::InProgress { run_id } => {
            // Check for failures first (higher priority)
            if summary.failed > 0 {
                Some(RackValidationState::FailedPartial {
                    run_id: run_id.clone(),
                })
            } else if summary.validated > 0 {
                Some(RackValidationState::Partial {
                    run_id: run_id.clone(),
                })
            } else {
                None
            }
        }
        RackValidationState::Partial { run_id } => {
            if summary.validated == summary.total_partitions {
                Some(RackValidationState::Validated {
                    run_id: run_id.clone(),
                })
            } else if summary.failed > 0 {
                Some(RackValidationState::FailedPartial {
                    run_id: run_id.clone(),
                })
            } else {
                None
            }
        }
        RackValidationState::FailedPartial { run_id } => {
            if summary.total_partitions == 0 {
                // No partitions currently observed. Treat this as a reset to
                // Pending so racks don't enter terminal failure just because
                // validation instances/labels are temporarily absent.
                Some(RackValidationState::Pending)
            } else if summary.failed == summary.total_partitions {
                Some(RackValidationState::Failed {
                    run_id: run_id.clone(),
                })
            } else if summary.failed == 0 {
                // All failures resolved -- figure out where to go next
                if summary.validated > 0 {
                    Some(RackValidationState::Partial {
                        run_id: run_id.clone(),
                    })
                } else if summary.in_progress > 0 {
                    Some(RackValidationState::InProgress {
                        run_id: run_id.clone(),
                    })
                } else {
                    // All partitions back to idle/pending (e.g. RVS reset
                    // instances before a re-run). Transition to Pending so
                    // the validation cycle can restart cleanly.
                    Some(RackValidationState::Pending)
                }
            } else {
                None
            }
        }
        RackValidationState::Failed { run_id } => {
            // Can recover if at least one partition is no longer failed
            if summary.failed != summary.total_partitions {
                Some(RackValidationState::FailedPartial {
                    run_id: run_id.clone(),
                })
            } else {
                None
            }
        }
        RackValidationState::Validated { .. } => {
            // Terminal success sub-state. The handler promotes this to
            // RackState::Ready; no further validation transition needed.
            None
        }
        _ => None,
    }
}

//------------------------------------------------------------------------------
// State handler

pub(super) async fn handle_validating(
    id: &RackId,
    state: &mut Rack,
    validating_state: &RackValidationState,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    if !ctx.services.site_config.rack_validation_config.enabled {
        tracing::info!(
            rack_id = %id,
            "Rack validation disabled, skipping to Ready",
        );
        return Ok(StateHandlerOutcome::transition(RackState::Ready));
    }

    match validating_state {
        RackValidationState::Pending => {
            // Stay in Pending until RVS sets rv.run-id on at least one rack machine.
            if let Some(found_run_id) = find_rv_run_id(id, state, ctx).await? {
                tracing::info!(
                    rack_id = %id,
                    found_run_id = %found_run_id,
                    "Rack validation run started, entering InProgress",
                );
                Ok(StateHandlerOutcome::transition(RackState::Validating {
                    validating_state: RackValidationState::InProgress {
                        run_id: found_run_id,
                    },
                }))
            } else {
                tracing::debug!(
                    rack_id = %id,
                    "Rack in Validating(Pending), waiting for RVS to set rv.run-id",
                );
                Ok(StateHandlerOutcome::do_nothing())
            }
        }
        other => {
            let run_id = other.run_id().ok_or_else(|| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "validating substates must carry the active run_id"
                ))
            })?;

            let summary = load_partition_summary(id, state, run_id, ctx).await?;

            tracing::debug!(
                rack_id = %id,
                total_partition_count = summary.total_partitions,
                pending_partition_count = summary.pending,
                in_progress_partition_count = summary.in_progress,
                validated_partition_count = summary.validated,
                failed_partition_count = summary.failed,
                "Rack partition validation summary",
            );

            if let Some(next_vs) = compute_validation_transition(other, &summary) {
                tracing::info!(
                    rack_id = %id,
                    previous_state = %other,
                    next_state = %next_vs,
                    "Rack validation state transition",
                );
                Ok(StateHandlerOutcome::transition(RackState::Validating {
                    validating_state: next_vs,
                }))
            } else if matches!(other, RackValidationState::Validated { .. }) {
                tracing::info!(
                    rack_id = %id,
                    "Rack fully validated, transitioning to Ready",
                );
                Ok(StateHandlerOutcome::transition(RackState::Ready))
            } else if matches!(other, RackValidationState::Failed { .. }) {
                tracing::warn!(
                    rack_id = %id,
                    "Rack is in Validating(Failed) state, requires intervention",
                );
                Ok(StateHandlerOutcome::do_nothing())
            } else {
                Ok(StateHandlerOutcome::do_nothing())
            }
        }
    }
}

//------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_test_support::{Check, check_values};

    use super::*;

    fn metadata_with_labels(pairs: &[(&str, &str)]) -> Metadata {
        Metadata {
            name: String::new(),
            description: String::new(),
            labels: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn test_machine_rv_state_from_metadata() {
        let m = metadata_with_labels(&[("rv.st", "idle"), ("rv.part-id", "p0")]);
        let s: MachineRvState = m.try_into().unwrap();
        assert_eq!(s, MachineRvState::Idle);

        let m = metadata_with_labels(&[("rv.st", "inp")]);
        let s: MachineRvState = m.try_into().unwrap();
        assert_eq!(s, MachineRvState::Inp);

        let m = metadata_with_labels(&[("rv.st", "pass")]);
        let s: MachineRvState = m.try_into().unwrap();
        assert_eq!(s, MachineRvState::Pass);

        let m = metadata_with_labels(&[("rv.st", "fail")]);
        let s: MachineRvState = m.try_into().unwrap();
        assert_eq!(s, MachineRvState::Fail(String::new()));

        let m = metadata_with_labels(&[("rv.st", "fail"), ("rv.fail-desc", "nccl-timeout")]);
        let s: MachineRvState = m.try_into().unwrap();
        assert_eq!(s, MachineRvState::Fail("nccl-timeout".into()));

        let m = metadata_with_labels(&[("rv.part-id", "p0")]);
        let s: Result<MachineRvState, StateHandlerError> = m.try_into();
        assert!(matches!(
            s,
            Err(StateHandlerError::InvalidState(msg)) if msg.contains("missing")
        ));

        let m = metadata_with_labels(&[("rv.st", "bogus")]);
        let s: Result<MachineRvState, StateHandlerError> = m.try_into();
        assert!(matches!(
            s,
            Err(StateHandlerError::InvalidState(msg)) if msg.contains("bogus")
        ));
    }

    #[test]
    fn test_partitions_from_meta_iter() {
        let metas = [
            metadata_with_labels(&[
                ("rv.part-id", "p0"),
                ("rv.st", "pass"),
                ("rv.run-id", "run-005"),
            ]),
            metadata_with_labels(&[
                ("rv.part-id", "p0"),
                ("rv.st", "inp"),
                ("rv.run-id", "run-005"),
            ]),
            metadata_with_labels(&[
                ("rv.part-id", "p1"),
                ("rv.st", "idle"),
                ("rv.run-id", "run-005"),
            ]),
            // No rv.part-id -> should be skipped
            metadata_with_labels(&[("some-other", "label"), ("rv.run-id", "run-005")]),
            // No rv.run-id -> should be skipped
            metadata_with_labels(&[("rv.part-id", "p2"), ("rv.st", "pass")]),
        ];

        let parts = RvPartitions::from_meta_iter(metas.iter().cloned(), "run-005").unwrap();

        assert_eq!(parts.inner.len(), 2);
        assert_eq!(parts.inner["p0"].len(), 2);
        assert_eq!(parts.inner["p0"][0], MachineRvState::Pass);
        assert_eq!(parts.inner["p0"][1], MachineRvState::Inp);
        assert_eq!(parts.inner["p1"].len(), 1);
        assert_eq!(parts.inner["p1"][0], MachineRvState::Idle);
    }

    #[test]
    fn test_partitions_run_id_filtering() {
        let metas = [
            // Current run -- should be included
            metadata_with_labels(&[
                ("rv.part-id", "p0"),
                ("rv.st", "pass"),
                ("rv.run-id", "run-005"),
            ]),
            // Stale run -- should be skipped
            metadata_with_labels(&[
                ("rv.part-id", "p0"),
                ("rv.st", "pass"),
                ("rv.run-id", "run-004"),
            ]),
            // No run ID -- should be skipped
            metadata_with_labels(&[("rv.part-id", "p1"), ("rv.st", "idle")]),
        ];

        let parts = RvPartitions::from_meta_iter(metas.iter().cloned(), "run-005").unwrap();

        assert_eq!(parts.inner.len(), 1);
        assert_eq!(parts.inner["p0"].len(), 1);
        assert_eq!(parts.inner["p0"][0], MachineRvState::Pass);
    }

    #[test]
    fn test_partitions_summarize() {
        let metas = [
            // Partition p0: one node pass, one node fail -> Failed
            metadata_with_labels(&[
                ("rv.part-id", "p0"),
                ("rv.st", "pass"),
                ("rv.run-id", "run-005"),
            ]),
            metadata_with_labels(&[
                ("rv.part-id", "p0"),
                ("rv.st", "fail"),
                ("rv.fail-desc", "nccl"),
                ("rv.run-id", "run-005"),
            ]),
            // Partition p1: all nodes pass -> Validated
            metadata_with_labels(&[
                ("rv.part-id", "p1"),
                ("rv.st", "pass"),
                ("rv.run-id", "run-005"),
            ]),
            metadata_with_labels(&[
                ("rv.part-id", "p1"),
                ("rv.st", "pass"),
                ("rv.run-id", "run-005"),
            ]),
            // Partition p2: one node is idle, one is inp -> InProgress
            metadata_with_labels(&[
                ("rv.part-id", "p2"),
                ("rv.st", "idle"),
                ("rv.run-id", "run-005"),
            ]),
            metadata_with_labels(&[
                ("rv.part-id", "p2"),
                ("rv.st", "inp"),
                ("rv.run-id", "run-005"),
            ]),
            // Partition p3: all nodes idle -> Pending
            metadata_with_labels(&[
                ("rv.part-id", "p3"),
                ("rv.st", "idle"),
                ("rv.run-id", "run-005"),
            ]),
        ];

        let parts = RvPartitions::from_meta_iter(metas.iter().cloned(), "run-005").unwrap();
        let summary = parts.summarize();

        assert_eq!(summary.total_partitions, 4);
        assert_eq!(summary.failed, 1); // p0
        assert_eq!(summary.validated, 1); // p1
        assert_eq!(summary.in_progress, 1); // p2
        assert_eq!(summary.pending, 1); // p3
    }

    #[test]
    fn test_strip_rv_labels_removes_only_rv_keys() {
        let mut m = metadata_with_labels(&[
            ("rv.run-id", "run-1"),
            ("rv.part-id", "p0"),
            ("rv.st", "pass"),
            ("other", "keep-me"),
        ]);
        assert!(strip_rv_labels(&mut m));
        assert!(!m.labels.contains_key("rv.run-id"));
        assert!(!m.labels.contains_key("rv.part-id"));
        assert!(!m.labels.contains_key("rv.st"));
        assert_eq!(m.labels.get("other").map(String::as_str), Some("keep-me"));
    }

    #[test]
    fn test_strip_rv_labels_returns_false_when_already_clean() {
        let mut m = metadata_with_labels(&[("foo", "bar"), ("baz", "qux")]);
        assert!(!strip_rv_labels(&mut m));
        assert_eq!(m.labels.len(), 2);
    }

    #[test]
    fn test_strip_rv_labels_empty_metadata() {
        let mut m = metadata_with_labels(&[]);
        assert!(!strip_rv_labels(&mut m));
    }

    // -------------------------------------------------------------------------
    // compute_validation_transition tests

    /// One transition case: a current sub-state plus the partition summary it is
    /// evaluated against. The expected value is the next sub-state, or `None` when
    /// the state machine should hold.
    struct TransitionCase {
        state: RackValidationState,
        summary: RackPartitionSummary,
    }

    fn in_progress() -> RackValidationState {
        RackValidationState::InProgress {
            run_id: "run-001".to_string(),
        }
    }

    fn partial() -> RackValidationState {
        RackValidationState::Partial {
            run_id: "run-001".to_string(),
        }
    }

    fn failed_partial() -> RackValidationState {
        RackValidationState::FailedPartial {
            run_id: "run-001".to_string(),
        }
    }

    fn failed() -> RackValidationState {
        RackValidationState::Failed {
            run_id: "run-001".to_string(),
        }
    }

    fn validated() -> RackValidationState {
        RackValidationState::Validated {
            run_id: "run-001".to_string(),
        }
    }

    #[test]
    fn test_compute_validation_transition() {
        check_values(
            [
                // ── from InProgress ──────────────────────────────────────
                Check {
                    scenario: "in progress / still in progress holds",
                    input: TransitionCase {
                        state: in_progress(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            pending: 2,
                            in_progress: 2,
                            ..Default::default()
                        },
                    },
                    expect: None,
                },
                Check {
                    scenario: "in progress / one validated -> Partial",
                    input: TransitionCase {
                        state: in_progress(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            pending: 2,
                            in_progress: 1,
                            validated: 1,
                            ..Default::default()
                        },
                    },
                    expect: Some(partial()),
                },
                Check {
                    scenario: "in progress / one failed outranks validated -> FailedPartial",
                    input: TransitionCase {
                        state: in_progress(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            pending: 1,
                            in_progress: 1,
                            validated: 1,
                            failed: 1,
                        },
                    },
                    expect: Some(failed_partial()),
                },
                // ── from Partial ─────────────────────────────────────────
                Check {
                    scenario: "partial / more in progress holds",
                    input: TransitionCase {
                        state: partial(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            in_progress: 2,
                            validated: 2,
                            ..Default::default()
                        },
                    },
                    expect: None,
                },
                Check {
                    scenario: "partial / all validated -> Validated",
                    input: TransitionCase {
                        state: partial(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            validated: 4,
                            ..Default::default()
                        },
                    },
                    expect: Some(validated()),
                },
                Check {
                    scenario: "partial / one failed -> FailedPartial",
                    input: TransitionCase {
                        state: partial(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            validated: 3,
                            failed: 1,
                            ..Default::default()
                        },
                    },
                    expect: Some(failed_partial()),
                },
                // ── from FailedPartial ───────────────────────────────────
                Check {
                    scenario: "failed partial / all failed -> Failed",
                    input: TransitionCase {
                        state: failed_partial(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            failed: 4,
                            ..Default::default()
                        },
                    },
                    expect: Some(failed()),
                },
                Check {
                    scenario: "failed partial / recovery with some validated -> Partial",
                    input: TransitionCase {
                        state: failed_partial(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            in_progress: 2,
                            validated: 2,
                            ..Default::default()
                        },
                    },
                    expect: Some(partial()),
                },
                Check {
                    scenario: "failed partial / recovery none validated yet -> InProgress",
                    input: TransitionCase {
                        state: failed_partial(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            pending: 2,
                            in_progress: 2,
                            ..Default::default()
                        },
                    },
                    expect: Some(in_progress()),
                },
                Check {
                    scenario: "failed partial / still some failed and some validated holds",
                    input: TransitionCase {
                        state: failed_partial(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            validated: 2,
                            failed: 2,
                            ..Default::default()
                        },
                    },
                    expect: None,
                },
                Check {
                    scenario: "failed partial / all partitions reset to idle -> Pending",
                    input: TransitionCase {
                        state: failed_partial(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            pending: 4,
                            ..Default::default()
                        },
                    },
                    expect: Some(RackValidationState::Pending),
                },
                // ── from Failed ──────────────────────────────────────────
                Check {
                    scenario: "failed / still all failed holds",
                    input: TransitionCase {
                        state: failed(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            failed: 4,
                            ..Default::default()
                        },
                    },
                    expect: None,
                },
                Check {
                    scenario: "failed / recovery started -> FailedPartial",
                    input: TransitionCase {
                        state: failed(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            in_progress: 1,
                            failed: 3,
                            ..Default::default()
                        },
                    },
                    expect: Some(failed_partial()),
                },
                // ── from Validated (terminal) ────────────────────────────
                Check {
                    scenario: "validated / terminal sub-state always holds",
                    input: TransitionCase {
                        state: validated(),
                        summary: RackPartitionSummary {
                            total_partitions: 4,
                            validated: 4,
                            ..Default::default()
                        },
                    },
                    expect: None,
                },
            ],
            |TransitionCase { state, summary }| compute_validation_transition(&state, &summary),
        );
    }
}
