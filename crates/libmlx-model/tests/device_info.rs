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
use carbide_test_support::value_scenarios;

enum Fixture {
    Complete,
    Partial,
}

struct FieldCase {
    fixture: Fixture,
    field: &'static str,
}

#[test]
fn field_value_cases() {
    value_scenarios!(
        run = |FieldCase { fixture, field }| match fixture {
            Fixture::Complete => MlxDeviceInfo::create_test_device(),
            Fixture::Partial => MlxDeviceInfo::create_test_device_with_missing_data(),
        }
        .get_field_value(field);
        "complete device fields retain their values" {
            FieldCase { fixture: Fixture::Complete, field: "pci_name" }
                => "01:00.0".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "device_type" }
                => "ConnectX-6 Dx".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "psid" }
                => "MT_00000055".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "device_description" }
                => "Mellanox ConnectX-6 Dx EN 100GbE dual port".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "part_number" }
                => "MCX623106AN-CDAT".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "fw_version_current" }
                => "22.32.1010".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "pxe_version_current" }
                => "3.6.0502".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "uefi_version_current" }
                => "14.25.1020".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "uefi_version_virtio_blk_current" }
                => "1.0.0".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "uefi_version_virtio_net_current" }
                => "1.0.0".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "base_mac" }
                => "B8:3F:D2:12:34:56".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "base_guid" }
                => "b83fd20300123456".to_string(),
            FieldCase { fixture: Fixture::Complete, field: "status" }
                => "--".to_string(),
        }

        "partial device fields retain values or use the placeholder" {
            FieldCase { fixture: Fixture::Partial, field: "pci_name" }
                => "b4:00.0".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "device_type" }
                => "BlueField3".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "psid" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "device_description" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "part_number" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "fw_version_current" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "pxe_version_current" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "uefi_version_current" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "uefi_version_virtio_blk_current" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "uefi_version_virtio_net_current" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "base_mac" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "base_guid" }
                => "--".to_string(),
            FieldCase { fixture: Fixture::Partial, field: "status" }
                => "Failed to open device".to_string(),
        }

        "unknown fields use the explicit sentinel" {
            FieldCase { fixture: Fixture::Complete, field: "not_a_field" }
                => "<unknown-field>".to_string(),
        }
    );
}

#[test]
fn all_fields_are_in_display_order() {
    assert_eq!(
        MlxDeviceInfo::get_all_fields(),
        [
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
    );
}
