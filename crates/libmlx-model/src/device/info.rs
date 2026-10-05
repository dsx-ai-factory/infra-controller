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

#[cfg(feature = "test-support")]
use std::str::FromStr;

use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

// MlxDeviceInfo represents detailed information about a Mellanox
// network device. Most fields are optional, because when querying
// with mlxfwmanager, a device that is in lockdown won't return
// all data (just pci_name and device_type), which can be the case
// with DPUs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MlxDeviceInfo {
    // pci_name is the PCI address or MST device
    // path for the device.
    pub pci_name: String,
    // device_type identifies the specific
    // Mellanox device model.
    pub device_type: String,
    // psid (Parameter-Set IDentification) is a 16-ASCII character
    // string embedded in the firmware image which provides a unique
    // identification for the configuration of the firmware.
    pub psid: Option<String>,
    // device_description provides a human-readable
    // description of the device.
    pub device_description: Option<String>,
    // part_number is the manufacturer part number
    // for the device.
    pub part_number: Option<String>,
    // fw_version_current is the currently
    // installed firmware version.
    pub fw_version_current: Option<String>,
    // pxe_version_current is the currently installed
    // PXE boot version.
    pub pxe_version_current: Option<String>,
    // uefi_version_current is the currently installed
    // UEFI boot version.
    pub uefi_version_current: Option<String>,
    // uefi_version_virtio_blk_current is the currently
    // installed UEFI VirtIO block driver version.
    pub uefi_version_virtio_blk_current: Option<String>,
    // uefi_version_virtio_net_current is the currently
    // installed UEFI VirtIO network driver version.
    pub uefi_version_virtio_net_current: Option<String>,
    // base_mac is the base MAC address for the device.
    pub base_mac: Option<MacAddress>,
    /// `base_guid` retains MFT's reported `Base_Guid`, or `None` when absent.
    /// It does not establish which devices share a physical card or reset.
    pub base_guid: Option<String>,
    // status is the "status" of the device that is
    // returned. Sometimes there's useful stuff, other
    // times there isn't.
    pub status: Option<String>,
}

impl MlxDeviceInfo {
    pub fn pci_name_pretty(&self) -> String {
        self.pci_name.clone()
    }

    pub fn device_type_pretty(&self) -> String {
        self.device_type.clone()
    }

    pub fn psid_pretty(&self) -> String {
        self.psid.as_deref().unwrap_or("--").to_string()
    }

    pub fn device_description_pretty(&self) -> String {
        self.device_description
            .as_deref()
            .unwrap_or("--")
            .to_string()
    }

    pub fn part_number_pretty(&self) -> String {
        self.part_number.as_deref().unwrap_or("--").to_string()
    }

    pub fn fw_version_current_pretty(&self) -> String {
        self.fw_version_current
            .as_deref()
            .unwrap_or("--")
            .to_string()
    }

    pub fn pxe_version_current_pretty(&self) -> String {
        self.pxe_version_current
            .as_deref()
            .unwrap_or("--")
            .to_string()
    }

    pub fn uefi_version_current_pretty(&self) -> String {
        self.uefi_version_current
            .as_deref()
            .unwrap_or("--")
            .to_string()
    }

    pub fn uefi_version_virtio_blk_current_pretty(&self) -> String {
        self.uefi_version_virtio_blk_current
            .as_deref()
            .unwrap_or("--")
            .to_string()
    }

    pub fn uefi_version_virtio_net_current_pretty(&self) -> String {
        self.uefi_version_virtio_net_current
            .as_deref()
            .unwrap_or("--")
            .to_string()
    }

    pub fn base_mac_pretty(&self) -> String {
        self.base_mac
            .map(|mac| mac.to_string())
            .unwrap_or_else(|| "--".to_string())
    }

    /// `base_guid_pretty` returns the reported GUID, or `--` when absent.
    fn base_guid_pretty(&self) -> String {
        self.base_guid.as_deref().unwrap_or("--").to_string()
    }

    pub fn status_pretty(&self) -> String {
        self.status.as_deref().unwrap_or("--").to_string()
    }

    // get_field_value returns the value of a field by name for
    // display purposes, used by the CLI cmd module, and
    // anything else that wants a "pretty" string representation.
    pub fn get_field_value(&self, field_name: &str) -> String {
        match field_name {
            "pci_name" => self.pci_name_pretty(),
            "device_type" => self.device_type_pretty(),
            "psid" => self.psid_pretty(),
            "device_description" => self.device_description_pretty(),
            "part_number" => self.part_number_pretty(),
            "fw_version_current" => self.fw_version_current_pretty(),
            "pxe_version_current" => self.pxe_version_current_pretty(),
            "uefi_version_current" => self.uefi_version_current_pretty(),
            "uefi_version_virtio_blk_current" => self.uefi_version_virtio_blk_current_pretty(),
            "uefi_version_virtio_net_current" => self.uefi_version_virtio_net_current_pretty(),
            "base_mac" => self.base_mac_pretty(),
            "base_guid" => self.base_guid_pretty(),
            "status" => self.status_pretty(),
            _ => "<unknown-field>".to_string(),
        }
    }
    // get_all_fields returns a vector of all field names for this struct.
    pub fn get_all_fields() -> Vec<&'static str> {
        vec![
            "pci_name",
            "base_mac",
            "base_guid",
            "psid",
            "device_type",
            "part_number",
            "device_description",
            "fw_version_current",
            "pxe_version_current",
            "uefi_version_current",
            "uefi_version_virtio_blk_current",
            "uefi_version_virtio_net_current",
            "status",
        ]
    }
}

#[cfg(feature = "test-support")]
impl MlxDeviceInfo {
    /// create_test_device creates a sample device for testing purposes.
    pub fn create_test_device() -> Self {
        Self {
            pci_name: "01:00.0".to_string(),
            device_type: "ConnectX-6 Dx".to_string(),
            psid: Some("MT_00000055".to_string()),
            device_description: Some("Mellanox ConnectX-6 Dx EN 100GbE dual port".to_string()),
            part_number: Some("MCX623106AN-CDAT".to_string()),
            fw_version_current: Some("22.32.1010".to_string()),
            pxe_version_current: Some("3.6.0502".to_string()),
            uefi_version_current: Some("14.25.1020".to_string()),
            uefi_version_virtio_blk_current: Some("1.0.0".to_string()),
            uefi_version_virtio_net_current: Some("1.0.0".to_string()),
            base_mac: Some(MacAddress::from_str("b8:3f:d2:12:34:56").unwrap()),
            base_guid: Some("b83fd20300123456".to_string()),
            status: None,
        }
    }

    /// create_test_device_with_missing_data creates a device with partial data (like a DPU).
    pub fn create_test_device_with_missing_data() -> Self {
        Self {
            pci_name: "b4:00.0".to_string(),
            device_type: "BlueField3".to_string(),
            psid: None,
            device_description: None,
            part_number: None,
            fw_version_current: None,
            pxe_version_current: None,
            uefi_version_current: None,
            uefi_version_virtio_blk_current: None,
            uefi_version_virtio_net_current: None,
            base_mac: None,
            base_guid: None,
            status: Some("Failed to open device".to_string()),
        }
    }
}
