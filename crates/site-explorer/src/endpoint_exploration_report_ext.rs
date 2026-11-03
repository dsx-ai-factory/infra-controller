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

use std::collections::HashMap;

use carbide_utils::none_if_empty::NoneIfEmpty;
use model::site_explorer::{
    Chassis, EndpointExplorationReport, PCIeDevice, is_bluefield_part_number,
};

/// Host inventory operations used for host/DPU matching.
pub(super) trait EndpointExplorationReportExt {
    /// Returns part and serial numbers from PCIe and chassis inventory,
    /// deduplicated by trimmed, nonempty serial number.
    ///
    /// Devices are included regardless of their type or part number. When copies of a
    /// serial disagree, prefer the first record with a BlueField part number;
    /// otherwise keep the first record. PCIe records precede chassis records,
    /// so PCIe wins when both sources supply conflicting BlueField part numbers.
    fn devices_with_unique_serials(&self) -> Vec<DeviceIdentifiers<'_>>;
}

impl EndpointExplorationReportExt for EndpointExplorationReport {
    fn devices_with_unique_serials(&self) -> Vec<DeviceIdentifiers<'_>> {
        let devices = self
            .systems
            .iter()
            .flat_map(|system| &system.pcie_devices)
            .map(DeviceIdentifiers::from)
            .chain(self.chassis.iter().flat_map(host_dpu_inventory_records));
        deduplicate_devices_by_serial(devices)
    }
}

/// Part and serial numbers read from a host inventory device.
#[derive(Clone, Copy)]
pub(super) struct DeviceIdentifiers<'a> {
    pub(super) part_number: Option<&'a str>,
    pub(super) serial_number: Option<&'a str>,
}

impl<'a> DeviceIdentifiers<'a> {
    /// Returns the trimmed serial number, or `None` when it is missing or blank.
    pub(super) fn serial_number(&self) -> Option<&'a str> {
        self.serial_number.map(str::trim).none_if_empty()
    }

    pub(super) fn is_bluefield(&self) -> bool {
        self.part_number.is_some_and(is_bluefield_part_number)
    }
}

impl<'a> From<&'a PCIeDevice> for DeviceIdentifiers<'a> {
    fn from(device: &'a PCIeDevice) -> Self {
        Self {
            part_number: device.part_number.as_deref(),
            serial_number: device.serial_number.as_deref(),
        }
    }
}

fn deduplicate_devices_by_serial<'a>(
    inventory: impl Iterator<Item = DeviceIdentifiers<'a>>,
) -> Vec<DeviceIdentifiers<'a>> {
    let mut positions = HashMap::<&str, usize>::new();
    let mut devices: Vec<DeviceIdentifiers<'a>> = Vec::new();
    for device in inventory {
        let Some(serial) = device.serial_number() else {
            continue;
        };
        if let Some(index) = positions.get(serial) {
            let existing = &mut devices[*index];
            if !existing.is_bluefield() && device.is_bluefield() {
                *existing = device;
            }
        } else {
            positions.insert(serial, devices.len());
            devices.push(device);
        }
    }
    devices
}

