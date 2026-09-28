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
use carbide_libmlx_model::device::info::MlxDeviceInfo;
use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use libmlx::device::discovery::{convert_pci_name_to_address, parse_mlxfwmanager_xml};

// Test XML to use for a single DPU with failed access due to lockdown.
const DPU_FAILED_XML: &str = r#"
    <Devices>
        <Device pciName="0000:b4:00.0" type="BlueField3" psid="" partNumber="--">
          <Versions>
            <FW current="--" available=""/>
          </Versions>
          <MACs Base_Mac="N/A" />
          <Status>Failed to open device</Status>
          <Description></Description>
        </Device>
    </Devices>
    "#;

// Test XML to use for mixed accessible SuperNICs and a locked down DPU.
const MIXED_DEVICES_XML: &str = r#"
    <Devices>
        <Device pciName="0000:dc:00.0" type="BlueField3" psid="MT_0000001010" partNumber="900-9D3B4-00EN-E_Ax">
          <Versions>
            <FW current="32.42.1000" available="N/A"/>
            <PXE current="3.7.0500" available="N/A"/>
            <UEFI current="14.35.0015" available="N/A"/>
            <UEFI_Virtio_blk current="22.4.0013" available="N/A"/>
            <UEFI_Virtio_net current="21.4.0013" available="N/A"/>
          </Versions>
          <MACs Base_Mac="c470bd31eb46" />
          <Status>No matching image found</Status>
          <Description>NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC; 400GbE / NDR IB (default mode); Single-port QSFP112; PCIe Gen5.0 x16; 8 Arm cores; 16GB on-board DDR; integrated BMC; Crypto Enabled</Description>
        </Device>
        <Device pciName="0000:9d:00.0" type="BlueField3">
          <Status>Failed to open device</Status>
        </Device>
        <Device pciName="0000:9c:00.0" type="BlueField3" psid="MT_0000001010" partNumber="900-9D3B4-00EN-E_Ax">
          <Versions>
            <FW current="32.42.1000" available="N/A"/>
            <PXE current="3.7.0500" available="N/A"/>
            <UEFI current="14.35.0015" available="N/A"/>
            <UEFI_Virtio_blk current="22.4.0013" available="N/A"/>
            <UEFI_Virtio_net current="21.4.0013" available="N/A"/>
          </Versions>
          <MACs Base_Mac="c470bd31ea12" />
          <Status>No matching image found</Status>
          <Description>NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC; 400GbE / NDR IB (default mode); Single-port QSFP112; PCIe Gen5.0 x16; 8 Arm cores; 16GB on-board DDR; integrated BMC; Crypto Enabled</Description>
        </Device>
    </Devices>
    "#;

const MISSING_OPTIONALS_XML: &str = r#"
    <Devices>
        <Device pciName="0000:01:00.0" type="ConnectX-6" psid="N/A" partNumber="N/A">
          <Versions></Versions>
          <MACs Base_Mac="N/A" />
          <Description>N/A</Description>
        </Device>
    </Devices>
    "#;

const EMPTY_DEVICES_XML: &str = "<Devices></Devices>";
const MALFORMED_XML: &str = "<Devices><Device";

// The child command's PATH is private to each case; no test changes the
// process-wide environment or invokes a hardware management tool.
#[cfg(unix)]
#[test]
fn report_collection_preserves_the_command_exit_policy() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use libmlx::device::report::MlxDeviceReport;

    struct Case {
        scenario: &'static str,
        exit_code: Option<u8>,
        error: Option<&'static str>,
    }
    let cases = [
        Case {
            scenario: "successful query",
            exit_code: Some(0),
            error: None,
        },
        Case {
            scenario: "partial query",
            exit_code: Some(1),
            error: None,
        },
        Case {
            scenario: "unexpected tool failure despite valid XML",
            exit_code: Some(2),
            error: Some("mlxfwmanager failed with unexpected exit code"),
        },
        Case {
            scenario: "missing tool",
            exit_code: None,
            error: Some("failed to build cmd"),
        },
    ];
    for case in cases {
        let directory = tempfile::tempdir().unwrap();
        if let Some(exit_code) = case.exit_code {
            let tool = directory.path().join("mlxfwmanager");
            std::fs::write(
                &tool,
                format!("#!/bin/sh\nprintf '%s' \"$MLX_TEST_XML\"\nexit {exit_code}\n"),
            )
            .unwrap();
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_mlxconfig-device"))
            .args(["device", "report", "--format", "json"])
            .env("PATH", directory.path())
            .env("MLX_TEST_XML", MIXED_DEVICES_XML)
            .output()
            .unwrap();
        match case.error {
            Some(error) => {
                assert!(!output.status.success(), "{}", case.scenario);
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains(error),
                    "{}: {:?}",
                    case.scenario,
                    output,
                );
            }
            None => {
                assert!(output.status.success(), "{}: {:?}", case.scenario, output);
                let report: MlxDeviceReport = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(report.devices.len(), 3, "{}", case.scenario);
            }
        }
    }
}

