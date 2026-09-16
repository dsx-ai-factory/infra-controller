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

pub(crate) struct LenovoGB300Nvl72Rack;

impl LenovoGB300Nvl72Rack {
    pub(crate) fn rack_elevation(&self) -> RackElevation {
        hw::nvidia_gb300::nvl72_rack_elevation(
            HardwareType::LenovoGB300Nvl,
            HardwareType::LiteOnPowerShelf,
            HardwareType::NvidiaSwitchN5700Ld,
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
        let elevation = LenovoGB300Nvl72Rack.rack_elevation();

        assert_eq!(elevation.version, 1);
        assert_eq!(elevation.units.len(), 33);
        assert_eq!(
            elevation
                .units
                .iter()
                .map(|unit| unit.position)
                .collect::<BTreeSet<_>>()
                .len(),
            33
        );
        assert_eq!(
            elevation
                .units
                .iter()
                .filter(|unit| unit.hardware_type == HardwareType::LenovoGB300Nvl)
                .map(|unit| unit.position)
                .collect::<Vec<_>>(),
            (11..=18).chain(28..=37).collect::<Vec<_>>()
        );
        assert_eq!(
            elevation
                .units
                .iter()
                .filter(|unit| unit.hardware_type == HardwareType::NvidiaSwitchN5700Ld)
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
            (7..=9).chain(39..=41).collect::<Vec<_>>()
        );
        assert!(
            elevation
                .units
                .iter()
                .all(|unit| unit.hardware_type.fixed_number_of_dpu().is_some())
        );
    }
}