/// Reads the chassis identity when it is a BlueField with a usable serial;
/// otherwise reads its network adapters. Keep all occurrences so chassis-ID
/// attribution can still detect conflicting parent chassis for one serial.
pub(super) fn host_dpu_inventory_records(chassis: &Chassis) -> Vec<DeviceIdentifiers<'_>> {
    let device = DeviceIdentifiers {
        part_number: chassis.part_number.as_deref(),
        serial_number: chassis.serial_number.as_deref(),
    };
    if device.is_bluefield() && device.serial_number().is_some() {
        return vec![device];
    }

    chassis
        .network_adapters
        .iter()
        .map(|adapter| DeviceIdentifiers {
            part_number: adapter.part_number.as_deref(),
            serial_number: adapter.serial_number.as_deref(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use model::site_explorer::{ComputerSystem, NetworkAdapter};

    use super::*;

    const BLUEFIELD: &str = "900-9D3B6-00CV-AA0";

    fn report(devices: &[(Option<&str>, Option<&str>)]) -> EndpointExplorationReport {
        EndpointExplorationReport {
            systems: devices
                .iter()
                .map(|(part_number, serial_number)| ComputerSystem {
                    pcie_devices: vec![PCIeDevice {
                        description: None,
                        firmware_version: None,
                        gpu_vendor: None,
                        id: None,
                        manufacturer: None,
                        name: None,
                        part_number: part_number.map(str::to_string),
                        serial_number: serial_number.map(str::to_string),
                        status: None,
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn chassis(
        part_number: Option<&str>,
        serial_number: Option<&str>,
        adapters: &[(Option<&str>, Option<&str>)],
    ) -> Chassis {
        Chassis {
            part_number: part_number.map(str::to_string),
            serial_number: serial_number.map(str::to_string),
            network_adapters: adapters
                .iter()
                .map(|(part_number, serial_number)| NetworkAdapter {
                    part_number: part_number.map(str::to_string),
                    serial_number: serial_number.map(str::to_string),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn chassis_devices_with_unique_serials() {
        struct Case {
            name: &'static str,
            pcie_devices: &'static [(Option<&'static str>, Option<&'static str>)],
            chassis: Vec<Chassis>,
            serials: &'static [(&'static str, Option<&'static str>)],
        }

        for case in [
            Case {
                name: "empty inventory",
                pcie_devices: &[],
                chassis: vec![],
                serials: &[],
            },
            Case {
                name: "a BlueField chassis with a serial supplies its own identity",
                pcie_devices: &[],
                chassis: vec![chassis(
                    Some(BLUEFIELD),
                    Some(" CHASSIS "),
                    &[(Some(BLUEFIELD), Some("ADAPTER"))],
                )],
                serials: &[("CHASSIS", Some(BLUEFIELD))],
            },
            Case {
                name: "a non-BlueField chassis supplies its network adapters",
                pcie_devices: &[],
                chassis: vec![chassis(
                    Some("OTHER-PART"),
                    Some("CHASSIS"),
                    &[(Some(BLUEFIELD), Some("ADAPTER"))],
                )],
                serials: &[("ADAPTER", Some(BLUEFIELD))],
            },
            Case {
                name: "a BlueField chassis without a usable serial supplies its adapters",
                pcie_devices: &[],
                chassis: vec![chassis(
                    Some(BLUEFIELD),
                    Some("  "),
                    &[(Some(BLUEFIELD), Some("ADAPTER"))],
                )],
                serials: &[("ADAPTER", Some(BLUEFIELD))],
            },
            Case {
                name: "duplicate serials retain a BlueField part number across chassis",
                pcie_devices: &[],
                chassis: vec![
                    chassis(
                        None,
                        None,
                        &[
                            (None, Some(" DPU-A ")),
                            (None, Some("DPU-A")),
                            (None, Some(" DPU-B ")),
                            (None, Some("DPU-B")),
                        ],
                    ),
                    chassis(Some(BLUEFIELD), Some("DPU-A"), &[]),
                ],
                serials: &[("DPU-A", Some(BLUEFIELD)), ("DPU-B", None)],
            },
            Case {
                name: "PCIe and chassis share serials and omit records without usable serials",
                pcie_devices: &[
                    (Some(BLUEFIELD), Some("UNEXPLORED")),
                    (Some(BLUEFIELD), None),
                ],
                chassis: vec![chassis(
                    None,
                    None,
                    &[
                        (Some(BLUEFIELD), Some(" UNEXPLORED ")),
                        (None, Some("READY")),
                        (Some(BLUEFIELD), Some("READY")),
                        (Some(BLUEFIELD), None),
                    ],
                )],
                serials: &[("UNEXPLORED", Some(BLUEFIELD)), ("READY", Some(BLUEFIELD))],
            },
            Case {
                name: "distinct PCIe and chassis serials are combined in source order",
                pcie_devices: &[(Some(BLUEFIELD), Some(" PCIE "))],
                chassis: vec![chassis(None, None, &[(None, Some("CHASSIS"))])],
                serials: &[("PCIE", Some(BLUEFIELD)), ("CHASSIS", None)],
            },
            Case {
                name: "PCIe wins when both sources have conflicting BlueField part numbers",
                pcie_devices: &[(Some("900-9D3B4-00EN-EA0"), Some(" DPU "))],
                chassis: vec![chassis(Some(BLUEFIELD), Some("DPU"), &[])],
                serials: &[("DPU", Some("900-9D3B4-00EN-EA0"))],
            },
            Case {
                name: "chassis supplies the BlueField part number missing from PCIe",
                pcie_devices: &[(None, Some(" DPU "))],
                chassis: vec![chassis(Some(BLUEFIELD), Some("DPU"), &[])],
                serials: &[("DPU", Some(BLUEFIELD))],
            },
        ] {
            let mut report = report(case.pcie_devices);
            report.chassis = case.chassis;
            let serials = report
                .devices_with_unique_serials()
                .into_iter()
                .map(|device| (device.serial_number().unwrap(), device.part_number))
                .collect::<Vec<_>>();
            assert_eq!(serials, case.serials, "{}: serials", case.name);
        }
    }

    #[test]
    fn pcie_devices_with_unique_serials() {
        struct Case {
            name: &'static str,
            devices: &'static [(Option<&'static str>, Option<&'static str>)],
            serials: &'static [(&'static str, Option<&'static str>)],
        }

        for case in [
            Case {
                name: "empty inventory",
                devices: &[],
                serials: &[],
            },
            Case {
                name: "serials are normalized without a BlueField requirement",
                devices: &[
                    (Some("OTHER-PART"), Some(" OTHER-SERIAL ")),
                    (None, Some("OTHER-SERIAL")),
                    (None, Some(" SERIAL-ONLY ")),
                    (None, Some("SERIAL-ONLY")),
                ],
                serials: &[("OTHER-SERIAL", Some("OTHER-PART")), ("SERIAL-ONLY", None)],
            },
            Case {
                name: "duplicates across systems retain the BlueField part number",
                devices: &[
                    (None, Some(" DPU-A ")),
                    (Some(BLUEFIELD), Some("DPU-A")),
                    (Some(BLUEFIELD), Some("DPU-B")),
                    (None, Some("DPU-B")),
                ],
                serials: &[("DPU-A", Some(BLUEFIELD)), ("DPU-B", Some(BLUEFIELD))],
            },
            Case {
                name: "devices without usable serials are omitted",
                devices: &[
                    (Some(BLUEFIELD), None),
                    (Some(BLUEFIELD), Some("")),
                    (Some(BLUEFIELD), Some("  ")),
                    (Some("OTHER-PART"), None),
                ],
                serials: &[],
            },
        ] {
            let report = report(case.devices);
            let serials = report
                .devices_with_unique_serials()
                .into_iter()
                .map(|device| (device.serial_number().unwrap(), device.part_number))
                .collect::<Vec<_>>();
            assert_eq!(serials, case.serials, "{}: serials", case.name);
        }
    }
}
