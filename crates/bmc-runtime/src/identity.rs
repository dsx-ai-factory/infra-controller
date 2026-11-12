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

//! The [`PlatformIdentity`] driver selection matches on, projected from an
//! exploration report.

use bmc_platform::{
    ChassisIdentity, FirmwareInventoryIdentity, ManagerIdentity, PlatformIdentity,
    ServiceRootIdentity, SystemIdentity,
};
use model::site_explorer::EndpointExplorationReport;

/// The identity of what `report` recorded: its service root, first manager,
/// primary system, chassis, and firmware inventory without drive entries.
pub fn platform_identity(report: &EndpointExplorationReport) -> PlatformIdentity {
    let service_root = report.service_root.clone().unwrap_or_default();
    PlatformIdentity {
        service_root: ServiceRootIdentity {
            vendor: service_root.vendor,
            product: service_root.product,
            oem_keys: service_root.oem_keys,
        },
        manager: report.managers.first().map(|manager| ManagerIdentity {
            id: manager.id.clone(),
            model: manager.model.clone(),
            firmware: manager.firmware_version.clone(),
            mac_addresses: manager
                .ethernet_interfaces
                .iter()
                .filter_map(|interface| interface.mac_address)
                .collect(),
        }),
        system: report.systems.first().map(|system| SystemIdentity {
            id: system.id.clone(),
            manufacturer: system.manufacturer.clone(),
            model: system.model.clone(),
            sku: system.sku.clone(),
            part_number: system.part_number.clone(),
            bios_version: system.bios_version.clone(),
        }),
        chassis: report
            .chassis
            .iter()
            .map(|chassis| ChassisIdentity {
                id: chassis.id.clone(),
                manufacturer: chassis.manufacturer.clone(),
                model: chassis.model.clone(),
                part_number: chassis.part_number.clone(),
            })
            .collect(),
        firmware_inventory: report
            .service
            .iter()
            .filter(|service| service.id == "FirmwareInventory")
            .flat_map(|service| &service.inventories)
            .filter(|inventory| inventory.drive.is_none())
            .map(|inventory| FirmwareInventoryIdentity {
                id: inventory.id.clone(),
                version: inventory.version.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use model::site_explorer::{
        Chassis, ComputerSystem, DriveIdentity, EthernetInterface, Inventory, Manager, Service,
        ServiceRoot as ModelServiceRoot,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn identity_takes_the_primary_resources_and_firmware_without_drives() {
        let inventory = |id: &str, version: &str, drive: Option<DriveIdentity>| Inventory {
            id: id.to_string(),
            version: Some(version.to_string()),
            drive,
            ..Default::default()
        };
        let report = EndpointExplorationReport {
            service_root: Some(ModelServiceRoot {
                vendor: Some("Dell".to_string()),
                product: Some("Integrated Dell Remote Access Controller".to_string()),
                oem_keys: vec!["Dell".to_string()],
            }),
            managers: vec![Manager {
                id: "iDRAC.Embedded.1".to_string(),
                model: Some("16G Monolithic".to_string()),
                firmware_version: Some("7.10.30.00".to_string()),
                ethernet_interfaces: vec![
                    EthernetInterface {
                        mac_address: Some("C4:5A:B1:C8:38:0D".parse().unwrap()),
                        ..Default::default()
                    },
                    EthernetInterface::default(),
                ],
                ..Default::default()
            }],
            systems: vec![
                ComputerSystem {
                    id: "System.Embedded.1".to_string(),
                    manufacturer: Some("Dell Inc.".to_string()),
                    model: Some("PowerEdge R760".to_string()),
                    sku: Some("ABC1234".to_string()),
                    part_number: Some("0X1Y2Z".to_string()),
                    bios_version: Some("2.3.5".to_string()),
                    ..Default::default()
                },
                ComputerSystem {
                    id: "System.Embedded.2".to_string(),
                    ..Default::default()
                },
            ],
            chassis: vec![Chassis {
                id: "System.Embedded.1".to_string(),
                manufacturer: Some("Dell Inc.".to_string()),
                model: Some("PowerEdge R760".to_string()),
                part_number: Some("0X1Y2Z".to_string()),
                ..Default::default()
            }],
            service: vec![Service {
                id: "FirmwareInventory".to_string(),
                inventories: vec![
                    inventory("Installed-BIOS", "2.3.5", None),
                    inventory(
                        "Drive_Dell_Disk.0",
                        "1.0",
                        Some(DriveIdentity {
                            model: "Dell".to_string(),
                            media_type: None,
                        }),
                    ),
                ],
            }],
            ..Default::default()
        };

        let expected: PlatformIdentity = serde_json::from_value(json!({
            "service_root": {
                "vendor": "Dell",
                "product": "Integrated Dell Remote Access Controller",
                "oem_keys": ["Dell"]
            },
            "manager": {
                "id": "iDRAC.Embedded.1",
                "model": "16G Monolithic",
                "firmware": "7.10.30.00",
                "mac_addresses": ["C4:5A:B1:C8:38:0D"]
            },
            "system": {
                "id": "System.Embedded.1",
                "manufacturer": "Dell Inc.",
                "model": "PowerEdge R760",
                "sku": "ABC1234",
                "part_number": "0X1Y2Z",
                "bios_version": "2.3.5"
            },
            "chassis": [{
                "id": "System.Embedded.1",
                "manufacturer": "Dell Inc.",
                "model": "PowerEdge R760",
                "part_number": "0X1Y2Z"
            }],
            "firmware_inventory": [{ "id": "Installed-BIOS", "version": "2.3.5" }]
        }))
        .unwrap();
        assert_eq!(platform_identity(&report), expected);
    }

    /// A BMC read live is identified by the resources exploration would
    /// record: the host that reports a BIOS rather than a baseboard listed
    /// before it, and the manager linked to that host.
    #[tokio::test]
    async fn fetched_identity_names_the_host_system_and_its_manager() {
        let bmc = bmc_mock::test_support::dgx_gb300_bmc().await;

        let identity = PlatformIdentity::fetch(bmc.service_root.as_ref())
            .await
            .expect("the identity reads");

        let system = identity.system.expect("the host system is named");
        assert_eq!(system.id, "System_0");
        assert!(system.model.is_some());
        assert_eq!(
            identity.manager.map(|manager| manager.id).as_deref(),
            Some("BMC_0")
        );
        assert!(identity.service_root.vendor.is_some());
    }
}
