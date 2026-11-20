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

use carbide_utils::none_if_empty::NoneIfEmpty;
use itertools::Itertools;
use mac_address::MacAddress;
use model::expected_machine::ExpectedMachineData;
use model::site_explorer::EndpointExplorationReport;

/// Inventory operations used for interface identification.
pub(super) trait EndpointExplorationReportExt {
    /// Returns host MACs used to predict interfaces for a zero-DPU host.
    ///
    /// System EthernetInterfaces remain Host candidates. Adapter Ports are broader
    /// chassis inventory, so when at least one usable System interface exists we
    /// include only discovered Port MACs that ExpectedMachine also declares as
    /// Host interfaces. When no usable System MAC exists, discovered Ports preserve
    /// the existing hardware fallback alongside any System MACs. ExpectedMachine
    /// supplies MACs by itself only when Redfish reports neither collection.
    fn host_mac_addresses_for_predicted_machine(
        &self,
        machine_data: Option<&ExpectedMachineData>,
    ) -> Vec<MacAddress>;

    /// Returns the first reported system interface MAC whose ID contains `oob`,
    /// ignoring ASCII case.
    fn dpu_oob_mac_address(&self) -> Option<MacAddress>;
}

impl EndpointExplorationReportExt for EndpointExplorationReport {
    fn host_mac_addresses_for_predicted_machine(
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

    fn dpu_oob_mac_address(&self) -> Option<MacAddress> {
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
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use model::expected_machine::{ExpectedInterface, ExpectedInterfaceRole};
    use model::site_explorer::{Chassis, ComputerSystem, EthernetInterface, NetworkAdapter};

    use super::*;

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
