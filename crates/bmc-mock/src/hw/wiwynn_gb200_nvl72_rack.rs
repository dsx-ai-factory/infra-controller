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

use crate::hw::rack::{RackElevation, RackPlacement};
use crate::{HardwareType, hw};

pub(crate) struct WiwynnGB200Nvl72Rack;

impl WiwynnGB200Nvl72Rack {
    pub(crate) fn rack_elevation(&self) -> RackElevation {
        hw::nvidia_gb200::nvl72_rack_elevation(
            HardwareType::WiwynnGB200Nvl,
            HardwareType::LiteOnPowerShelf,
            HardwareType::NvidiaSwitchNd5200Ld,
        )
    }

    pub(crate) fn placement(&self, position: u8) -> RackPlacement {
        hw::nvidia_gbx00::nvl72_placement(position)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn has_expected_elevation() {
        let elevation = WiwynnGB200Nvl72Rack.rack_elevation();

        assert_eq!(elevation.version, 1);
        assert_eq!(elevation.units.len(), 35);
        assert_eq!(
            elevation
                .units
                .iter()
                .map(|unit| unit.position)
                .collect::<BTreeSet<_>>()
                .len(),
            35
        );
        assert_eq!(
            elevation
                .units
                .iter()
                .filter(|unit| unit.hardware_type == HardwareType::WiwynnGB200Nvl)
                .map(|unit| unit.position)
                .collect::<Vec<_>>(),
            (11..=18).chain(28..=37).collect::<Vec<_>>()
        );
        assert_eq!(
            elevation
                .units
                .iter()
                .filter(|unit| unit.hardware_type == HardwareType::NvidiaSwitchNd5200Ld)
                .map(|unit| unit.position)
                .collect::<Vec<_>>(),
            (19..=27).collect::<Vec<_>>()
        );
        assert_eq!(
            elevation
                .units
                .iter()
                .filter(|unit| unit.hardware_type == HardwareType::LiteOnPowerShelf)
                .map(|unit| unit.position)
                .collect::<Vec<_>>(),
            (6..=9).chain(39..=42).collect::<Vec<_>>()
        );
        assert!(
            elevation
                .units
                .iter()
                .all(|unit| unit.hardware_type.fixed_number_of_dpu().is_some())
        );
    }
}
