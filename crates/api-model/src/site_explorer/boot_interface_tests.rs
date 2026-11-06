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

use carbide_test_support::Outcome::{FailsWith, Yields};
use carbide_test_support::{Case, check_cases};
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};

use super::*;
use crate::hardware_info::DpuHostPf;
use crate::site_explorer::{ComputerSystem, EthernetInterface, UefiDevicePath};

/// Creates one factual controller/PF entry so tests vary policy without changing factory identity.
fn host_pf(controller: u32, mac: &str) -> DpuHostPf {
    DpuHostPf {
        pci_controller_id: controller,
        pf_id: 0,
        mac_address: mac.parse().expect("valid test MAC"),
        physical_port_name: format!("c{controller}pf0"),
        netdev_name: None,
        pci_address: None,
    }
}

/// Supplies paired BF4 identity with a misleading factory MAC so automatic tests cannot pass by using it.
fn paired_dpu(number: u8, host_pfs: Vec<DpuHostPf>) -> HostBootInterfaceDpu {
    HostBootInterfaceDpu {
        dpu_machine_id: MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [number; 32],
            MachineType::Dpu,
        )
        .try_into()
        .expect("DPU machine ID"),
        hardware: DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(),
            factory_mac_address: "02:00:00:00:00:f0".to_string(),
            host_pfs, // Only agent-reported entries may satisfy BF4 policy.
            ..Default::default()
        },
        provisional_mac_address: Some("02:00:00:00:00:f0".parse().expect("valid decoy MAC")),
    }
}

