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

use serde::{Deserialize, Serialize};

/// Identity evidence reported by the Redfish service root.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceRootIdentity {
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub oem_keys: Vec<String>,
}

/// Identity evidence for the selected Redfish Manager resource.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagerIdentity {
    pub model: Option<String>,
    pub firmware: Option<String>,
}

/// Identity evidence for the selected Redfish ComputerSystem resource.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemIdentity {
    /// The resource id is selection evidence for platforms such as BlueField.
    pub id: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub sku: Option<String>,
    pub part_number: Option<String>,
    pub bios_version: Option<String>,
}

/// Identity evidence for one Redfish Chassis resource.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChassisIdentity {
    pub id: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub part_number: Option<String>,
}

/// Minimal hardware and firmware projection used to select operation drivers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformIdentity {
    pub service_root: ServiceRootIdentity,
    pub manager: Option<ManagerIdentity>,
    pub system: Option<SystemIdentity>,
    pub chassis: Vec<ChassisIdentity>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn identity_preserves_structured_evidence_and_defaults_missing_sections() {
        let identity: PlatformIdentity = serde_json::from_value(json!({
            "service_root": {
                "vendor": "NVIDIA",
                "product": "GB NVL",
                "oem_keys": ["Nvidia"]
            },
            "manager": {
                "model": "OpenBMC",
                "firmware": "1.2.3"
            },
            "system": {
                "id": "System_0",
                "manufacturer": "NVIDIA",
                "model": "GB300"
            },
            "chassis": [
                {
                    "id": "Chassis_0",
                    "manufacturer": "NVIDIA",
                    "model": "GB300"
                },
                {
                    "id": "Riser_0",
                    "manufacturer": "NVIDIA",
                    "part_number": "2G535"
                }
            ]
        }))
        .expect("platform identity deserializes");

        assert_eq!(identity.chassis.len(), 2);
        assert_eq!(identity.chassis[1].part_number.as_deref(), Some("2G535"));
        assert_eq!(
            identity.system.as_ref().map(|system| system.id.as_str()),
            Some("System_0")
        );
        assert_eq!(
            identity
                .manager
                .as_ref()
                .and_then(|manager| manager.firmware.as_deref()),
            Some("1.2.3")
        );

        let partial: PlatformIdentity = serde_json::from_value(json!({
            "service_root": {"vendor": "Dell"}
        }))
        .expect("missing full-identity sections default");
        assert!(partial.manager.is_none());
        assert!(partial.system.is_none());
        assert!(partial.chassis.is_empty());
    }
}
