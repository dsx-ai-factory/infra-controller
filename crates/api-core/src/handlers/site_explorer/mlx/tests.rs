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

use std::net::IpAddr;

use config_version::ConfigVersion;
use model::site_explorer::{
    BlueFieldOperatingMode, Chassis, ComputerSystem, ComputerSystemAttributes,
    EndpointExplorationReport, EndpointType, PCIeDevice, PreingestionState,
};

use super::*;

fn collect_explored_mlx_devices(endpoints: &[ExploredEndpoint]) -> Vec<ExploredMlxDevice> {
    let mut devices = devices_for_hosts(endpoints, &HostDpuRules::default());
    attach_dpu_endpoints(&mut devices, endpoints);
    devices
}

fn endpoint(address: &str, report: EndpointExplorationReport) -> ExploredEndpoint {
    ExploredEndpoint {
        address: address.parse().unwrap(),
        report,
        report_version: ConfigVersion::new(1),
        preingestion_state: PreingestionState::Initial,
        waiting_for_explorer_refresh: false,
        exploration_requested: false,
        last_redfish_bmc_reset: None,
        last_ipmitool_bmc_reset: None,
        last_redfish_reboot: None,
        last_redfish_powercycle: None,
        pause_remediation: false,
        boot_interface_mac: None,
        boot_interface_id: None,
        pause_ingestion_and_poweron: false,
    }
}

fn pcie(part: &str, fw: &str, serial: &str, id: &str) -> PCIeDevice {
    PCIeDevice {
        description: Some(format!("NVIDIA BlueField-3 {part}")),
        firmware_version: Some(fw.to_string()),
        gpu_vendor: None,
        id: Some(id.to_string()),
        manufacturer: Some("Nvidia".to_string()),
        name: Some("Network Device".to_string()),
        part_number: Some(part.to_string()),
        serial_number: Some(serial.to_string()),
        status: None,
    }
}

