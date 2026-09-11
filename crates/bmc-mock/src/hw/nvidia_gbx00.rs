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

//! Common functions for GB200 and GB300.

use std::borrow::Cow;

use serde_json::json;

use crate::hw::rack::{RackPlacement, TrayPlacement};
use crate::redfish;

/// The topology id an NVL72 rack's compute trays report.
const NVL72_TOPOLOGY_ID: u32 = 128;
const CBC_REVISION_ID: u32 = 2;
/// Compute trays do not begin at slot zero, so the chassis slot number is the
/// tray index plus this offset.
const CBC_CHASSIS_PHYSICAL_SLOT_OFFSET: u32 = 10;

// NVL72 layout: eighteen compute trays in two banks, with the nine NVLink
// switch trays between them.
const FIRST_COMPUTE_RANGE_START: u8 = 11;
const FIRST_COMPUTE_RANGE_END: u8 = 18;
const SWITCH_RANGE_START: u8 = 19;
const SWITCH_RANGE_END: u8 = 27;
const SECOND_COMPUTE_RANGE_START: u8 = 28;
const SECOND_COMPUTE_RANGE_END: u8 = 37;
const FIRST_COMPUTE_BANK_SIZE: u8 = FIRST_COMPUTE_RANGE_END - FIRST_COMPUTE_RANGE_START + 1;

/// Placement of the unit at a rack position in an NVL72 rack.
///
/// Compute trays are indexed from the bottom across both banks and report a
/// chassis slot offset from that index. Switch trays are indexed from the
/// bottom of their bank; a switch tray's chassis publishes no slot of its own,
/// so the rack unit it occupies is the one physical slot that exists for it.
/// Any other position holds no tray.
pub(crate) fn nvl72_placement(position: u8) -> RackPlacement {
    let tray = match position {
        FIRST_COMPUTE_RANGE_START..=FIRST_COMPUTE_RANGE_END => {
            Some(compute_tray(position - FIRST_COMPUTE_RANGE_START))
        }
        SECOND_COMPUTE_RANGE_START..=SECOND_COMPUTE_RANGE_END => Some(compute_tray(
            position - SECOND_COMPUTE_RANGE_START + FIRST_COMPUTE_BANK_SIZE,
        )),
        SWITCH_RANGE_START..=SWITCH_RANGE_END => Some(TrayPlacement::Switch {
            tray_index: position - SWITCH_RANGE_START,
            slot_number: u32::from(position),
        }),
        _ => None,
    };
    RackPlacement::new(position, NVL72_TOPOLOGY_ID, tray)
}

fn compute_tray(tray_index: u8) -> TrayPlacement {
    TrayPlacement::Compute {
        tray_index,
        chassis_physical_slot_number: u32::from(tray_index) + CBC_CHASSIS_PHYSICAL_SLOT_OFFSET,
    }
}

pub(crate) struct Topology {
    pub(crate) chassis_physical_slot_number: u32,
    pub(crate) compute_tray_index: u32,
    pub(crate) revision_id: u32,
    pub(crate) topology_id: u32,
}

impl Topology {
    pub(crate) fn from_rack_placement(placement: RackPlacement) -> Option<Self> {
        let TrayPlacement::Compute {
            tray_index,
            chassis_physical_slot_number,
        } = placement.tray()?
        else {
            return None;
        };
        Some(Self {
            chassis_physical_slot_number,
            compute_tray_index: u32::from(tray_index),
            revision_id: CBC_REVISION_ID,
            topology_id: placement.topology_id(),
        })
    }
}

