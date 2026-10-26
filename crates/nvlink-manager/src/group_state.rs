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

//! Change detection for the partition monitor's machine groups.
//!
//! A group whose database inputs are unchanged since its last successful
//! check, whose instances all have their NVLink config applied and whose
//! staggered full check is not due is skipped: NMX-C is not contacted and no
//! observation is written. A skipped rack keeps the domain UUID observed at
//! its last full check, and republishes it when a switch or power shelf added
//! to the rack since then has a missing or different one. Drift on the NMX-C
//! side is caught by the full check. The state is in memory only; the monitor
//! runs on one replica under its work lock, and a restart costs one round of
//! full checks.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use carbide_uuid::nvlink::NvLinkDomainId;
use model::machine::ManagedHostStateSnapshot;
use model::machine::nvlink::nvlink_config_synced;

use crate::metrics::NmxcMetrics;
use crate::{ProcessMachineGroupInput, managed_host_chassis_serial};

/// What the monitor remembers about one machine group between iterations.
pub(super) struct GroupReconcileState {
    /// [`group_fingerprint`] of the inputs at the last full check.
    fingerprint: u64,
    /// Whether the last full check reached NMX-C, every operation it issued
    /// completed, and its GPU health pass was fetched and persisted.
    last_check_ok: bool,
    /// When the next full check is due regardless of input changes.
    next_full_check: Instant,
    /// NMX-C metadata and health counts from the last full check, carried
    /// forward for the per-domain gauges while the group is skipped.
    last_health: NmxcMetrics,
    /// NVLink domain UUID published to the rack's switches and power shelves
    /// at the last full check. Chassis groups have none.
    rack_domain_uuid: Option<NvLinkDomainId>,
}

/// What a full check leaves behind for the next skip decision.
pub(super) struct FullCheckOutcome {
    pub(super) fingerprint: u64,
    /// Whether NMX-C was reached, every operation completed, and GPU health
    /// was fetched and persisted. A `false` check is repeated next iteration.
    pub(super) check_ok: bool,
    pub(super) applied_changes: bool,
    pub(super) health: NmxcMetrics,
    pub(super) rack_domain_uuid: Option<NvLinkDomainId>,
}

impl GroupReconcileState {
    /// Records a full check. A group's first check spreads its next full
    /// check over the interval so groups configured together are not all
    /// re-checked in the same iteration. A check that applied changes is
    /// due again immediately so the group converges on the fast re-check.
    pub(super) fn after_full_check(
        previous: Option<&Self>,
        group_id: &str,
        outcome: FullCheckOutcome,
        now: Instant,
        full_check_interval: Duration,
    ) -> Self {
        let next_full_check = if outcome.applied_changes {
            now
        } else {
            let wait = if previous.is_some() {
                full_check_interval
            } else {
                stagger_offset(group_id, full_check_interval)
            };
            now.checked_add(wait).unwrap_or(now)
        };
        Self {
            fingerprint: outcome.fingerprint,
            last_check_ok: outcome.check_ok,
            next_full_check,
            last_health: outcome.health,
            rack_domain_uuid: outcome.rack_domain_uuid,
        }
    }

    /// Whether the group can skip NMX-C this iteration.
    pub(super) fn can_skip(&self, fingerprint: u64, instances_synced: bool, now: Instant) -> bool {
        self.last_check_ok
            && instances_synced
            && self.fingerprint == fingerprint
            && now < self.next_full_check
    }

    pub(super) fn last_health(&self) -> &NmxcMetrics {
        &self.last_health
    }

    pub(super) fn rack_domain_uuid(&self) -> Option<NvLinkDomainId> {
        self.rack_domain_uuid
    }
}

/// Offset of a group's first full check within the interval, derived from
/// its id so it is stable across iterations.
fn stagger_offset(group_id: &str, full_check_interval: Duration) -> Duration {
    let interval_nanos = full_check_interval.as_nanos();
    if interval_nanos == 0 {
        return Duration::ZERO;
    }
    let mut hasher = DefaultHasher::new();
    group_id.hash(&mut hasher);
    let offset = u128::from(hasher.finish()) % interval_nanos;
    Duration::from_nanos(u64::try_from(offset).unwrap_or(u64::MAX))
}

