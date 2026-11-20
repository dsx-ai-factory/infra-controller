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
use itertools::Itertools;
use mac_address::MacAddress;

use super::{Chassis, EndpointExplorationReport, PCIeDevice, is_bluefield_part_number};
use crate::expected_machine::ExpectedMachineData;

impl EndpointExplorationReport {
    /// Returns host MACs used to predict interfaces for a zero-DPU host.
    ///
    /// System EthernetInterfaces remain Host candidates. Adapter Ports are broader
    /// chassis inventory, so when at least one usable System interface exists we
    /// include only discovered Port MACs that ExpectedMachine also declares as
    /// Host interfaces. When no usable System MAC exists, discovered Ports preserve
    /// the existing hardware fallback alongside any System MACs. ExpectedMachine
    /// supplies MACs by itself only when Redfish reports neither collection.
    pub fn host_mac_addresses_for_predicted_machine(
        &self,
        machine_data: Option<&ExpectedMachineData>,
    ) -> Vec<MacAddress> {
        let system_mac_addresses = self
            .systems
            .iter()
            .flat_map(|system| &system.ethernet_interfaces)
            .filter_map(|interface| interface.mac_address)
            .unique()
            .collect::<Vec<_>>();
        let has_usable_system_mac_address = self
            .systems
            .iter()
            .flat_map(|system| &system.ethernet_interfaces)
            .any(|interface| {
                interface.interface_enabled != Some(false) && interface.mac_address.is_some()
            });
        let port_mac_addresses = self
            .chassis
            .iter()
            .flat_map(|chassis| &chassis.network_adapters)
            .flat_map(|adapter| adapter.port_mac_addresses.iter().copied())
            .unique()
            .collect::<Vec<_>>();
        let is_host_report = !(self.is_dpu() || self.is_switch() || self.is_power_shelf());

        if has_usable_system_mac_address {
            let declared_host_mac_addresses = machine_data
                .into_iter()
                .flat_map(|data| &data.interfaces)
                .filter(|interface| interface.role.is_host())
                .filter_map(|interface| interface.mac_address)
                .collect::<Vec<_>>();

            return system_mac_addresses
                .into_iter()
                .chain(port_mac_addresses.into_iter().filter(|mac_address| {
                    is_host_report && declared_host_mac_addresses.contains(mac_address)
                }))
                .unique()
                .collect();
        }

        if !port_mac_addresses.is_empty() && is_host_report {
            return system_mac_addresses
                .into_iter()
                .chain(port_mac_addresses)
                .unique()
                .collect();
        }
        if !system_mac_addresses.is_empty() {
            return system_mac_addresses;
        }

        machine_data
            .filter(|_| is_host_report)
            .map(|data| {
                data.interfaces
                    .iter()
                    .filter(|interface| interface.role.is_host())
                    .filter_map(|interface| interface.mac_address)
                    .unique()
                    .collect::<Vec<_>>()
            })
            .none_if_empty()
            .inspect(|host_mac_addresses| {
                tracing::info!(
                    host_nic_count = host_mac_addresses.len(),
                    "Redfish host MAC inventory missing; using ExpectedMachine.interfaces for predicted machine interfaces"
                );
            })
            .unwrap_or_default()
    }

    /// Returns the first reported system interface MAC whose ID contains `oob`, ignoring ASCII case.
    pub fn dpu_oob_mac_address(&self) -> Option<MacAddress> {
        self.systems.iter().find_map(|system| {
            system.ethernet_interfaces.iter().find_map(|interface| {
                if interface.id.as_ref().is_some_and(|id| {
                    id.as_bytes()
                        .windows(3)
                        .any(|bytes| bytes.eq_ignore_ascii_case(b"oob"))
                }) {
                    interface.mac_address
                } else {
                    None
                }
            })
        })
    }

