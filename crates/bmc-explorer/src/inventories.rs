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

use model::site_explorer::{Inventory as ModelInventory, Service as ModelService};
use nv_redfish::update_service::SoftwareInventory;
use nv_redfish::{Bmc, ServiceRoot};

use crate::{Error, hw};

pub(crate) struct ExploredInventories<B: Bmc> {
    members: Vec<SoftwareInventory<B>>,
}

impl<B: Bmc> ExploredInventories<B> {
    pub(crate) async fn explore(root: &ServiceRoot<B>) -> Result<Self, Error<B>> {
        Ok(Self {
            members: root
                .update_service()
                .await
                .map_err(Error::nv_redfish("update service"))?
                .ok_or_else(Error::bmc_not_provided("update service"))?
                .firmware_inventories()
                .await
                .map_err(Error::nv_redfish("firmware inventories"))?
                .unwrap_or_default(),
        })
    }

    pub(crate) fn to_model(&self, hw_type: Option<hw::HwType>) -> Vec<ModelService> {
        let fw_inventories = ModelService {
            id: "FirmwareInventory".to_string(),
            inventories: self
                .members
                .iter()
                .map(|inventory| ModelInventory {
                    id: inventory.raw().id.clone(),
                    description: inventory.raw().description.clone().flatten(),
                    version: match hw_type {
                        Some(hw::HwType::Lenovo) => {
                            inventory.version().map(|v| {
                                // Original comment from libredfish:
                                //
                                // Lenovo prepends the last two characters of
                                // their "Build/Vendor" ID and a dash to most
                                // of the versions.  This confuses things, so
                                // trim off anything that's before a dash.
                                v.into_inner()
                                    .split('-')
                                    .next_back()
                                    .unwrap_or("")
                                    .to_string()
                            })
                        }
                        Some(hw::HwType::Gb200) => {
                            inventory.version().map(|v| {
                                // Original comment from libredfish:
                                //
                                // BMC firmware gets prepended with "GB200Nvl-", (L, not 1!) so trim that off when we see it.
                                let x = v.into_inner();
                                x.strip_prefix("GB200Nvl-").unwrap_or(x).to_string()
                            })
                        }
                        _ => inventory.version().map(|v| v.to_string()),
                    },
                    release_date: inventory.release_date().map(|v| v.into_inner().to_string()),
                })
                .collect(),
        };
        vec![fw_inventories]
    }
}
