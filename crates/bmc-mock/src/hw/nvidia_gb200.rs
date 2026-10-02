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
use std::fmt;

use crate::hw::rack::{RackElevation, RackUnit};
use crate::{HardwareType, redfish};

pub(crate) fn nvl72_rack_elevation(
    compute_tray: HardwareType,
    power_shelf: HardwareType,
    nvlink_switch_tray: HardwareType,
) -> RackElevation {
    let mut units = Vec::with_capacity(35);

    units.extend((11..=18).chain(28..=37).map(|position| RackUnit {
        position,
        hardware_type: compute_tray,
    }));
    units.extend((19..=27).map(|position| RackUnit {
        position,
        hardware_type: nvlink_switch_tray,
    }));
    units.extend((6..=9).chain(39..=42).map(|position| RackUnit {
        position,
        hardware_type: power_shelf,
    }));
    units.sort_unstable_by_key(|unit| unit.position);

    RackElevation { version: 1, units }
}

#[derive(Clone, Copy)]
pub(crate) enum BoardIndex {
    Board0,
    Board1,
}

impl fmt::Display for BoardIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Board0 => "0",
            Self::Board1 => "1",
        }
        .fmt(f)
    }
}

pub(crate) struct BiancaBoard<'a> {
    pub(crate) index: BoardIndex,
    pub(crate) cpu_serial_number: Cow<'a, str>,
    pub(crate) gpu_serial_number: Cow<'a, str>,
}

struct GpuChassisIds {
    chassis_id: Cow<'static, str>,
    pcie_device_id: Cow<'static, str>,
    index: usize,
}

const GPU_MODEL: &str = "GB200 186GB HBM3e";

/// Stable per-slot GPU UUID, so a test can assert that a given slot reports a
/// specific device identity.
///
/// Hardware reports one UUID per GPU on the GPU's `Processor`, its enclosing
/// `Chassis` and its `PCIeDevice` alike, so every fixture for a slot uses this.
fn gpu_uuid(index: usize) -> String {
    format!("2b8c1f4a-6d33-4a11-9c02-7f5e0a1b{index:04x}")
}

impl BiancaBoard<'_> {
    pub(super) fn hgx_cpu_chassis(
        &self,
        id: Cow<'static, str>,
    ) -> redfish::chassis::SingleChassisConfig {
        let sensors = redfish::sensor::generate_chassis_sensors(
            &id,
            redfish::sensor::Layout {
                temperature: 2,
                power: 3,
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
            part_number: Some("900-2G548-0001-000".into()),
            model: Some("Grace A02P".into()),
            serial_number: Some(self.cpu_serial_number.to_string().into()),
            sensors: Some(sensors),
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }

    fn gpu_base_index(&self) -> usize {
        match self.index {
            BoardIndex::Board0 => 0,
            BoardIndex::Board1 => 2,
        }
    }

    fn gpu_chassis_ids(&self) -> [GpuChassisIds; 2] {
        let base = self.gpu_base_index();
        [0, 1].map(|local| {
            let n = base + local;
            GpuChassisIds {
                chassis_id: format!("HGX_GPU_{n}").into(),
                pcie_device_id: format!("GPU_{n}").into(),
                index: n,
            }
        })
    }

    pub(super) fn hgx_gpu_processors(&self, system_id: &str) -> [redfish::processor::Processor; 2] {
        self.gpu_chassis_ids().map(|ids| {
            let voltage_sensor_id =
                redfish::sensor::sensor_id(redfish::sensor::SensorKind::Voltage, 1);
            redfish::processor::gpu(
                system_id,
                &ids.pcie_device_id,
                redfish::sensor::chassis_resource(&ids.chassis_id, &voltage_sensor_id)
                    .odata_id
                    .as_ref(),
                &redfish::processor::GpuIdentity {
                    uuid: &gpu_uuid(ids.index),
                    serial_number: &self.gpu_serial_number,
                    model: GPU_MODEL,
                },
            )
        })
    }

    /// The HBM stack behind each GPU on this board.
    ///
    /// Capacity mirrors the `GB200 186GB HBM3e` model the GPU chassis
    /// reports.
    pub(super) fn hgx_gpu_memory(&self, system_id: &str) -> [redfish::memory::Memory; 2] {
        self.gpu_chassis_ids().map(|ids| {
            let memory_id = format!("{}_DRAM_0", ids.pcie_device_id);
            redfish::memory::hbm(system_id, &memory_id, 186 * 1024)
        })
    }

    pub(super) fn hgx_gpu_chassis(&self) -> [redfish::chassis::SingleChassisConfig; 2] {
        self.gpu_chassis_ids().map(|ids| {
            let sensors = redfish::sensor::generate_chassis_sensors(
                &ids.chassis_id,
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
                chassis_type: "Component".into(),
                manufacturer: Some("NVIDIA".into()),
                part_number: Some("NA".into()),
                model: Some(GPU_MODEL.into()),
                serial_number: Some(self.gpu_serial_number.to_string().into()),
                uuid: Some(gpu_uuid(ids.index).into()),
                pcie_devices: Some(vec![
                    redfish::pcie_device::builder(&redfish::pcie_device::chassis_resource(
                        &ids.chassis_id,
                        &ids.pcie_device_id,
                    ))
                    .manufacturer("NVIDIA")
                    .model(GPU_MODEL)
                    .part_number("2941-892-A1")
                    .serial_number(&self.gpu_serial_number)
                    .uuid(&gpu_uuid(ids.index))
                    .build(),
                ]),
                id: ids.chassis_id,
                sensors: Some(sensors),
                ..redfish::chassis::SingleChassisConfig::defaults()
            }
        })
    }
}

pub(crate) struct IoBoard<'a> {
    pub(crate) serial_number: Cow<'a, str>,
}

impl IoBoard<'_> {
    pub(super) fn as_chassis(
        &self,
        id: Cow<'static, str>,
    ) -> redfish::chassis::SingleChassisConfig {
        let sensors = redfish::sensor::generate_chassis_sensors(
            &id,
            redfish::sensor::Layout {
                temperature: 4,
                ..Default::default()
            },
        );
        redfish::chassis::SingleChassisConfig {
            chassis_type: "Component".into(),
            manufacturer: Some("Nvidia".into()),
            part_number: Some("900-24768-0002-000".into()),
            model: Some("2x ConnectX-7 Mezz".into()),
            serial_number: Some(self.serial_number.to_string().into()),
            network_adapters: Some(
                (0..2)
                    .map(|n| {
                        redfish::network_adapter::builder(
                            &redfish::network_adapter::chassis_resource(
                                &id,
                                &format!("{id}_CX7_{n}"),
                            ),
                        )
                        .manufacturer("Nvidia")
                        .model("2x ConnectX-7 Mezz")
                        .part_number("900-24768-0002-000")
                        .serial_number(&self.serial_number)
                        .build()
                    })
                    .collect(),
            ),
            pcie_devices: Some(vec![]),
            sensors: Some(sensors),
            id,
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }
}