/// Gives each target a distinct Redfish ID and numeric PCI path so selection can be compared with decoys.
fn host_report() -> EndpointExplorationReport {
    let interfaces = [
        ("96:ab:d0:3e:5f:a6", "controller-1", 16),
        ("96:ab:d0:3e:5f:a8", "controller-2", 2),
        ("02:00:00:00:00:f0", "factory-decoy", 0),
        ("02:00:00:00:00:03", "integrated", 1),
    ]
    .into_iter()
    .map(|(mac, id, device)| EthernetInterface {
        mac_address: Some(mac.parse().expect("valid reported MAC")),
        id: Some(id.to_string()),
        uefi_device_path: Some(
            format!("PciRoot(0x0)/Pci(0x{device:x},0x0)")
                .parse::<UefiDevicePath>()
                .expect("valid UEFI PCI path"),
        ),
        ..Default::default()
    })
    .collect();
    EndpointExplorationReport {
        systems: vec![ComputerSystem {
            ethernet_interfaces: interfaces,
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Selects the configured controller and preserves distinct failures so BF4 cannot boot through a lower-PCI factory decoy.
#[test]
fn configured_bf4_selection_and_unresolved_reasons() {
    // Both controller candidates are valid; the decoy's lower path must never decide BF4 selection.
    let inventory = paired_dpu(
        1,
        vec![
            host_pf(1, "96:ab:d0:3e:5f:a6"),
            host_pf(2, "96:ab:d0:3e:5f:a8"),
        ],
    );
    let controller_one = inventory.hardware.host_pfs[0].mac_address;
    let controller_two = inventory.hardware.host_pfs[1].mac_address;
    let report = host_report();
    // Remove only the selected target's Redfish observation.
    let mut missing_interface = report.clone();
    missing_interface.systems[0]
        .ethernet_interfaces
        .retain(|interface| interface.mac_address != Some(controller_one));
    // A present Redfish MAC without a PCI path remains insufficient for automatic selection.
    let mut missing_path = report.clone();
    missing_path.systems[0].ethernet_interfaces[0].uefi_device_path = None;
    // Retain the selected MAC and PCI path while omitting its Redfish boot identifier.
    let mut missing_id = report.clone();
    missing_id.systems[0].ethernet_interfaces[0].id = None;
    // Whitespace must not become a boot identifier merely because the field is present.
    let mut blank_id = report.clone();
    blank_id.systems[0].ethernet_interfaces[0].id = Some(" \t".to_string());
    // Incomplete registration must remain different from a valid inventory that lacks the configured target.
    let absent = paired_dpu(1, vec![]);
    let wrong_controller = paired_dpu(1, vec![host_pf(2, "96:ab:d0:3e:5f:a8")]);
    // A duplicate semantic key cannot be resolved by input order.
    let duplicate = paired_dpu(
        1,
        vec![
            host_pf(1, "96:ab:d0:3e:5f:a6"),
            host_pf(1, "96:ab:d0:3e:5f:a8"),
        ],
    );
    // Invalid boot MACs are rejected even though the Rust MAC type can represent zero.
    let invalid = paired_dpu(1, vec![host_pf(1, "00:00:00:00:00:00")]);
    let dpu_machine_id = inventory.dpu_machine_id;
    let failure = |source| HostPrimaryInterfaceError::DpuInventory {
        dpu_machine_id,
        source,
    };

    // Use one operation for policy, precedence, and each distinct unresolved fact.
    check_cases(
        [
            // Controller 1 is selected despite a lower path on controller 2 and the factory decoy.
            Case {
                scenario: "default controller",
                input: (inventory.clone(), report.clone(), 1, None),
                expect: Yields((controller_one, BootInterfaceSelectionSource::RedfishUefiPci)),
            },
            // An override selects the other factual entry rather than changing inventory.
            Case {
                scenario: "controller override",
                input: (inventory.clone(), report.clone(), 2, None),
                expect: Yields((controller_two, BootInterfaceSelectionSource::RedfishUefiPci)),
            },
            // A resolved integrated declaration bypasses automatic inventory requirements.
            Case {
                scenario: "declared integrated primary",
                input: (
                    absent.clone(),
                    report.clone(),
                    1,
                    Some("02:00:00:00:00:03".parse().expect("declared MAC")),
                ),
                expect: Yields((
                    "02:00:00:00:00:03".parse().expect("declared MAC"),
                    BootInterfaceSelectionSource::ExpectedMachine,
                )),
            },
            // Empty registration is not evidence for a factory-MAC fallback.
            Case {
                scenario: "missing inventory",
                input: (absent, report.clone(), 1, None),
                expect: FailsWith(failure(DpuHostPfError::MissingInventory)),
            },
            // Another controller cannot substitute for the configured one.
            Case {
                scenario: "missing target",
                input: (wrong_controller, report.clone(), 1, None),
                expect: FailsWith(failure(DpuHostPfError::MissingTarget {
                    controller: 1,
                    pf: 0,
                })),
            },
            // Ambiguous controller/PF observations must fail before Redfish ordering.
            Case {
                scenario: "duplicate target",
                input: (duplicate, report.clone(), 1, None),
                expect: FailsWith(failure(DpuHostPfError::DuplicateIdentity {
                    controller: 1,
                    pf: 0,
                })),
            },
            // A zero MAC is malformed boot identity rather than an absent Redfish match.
            Case {
                scenario: "invalid target MAC",
                input: (invalid, report, 1, None),
                expect: FailsWith(failure(DpuHostPfError::InvalidMacAddress(
                    "00:00:00:00:00:00"
                        .parse()
                        .expect("zero MAC representation"),
                ))),
            },
            // Registered mapping alone cannot establish a host-visible boot device.
            Case {
                scenario: "missing host interface",
                input: (inventory.clone(), missing_interface, 1, None),
                expect: FailsWith(HostPrimaryInterfaceError::MissingHostInterface(
                    controller_one,
                )),
            },
            // A matched MAC and PCI path cannot replace the missing Redfish identifier.
            Case {
                scenario: "missing Redfish interface ID",
                input: (inventory.clone(), missing_id, 1, None),
                expect: FailsWith(HostPrimaryInterfaceError::MissingInterfaceId(
                    controller_one,
                )),
            },
            // A present whitespace identifier must fail the same completeness contract.
            Case {
                scenario: "blank Redfish interface ID",
                input: (inventory.clone(), blank_id, 1, None),
                expect: FailsWith(HostPrimaryInterfaceError::MissingInterfaceId(
                    controller_one,
                )),
            },
            // Every automatic candidate needs PCI evidence, even if another decoy has it.
            Case {
                scenario: "missing UEFI path",
                input: (inventory, missing_path, 1, None),
                expect: FailsWith(HostPrimaryInterfaceError::MissingUefiPath(controller_one)),
            },
        ],
        |(dpu, report, controller, declared)| {
            report
                .select_host_primary_interface_from_dpus(&[dpu], declared, controller)
                .map(|selection| (selection.mac_address, selection.source))
        },
    );
}

/// Waits for every paired BF4 mapping, then selects the lowest numeric PCI path instead of phone-home order.
#[test]
fn all_bf4_dpus_participate_before_numeric_pci_selection() {
    // The first DPU's device 16 follows the second DPU's device 2 in numeric PCI order.
    let first = paired_dpu(1, vec![host_pf(1, "96:ab:d0:3e:5f:a6")]);
    let mut second = paired_dpu(2, vec![]);
    let report = host_report();

    // A usable first DPU cannot hide an unregistered paired DPU.
    let unresolved =
        report.select_host_primary_interface_from_dpus(&[first.clone(), second.clone()], None, 1);
    assert!(matches!(
        unresolved,
        Err(HostPrimaryInterfaceError::DpuInventory {
            source: DpuHostPfError::MissingInventory,
            ..
        })
    ));

    // Register the second DPU's controller-1 PF 0 without changing discovery order.
    second
        .hardware
        .host_pfs
        .push(host_pf(1, "96:ab:d0:3e:5f:a8"));
    let selected = report
        .select_host_primary_interface_from_dpus(&[first, second], None, 1)
        .expect("complete paired inventory");

    // Numeric UEFI comparison selects device 2 rather than textual device 16.
    assert_eq!(
        selected.mac_address,
        "96:ab:d0:3e:5f:a8"
            .parse::<MacAddress>()
            .expect("second DPU MAC")
    );
}
