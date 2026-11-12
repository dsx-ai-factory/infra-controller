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

use std::collections::{HashMap, HashSet};
use std::fmt::Display;

use carbide_utils::none_if_empty::NoneIfEmpty;
use itertools::Itertools;
use mac_address::MacAddress;

use super::host_inventory::host_dpu_inventory_records;
use super::{EndpointExplorationReport, ExploredDpu, HostPrimaryInterfaceSelection};
use crate::machine_boot_interface::BootInterfaceSelectionSource;

/// Result of applying a boot interface selected from a report to DPU ordering.
///
/// An empty DPU list needs no ordering. When DPU records exist, exactly one of
/// their host PF MAC addresses must match the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportSelectionOrdering {
    /// The selected DPU is first in the resulting order.
    Applied,
    /// There were no DPU records to order.
    NoDpus,
    /// DPU records exist, but none has the selected host PF MAC.
    Mismatch,
}

/// Natural sorting key for an entire Redfish `Chassis.id`.
///
/// Text is converted to lowercase and ASCII decimal runs are compared
/// numerically, so values such as `Slot2` sort before `Slot10`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ChassisIdSortKey {
    /// Alternating text and number parts, beginning and ending with text.
    parts: Vec<ChassisIdSortPart>,
}

/// One text or numeric component of a [`ChassisIdSortKey`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ChassisIdSortPart {
    /// Lowercase text between decimal runs.
    Text(String),
    /// An ASCII decimal run. Comparing the normalized length before the digits
    /// provides numeric order without imposing a numeric size limit.
    Number {
        /// Number of normalized decimal digits.
        significant_digit_count: usize,
        /// Decimal digits without leading zeros, or `0` for zero.
        digits: String,
    },
}

/// Why all DPUs could not be ordered by Redfish `Chassis.id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChassisIdOrderingFailure {
    /// At least one DPU has no unambiguous, nonblank host chassis identity.
    MissingChassisId,
    /// Two chassis identities become equal after lowercase and decimal normalization.
    DuplicateChassisId,
    /// The selected DPU has no host PF MAC address to use as the boot interface.
    MissingHostPfMac,
}

impl Display for ChassisIdOrderingFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::MissingChassisId => "missing or ambiguous host chassis ID",
            Self::DuplicateChassisId => "duplicate normalized host chassis ID",
            Self::MissingHostPfMac => "selected DPU has no host PF MAC address",
        };
        f.write_str(message)
    }
}

/// `assign_host_chassis_ids` joins each explored DPU to one host BMC
/// `Chassis.id` through the serial number used for DPU pairing. A missing,
/// blank, or ambiguous chassis identity remains `None`, which prevents a
/// partial chassis ordering later in this pass.
pub fn assign_host_chassis_ids(host_report: &EndpointExplorationReport, dpus: &mut [ExploredDpu]) {
    let chassis_ids = host_chassis_ids_by_dpu_serial(host_report);
    for dpu in dpus {
        dpu.host_chassis_id = dpu
            .report
            .dpu_pairing_serial_number()
            .and_then(|serial| chassis_ids.get(serial))
            .cloned()
            .flatten();
    }
}

