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

//! Narrow Redfish projections for observed NVIDIA compute-tray position.
//! Decoder errors can contain response values; callers must omit that text from logs.

use std::time::Duration;

use nv_redfish::core::{EntityTypeRef, ODataETag, ODataId};
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};

/// Cumulative deadline for optional GPU position reads, including client waits.
/// Expiration cancels the scan and preserves the chassis-derived report fields.
pub const GPU_POSITION_TIMEOUT: Duration = Duration::from_secs(60);

/// Accepts systems linked to the host chassis or the HGX baseboard's chassis or SMM.
/// Callers must first identify a supported NVIDIA compute-tray platform.
pub fn is_compute_tray_system<'a>(
    system_id: &str,
    mut chassis_ids: impl Iterator<Item = &'a ODataId>,
    host_chassis_ids: &[ODataId],
) -> bool {
    if matches!(system_id, "Bluefield" | "BlueField_0") {
        return false;
    }

    chassis_ids.any(|id| {
        host_chassis_ids.contains(id)
            || (system_id == "HGX_Baseboard_0"
                && matches!(id.last_segment(), Some("HGX_Chassis_0" | "HGX_SMM_0")))
    })
}

/// Valid observed slot and tray values, decoded independently as nonnegative i32.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Position {
    /// Physical slot number, preserving the firmware's numbering.
    #[serde(
        default,
        rename = "TraySlotNumber",
        alias = "ChassisPhysicalSlotNumber",
        deserialize_with = "position_number"
    )]
    pub slot: Option<i32>,

    /// Compute tray index; zero is a valid observation.
    #[serde(
        default,
        rename = "TraySlotIndex",
        alias = "ComputeTrayIndex",
        deserialize_with = "position_number"
    )]
    pub tray: Option<i32>,
}

fn position_number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Integer(i64),
        Invalid(IgnoredAny),
    }

    let value = Number::deserialize(deserializer)?;

    Ok(match value {
        Number::Integer(value) if value >= 0 => i32::try_from(value).ok(),
        _ => None,
    })
}

/// A Redfish resource link advertised by a collection or navigation property.
#[derive(Deserialize)]
pub struct Link {
    /// Resource URI supplied by the BMC.
    #[serde(rename = "@odata.id")]
    pub odata_id: ODataId,
}

/// Chassis relationships used to restrict GPUs to the explored compute tray.
#[derive(Default, Deserialize)]
pub struct Links {
    /// Chassis resources belonging to a computer system.
    #[serde(default, rename = "Chassis")]
    pub chassis: Vec<Link>,
}

/// NVIDIA OEM position properties, ignoring unrelated topology fields.
#[derive(Default, Deserialize)]
pub struct NvidiaOem {
    /// GPU topology position; omitted and null values mean no observation.
    #[serde(default, rename = "MNNVLinkTopology")]
    pub mnnv_link_topology: Option<Position>,
}

/// OEM projection containing the NVIDIA position extension.
#[derive(Default, Deserialize)]
pub struct Oem {
    /// NVIDIA position extension, absent when the resource advertises none.
    #[serde(default, rename = "Nvidia")]
    pub nvidia: NvidiaOem,
}

/// System navigation or collection members used to locate GPU resources.
#[derive(Deserialize)]
pub struct Resource {
    /// URI identifying the resource.
    #[serde(rename = "@odata.id")]
    pub odata_id: ODataId,

    /// Processor collection advertised by a computer system.
    #[serde(default, rename = "Processors")]
    pub processors: Option<Link>,

    /// Chassis membership advertised by a computer system.
    #[serde(default, rename = "Links")]
    pub links: Links,

    /// Resource links advertised by a collection.
    #[serde(default, rename = "Members")]
    pub members: Vec<Link>,
}

impl EntityTypeRef for Resource {
    fn odata_id(&self) -> &ODataId {
        &self.odata_id
    }

    fn etag(&self) -> Option<&ODataETag> {
        None
    }
}

/// Processor position projection that ignores unrelated navigation fields.
#[derive(Deserialize)]
pub struct ProcessorResource {
    /// URI identifying the processor.
    #[serde(rename = "@odata.id")]
    pub odata_id: ODataId,

