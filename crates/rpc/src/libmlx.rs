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

use std::str::FromStr;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
use carbide_libmlx_model::nvconfig::DpuNvConfigProfile;
use carbide_utils::none_if_empty::NoneIfEmpty;
use mac_address::MacAddress;

use crate::forge::DpuNvConfigProfile as DpuNvConfigProfilePb;
use crate::protos::mlx_device::{
    FirmwareFlashReport as FirmwareFlashReportPb, MlxDeviceInfo as MlxDeviceInfoPb,
};

impl From<DpuNvConfigProfile> for DpuNvConfigProfilePb {
    fn from(profile: DpuNvConfigProfile) -> Self {
        match profile {
            DpuNvConfigProfile::Gb200B3240V1 => Self::Gb200B3240V1,
        }
    }
}

// Implement conversion from Rust MlxDeviceInfo to protobuf.
impl From<MlxDeviceInfo> for MlxDeviceInfoPb {
    fn from(info: MlxDeviceInfo) -> Self {
        MlxDeviceInfoPb {
            pci_name: info.pci_name,
            device_type: info.device_type,
            psid: info.psid.unwrap_or_default(),
            device_description: info.device_description.unwrap_or_default(),
            part_number: info.part_number.unwrap_or_default(),
            fw_version_current: info.fw_version_current.unwrap_or_default(),
            pxe_version_current: info.pxe_version_current.unwrap_or_default(),
            uefi_version_current: info.uefi_version_current.unwrap_or_default(),
            uefi_version_virtio_blk_current: info
                .uefi_version_virtio_blk_current
                .unwrap_or_default(),
            uefi_version_virtio_net_current: info
                .uefi_version_virtio_net_current
                .unwrap_or_default(),
            base_mac: info.base_mac.map(|mac| mac.to_string()).unwrap_or_default(),
            status: info.status.unwrap_or_default(),
        }
    }
}

// Implement conversion from protobuf MlxDeviceInfo to Rust.
impl TryFrom<MlxDeviceInfoPb> for MlxDeviceInfo {
    type Error = String;

    fn try_from(proto: MlxDeviceInfoPb) -> Result<Self, Self::Error> {
        let base_mac = if proto.base_mac.is_empty() {
            None
        } else {
            Some(
                MacAddress::from_str(&proto.base_mac)
                    .map_err(|e| format!("Invalid MAC address '{}': {}", proto.base_mac, e))?,
            )
        };

        Ok(MlxDeviceInfo {
            pci_name: proto.pci_name,
            device_type: proto.device_type,
            psid: proto.psid.none_if_empty(),
            device_description: proto.device_description.none_if_empty(),
            part_number: proto.part_number.none_if_empty(),
            fw_version_current: proto.fw_version_current.none_if_empty(),
            pxe_version_current: proto.pxe_version_current.none_if_empty(),
            uefi_version_current: proto.uefi_version_current.none_if_empty(),
            uefi_version_virtio_blk_current: proto.uefi_version_virtio_blk_current.none_if_empty(),
            uefi_version_virtio_net_current: proto.uefi_version_virtio_net_current.none_if_empty(),
            status: proto.status.none_if_empty(),
            base_mac,
        })
    }
}

// From implementations for converting FirmwareFlashReport
// to/from a FirmwareFlashReportPb protobuf message and back.
impl From<FirmwareFlashReport> for FirmwareFlashReportPb {
    fn from(result: FirmwareFlashReport) -> Self {
        FirmwareFlashReportPb {
            flashed: result.flashed,
            reset: result.reset,
            verified_image: result.verified_image,
            verified_version: result.verified_version,
            observed_version: result.observed_version,
            expected_version: result.expected_version,
        }
    }
}

impl From<FirmwareFlashReportPb> for FirmwareFlashReport {
    fn from(proto: FirmwareFlashReportPb) -> Self {
        FirmwareFlashReport {
            flashed: proto.flashed,
            reset: proto.reset,
            verified_image: proto.verified_image,
            verified_version: proto.verified_version,
            observed_version: proto.observed_version,
            expected_version: proto.expected_version,
        }
    }
}

