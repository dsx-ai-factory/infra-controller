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

use mac_address::MacAddress;
use nv_redfish::computer_system::ComputerSystem;
use nv_redfish::manager::Manager;
use nv_redfish::{Bmc, ServiceRoot};
use serde::{Deserialize, Serialize};

use crate::{ClassifyBmcError, PlatformError, manager_index_for, primary_system_index};

/// Identity evidence reported by the Redfish service root.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceRootIdentity {
    /// ServiceRoot `Vendor`.
    pub vendor: Option<String>,
    /// ServiceRoot `Product`.
    pub product: Option<String>,
    /// Keys of the ServiceRoot `Oem` object, for example `Ami` or `Dell`.
    pub oem_keys: Vec<String>,
}

impl ServiceRootIdentity {
    /// The identity `root` serves. It is all a BMC in factory state serves,
    /// and all that identifies a BMC exploration could not read further.
    pub fn from_service_root<B: Bmc>(root: &ServiceRoot<B>) -> Self {
        Self {
            vendor: root.vendor().map(|vendor| vendor.into_inner().to_string()),
            product: root
                .product()
                .map(|product| product.into_inner().to_string()),
            oem_keys: root
                .root
                .oem
                .as_ref()
                .and_then(|oem| oem.additional_properties.as_object())
                .map(|oem| oem.keys().cloned().collect())
                .unwrap_or_default(),
        }
    }
}

/// Identity evidence for the selected Redfish Manager resource.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagerIdentity {
    /// Resource id of the manager linked to the selected ComputerSystem.
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub firmware: Option<String>,
    /// MAC addresses of the manager's network interfaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mac_addresses: Vec<MacAddress>,
}

impl ManagerIdentity {
    /// The identity `manager` reports, read as exploration records it but
    /// without MAC addresses, which take a request per network interface.
    pub fn from_manager<B: Bmc>(manager: &Manager<B>) -> Self {
        let raw = manager.raw();
        Self {
            id: raw.id.clone(),
            model: raw.model.clone().flatten(),
            firmware: raw.firmware_version.clone().flatten(),
            mac_addresses: Vec::new(),
        }
    }
}

/// Identity evidence for the selected Redfish ComputerSystem resource.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SystemIdentity {
    /// The resource id is selection evidence for platforms such as BlueField.
    pub id: String,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub sku: Option<String>,
    #[serde(default)]
    pub part_number: Option<String>,
    #[serde(default)]
    pub bios_version: Option<String>,
}

impl SystemIdentity {
    /// The identity `system` reports, read as exploration records it.
    pub fn from_system<B: Bmc>(system: &ComputerSystem<B>) -> Self {
        let hardware = system.hardware_id();
        let raw = system.raw();
        Self {
            id: raw.id.clone(),
            manufacturer: hardware.manufacturer.map(|value| value.to_string()),
            model: hardware.model.map(|value| value.to_string()),
            sku: system.sku().map(|value| value.to_string()),
            part_number: raw.part_number.clone().flatten(),
            bios_version: raw
                .bios_version
                .clone()
                .flatten()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        }
    }
}

/// Identity evidence for one Redfish Chassis resource.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChassisIdentity {
    pub id: String,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub part_number: Option<String>,
}

/// One `UpdateService` firmware inventory entry.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FirmwareInventoryIdentity {
    /// Resource id, such as `UEFI` or `HostBMC_0`.
    pub id: String,
    /// The version as site exploration reports it, with Lenovo's build-id
    /// prefix (`26G-2.20`) and GB200's `GB200Nvl-` prefix trimmed.
    #[serde(default)]
    pub version: Option<String>,
}

/// Minimal hardware and firmware projection used to select operation drivers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformIdentity {
    pub service_root: ServiceRootIdentity,
    pub manager: Option<ManagerIdentity>,
    pub system: Option<SystemIdentity>,
    pub chassis: Vec<ChassisIdentity>,
    pub firmware_inventory: Vec<FirmwareInventoryIdentity>,
}

impl PlatformIdentity {
    /// Reads the identity of a BMC exploration has not identified: its
    /// service root, the system exploration would choose as primary, and that
    /// system's manager. Chassis and firmware inventory stay empty, so rules
    /// that need them cannot match.
    pub async fn fetch<B>(root: &ServiceRoot<B>) -> Result<Self, PlatformError>
    where
        B: Bmc,
        B::Error: ClassifyBmcError,
    {
        let systems = match root.systems().await.map_err(PlatformError::from_redfish)? {
            Some(systems) => systems
                .members()
                .await
                .map_err(PlatformError::from_redfish)?,
            None => Vec::new(),
        };
        let managers = match root.managers().await.map_err(PlatformError::from_redfish)? {
            Some(managers) => managers
                .members()
                .await
                .map_err(PlatformError::from_redfish)?,
            None => Vec::new(),
        };
        let system = primary_system_index(&systems).map(|index| &systems[index]);
        let manager = manager_index_for(&managers, system).map(|index| &managers[index]);
        Ok(Self {
            service_root: ServiceRootIdentity::from_service_root(root),
            manager: manager.map(ManagerIdentity::from_manager),
            system: system.map(SystemIdentity::from_system),
            ..Self::default()
        })
    }
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
                "id": "BMC_0",
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
        assert!(
            serde_json::from_value::<PlatformIdentity>(json!({"system": {"model": "R750"}}))
                .is_err(),
            "a selected resource without its id is rejected"
        );
    }
}