/// Orders all DPUs by each full host `Chassis.id` after lowercasing text and
/// comparing ASCII decimal runs numerically. The caller first assigns the
/// trimmed, unambiguous identities through [`assign_host_chassis_ids`].
/// Validation precedes mutation so every failure leaves discovery order
/// intact for the fallback that orders by serial number.
pub fn sort_dpus_by_redfish_chassis_id(
    dpus: &mut [ExploredDpu],
) -> Result<HostPrimaryInterfaceSelection, ChassisIdOrderingFailure> {
    let keys = dpus
        .iter()
        .map(|dpu| {
            let id = dpu
                .host_chassis_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .ok_or(ChassisIdOrderingFailure::MissingChassisId)?;
            Ok(chassis_id_sort_key(id))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut unique = HashSet::with_capacity(keys.len());
    if keys.iter().any(|key| !unique.insert(key)) {
        return Err(ChassisIdOrderingFailure::DuplicateChassisId);
    }

    let mut ordered = dpus.iter().cloned().zip(keys).collect_vec();
    ordered.sort_by(|(_, left), (_, right)| left.cmp(right));
    let mac_address = ordered
        .first()
        .and_then(|(dpu, _)| dpu.host_pf_mac_address)
        .ok_or(ChassisIdOrderingFailure::MissingHostPfMac)?;
    for (target, (dpu, _)) in dpus.iter_mut().zip(ordered) {
        *target = dpu;
    }

    Ok(HostPrimaryInterfaceSelection {
        mac_address,
        source: BootInterfaceSelectionSource::RedfishChassisId,
    })
}

/// Applies the existing case-insensitive Redfish serial number fallback and
/// returns the selected first DPU's host PF MAC with its exact source.
/// `sort_by_key` is stable, so equal and missing serials keep discovery order.
pub fn sort_dpus_by_redfish_serial(
    dpus: &mut [ExploredDpu],
) -> Option<HostPrimaryInterfaceSelection> {
    dpus.sort_by_key(|dpu| {
        dpu.report
            .systems
            .first()
            .and_then(|system| system.serial_number.as_deref())
            .unwrap_or("")
            .to_lowercase()
    });

    dpus.first()
        .and_then(|dpu| dpu.host_pf_mac_address)
        .map(|mac_address| HostPrimaryInterfaceSelection {
            mac_address,
            source: BootInterfaceSelectionSource::RedfishSerialNumber,
        })
}

/// Moves a DPU selected from a report to the established primary position
/// while preserving the existing distinction between a mismatch and no DPUs.
pub fn apply_report_selection(
    dpus: &mut [ExploredDpu],
    mac_address: MacAddress,
) -> ReportSelectionOrdering {
    let Some(primary_dpu_position) = dpus
        .iter()
        .position(|dpu| dpu.host_pf_mac_address.unwrap_or_default() == mac_address)
    else {
        return if dpus.is_empty() {
            ReportSelectionOrdering::NoDpus
        } else {
            ReportSelectionOrdering::Mismatch
        };
    };

    if primary_dpu_position != 0 {
        dpus[..=primary_dpu_position].rotate_right(1);
    }
    ReportSelectionOrdering::Applied
}

/// `host_chassis_ids_by_dpu_serial` keeps one usable parent `Chassis.id` for
/// each host reported DPU serial. A blank identity or conflicting IDs for one
/// serial become `None`, which makes the whole host use the safer serial
/// fallback.
fn host_chassis_ids_by_dpu_serial(
    report: &EndpointExplorationReport,
) -> HashMap<String, Option<String>> {
    let mut chassis_ids = HashMap::<String, Option<String>>::new();
    for chassis in &report.chassis {
        for record in host_dpu_inventory_records(chassis) {
            let Some(serial_number) = record.serial_number.map(str::trim).none_if_empty() else {
                continue;
            };
            let candidate = chassis.id.trim().none_if_empty().map(str::to_string);
            chassis_ids
                .entry(serial_number.to_string())
                .and_modify(|existing| {
                    if existing.as_deref() != candidate.as_deref() {
                        *existing = None;
                    }
                })
                .or_insert(candidate);
        }
    }
    chassis_ids
}

/// Builds a natural sorting key from the full chassis ID.
///
/// Lowercase text stays in place and ASCII decimal runs are normalized without
/// a numeric size limit.
fn chassis_id_sort_key(id: &str) -> ChassisIdSortKey {
    let bytes = id.as_bytes();
    let mut parts = Vec::new();
    let mut cursor = 0;

    while let Some(relative_start) = bytes[cursor..]
        .iter()
        .position(|byte| byte.is_ascii_digit())
    {
        let number_start = cursor + relative_start;
        parts.push(ChassisIdSortPart::Text(
            id[cursor..number_start].to_lowercase(),
        ));

        let number_end = bytes[number_start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .map_or(bytes.len(), |relative_end| number_start + relative_end);
        let digits = id[number_start..number_end].trim_start_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };
        parts.push(ChassisIdSortPart::Number {
            significant_digit_count: digits.len(),
            digits: digits.to_string(),
        });
        cursor = number_end;
    }
    parts.push(ChassisIdSortPart::Text(id[cursor..].to_lowercase()));

    ChassisIdSortKey { parts }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use carbide_test_support::{Check, check_values, value_scenarios};

    use super::*;
    use crate::site_explorer::{Chassis, ComputerSystem, NetworkAdapter};

    #[test]
    fn host_chassis_id_correlation_requires_one_nonblank_parent_identity() {
        let nested = |id: &str, serials: &[&str]| Chassis {
            id: id.to_string(),
            network_adapters: serials
                .iter()
                .enumerate()
                .map(|(index, serial)| NetworkAdapter {
                    id: format!("adapter-{index}"),
                    serial_number: Some((*serial).to_string()),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let direct = |id: &str, serial: &str| Chassis {
            id: id.to_string(),
            part_number: Some("900-9D3B6-00CV-AA0".to_string()),
            serial_number: Some(serial.to_string()),
            ..Default::default()
        };
        let expected = |entries: &[(&str, Option<&str>)]| {
            entries
                .iter()
                .map(|(serial, id)| ((*serial).to_string(), id.map(|id| id.to_string())))
                .collect::<HashMap<_, _>>()
        };

        check_values(
            [
                Check {
                    scenario: "chassis and nested adapter serials both correlate",
                    input: vec![
                        direct("Riser_Slot1", " DPU-1 "),
                        nested("Riser_Slot2", &["DPU-2"]),
                    ],
                    expect: expected(&[
                        ("DPU-1", Some("Riser_Slot1")),
                        ("DPU-2", Some("Riser_Slot2")),
                    ]),
                },
                Check {
                    scenario: "repeated serial under the same chassis retains one identity",
                    input: vec![nested("Riser_Slot1", &["DPU-1", "DPU-1"])],
                    expect: expected(&[("DPU-1", Some("Riser_Slot1"))]),
                },
                Check {
                    scenario: "surrounding chassis identity whitespace is removed",
                    input: vec![nested("  Riser_Slot1 Card  ", &["DPU-1"])],
                    expect: expected(&[("DPU-1", Some("Riser_Slot1 Card"))]),
                },
                Check {
                    scenario: "serial under different chassis objects is ambiguous",
                    input: vec![
                        nested("Riser_Slot1", &["DPU-1"]),
                        nested("Riser_Slot2", &["DPU-1"]),
                    ],
                    expect: expected(&[("DPU-1", None)]),
                },
                Check {
                    scenario: "empty chassis identity is unusable",
                    input: vec![nested("", &["DPU-1"])],
                    expect: expected(&[("DPU-1", None)]),
                },
                Check {
                    scenario: "whitespace-only chassis identity is unusable",
                    input: vec![nested("   ", &["DPU-1"])],
                    expect: expected(&[("DPU-1", None)]),
                },
            ],
            |chassis| {
                host_chassis_ids_by_dpu_serial(&EndpointExplorationReport {
                    chassis,
                    ..Default::default()
                })
            },
        );
    }

    /// Builds one explored DPU for chassis ordering tests. The final MAC byte
    /// keeps expected orders compact and readable.
    fn chassis_ordering_dpu(chassis_id: Option<&str>, mac_last_byte: Option<u8>) -> ExploredDpu {
        ExploredDpu {
            bmc_ip: "192.0.2.1".parse().unwrap(),
            host_pf_mac_address: mac_last_byte.map(fallback_ordering_mac),
            host_chassis_id: chassis_id.map(str::to_string),
            report: Arc::new(EndpointExplorationReport {
                systems: vec![ComputerSystem {
                    serial_number: Some("DPU-SERIAL".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        }
    }

    /// Returns the locally administered MAC used by fallback ordering tests.
    fn fallback_ordering_mac(last_byte: u8) -> MacAddress {
        MacAddress::new([0x02, 0, 0, 0, 0, last_byte])
    }

    #[test]
    fn redfish_chassis_ordering_uses_a_lowercase_natural_key_for_the_full_id() {
        value_scenarios!(run = |mut dpus| {
                let selection = sort_dpus_by_redfish_chassis_id(&mut dpus)
                    .expect("full unique chassis IDs should resolve an order");
                (
                    dpus.iter()
                        .filter_map(|dpu| dpu.host_pf_mac_address)
                        .collect_vec(),
                    selection,
                )
            };
            "slot numbers order naturally after lowercasing" {
                vec![
                    chassis_ordering_dpu(Some("Slot10"), Some(10)),
                    chassis_ordering_dpu(Some("slot2"), Some(2)),
                    chassis_ordering_dpu(Some("SLOT11"), Some(11)),
                    chassis_ordering_dpu(Some("sLoT1"), Some(1)),
                ] => (
                    vec![
                        fallback_ordering_mac(1),
                        fallback_ordering_mac(2),
                        fallback_ordering_mac(10),
                        fallback_ordering_mac(11),
                    ],
                    HostPrimaryInterfaceSelection {
                        mac_address: fallback_ordering_mac(1),
                        source: BootInterfaceSelectionSource::RedfishChassisId,
                    },
                ),
            }
            "multiple numeric runs participate in full ID ordering" {
                vec![
                    chassis_ordering_dpu(Some("Riser2_Slot1"), Some(3)),
                    chassis_ordering_dpu(Some("Riser1_Slot11"), Some(2)),
                    chassis_ordering_dpu(Some("Riser1_Slot2"), Some(1)),
                ] => (
                    vec![
                        fallback_ordering_mac(1),
                        fallback_ordering_mac(2),
                        fallback_ordering_mac(3),
                    ],
                    HostPrimaryInterfaceSelection {
                        mac_address: fallback_ordering_mac(1),
                        source: BootInterfaceSelectionSource::RedfishChassisId,
                    },
                ),
            }
            "digitless identities order by lowercase text" {
                vec![
                    chassis_ordering_dpu(Some("secondaryDpu"), Some(2)),
                    chassis_ordering_dpu(Some("PrimaryDPU"), Some(1)),
                ] => (
                    vec![fallback_ordering_mac(1), fallback_ordering_mac(2)],
                    HostPrimaryInterfaceSelection {
                        mac_address: fallback_ordering_mac(1),
                        source: BootInterfaceSelectionSource::RedfishChassisId,
                    },
                ),
            }
            "different ID structures use full natural order" {
                vec![
                    chassis_ordering_dpu(Some("Riser_Slot2"), Some(2)),
                    chassis_ordering_dpu(Some("Bay11/DPU"), Some(1)),
                ] => (
                    vec![fallback_ordering_mac(1), fallback_ordering_mac(2)],
                    HostPrimaryInterfaceSelection {
                        mac_address: fallback_ordering_mac(1),
                        source: BootInterfaceSelectionSource::RedfishChassisId,
                    },
                ),
            }
            "large decimal runs have no numeric size limit" {
                vec![
                    chassis_ordering_dpu(Some("Slot18446744073709551617"), Some(2)),
                    chassis_ordering_dpu(Some("Slot18446744073709551616"), Some(1)),
                ] => (
                    vec![fallback_ordering_mac(1), fallback_ordering_mac(2)],
                    HostPrimaryInterfaceSelection {
                        mac_address: fallback_ordering_mac(1),
                        source: BootInterfaceSelectionSource::RedfishChassisId,
                    },
                ),
            }
            "text and numeric prefixes remain ordered" {
                vec![
                    chassis_ordering_dpu(Some("Slot1A2"), Some(4)),
                    chassis_ordering_dpu(Some("Slot1A"), Some(3)),
                    chassis_ordering_dpu(Some("Slot1"), Some(2)),
                    chassis_ordering_dpu(Some("Slot"), Some(1)),
                ] => (
                    vec![
                        fallback_ordering_mac(1),
                        fallback_ordering_mac(2),
                        fallback_ordering_mac(3),
                        fallback_ordering_mac(4),
                    ],
                    HostPrimaryInterfaceSelection {
                        mac_address: fallback_ordering_mac(1),
                        source: BootInterfaceSelectionSource::RedfishChassisId,
                    },
                ),
            }
            "Unicode text is lowercased around ASCII decimal runs" {
                vec![
                    chassis_ordering_dpu(Some("Ä10"), Some(2)),
                    chassis_ordering_dpu(Some("ä2"), Some(1)),
                ] => (
                    vec![fallback_ordering_mac(1), fallback_ordering_mac(2)],
                    HostPrimaryInterfaceSelection {
                        mac_address: fallback_ordering_mac(1),
                        source: BootInterfaceSelectionSource::RedfishChassisId,
                    },
                ),
            }
        );
    }

    #[test]
    fn redfish_chassis_ordering_failures_leave_discovery_order_intact() {
        value_scenarios!(run = |mut dpus| {
                let discovery_order = dpus.clone();
                let failure = sort_dpus_by_redfish_chassis_id(&mut dpus)
                    .expect_err("invalid chassis evidence must not resolve an order");
                assert_eq!(dpus, discovery_order);
                failure
            };
            "missing identity" {
                vec![
                    chassis_ordering_dpu(None, Some(1)),
                    chassis_ordering_dpu(Some("Riser_Slot2"), Some(2)),
                ] => ChassisIdOrderingFailure::MissingChassisId,
            }
            "empty identity" {
                vec![
                    chassis_ordering_dpu(Some(""), Some(1)),
                    chassis_ordering_dpu(Some("Riser_Slot2"), Some(2)),
                ] => ChassisIdOrderingFailure::MissingChassisId,
            }
            "equal normalized identities" {
                vec![
                    chassis_ordering_dpu(Some("Slot01"), Some(1)),
                    chassis_ordering_dpu(Some("slot1"), Some(2)),
                ] => ChassisIdOrderingFailure::DuplicateChassisId,
            }
            "selected DPU has no host PF MAC address" {
                vec![
                    chassis_ordering_dpu(Some("Riser_Slot2"), Some(2)),
                    chassis_ordering_dpu(Some("Riser_Slot1"), None),
                ] => ChassisIdOrderingFailure::MissingHostPfMac,
            }
        );
    }

    /// Retains stable serial and report-selection ordering, including absent reports, so deferred discovery cannot panic or change tied fallback winners.
    #[test]
    fn redfish_serial_fallback_preserves_existing_winner_semantics() {
        let dpu = |serial_number: Option<&str>, mac_last_byte| ExploredDpu {
            bmc_ip: "192.0.2.1".parse().unwrap(),
            host_pf_mac_address: Some(fallback_ordering_mac(mac_last_byte)),
            host_chassis_id: None,
            report: Arc::new(EndpointExplorationReport {
                systems: vec![ComputerSystem {
                    serial_number: serial_number.map(str::to_string),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        };
        check_values(
            [
                Check {
                    scenario: "lower serial wins case-insensitively",
                    input: vec![dpu(Some("ZETA"), 1), dpu(Some("alpha"), 2)],
                    expect: (
                        vec![fallback_ordering_mac(2), fallback_ordering_mac(1)],
                        Some(HostPrimaryInterfaceSelection {
                            mac_address: fallback_ordering_mac(2),
                            source: BootInterfaceSelectionSource::RedfishSerialNumber,
                        }),
                    ),
                },
                Check {
                    scenario: "equal serials retain discovery order",
                    input: vec![dpu(Some("ALPHA"), 1), dpu(Some("alpha"), 2)],
                    expect: (
                        vec![fallback_ordering_mac(1), fallback_ordering_mac(2)],
                        Some(HostPrimaryInterfaceSelection {
                            mac_address: fallback_ordering_mac(1),
                            source: BootInterfaceSelectionSource::RedfishSerialNumber,
                        }),
                    ),
                },
                Check {
                    scenario: "missing and empty serials retain discovery order",
                    input: vec![dpu(None, 1), dpu(Some(""), 2)],
                    expect: (
                        vec![fallback_ordering_mac(1), fallback_ordering_mac(2)],
                        Some(HostPrimaryInterfaceSelection {
                            mac_address: fallback_ordering_mac(1),
                            source: BootInterfaceSelectionSource::RedfishSerialNumber,
                        }),
                    ),
                },
                // Serialized explored DPUs omit reports, so an absent first system must remain a safe empty key.
                Check {
                    scenario: "missing report uses the existing empty-key priority",
                    input: vec![
                        dpu(Some("alpha"), 1),
                        ExploredDpu {
                            report: Arc::new(EndpointExplorationReport::default()),
                            ..dpu(None, 2)
                        },
                    ],
                    expect: (
                        vec![fallback_ordering_mac(2), fallback_ordering_mac(1)],
                        Some(HostPrimaryInterfaceSelection {
                            mac_address: fallback_ordering_mac(2),
                            source: BootInterfaceSelectionSource::RedfishSerialNumber,
                        }),
                    ),
                },
                Check {
                    scenario: "missing serial keeps the established empty-key priority",
                    input: vec![dpu(Some("alpha"), 1), dpu(None, 2)],
                    expect: (
                        vec![fallback_ordering_mac(2), fallback_ordering_mac(1)],
                        Some(HostPrimaryInterfaceSelection {
                            mac_address: fallback_ordering_mac(2),
                            source: BootInterfaceSelectionSource::RedfishSerialNumber,
                        }),
                    ),
                },
            ],
            |mut dpus| {
                let selection = sort_dpus_by_redfish_serial(&mut dpus);
                let order = dpus
                    .iter()
                    .map(|dpu| dpu.host_pf_mac_address.unwrap())
                    .collect();
                (order, selection)
            },
        );

        let mac = fallback_ordering_mac;
        check_values(
            [
                Check {
                    scenario: "report-selected second DPU moves to primary position",
                    input: (vec![dpu(Some("one"), 1), dpu(Some("two"), 2)], mac(2)),
                    expect: (ReportSelectionOrdering::Applied, vec![mac(2), mac(1)]),
                },
                Check {
                    scenario: "report-selected first DPU retains order",
                    input: (vec![dpu(Some("one"), 1), dpu(Some("two"), 2)], mac(1)),
                    expect: (ReportSelectionOrdering::Applied, vec![mac(1), mac(2)]),
                },
                Check {
                    scenario: "unknown report-selected MAC remains a blocker",
                    input: (vec![dpu(Some("one"), 1), dpu(Some("two"), 2)], mac(3)),
                    expect: (ReportSelectionOrdering::Mismatch, vec![mac(1), mac(2)]),
                },
                Check {
                    scenario: "a report selection with no managed DPUs remains allowed",
                    input: (vec![], mac(1)),
                    expect: (ReportSelectionOrdering::NoDpus, vec![]),
                },
            ],
            |(mut dpus, selected_mac)| {
                let outcome = apply_report_selection(&mut dpus, selected_mac);
                let order = dpus
                    .iter()
                    .map(|dpu| dpu.host_pf_mac_address.unwrap())
                    .collect();
                (outcome, order)
            },
        );
    }
}
