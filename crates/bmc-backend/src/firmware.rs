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

use std::sync::Arc;

use bmc_drivers::CatalogError;
use bmc_platform::{Firmware, PlatformError};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::Redfish;
use nv_redfish::schema::software_inventory::SoftwareInventory;
use serde_json::json;

use crate::BackendBmc;
use crate::convert::platform_error;

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::firmware`].
    pub fn firmware(&self) -> Result<BackendFirmware<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendFirmware::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.firmware().map(BackendFirmware::Drivers),
        }
    }
}

/// The firmware operations of a [`BackendBmc`].
pub enum BackendFirmware<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn Firmware<RedfishBmc>, RedfishBmc>),
}

impl BackendFirmware<'_> {
    /// The [`Firmware::inventory`] members whose `Id` `select` accepts.
    /// libredfish reads only those members, so the others cannot fail the
    /// read; the drivers read the whole inventory.
    pub async fn inventory_matching(
        &self,
        select: impl Fn(&str) -> bool,
    ) -> Result<Vec<Arc<SoftwareInventory>>, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let ids = client
                    .get_software_inventories()
                    .await
                    .map_err(platform_error)?;
                let mut inventories = Vec::new();
                for id in ids.iter().filter(|id| select(id)) {
                    let inventory = client.get_firmware(id).await.map_err(platform_error)?;
                    inventories.push(Arc::new(software_inventory(inventory)?));
                }
                Ok(inventories)
            }
            Self::Drivers(firmware) => {
                let mut inventories = firmware.inventory().await?;
                inventories.retain(|inventory| select(&inventory.id));
                Ok(inventories)
            }
        }
    }
}

/// `inventory` as the drivers report it. libredfish keeps no `Name`, which the
/// schema requires, so the `Id` stands in for it.
fn software_inventory(
    inventory: libredfish::model::software_inventory::SoftwareInventory,
) -> Result<SoftwareInventory, PlatformError> {
    serde_json::from_value(json!({
        "@odata.id": inventory.odata.odata_id,
        "Id": inventory.id,
        "Name": inventory.id,
        "Description": inventory.description,
        "Version": inventory.version,
    }))
    .map_err(|error| PlatformError::InvalidResponse {
        message: format!("firmware inventory {}: {error}", inventory.id),
    })
}

#[cfg(test)]
mod tests {
    use carbide_redfish::libredfish::RedfishClientPool;
    use carbide_redfish::libredfish::test_support::RedfishSim;
    use carbide_utils::redfish::BmcAccessInfo;
    use mac_address::MacAddress;

    use super::*;

    #[tokio::test]
    async fn libredfish_inventory_reads_the_selected_members() {
        let sim = Arc::new(RedfishSim::default());
        let access = BmcAccessInfo {
            host: "127.0.0.1".to_string(),
            port: Some(443),
            mac_address: MacAddress::new([2, 0, 0, 0, 0, 2]),
        };
        let bmc = BackendBmc::from(
            sim.client_by_info(&access)
                .await
                .expect("sim client builds"),
        );

        let inventory = bmc
            .firmware()
            .expect("firmware")
            .inventory_matching(|id| id != "DPU_NIC")
            .await
            .expect("inventory succeeds");

        let versions: Vec<_> = inventory
            .iter()
            .map(|inventory| {
                (
                    inventory.id.as_str(),
                    inventory.version.clone().flatten(),
                    inventory.odata_id.to_string(),
                )
            })
            .collect();
        assert_eq!(
            versions,
            [
                (
                    "BMC_Firmware",
                    Some("BF-24.10-17".to_string()),
                    "/redfish/v1/UpdateService/FirmwareInventory/BMC_Firmware".to_string(),
                ),
                (
                    "Bluefield_FW_ERoT",
                    Some("00.02.0180.0000".to_string()),
                    "/redfish/v1/UpdateService/FirmwareInventory/Bluefield_FW_ERoT".to_string(),
                ),
            ]
        );
    }

    #[test]
    fn a_libredfish_inventory_without_a_version_or_description_converts() {
        let inventory =
            software_inventory(libredfish::model::software_inventory::SoftwareInventory {
                odata: libredfish::model::ODataLinks {
                    odata_context: None,
                    odata_id: "/redfish/v1/UpdateService/FirmwareInventory/DPU_UEFI".to_string(),
                    odata_type: "#SoftwareInventory.v1_4_0.SoftwareInventory".to_string(),
                    odata_etag: None,
                    links: None,
                },
                description: None,
                id: "DPU_UEFI".to_string(),
                version: None,
                release_date: None,
            })
            .expect("converts");

        assert_eq!(inventory.id, "DPU_UEFI");
        assert_eq!(inventory.version.flatten(), None);
        assert_eq!(inventory.description.flatten(), None);
    }
}