#[cfg(test)]
mod test {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    // Proto -> model `TryFrom`: every input proto should convert back to the
    // expected `MlxDeviceInfo`, with empty proto strings (and an empty MAC)
    // becoming `None`. The roundtrip rows feed a model through its own
    // `Into<MlxDeviceInfoPb>` first.
    #[test]
    fn device_info_proto_to_model() {
        scenarios!(
            run = |proto: MlxDeviceInfoPb| MlxDeviceInfo::try_from(proto).map_err(drop);
            "full device roundtrips" {
                MlxDeviceInfo::create_test_device().into() => Yields(MlxDeviceInfo::create_test_device()),
            }

            "missing-data device roundtrips" {
                MlxDeviceInfo::create_test_device_with_missing_data().into() => Yields(MlxDeviceInfo::create_test_device_with_missing_data()),
            }

            "empty string fields become none" {
                MlxDeviceInfoPb {
                    pci_name: "01:00.0".to_string(),
                    device_type: "BlueField3".to_string(),
                    psid: "".to_string(), // Empty string should become None
                    device_description: "".to_string(),
                    part_number: "".to_string(),
                    fw_version_current: "".to_string(),
                    pxe_version_current: "".to_string(),
                    uefi_version_current: "".to_string(),
                    uefi_version_virtio_blk_current: "".to_string(),
                    uefi_version_virtio_net_current: "".to_string(),
                    base_mac: "".to_string(), // Empty MAC becomes None
                    status: "".to_string(),
                } => Yields(MlxDeviceInfo {
                    pci_name: "01:00.0".to_string(),
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
                    status: None,
                }),
            }
        );
    }

    // `FirmwareFlashReport` -> proto -> `FirmwareFlashReport` roundtrip. The
    // report isn't `PartialEq`, so project to the tuple of fields the originals
    // asserted; the conversions are total (`From`), hence `check_values`.
    #[test]
    fn firmware_flash_report_roundtrip() {
        value_scenarios!(
            run = |original: FirmwareFlashReport| {
                let proto: FirmwareFlashReportPb = original.into();
                let converted: FirmwareFlashReport = proto.into();
                (
                    converted.flashed,
                    converted.reset,
                    converted.verified_image,
                    converted.verified_version,
                    converted.observed_version,
                    converted.expected_version,
                )
            };
            "all steps success" {
                FirmwareFlashReport {
                    flashed: true,
                    reset: Some(true),
                    verified_image: Some(true),
                    verified_version: Some(true),
                    observed_version: Some("32.43.1014".to_string()),
                    expected_version: Some("32.43.1014".to_string()),
                } => (
                    true,
                    Some(true),
                    Some(true),
                    Some(true),
                    Some("32.43.1014".to_string()),
                    Some("32.43.1014".to_string()),
                ),
            }

            "flash only" {
                FirmwareFlashReport {
                    flashed: true,
                    reset: None,
                    verified_image: None,
                    verified_version: None,
                    observed_version: None,
                    expected_version: None,
                } => (true, None, None, None, None, None),
            }

            "partial failure" {
                FirmwareFlashReport {
                    flashed: true,
                    reset: Some(false),
                    verified_image: Some(false),
                    verified_version: Some(false),
                    observed_version: Some("32.42.900".to_string()),
                    expected_version: Some("32.43.1014".to_string()),
                } => (
                    true,
                    Some(false),
                    Some(false),
                    Some(false),
                    Some("32.42.900".to_string()),
                    Some("32.43.1014".to_string()),
                ),
            }
        );
    }

    #[test]
    fn test_flasher_result_default() {
        let report = FirmwareFlashReport::default();
        assert!(!report.flashed);
        assert!(report.reset.is_none());
        assert!(report.verified_image.is_none());
        assert!(report.verified_version.is_none());
        assert!(report.observed_version.is_none());
        assert!(report.expected_version.is_none());
    }
}
