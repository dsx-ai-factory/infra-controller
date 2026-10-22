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

use crate::json::JsonExt;
use crate::redfish;
use crate::redfish::ModelExt;

pub(crate) fn resource<'a>(system_id: &str) -> redfish::Resource<'a> {
    let odata_id = format!(
        "{}/Bios",
        redfish::computer_system::resource(system_id).odata_id
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(redfish::schema::bios::Bios::ODATA_TYPE),
        name: Cow::Borrowed("BIOS Configuration"),
        id: Cow::Borrowed("BIOS"),
    }
}

pub(super) fn change_password_target(resource: &redfish::Resource<'_>) -> String {
    format!("{}/Actions/Bios.ChangePassword", resource.odata_id)
}

pub(super) fn reset_target(resource: &redfish::Resource<'_>) -> String {
    format!("{}/Actions/Bios.ResetBios", resource.odata_id)
}

/// The `Actions` object of the `Bios` resource. libredfish's HPE driver
/// resolves both action URLs from here instead of building them.
pub(super) fn actions(resource: &redfish::Resource<'_>) -> serde_json::Value {
    json!({
        "Actions": {
            "#Bios.ChangePassword": {"target": change_password_target(resource)},
            "#Bios.ResetBios": {"target": reset_target(resource)},
        }
    })
}

pub(crate) fn builder(resource: &redfish::Resource) -> super::schema::bios::Bios {
    super::schema::bios::Bios::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
}

impl super::schema::bios::Bios {
    /// BIOS attributes are registry-defined. The server generator currently emits
    /// an opaque Attributes struct, so retain the configured attribute payload.
    pub(crate) fn with_attributes(self, value: serde_json::Value) -> serde_json::Value {
        self.into_json().patch(json!({"Attributes": value}))
    }
}
