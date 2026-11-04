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
use crate::redfish::schema::host_interface as schema;

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
        odata_type: Cow::Borrowed(redfish::schema::host_interface::HostInterface::ODATA_TYPE),
        id: Cow::Borrowed(iface_id),
        name: Cow::Borrowed("Host Interface"),
    }
}

pub(crate) fn builder(resource: &redfish::Resource) -> schema::HostInterface {
    schema::HostInterface::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
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

impl schema::HostInterface {
    pub(crate) fn build(self) -> HostInterface {
        HostInterface {
            id: Cow::Owned(self.id.clone()),
            value: serde_json::to_value(self).expect("generated Redfish model is serializable"),
        }
    }
}