#[test]
fn projects_pcie_inventory_and_joins_dpu_by_serial() {
    // A host that reports a NIC-mode DPU (outdated FW) and a native SuperNIC.
    let host = endpoint(
        "192.0.2.20",
        EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            systems: vec![ComputerSystem {
                pcie_devices: vec![
                    pcie("900-9D3B4-00EN-EA0", "32.38.1002", "MT2403X00984", "188-0"),
                    pcie(
                        "900-9D3D4-00EN-HA0_Ax",
                        "32.42.1000",
                        "MT2403X09999",
                        "204-0",
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    // The NIC-mode DPU's own BMC endpoint, keyed by the matching serial.
    let dpu = endpoint(
        "192.0.2.50",
        EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                serial_number: Some("MT2403X00984".to_string()),
                attributes: ComputerSystemAttributes {
                    nic_mode: Some(BlueFieldOperatingMode::Nic),
                    ..Default::default()
                },
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "Card1".to_string(),
                model: Some("NVIDIA BlueField 3".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );

    let mut devices = collect_explored_mlx_devices(&[host, dpu]);
    devices.sort_by(|a, b| a.pcie_id.cmp(&b.pcie_id));
    assert_eq!(devices.len(), 2, "only the host's two BlueField devices");

    let nic_dpu = &devices[0];
    assert_eq!(nic_dpu.device_kind, MlxDeviceKind::Bf3NicMode);
    assert_eq!(nic_dpu.part_number.as_deref(), Some("900-9D3B4-00EN-EA0"));
    assert_eq!(nic_dpu.firmware_version.as_deref(), Some("32.38.1002"));
    assert_eq!(nic_dpu.serial_number.as_deref(), Some("MT2403X00984"));
    assert_eq!(nic_dpu.host_bmc_ip, "192.0.2.20".parse::<IpAddr>().unwrap());
    // matched to its DPU endpoint by serial
    assert_eq!(
        nic_dpu.dpu_bmc_ip,
        Some("192.0.2.50".parse::<IpAddr>().unwrap())
    );
    assert_eq!(nic_dpu.nic_mode, Some(BlueFieldOperatingMode::Nic));

    let supernic = &devices[1];
    assert_eq!(supernic.device_kind, MlxDeviceKind::Bf3SuperNic);
    // no DPU endpoint matched this serial, so the join fields stay unset
    assert_eq!(supernic.dpu_bmc_ip, None);
    assert_eq!(supernic.nic_mode, None);
}

#[test]
fn duplicate_pcie_records_keep_firmware_independently_of_classification() {
    use carbide_hw_identification::hw::nvidia_bluefield_3::{
        DPU_9D3B6_00CV_AA0, SUPERNIC_9D3B4_00EN_EA0,
    };
    use carbide_test_support::{Check, check_values};

    check_values(
        [
            Check {
                scenario: "later firmware fills a missing version without overriding classification",
                input: (
                    SUPERNIC_9D3B4_00EN_EA0,
                    None,
                    DPU_9D3B6_00CV_AA0,
                    Some("32.42.1000"),
                ),
                expect: (
                    MlxDeviceKind::Bf3NicMode,
                    Some(SUPERNIC_9D3B4_00EN_EA0.to_owned()),
                ),
            },
            Check {
                scenario: "later classification retains the firmware from an earlier PCIe record",
                input: ("UNKNOWN-OEM", Some("32.42.1000"), DPU_9D3B6_00CV_AA0, None),
                expect: (
                    MlxDeviceKind::Bf3DpuMode,
                    Some(DPU_9D3B6_00CV_AA0.to_owned()),
                ),
            },
        ],
        |(first_part, first_fw, second_part, second_fw)| {
            let device = |part, firmware: Option<&str>| PCIeDevice {
                firmware_version: firmware.map(str::to_owned),
                ..pcie(part, "", "DUPLICATE", "188-0")
            };
            let host = endpoint(
                "192.0.2.20",
                EndpointExplorationReport {
                    systems: vec![
                        ComputerSystem {
                            pcie_devices: vec![device(first_part, first_fw)],
                            ..Default::default()
                        },
                        ComputerSystem {
                            pcie_devices: vec![device(second_part, second_fw)],
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            );
            let devices = collect_explored_mlx_devices(&[host]);
            let [card] = devices.as_slice() else {
                panic!("expected one MLX row for the duplicate serial");
            };
            assert_eq!(card.firmware_version.as_deref(), Some("32.42.1000"));
            assert_eq!(card.pcie_id.as_deref(), Some("188-0"));
            (card.device_kind, card.part_number.clone())
        },
    );
}

#[test]
fn projects_bf4_and_joins_dpu_by_bluefield_bmc_chassis_serial() {
    const BF4_SERIAL: &str = "MT020000000003";
    let host = endpoint(
        "192.0.2.20",
        EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            systems: vec![ComputerSystem {
                pcie_devices: vec![pcie(
                    "900-9D4B4-CWAA-TSA",
                    "82.48.0802",
                    BF4_SERIAL,
                    "mat_2",
                )],
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let dpu = endpoint(
        "192.0.2.50",
        EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                // BF4 leaves the system serial unset; the stable product
                // serial used for host pairing is on the Bluefield_BMC
                // chassis below.
                serial_number: None,
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "Bluefield_BMC".to_string(),
                serial_number: Some(BF4_SERIAL.to_string()),
                ..Default::default()
            }],
            ..Default::default()
        },
    );

    let devices = collect_explored_mlx_devices(&[host, dpu]);
    assert_eq!(devices.len(), 1);

    let bf4 = &devices[0];
    assert_eq!(bf4.device_kind, MlxDeviceKind::Unknown);
    assert_eq!(bf4.part_number.as_deref(), Some("900-9D4B4-CWAA-TSA"));
    assert_eq!(bf4.serial_number.as_deref(), Some(BF4_SERIAL));
    assert_eq!(bf4.dpu_bmc_ip, Some("192.0.2.50".parse().unwrap()));
    // BF4 does not currently expose DPU/NIC mode through Redfish; missing
    // mode must not prevent the serial join.
    assert_eq!(bf4.nic_mode, None);
}

#[test]
fn serial_join_skips_empty_and_ambiguous_serials() {
    let dpu = |addr: &str, serial: &str| {
        endpoint(
            addr,
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                systems: vec![ComputerSystem {
                    id: "Bluefield".to_string(),
                    serial_number: Some(serial.to_string()),
                    attributes: ComputerSystemAttributes {
                        nic_mode: Some(BlueFieldOperatingMode::Nic),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                chassis: vec![Chassis {
                    id: "Card1".to_string(),
                    model: Some("NVIDIA BlueField 3".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
    };
    // Host reports one device with an empty serial and one whose serial is
    // claimed by two different DPU endpoints.
    let host = endpoint(
        "192.0.2.20",
        EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            systems: vec![ComputerSystem {
                pcie_devices: vec![
                    pcie("900-9D3B4-00EN-EA0", "32.38.1002", "", "188-0"),
                    pcie("900-9D3B4-00EN-EA0", "32.38.1002", "DUP123", "204-0"),
                ],
                ..Default::default()
            }],
            ..Default::default()
        },
    );

    let devices = collect_explored_mlx_devices(&[
        host,
        dpu("192.0.2.50", "DUP123"),
        dpu("192.0.2.51", "DUP123"),
    ]);

    // Both devices project, but neither joins: the empty serial is skipped and
    // the duplicated "DUP123" serial is ambiguous.
    assert_eq!(devices.len(), 2);
    for device in &devices {
        assert_eq!(device.dpu_bmc_ip, None);
        assert_eq!(device.nic_mode, None);
    }
}