// CBC chassis definition.
pub(super) fn cbc_chassis(
    chassis_id: Cow<'static, str>,
    topology: Option<&Topology>,
) -> redfish::chassis::SingleChassisConfig {
    redfish::chassis::SingleChassisConfig {
        id: chassis_id,
        chassis_type: "Component".into(),
        manufacturer: Some("Nvidia".into()),
        part_number: Some("750-0567-002".into()),
        model: Some("18x1RU CBL Cartridge".into()),
        serial_number: Some("1821220000000".into()),
        pcie_devices: Some(vec![]),
        oem: topology.map(|topology| {
            json!({
                "Nvidia": {
                    "@odata.type": "#NvidiaChassis.v1_4_0.NvidiaCBCChassis",
                    "ChassisPhysicalSlotNumber": topology.chassis_physical_slot_number,
                    "ComputeTrayIndex": topology.compute_tray_index,
                    "RevisionId": topology.revision_id,
                    "TopologyId": topology.topology_id,
                }
            })
        }),
        ..redfish::chassis::SingleChassisConfig::defaults()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::{RackInfo, RackType};

    #[derive(Debug)]
    struct Input {
        rack_type: RackType,
        position: u8,
    }

    #[test]
    fn cbc_values_follow_rack_placement() {
        check_values(
            [
                Check {
                    scenario: "first lower compute tray",
                    input: Input {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        position: 11,
                    },
                    expect: Some((10, 0, 2, 128)),
                },
                Check {
                    scenario: "last lower compute tray",
                    input: Input {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        position: 18,
                    },
                    expect: Some((17, 7, 2, 128)),
                },
                Check {
                    scenario: "first upper compute tray",
                    input: Input {
                        rack_type: RackType::LenovoGb300Nvl72,
                        position: 28,
                    },
                    expect: Some((18, 8, 2, 128)),
                },
                Check {
                    scenario: "last upper compute tray",
                    input: Input {
                        rack_type: RackType::LenovoGb300Nvl72,
                        position: 37,
                    },
                    expect: Some((27, 17, 2, 128)),
                },
                Check {
                    scenario: "non-compute rack member",
                    input: Input {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        position: 19,
                    },
                    expect: None,
                },
            ],
            |input| {
                let placement = RackInfo {
                    rack_type: input.rack_type,
                }
                .placement(input.position);
                let topology = Topology::from_rack_placement(placement);
                let chassis = cbc_chassis("CBC_0".into(), topology.as_ref());
                let nvidia = chassis.oem?.get("Nvidia")?.clone();
                Some((
                    nvidia.get("ChassisPhysicalSlotNumber")?.as_u64()?,
                    nvidia.get("ComputeTrayIndex")?.as_u64()?,
                    nvidia.get("RevisionId")?.as_u64()?,
                    nvidia.get("TopologyId")?.as_u64()?,
                ))
            },
        );
    }

    fn switch(position: u8) -> Option<(u8, u32)> {
        match nvl72_placement(position).tray()? {
            TrayPlacement::Switch {
                tray_index,
                slot_number,
            } => Some((tray_index, slot_number)),
            TrayPlacement::Compute { .. } => None,
        }
    }

    fn compute(position: u8) -> Option<(u8, u32)> {
        match nvl72_placement(position).tray()? {
            TrayPlacement::Compute {
                tray_index,
                chassis_physical_slot_number,
            } => Some((tray_index, chassis_physical_slot_number)),
            TrayPlacement::Switch { .. } => None,
        }
    }

    #[test]
    fn every_position_is_at_most_one_kind_of_tray() {
        for position in 1..=48u8 {
            assert!(
                !(compute(position).is_some() && switch(position).is_some()),
                "position {position} cannot be both a compute and a switch tray"
            );
            assert_eq!(nvl72_placement(position).position(), position);
            assert_eq!(nvl72_placement(position).topology_id(), NVL72_TOPOLOGY_ID);
        }
    }

    #[test]
    fn switch_trays_are_indexed_from_the_bottom_of_their_bank() {
        assert_eq!(switch(19), Some((0, 19)));
        assert_eq!(switch(27), Some((8, 27)));

        // Compute trays and power shelves are not switch trays.
        for position in [11, 18, 28, 37, 6, 9, 39, 42] {
            assert_eq!(switch(position), None, "{position}");
        }
    }

    #[test]
    fn compute_trays_keep_their_chassis_numbering() {
        assert_eq!(compute(11), Some((0, 10)));
        assert_eq!(compute(18), Some((7, 17)));
        assert_eq!(compute(28), Some((8, 18)));
        assert_eq!(compute(37), Some((17, 27)));

        // Switch trays and power shelves are not compute trays.
        for position in [19, 27, 6, 9, 39, 42] {
            assert_eq!(compute(position), None, "{position}");
            assert_eq!(nvl72_placement(position).compute_tray_index(), None);
        }
    }
}
