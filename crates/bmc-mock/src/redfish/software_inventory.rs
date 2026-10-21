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

use std::borrow::Cow;

use crate::redfish;
use crate::redfish::schema::software_inventory as schema;

pub(super) fn firmware_inventory_collection() -> redfish::Collection<'static> {
    let odata_id = format!(
        "{}/FirmwareInventory",
        redfish::update_service::resource().odata_id
    );
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#SoftwareInventoryCollection.SoftwareInventoryCollection"),
        name: Cow::Borrowed("Collection of Firmware Inventory"),
    }
}

pub(crate) fn firmware_inventory_resource<'a>(id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("{}/{id}", firmware_inventory_collection().odata_id);
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(
            redfish::schema::software_inventory::SoftwareInventory::ODATA_TYPE,
        ),
        name: Cow::Borrowed("Firmware Inventory Item"),
        id: Cow::Borrowed(id),
    }
}

/// Generate resource bound to chassis.
pub(crate) fn builder(resource: &redfish::Resource) -> schema::SoftwareInventory {
    schema::SoftwareInventory::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
}

pub(crate) struct SoftwareInventory {
    pub(crate) id: Cow<'static, str>,
    value: serde_json::Value,
}

impl SoftwareInventory {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }

    /// Update the `Version` field in-place.  Used by
    /// `UpdateServiceState::apply_staged_firmware` to reflect a firmware
    /// version that became active after a power-cycle.
    pub(crate) fn set_version(&mut self, version: &str) {
        self.value["Version"] = version.into();
    }
}

impl schema::SoftwareInventory {
    pub(crate) fn build(self) -> SoftwareInventory {
        SoftwareInventory {
            id: Cow::Owned(self.id.clone()),
            value: serde_json::to_value(self).expect("generated Redfish model is serializable"),
        }
    }
}

impl schema::SoftwareInventory {
    pub(crate) fn related_items(self, odata_ids: &[&str]) -> Self {
        self.related_item(odata_ids.iter().copied())
            .related_item_count(odata_ids.len() as u64)
    }
}
