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

pub(super) fn manager_collection(manager_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Managers/{manager_id}/HostInterfaces");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#HostInterfaceCollection.HostInterfaceCollection"),
        name: Cow::Borrowed("HostInterface Collection"),
    }
}

pub(crate) fn manager_resource<'a>(
    manager_id: &'a str,
    iface_id: &'a str,
) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Managers/{manager_id}/HostInterfaces/{iface_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#HostInterface.v1_3_3.HostInterface"),
        id: Cow::Borrowed(iface_id),
        name: Cow::Borrowed("Host Interface"),
    }
}

pub(crate) fn builder(resource: &redfish::Resource) -> HostInterfaceBuilder {
    HostInterfaceBuilder {
        id: Cow::Owned(resource.id.to_string()),
        value: resource.json_patch(),
    }
}

#[derive(Clone)]
pub(crate) struct HostInterface {
    pub(crate) id: Cow<'static, str>,
    value: serde_json::Value,
}

impl HostInterface {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }
}

pub(crate) struct HostInterfaceBuilder {
    id: Cow<'static, str>,
    value: serde_json::Value,
}

impl Builder for HostInterfaceBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            id: self.id,
        }
    }
}

impl HostInterfaceBuilder {
    pub(crate) fn interface_enabled(self, v: bool) -> Self {
        self.apply_patch(json!({ "InterfaceEnabled": v }))
    }

    pub(crate) fn build(self) -> HostInterface {
        HostInterface {
            id: self.id,
            value: self.value,
        }
    }
}
