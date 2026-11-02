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

/// Where a unit sits in its rack and what its platform reports for it.
///
/// Built by the rack's platform implementation (see `RackInfo::placement`),
/// which owns the arithmetic that turns a rack position into the numbers a
/// chassis reports. Redfish and RMS both read the result from here, so they
/// cannot disagree about where a node sits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RackPlacement {
    position: u8,
    topology_id: u32,
    tray: Option<TrayPlacement>,
}

/// The platform's numbering for the tray at a position, when it holds one.
/// Power shelves and empty positions have none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayPlacement {
    /// A compute tray: its index among the rack's compute trays and the
    /// physical slot number its chassis reports.
    Compute {
        tray_index: u8,
        chassis_physical_slot_number: u32,
    },
    /// An NVLink switch tray: its index among the rack's switch trays and the
    /// slot number reported for it.
    Switch { tray_index: u8, slot_number: u32 },
}

impl RackPlacement {
    pub(crate) fn new(position: u8, topology_id: u32, tray: Option<TrayPlacement>) -> Self {
        Self {
            position,
            topology_id,
            tray,
        }
    }

    pub fn position(self) -> u8 {
        self.position
    }

    pub fn topology_id(self) -> u32 {
        self.topology_id
    }

    pub fn tray(self) -> Option<TrayPlacement> {
        self.tray
    }

    pub(crate) fn compute_tray_index(self) -> Option<u8> {
        match self.tray? {
            TrayPlacement::Compute { tray_index, .. } => Some(tray_index),
            TrayPlacement::Switch { .. } => None,
        }
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
