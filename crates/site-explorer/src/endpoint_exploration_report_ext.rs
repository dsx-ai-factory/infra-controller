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

use carbide_utils::none_if_empty::NoneIfEmpty;
use model::site_explorer::{
    Chassis, EndpointExplorationReport, PCIeDevice, is_bluefield_part_number,
};

/// Host inventory operations used for host/DPU matching.
pub(super) trait EndpointExplorationReportExt {
    /// Returns PCIe DPU candidates with unique, trimmed, nonempty serials in report order.
    ///
    /// Devices are included regardless of their part number. When copies of a
    /// serial disagree, prefer the first record with a BlueField part number;
    /// otherwise keep the first record. This retains the part number used to
    /// select the DPU's target mode without attaching duplicate devices.
    fn pcie_devices_dpu_candidates(&self) -> Vec<HostDpuCandidate<'_>>;

    /// Counts PCIe DPUs identified by a BlueField part number or the serial predicate.
    ///
    /// A trimmed, nonempty serial counts once. Each BlueField record without a
    /// usable serial counts separately because its identity cannot be deduplicated.
    fn pcie_devices_dpu_count(&self, is_dpu_serial: impl Fn(&str) -> bool) -> usize;
}

impl EndpointExplorationReportExt for EndpointExplorationReport {
    fn pcie_devices_dpu_candidates(&self) -> Vec<HostDpuCandidate<'_>> {
        let mut positions = HashMap::<&str, usize>::new();
        let mut devices: Vec<HostDpuCandidate<'_>> = Vec::new();
        for device in self
            .systems
            .iter()
            .flat_map(|system| &system.pcie_devices)
            .map(HostDpuCandidate::from)
        {
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

    fn pcie_devices_dpu_count(&self, is_dpu_serial: impl Fn(&str) -> bool) -> usize {
        let mut seen = HashSet::new();
        self.systems
            .iter()
            .flat_map(|system| &system.pcie_devices)
            .map(HostDpuCandidate::from)
            .filter(|device| {
                device.is_bluefield() || device.serial_number().is_some_and(&is_dpu_serial)
            })
            .filter(|device| {
                device
                    .serial_number()
                    .is_none_or(|serial| seen.insert(serial))
            })
            .count()
    }
}

/// Host inventory fields that can identify one DPU by serial number.
#[derive(Clone, Copy)]
pub(super) struct HostDpuCandidate<'a> {
    pub(super) part_number: Option<&'a str>,
    pub(super) serial_number: Option<&'a str>,
}

impl<'a> HostDpuCandidate<'a> {
    /// Returns the trimmed serial number, or `None` when it is missing or blank.
    pub(super) fn serial_number(&self) -> Option<&'a str> {
        self.serial_number.map(str::trim).none_if_empty()
    }

    pub(super) fn is_bluefield(&self) -> bool {
        self.part_number.is_some_and(is_bluefield_part_number)
    }
}

impl<'a> From<&'a PCIeDevice> for HostDpuCandidate<'a> {
    fn from(device: &'a PCIeDevice) -> Self {
        Self {
            part_number: device.part_number.as_deref(),
            serial_number: device.serial_number.as_deref(),
        }
    }
}

/// Reads the chassis identity when it is a BlueField with a usable serial;
/// otherwise reads its network adapters. Keep all occurrences so chassis-ID
/// attribution can still detect conflicting parent chassis for one serial.
pub(super) fn host_dpu_inventory_records(chassis: &Chassis) -> Vec<HostDpuCandidate<'_>> {
    let device = HostDpuCandidate {
        part_number: chassis.part_number.as_deref(),
        serial_number: chassis.serial_number.as_deref(),
    };
    if device.is_bluefield() && device.serial_number().is_some() {
        return vec![device];
    }

    chassis
        .network_adapters
        .iter()
        .map(|adapter| HostDpuCandidate {
            part_number: adapter.part_number.as_deref(),
            serial_number: adapter.serial_number.as_deref(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use model::site_explorer::ComputerSystem;

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

    #[test]
    fn pcie_serials_and_reported_dpu_count() {
        struct Case {
            name: &'static str,
            devices: &'static [(Option<&'static str>, Option<&'static str>)],
            dpu_serials: &'static [&'static str],
            serials: &'static [(&'static str, Option<&'static str>)],
            count: usize,
        }

        for case in [
            Case {
                name: "empty inventory",
                devices: &[],
                dpu_serials: &[],
                serials: &[],
                count: 0,
            },
            Case {
                name: "serials are normalized without a BlueField requirement",
                devices: &[
                    (Some("OTHER-PART"), Some(" OTHER-SERIAL ")),
                    (None, Some("OTHER-SERIAL")),
                    (None, Some(" SERIAL-ONLY ")),
                    (None, Some("SERIAL-ONLY")),
                ],
                dpu_serials: &[],
                serials: &[("OTHER-SERIAL", Some("OTHER-PART")), ("SERIAL-ONLY", None)],
                count: 0,
            },
            Case {
                name: "serial predicate identifies DPUs without a BlueField part number",
                devices: &[
                    (None, Some(" SERIAL-ONLY ")),
                    (None, Some("SERIAL-ONLY")),
                    (Some("OTHER-PART"), Some("OTHER-SERIAL")),
                    (None, Some("UNKNOWN")),
                ],
                dpu_serials: &["SERIAL-ONLY", "OTHER-SERIAL"],
                serials: &[
                    ("SERIAL-ONLY", None),
                    ("OTHER-SERIAL", Some("OTHER-PART")),
                    ("UNKNOWN", None),
                ],
                count: 2,
            },
            Case {
                name: "duplicates across systems retain the BlueField part number",
                devices: &[
                    (None, Some(" DPU-A ")),
                    (Some(BLUEFIELD), Some("DPU-A")),
                    (Some(BLUEFIELD), Some("DPU-B")),
                    (None, Some("DPU-B")),
                ],
                dpu_serials: &["DPU-A"],
                serials: &[("DPU-A", Some(BLUEFIELD)), ("DPU-B", Some(BLUEFIELD))],
                count: 2,
            },
            Case {
                name: "BlueFields without usable serials still count",
                devices: &[
                    (Some(BLUEFIELD), None),
                    (Some(BLUEFIELD), Some("")),
                    (Some(BLUEFIELD), Some("  ")),
                    (Some("OTHER-PART"), None),
                ],
                dpu_serials: &[],
                serials: &[],
                count: 3,
            },
        ] {
            let report = report(case.devices);
            let serials = report
                .pcie_devices_dpu_candidates()
                .into_iter()
                .map(|device| (device.serial_number().unwrap(), device.part_number))
                .collect::<Vec<_>>();
            assert_eq!(serials, case.serials, "{}: serials", case.name);
            assert_eq!(
                report.pcie_devices_dpu_count(|serial| case.dpu_serials.contains(&serial)),
                case.count,
                "{}: count",
                case.name
            );
        }
    }
}