/// Whether every instance in the group has its NVLink config applied
/// according to the stored observation, by the same rule that gates the
/// instance state. Hosts without an instance have no pending config.
pub(super) fn group_instances_synced(snapshots: &[&ManagedHostStateSnapshot]) -> bool {
    snapshots.iter().all(|snapshot| {
        snapshot.instance.as_ref().is_none_or(|instance| {
            nvlink_config_synced(
                snapshot
                    .host_snapshot
                    .status
                    .nvlink_status_observation
                    .as_ref(),
                Some(&instance.config.nvlink),
            )
            .is_ok()
        })
    })
}

/// Hashes the database inputs that decide what a group's check would do.
/// The NMX-C side is deliberately absent: the fingerprint says whether the
/// desired state changed, the full check finds drift in the observed state.
pub(super) fn group_fingerprint(input: &ProcessMachineGroupInput<'_>) -> u64 {
    let mut hasher = DefaultHasher::new();
    input.group_type.as_str().hash(&mut hasher);
    input.endpoint_url.hash(&mut hasher);

    let mut snapshots: Vec<&ManagedHostStateSnapshot> = input.snapshots.to_vec();
    snapshots.sort_by_key(|snapshot| snapshot.host_snapshot.id);
    let mut domains = HashSet::new();
    for snapshot in snapshots {
        let host_id = snapshot.host_snapshot.id;
        host_id.hash(&mut hasher);
        snapshot.use_admin_network().hash(&mut hasher);
        managed_host_chassis_serial(snapshot).hash(&mut hasher);
        match &snapshot.instance {
            None => false.hash(&mut hasher),
            Some(instance) => {
                true.hash(&mut hasher);
                instance.id.hash(&mut hasher);
                instance
                    .nvlink_config_version
                    .version_nr()
                    .hash(&mut hasher);
                for gpu in &instance.config.nvlink.gpu_configs {
                    gpu.device_instance.hash(&mut hasher);
                    gpu.logical_partition_id.hash(&mut hasher);
                }
            }
        }
        match input
            .machine_nvlink_info
            .get(&host_id)
            .and_then(Option::as_ref)
        {
            None => false.hash(&mut hasher),
            Some(info) => {
                true.hash(&mut hasher);
                info.domain_uuid.hash(&mut hasher);
                info.chassis_serial.hash(&mut hasher);
                for gpu in &info.gpus {
                    (gpu.tray_index, gpu.slot_id, gpu.device_id, gpu.guid).hash(&mut hasher);
                }
                domains.insert(info.domain_uuid);
            }
        }
    }

    let mut partitions: Vec<_> = input
        .db_nvl_partitions
        .iter()
        .filter(|partition| domains.contains::<NvLinkDomainId>(&partition.domain_uuid))
        .collect();
    partitions.sort_by_key(|partition| partition.id);
    let mut referenced_logical_partitions = HashSet::new();
    for partition in partitions {
        partition.id.hash(&mut hasher);
        partition.nmx_c_partition_id.hash(&mut hasher);
        partition.logical_partition_id.hash(&mut hasher);
        partition.domain_uuid.hash(&mut hasher);
        partition.deleted.is_some().hash(&mut hasher);
        referenced_logical_partitions.extend(partition.logical_partition_id);
    }

    // Only the groups holding a deleted logical partition's physical
    // partitions have work to do for it.
    let mut deleted_logical_partitions: Vec<_> = input
        .db_nvl_logical_partitions
        .iter()
        .filter(|partition| {
            referenced_logical_partitions.contains(&partition.id)
                && model::nvl_logical_partition::is_marked_as_deleted(partition)
        })
        .map(|partition| partition.id)
        .collect();
    deleted_logical_partitions.sort();
    deleted_logical_partitions.hash(&mut hasher);

    hasher.finish()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    const INTERVAL: Duration = Duration::from_secs(600);

    fn state(
        fingerprint: u64,
        last_check_ok: bool,
        next_full_check: Instant,
    ) -> GroupReconcileState {
        GroupReconcileState {
            fingerprint,
            last_check_ok,
            next_full_check,
            last_health: NmxcMetrics::default(),
            rack_domain_uuid: None,
        }
    }

    #[test]
    fn skip_needs_unchanged_inputs_synced_instances_a_good_check_and_time_left() {
        struct Case {
            same_fingerprint: bool,
            last_check_ok: bool,
            instances_synced: bool,
            due: bool,
        }

        let now = Instant::now();
        check_values(
            [
                Check {
                    scenario: "unchanged, synced, last check ok, not due",
                    input: Case {
                        same_fingerprint: true,
                        last_check_ok: true,
                        instances_synced: true,
                        due: false,
                    },
                    expect: true,
                },
                Check {
                    scenario: "unchanged but an instance is pending",
                    input: Case {
                        same_fingerprint: true,
                        last_check_ok: true,
                        instances_synced: false,
                        due: false,
                    },
                    expect: false,
                },
                Check {
                    scenario: "unchanged but the full check is due",
                    input: Case {
                        same_fingerprint: true,
                        last_check_ok: true,
                        instances_synced: true,
                        due: true,
                    },
                    expect: false,
                },
                Check {
                    scenario: "unchanged but the last check failed",
                    input: Case {
                        same_fingerprint: true,
                        last_check_ok: false,
                        instances_synced: true,
                        due: false,
                    },
                    expect: false,
                },
                Check {
                    scenario: "changed inputs with time left",
                    input: Case {
                        same_fingerprint: false,
                        last_check_ok: true,
                        instances_synced: true,
                        due: false,
                    },
                    expect: false,
                },
            ],
            |case| {
                let next_full_check = if case.due { now } else { now + INTERVAL };
                let state = state(1, case.last_check_ok, next_full_check);
                state.can_skip(
                    if case.same_fingerprint { 1 } else { 2 },
                    case.instances_synced,
                    now,
                )
            },
        );
    }

    #[test]
    fn full_check_schedules_the_next_one_by_history_and_changes() {
        #[derive(Debug, PartialEq)]
        enum NextFullCheck {
            Now,
            WithinInterval,
            AtInterval,
        }

        struct Case {
            previous: bool,
            applied_changes: bool,
        }

        let now = Instant::now();
        check_values(
            [
                Check {
                    scenario: "first check is staggered over the interval",
                    input: Case {
                        previous: false,
                        applied_changes: false,
                    },
                    expect: NextFullCheck::WithinInterval,
                },
                Check {
                    scenario: "repeat check waits the full interval",
                    input: Case {
                        previous: true,
                        applied_changes: false,
                    },
                    expect: NextFullCheck::AtInterval,
                },
                Check {
                    scenario: "applied changes are re-checked next iteration",
                    input: Case {
                        previous: true,
                        applied_changes: true,
                    },
                    expect: NextFullCheck::Now,
                },
            ],
            |case| {
                let previous = state(7, true, now);
                let state = GroupReconcileState::after_full_check(
                    case.previous.then_some(&previous),
                    "rack-1",
                    FullCheckOutcome {
                        fingerprint: 7,
                        check_ok: true,
                        applied_changes: case.applied_changes,
                        health: NmxcMetrics::default(),
                        rack_domain_uuid: None,
                    },
                    now,
                    INTERVAL,
                );
                assert_eq!(state.fingerprint, 7);
                assert!(state.last_check_ok);
                if state.next_full_check == now {
                    NextFullCheck::Now
                } else if state.next_full_check == now + INTERVAL {
                    NextFullCheck::AtInterval
                } else if state.next_full_check < now + INTERVAL {
                    NextFullCheck::WithinInterval
                } else {
                    panic!("next full check beyond the interval")
                }
            },
        );
    }

    #[test]
    fn stagger_offset_is_stable_per_group_and_inside_the_interval() {
        let offset = stagger_offset("rack-1", INTERVAL);
        assert_eq!(offset, stagger_offset("rack-1", INTERVAL));
        assert!(offset < INTERVAL);
        assert_eq!(stagger_offset("rack-1", Duration::ZERO), Duration::ZERO);
    }
}