    /// Returns part and serial numbers from PCIe and chassis inventory,
    /// deduplicated by trimmed, nonempty serial number.
    ///
    /// Devices are included regardless of their type or part number. When copies of a
    /// serial disagree, prefer the first record with a BlueField part number;
    /// otherwise keep the first record. PCIe records precede chassis records,
    /// so PCIe wins when both sources supply conflicting BlueField part numbers.
    pub fn devices_with_unique_serials(&self) -> Vec<DeviceIdentifiers<'_>> {
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
pub struct DeviceIdentifiers<'a> {
    /// Part number used to recognize BlueField records during deduplication.
    pub part_number: Option<&'a str>,
    /// Raw serial number trimmed by the pairing lookup.
    pub serial_number: Option<&'a str>,
}

impl<'a> DeviceIdentifiers<'a> {
    /// Returns the trimmed serial number, or `None` when it is missing or blank.
    pub fn serial_number(&self) -> Option<&'a str> {
        self.serial_number.map(str::trim).none_if_empty()
    }

    /// Identifies a BlueField part so duplicate inventory keeps its DPU evidence.
    pub fn is_bluefield(&self) -> bool {
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

/// Keeps inventory encounter order while preferring BlueField evidence for repeated serials.
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
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::expected_machine::{ExpectedInterface, ExpectedInterfaceRole};
    use crate::site_explorer::{ComputerSystem, EthernetInterface, NetworkAdapter};

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
    /// Redfish inventory stays authoritative, ExpectedMachine narrows
    /// supplemental Ports, and the zero-DPU fallback uses only Host
    /// declarations from a host ExpectedMachine.
    #[test]
    fn host_mac_addresses_for_predicted_machine_cases() {
        let host_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let dpu_os_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
        let dpu_bmc_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x03]);
        let host_bmc_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x04]);
        let redfish_mac = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x05]);
        let declared_port_mac = MacAddress::new([0x94, 0x6d, 0xae, 0x53, 0xcb, 0x9b]);
        let unrelated_port_mac = MacAddress::new([0x94, 0x6d, 0xae, 0x53, 0xcb, 0x9a]);
        let system_macs = [
            MacAddress::new([0x0a, 0x8f, 0xc3, 0xa5, 0x8a, 0x41]),
            MacAddress::new([0x00, 0x62, 0x0b, 0x4c, 0x28, 0xa8]),
            MacAddress::new([0x00, 0x62, 0x0b, 0x4c, 0x28, 0xa9]),
            MacAddress::new([0x00, 0x62, 0x0b, 0x4c, 0x28, 0xaa]),
            MacAddress::new([0x00, 0x62, 0x0b, 0x4c, 0x28, 0xab]),
        ];
        let interface = |mac_address, role| ExpectedInterface {
            mac_address: Some(mac_address),
            role,
            ..Default::default()
        };
        let machine_data = |interfaces| ExpectedMachineData {
            interfaces,
            ..Default::default()
        };
        let all_roles = || {
            machine_data(vec![
                interface(host_mac, ExpectedInterfaceRole::Host),
                interface(dpu_os_mac, ExpectedInterfaceRole::DpuOs),
                interface(dpu_bmc_mac, ExpectedInterfaceRole::DpuBmc),
                interface(host_bmc_mac, ExpectedInterfaceRole::HostBmc),
            ])
        };
        let report_with_ports = |port_mac_addresses| EndpointExplorationReport {
            chassis: vec![Chassis {
                network_adapters: vec![NetworkAdapter {
                    id: "slot-15".to_string(),
                    port_mac_addresses,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let excluded_report = |chassis_id: &str| EndpointExplorationReport {
            chassis: vec![Chassis {
                id: chassis_id.to_string(),
                network_adapters: vec![NetworkAdapter {
                    id: "slot-15".to_string(),
                    port_mac_addresses: vec![declared_port_mac],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        check_values(
            [
                Check {
                    scenario: "Redfish EthernetInterfaces are authoritative",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                ethernet_interfaces: vec![EthernetInterface {
                                    mac_address: Some(redfish_mac),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                        Some(all_roles()),
                    ),
                    expect: vec![redfish_mac],
                },
                Check {
                    scenario: "declared discovered Port supplements partial System inventory",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                ethernet_interfaces: system_macs
                                    .into_iter()
                                    .map(|mac_address| EthernetInterface {
                                        mac_address: Some(mac_address),
                                        ..Default::default()
                                    })
                                    .collect(),
                                ..Default::default()
                            }],
                            chassis: report_with_ports(vec![
                                declared_port_mac,
                                unrelated_port_mac,
                                dpu_os_mac,
                                system_macs[0],
                            ])
                            .chassis,
                            ..Default::default()
                        },
                        Some(machine_data(vec![
                            ExpectedInterface {
                                primary: Some(true),
                                ..interface(declared_port_mac, ExpectedInterfaceRole::Host)
                            },
                            interface(system_macs[0], ExpectedInterfaceRole::Host),
                            interface(dpu_os_mac, ExpectedInterfaceRole::DpuOs),
                        ])),
                    ),
                    expect: system_macs.into_iter().chain([declared_port_mac]).collect(),
                },
                Check {
                    scenario: "undeclared Ports do not supplement usable System inventory",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                ethernet_interfaces: vec![EthernetInterface {
                                    mac_address: Some(redfish_mac),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            chassis: report_with_ports(vec![declared_port_mac]).chassis,
                            ..Default::default()
                        },
                        None,
                    ),
                    expect: vec![redfish_mac],
                },
                Check {
                    scenario: "disabled System interfaces do not suppress the adapter-Port fallback",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                ethernet_interfaces: vec![EthernetInterface {
                                    mac_address: Some(redfish_mac),
                                    interface_enabled: Some(false),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            chassis: report_with_ports(vec![declared_port_mac, unrelated_port_mac])
                                .chassis,
                            ..Default::default()
                        },
                        None,
                    ),
                    expect: vec![redfish_mac, declared_port_mac, unrelated_port_mac],
                },
                Check {
                    scenario: "adapter Ports remain the hardware fallback without System inventory",
                    input: (
                        report_with_ports(vec![declared_port_mac, unrelated_port_mac]),
                        None,
                    ),
                    expect: vec![declared_port_mac, unrelated_port_mac],
                },
                Check {
                    scenario: "Host declarations provide the zero-DPU fallback",
                    input: (EndpointExplorationReport::default(), Some(all_roles())),
                    expect: vec![host_mac],
                },
                Check {
                    scenario: "DPU-only declarations do not become Host predictions",
                    input: (
                        EndpointExplorationReport::default(),
                        Some(machine_data(vec![
                            interface(dpu_os_mac, ExpectedInterfaceRole::DpuOs),
                            interface(dpu_bmc_mac, ExpectedInterfaceRole::DpuBmc),
                            interface(host_bmc_mac, ExpectedInterfaceRole::HostBmc),
                        ])),
                    ),
                    expect: vec![],
                },
                Check {
                    scenario: "missing ExpectedMachine data has no fallback",
                    input: (EndpointExplorationReport::default(), None),
                    expect: vec![],
                },
                Check {
                    scenario: "DPU reports do not use Host declarations",
                    input: (
                        EndpointExplorationReport {
                            systems: vec![ComputerSystem {
                                id: "Bluefield".to_string(),
                                ethernet_interfaces: vec![EthernetInterface {
                                    mac_address: Some(redfish_mac),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            chassis: vec![Chassis {
                                id: "Card1".to_string(),
                                model: Some("BlueField 3 DPU".to_string()),
                                network_adapters: vec![NetworkAdapter {
                                    id: "slot-15".to_string(),
                                    port_mac_addresses: vec![declared_port_mac],
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                        Some(machine_data(vec![interface(
                            declared_port_mac,
                            ExpectedInterfaceRole::Host,
                        )])),
                    ),
                    expect: vec![redfish_mac],
                },
                Check {
                    scenario: "switch reports do not use Host declarations",
                    input: (excluded_report("MGX_NVSwitch_0"), Some(all_roles())),
                    expect: vec![],
                },
                Check {
                    scenario: "power-shelf reports do not use Host declarations",
                    input: (excluded_report("PowerShelf"), Some(all_roles())),
                    expect: vec![],
                },
            ],
            |(report, machine_data)| {
                report.host_mac_addresses_for_predicted_machine(machine_data.as_ref())
            },
        );
    }
}
