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

use std::borrow::Cow;

use crate::hw::rack::{RackElevation, RackUnit};
use crate::{HardwareType, redfish};

pub(crate) fn nvl72_rack_elevation(
    compute_tray: HardwareType,
    power_shelf: HardwareType,
    nvlink_switch_tray: HardwareType,
) -> RackElevation {
    let mut units = Vec::with_capacity(33);

    units.extend((11..=18).chain(28..=37).map(|position| RackUnit {
        position,
        hardware_type: compute_tray,
    }));
    units.extend((19..=27).map(|position| RackUnit {
        position,
        hardware_type: nvlink_switch_tray,
    }));
    units.extend((7..=9).chain(39..=41).map(|position| RackUnit {
        position,
        hardware_type: power_shelf,
    }));
    units.sort_unstable_by_key(|unit| unit.position);

    RackElevation { version: 1, units }
}

pub(crate) struct NvidiaGB300Gpu<'a> {
    pub(crate) serial_number: Cow<'a, str>,
}

impl NvidiaGB300Gpu<'_> {
    pub(super) fn as_hgx_chassis(
        &self,
        id: Cow<'static, str>,
    ) -> redfish::chassis::SingleChassisConfig {
        let sensors = redfish::sensor::generate_chassis_sensors(
            &id,
            redfish::sensor::Layout {
                temperature: 3,
                power: 2,
                voltage: 1,
                fan: 0,
                current: 0,
                // + 1 Energy
            },
        );
        redfish::chassis::SingleChassisConfig {
            id,
            chassis_type: "Component".into(),
            manufacturer: Some("NVIDIA".into()),
            part_number: Some("SC57C26750".into()),
            model: Some("NVIDIA GB300".into()),
            serial_number: Some(self.serial_number.to_string().into()),
            sensors: Some(sensors),
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }
}

pub(crate) struct NvidiaGB300Cpu<'a> {
    pub(crate) serial_number: Cow<'a, str>,
}

impl NvidiaGB300Cpu<'_> {
    pub(super) fn as_hgx_chassis(
        &self,
        id: Cow<'static, str>,
    ) -> redfish::chassis::SingleChassisConfig {
        let sensors = redfish::sensor::generate_chassis_sensors(
            &id,
            redfish::sensor::Layout {
                temperature: 2,
                power: 5,
                voltage: 2,
                fan: 0,
                current: 0,
                // + 1 Energy
                // + 72 CPU core utilzation
                // + 1 Memory Frequency
            },
        );
        redfish::chassis::SingleChassisConfig {
            id,
            chassis_type: "Component".into(),
            manufacturer: Some("NVIDIA".into()),
            part_number: Some("900-2G548-0081-000".into()),
            model: Some("Grace A02P".into()),
            serial_number: Some(self.serial_number.to_string().into()),
            sensors: Some(sensors),
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }
}

pub(crate) struct NvidiaGB300IoBoard<'a> {
    pub(crate) serial_number: Cow<'a, str>,
}

impl NvidiaGB300IoBoard<'_> {
    pub(super) fn as_chassis(
        &self,
        id: Cow<'static, str>,
    ) -> redfish::chassis::SingleChassisConfig {
        let sensors = redfish::sensor::generate_chassis_sensors(
            &id,
            redfish::sensor::Layout {
                temperature: 8,
                ..Default::default()
            },
        );
        redfish::chassis::SingleChassisConfig {
            id,
            chassis_type: "Component".into(),
            manufacturer: Some("Nvidia".into()),
            part_number: Some("900-9X86E-00CX-ST0           ".into()),
            model: Some("P4768-B01".into()),
            serial_number: Some(self.serial_number.to_string().into()),
            sensors: Some(sensors),
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }
}