    /// Processor classification; only GPU resources supply fallback position.
    #[serde(default, rename = "ProcessorType")]
    pub processor_type: Option<String>,

    /// NVIDIA OEM position extension.
    #[serde(default, rename = "Oem")]
    pub oem: Oem,
}

impl EntityTypeRef for ProcessorResource {
    fn odata_id(&self) -> &ODataId {
        &self.odata_id
    }

    fn etag(&self) -> Option<&ODataETag> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systems_must_belong_to_the_compute_tray() {
        let host_chassis = [ODataId::from("/redfish/v1/Chassis/Chassis_0".to_string())];

        let cases = [
            ("host link", "System_0", Some("Chassis_0"), true),
            (
                "HGX baseboard",
                "HGX_Baseboard_0",
                Some("HGX_Chassis_0"),
                true,
            ),
            ("VR HGX SMM", "HGX_Baseboard_0", Some("HGX_SMM_0"), true),
            ("unrelated system", "Other", Some("HGX_Chassis_0"), false),
            ("unrelated chassis", "System_0", Some("Other"), false),
            ("DPU", "BlueField_0", Some("Chassis_0"), false),
            ("missing links", "HGX_Baseboard_0", None, false),
        ];

        carbide_test_support::check_values(
            cases.map(
                |(scenario, system, chassis, expect)| carbide_test_support::Check {
                    scenario,
                    input: (
                        system,
                        chassis.map(|id| ODataId::from(format!("/redfish/v1/Chassis/{id}"))),
                    ),
                    expect,
                },
            ),
            |(system, chassis)| is_compute_tray_system(system, chassis.iter(), &host_chassis),
        );
    }

    #[test]
    fn position_fields_validate_independently() {
        carbide_test_support::value_scenarios!(run = |json| {
            let position: Position = serde_json::from_str(json).expect("position projection");
            (position.slot, position.tray)
        };
            "missing" { "{}" => (None, None), }
            "zero" {
                r#"{"TraySlotNumber":26,"TraySlotIndex":0}"# => (Some(26), Some(0)),
            }
            "negative" {
                r#"{"TraySlotNumber":-1,"TraySlotIndex":16}"# => (None, Some(16)),
            }
            "null" {
                r#"{"TraySlotNumber":26,"TraySlotIndex":null}"# => (Some(26), None),
            }
            "string" {
                r#"{"TraySlotNumber":"26","TraySlotIndex":16}"# => (None, Some(16)),
            }
            "fraction" {
                r#"{"TraySlotNumber":26,"TraySlotIndex":1.5}"# => (Some(26), None),
            }
            "i32 overflow" {
                r#"{"TraySlotNumber":2147483648,"TraySlotIndex":16}"# => (None, Some(16)),
            }
            "i64 overflow" {
                r#"{"TraySlotNumber":18446744073709551615,"TraySlotIndex":16}"# => (None, Some(16)),
            }
            "largest i32" {
                r#"{"TraySlotNumber":2147483647,"TraySlotIndex":65535}"# => (Some(i32::MAX), Some(65535)),
            }
            "malformed CBC" {
                r#"{"ChassisPhysicalSlotNumber":"bad","ComputeTrayIndex":0}"# => (None, Some(0)),
            }
            "CBC aliases" {
                r#"{"ChassisPhysicalSlotNumber":255,"ComputeTrayIndex":0}"# => (Some(255), Some(0)),
            }
        );
    }

    #[test]
    fn unrelated_topology_properties_do_not_prevent_position_decoding() {
        let resource: ProcessorResource = serde_json::from_str(
            r#"{
            "@odata.id":"/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_0",
            "ProcessorType":"GPU",
            "Links":{"Chassis":{"@odata.id":"/redfish/v1/Chassis/HGX_GPU_0"}},
            "Oem":{"Nvidia":{"MNNVLinkTopology":{
                "TraySlotNumber":26,"TraySlotIndex":16,"DeviceID":"malformed"
            }}}
        }"#,
        )
        .expect("narrow projection");

        assert_eq!(
            resource.oem.nvidia.mnnv_link_topology,
            Some(Position {
                slot: Some(26),
                tray: Some(16)
            })
        );
    }
}
