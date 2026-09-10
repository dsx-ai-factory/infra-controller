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

use crate::HardwareType;

/// Compute trays do not begin at slot zero, so the chassis slot number is the
/// tray index plus this offset.
pub(crate) const CBC_CHASSIS_PHYSICAL_SLOT_OFFSET: u32 = 10;

const FIRST_COMPUTE_RANGE_START: u8 = 11;
const FIRST_COMPUTE_RANGE_END: u8 = 18;
const SECOND_COMPUTE_RANGE_START: u8 = 28;
const SECOND_COMPUTE_RANGE_END: u8 = 37;
const SWITCH_RANGE_START: u8 = 19;
const SWITCH_RANGE_END: u8 = 27;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RackPlacement {
    position: u8,
    topology_id: u32,
}

impl RackPlacement {
    pub(crate) fn new(position: u8, topology_id: u32) -> Self {
        Self {
            position,
            topology_id,
        }
    }

    pub fn position(self) -> u8 {
        self.position
    }

    pub fn topology_id(self) -> u32 {
        self.topology_id
    }

    /// The tray's index within its rack, if this position holds a compute
    /// tray at all.
    ///
    /// Public because the RMS simulator reports the same index to NICo that
    /// the Redfish chassis reports for the same simulated node; deriving it
    /// twice would let the two drift apart.
    pub fn compute_tray_index(self) -> Option<u8> {
        match self.position {
            FIRST_COMPUTE_RANGE_START..=FIRST_COMPUTE_RANGE_END => {
                Some(self.position - FIRST_COMPUTE_RANGE_START)
            }
            SECOND_COMPUTE_RANGE_START..=SECOND_COMPUTE_RANGE_END => {
                Some(self.position - SECOND_COMPUTE_RANGE_START + 8)
            }
            _ => None,
        }
    }

    /// The physical slot number the chassis reports for this position.
    ///
    /// Offset from the tray index because the rack's compute trays do not
    /// start at slot zero.
    pub fn chassis_physical_slot_number(self) -> Option<u32> {
        Some(u32::from(self.compute_tray_index()?) + CBC_CHASSIS_PHYSICAL_SLOT_OFFSET)
    }

    /// The switch tray's index within its rack, if this position holds an
    /// NVLink switch tray at all.
    ///
    /// The nine switch trays sit between the two compute banks and are
    /// indexed from the bottom, so the index is unique within the rack. The
    /// RMS simulator reports it, and NICo orders a rack's switches by it.
    pub fn switch_tray_index(self) -> Option<u8> {
        match self.position {
            SWITCH_RANGE_START..=SWITCH_RANGE_END => Some(self.position - SWITCH_RANGE_START),
            _ => None,
        }
    }

    /// The slot number reported for an NVLink switch tray: the rack unit it
    /// occupies. A switch tray's Redfish chassis publishes no slot of its
    /// own, so the rack unit is the one physical slot that exists for it.
    pub fn switch_slot_number(self) -> Option<u32> {
        self.switch_tray_index()?;
        Some(u32::from(self.position))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RackUnit {
    pub position: u8,
    pub hardware_type: HardwareType,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RackElevation {
    pub version: u32,
    pub units: Vec<RackUnit>,
}

#[cfg(test)]
mod tests {
    use super::RackPlacement;

    fn placement(position: u8) -> RackPlacement {
        RackPlacement::new(position, 128)
    }

    #[test]
    fn compute_and_switch_positions_are_disjoint() {
        for position in 1..=42u8 {
            let p = placement(position);
            assert!(
                !(p.compute_tray_index().is_some() && p.switch_tray_index().is_some()),
                "position {position} cannot be both a compute and a switch tray"
            );
        }
    }

    #[test]
    fn switch_trays_are_indexed_from_the_bottom_of_their_bank() {
        assert_eq!(placement(19).switch_tray_index(), Some(0));
        assert_eq!(placement(27).switch_tray_index(), Some(8));
        assert_eq!(placement(19).switch_slot_number(), Some(19));
        assert_eq!(placement(27).switch_slot_number(), Some(27));

        // Compute trays and power shelves are not switch trays.
        for position in [11, 18, 28, 37, 6, 9, 39, 42] {
            assert_eq!(placement(position).switch_tray_index(), None, "{position}");
            assert_eq!(placement(position).switch_slot_number(), None, "{position}");
        }
    }

    #[test]
    fn compute_trays_keep_their_chassis_numbering() {
        assert_eq!(placement(11).compute_tray_index(), Some(0));
        assert_eq!(placement(11).chassis_physical_slot_number(), Some(10));
        assert_eq!(placement(28).compute_tray_index(), Some(8));
        assert_eq!(placement(28).chassis_physical_slot_number(), Some(18));
        assert_eq!(placement(19).compute_tray_index(), None);
        assert_eq!(placement(19).chassis_physical_slot_number(), None);
    }
}
