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

use mac_address::MacAddress;
use serde_json::json;

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

pub(super) fn manager_collection(manager_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Managers/{manager_id}/EthernetInterfaces");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#EthernetInterfaceCollection.EthernetInterfaceCollection"),
        name: Cow::Borrowed("Ethernet Network Interface Collection"),
    }
}

pub(crate) fn manager_resource<'a>(
    manager_id: &'a str,
    iface_id: &'a str,
) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Managers/{manager_id}/EthernetInterfaces/{iface_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#EthernetInterface.v1_6_0.EthernetInterface"),
        id: Cow::Borrowed(iface_id),
        name: Cow::Borrowed("Manager Ethernet Interface"),
    }
}

pub(super) fn system_collection(system_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/EthernetInterfaces");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#EthernetInterfaceCollection.EthernetInterfaceCollection"),
        name: Cow::Borrowed("Ethernet Network Interface Collection"),
    }
}

pub(crate) fn system_resource<'a>(system_id: &str, iface_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/EthernetInterfaces/{iface_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#EthernetInterface.v1_6_0.EthernetInterface"),
        id: Cow::Borrowed(iface_id),
        name: Cow::Borrowed("System Ethernet Interface"),
    }
}

pub(crate) fn builder(resource: &redfish::Resource) -> EthernetInterfaceBuilder {
    EthernetInterfaceBuilder {
        id: Cow::Owned(resource.id.to_string()),
        value: resource.json_patch(),
    }
}

#[derive(Clone)]
pub(crate) struct EthernetInterface {
    pub(crate) id: Cow<'static, str>,
    value: serde_json::Value,
}

impl EthernetInterface {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }
}

pub(crate) struct EthernetInterfaceBuilder {
    id: Cow<'static, str>,
    value: serde_json::Value,
}

impl Builder for EthernetInterfaceBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            id: self.id,
        }
    }
}

impl EthernetInterfaceBuilder {
    /// Adds the one static IPv4 address exposed by this mock interface.
    pub(crate) fn static_ipv4_address(
        self,
        address: &str,
        subnet_mask: &str,
        gateway: &str,
    ) -> Self {
        self.apply_patch(json!({
            "IPv4StaticAddresses": [{
                "Address": address,
                "SubnetMask": subnet_mask,
                "Gateway": gateway,
            }],
        }))
    }

    pub(crate) fn mac_address(self, addr: MacAddress) -> Self {
        self.add_str_field("MACAddress", &addr.to_string())
    }

    pub(crate) fn interface_enabled(self, v: bool) -> Self {
        self.apply_patch(json!({ "InterfaceEnabled": v }))
    }

    pub(crate) fn description(self, v: &str) -> Self {
        self.add_str_field("Description", v)
    }

    pub(crate) fn build(self) -> EthernetInterface {
        EthernetInterface {
            id: self.id,
            value: self.value,
        }
    }
}
