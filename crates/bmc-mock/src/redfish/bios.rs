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

pub(crate) fn resource<'a>(system_id: &str) -> redfish::Resource<'a> {
    let odata_id = format!(
        "{}/Bios",
        redfish::computer_system::resource(system_id).odata_id
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#Bios.v1_2_0.Bios"),
        name: Cow::Borrowed("BIOS Configuration"),
        id: Cow::Borrowed("BIOS"),
    }
}

pub(super) fn change_password_target(resource: &redfish::Resource<'_>) -> String {
    format!("{}/Actions/Bios.ChangePassword", resource.odata_id)
}

pub(crate) fn builder(resource: &redfish::Resource) -> BiosBuilder {
    BiosBuilder {
        value: resource.json_patch(),
    }
}

pub(crate) struct BiosBuilder {
    value: serde_json::Value,
}

impl BiosBuilder {
    pub(crate) fn attributes(self, value: serde_json::Value) -> Self {
        self.apply_patch(json!({"Attributes": value}))
    }

    /// libredfish's HPE `Bios` model requires `@odata.context` (real iLOs
    /// always send it); without it the machine controller's lockdown check
    /// fails to deserialize the response.
    pub(crate) fn odata_context(self, value: &str) -> Self {
        self.apply_patch(json!({"@odata.context": value}))
    }

    pub(crate) fn build(self) -> serde_json::Value {
        self.value
    }

    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}
