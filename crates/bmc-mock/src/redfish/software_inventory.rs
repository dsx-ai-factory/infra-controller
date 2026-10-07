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

use serde_json::json;

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

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
        odata_type: Cow::Borrowed("#SoftwareInventory.v1_4_0.SoftwareInventory"),
        name: Cow::Borrowed("Firmware Inventory Item"),
        id: Cow::Borrowed(id),
    }
}

/// Generate resource bound to chassis.
pub(crate) fn builder(resource: &redfish::Resource) -> SoftwareInventoryBuilder {
    SoftwareInventoryBuilder {
        id: Cow::Owned(resource.id.to_string()),
        value: resource.json_patch(),
    }
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
        if let Some(object) = self.value.as_object_mut() {
            object.insert(
                "Version".to_string(),
                serde_json::Value::String(version.to_string()),
            );
        }
    }
}

pub(crate) struct SoftwareInventoryBuilder {
    id: Cow<'static, str>,
    value: serde_json::Value,
}

impl Builder for SoftwareInventoryBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            id: self.id,
        }
    }
}

impl SoftwareInventoryBuilder {
    pub(crate) fn name(self, value: &str) -> Self {
        self.add_str_field("Name", value)
    }

    pub(crate) fn manufacturer(self, value: &str) -> Self {
        self.add_str_field("Manufacturer", value)
    }

    pub(crate) fn software_id(self, value: &str) -> Self {
        self.add_str_field("SoftwareId", value)
    }

    pub(crate) fn version(self, value: &str) -> Self {
        self.add_str_field("Version", value)
    }

    pub(crate) fn status(self, value: redfish::resource::Status) -> Self {
        self.apply_patch(json!({ "Status": value.into_json() }))
    }

    pub(crate) fn updateable(self, value: bool) -> Self {
        self.apply_patch(json!({ "Updateable": value }))
    }

    pub(crate) fn related_items(self, odata_ids: &[&str]) -> Self {
        let items = odata_ids
            .iter()
            .map(|odata_id| json!({ "@odata.id": odata_id }))
            .collect::<Vec<_>>();
        self.apply_patch(json!({
            "RelatedItem": items,
            "RelatedItem@odata.count": odata_ids.len(),
        }))
    }

    pub(crate) fn build(self) -> SoftwareInventory {
        SoftwareInventory {
            id: self.id,
            value: self.value,
        }
    }
}