fn device_with_missing_optionals(
    pci_name: &str,
    device_type: &str,
    status: Option<&str>,
) -> MlxDeviceInfo {
    MlxDeviceInfo {
        pci_name: pci_name.to_string(),
        device_type: device_type.to_string(),
        psid: None,
        device_description: None,
        part_number: None,
        fw_version_current: None,
        pxe_version_current: None,
        uefi_version_current: None,
        uefi_version_virtio_blk_current: None,
        uefi_version_virtio_net_current: None,
        base_mac: None,
        status: status.map(str::to_string),
    }
}

fn failed_device(pci_name: &str) -> MlxDeviceInfo {
    device_with_missing_optionals(pci_name, "BlueField3", Some("Failed to open device"))
}

fn accessible_device(pci_name: &str, base_mac: &str) -> MlxDeviceInfo {
    MlxDeviceInfo {
        pci_name: pci_name.to_string(),
        device_type: "BlueField3".to_string(),
        psid: Some("MT_0000001010".to_string()),
        device_description: Some(
            "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC; 400GbE / NDR IB \
             (default mode); Single-port QSFP112; PCIe Gen5.0 x16; 8 Arm cores; \
             16GB on-board DDR; integrated BMC; Crypto Enabled"
                .to_string(),
        ),
        part_number: Some("900-9D3B4-00EN-E_Ax".to_string()),
        fw_version_current: Some("32.42.1000".to_string()),
        pxe_version_current: Some("3.7.0500".to_string()),
        uefi_version_current: Some("14.35.0015".to_string()),
        uefi_version_virtio_blk_current: Some("22.4.0013".to_string()),
        uefi_version_virtio_net_current: Some("21.4.0013".to_string()),
        base_mac: Some(base_mac.parse().unwrap()),
        status: Some("No matching image found".to_string()),
    }
}

#[test]
fn parse_mlxfwmanager_xml_cases() {
    scenarios!(
        run = parse_mlxfwmanager_xml;
        "failed DPU normalizes omitted and placeholder values" {
            DPU_FAILED_XML => Yields(vec![failed_device("b4:00.0")]),
        }

        "mixed devices preserve every parsed field" {
            MIXED_DEVICES_XML => Yields(vec![
                accessible_device("dc:00.0", "c4:70:bd:31:eb:46"),
                failed_device("9d:00.0"),
                accessible_device("9c:00.0", "c4:70:bd:31:ea:12"),
            ]),
        }

        "placeholder fields become absent values" {
            MISSING_OPTIONALS_XML => Yields(vec![
                device_with_missing_optionals("01:00.0", "ConnectX-6", None),
            ]),
        }

        "omitted fields and empty sections become absent values" {
            r#"<Devices><Device pciName="0000:01:00.0" type="ConnectX-8"/></Devices>"#
                => Yields(vec![device_with_missing_optionals("01:00.0", "ConnectX-8", None)]),
            r#"<Devices><Device pciName="0000:01:00.0" type="ConnectX-8">
                <Versions><FW available="N/A"/><PXE current="3.7.0500"/></Versions>
                <MACs/>
            </Device></Devices>"# => Yields(vec![MlxDeviceInfo {
                pxe_version_current: Some("3.7.0500".to_string()),
                ..device_with_missing_optionals("01:00.0", "ConnectX-8", None)
            }]),
        }

        "structural device identity is required" {
            r#"<Devices><Device type="ConnectX-8"/></Devices>"# => Fails,
            r#"<Devices><Device pciName="0000:01:00.0"/></Devices>"# => Fails,
        }

        "empty device lists are rejected" {
            EMPTY_DEVICES_XML => Fails,
        }

        "malformed XML is rejected" {
            MALFORMED_XML => Fails,
        }
    );
}

// convert_pci_name_to_address strips a single leading "0000:" domain prefix from a
// PCI name and passes everything else (clean addresses, mst paths, arbitrary
// strings) through untouched.
#[test]
fn test_convert_pci_name_to_address() {
    scenarios!(
        run = convert_pci_name_to_address;
        "removes domain prefix" {
            "0000:01:00.0" => Yields("01:00.0".to_string()),
        }

        "passes through a clean address" {
            "01:00.0" => Yields("01:00.0".to_string()),
        }

        "passes through an mst path" {
            "/dev/mst/mt41692_pciconf0" => Yields("/dev/mst/mt41692_pciconf0".to_string()),
        }

        "passes through an unrelated format" {
            "custom_device_path" => Yields("custom_device_path".to_string()),
        }

        "removes only the first of multiple domain prefixes" {
            "0000:0000:01:00.0" => Yields("0000:01:00.0".to_string()),
        }

        "passes through an empty string" {
            "" => Yields("".to_string()),
        }
    );
}
