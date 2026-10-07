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

pub(crate) fn chassis_collection(
    chassis_id: &str,
    network_adapter_id: &str,
) -> redfish::Collection<'static> {
    let odata_id = format!(
        "/redfish/v1/Chassis/{chassis_id}/NetworkAdapters/{network_adapter_id}/NetworkDeviceFunctions"
    );
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(
            "#NetworkDeviceFunctionCollection.NetworkDeviceFunctionCollection",
        ),
        name: Cow::Borrowed("Network Device Function Collection"),
    }
}

pub(crate) fn chassis_resource<'a>(
    chassis_id: &'a str,
    network_adapter_id: &'a str,
    function_id: &'a str,
) -> redfish::Resource<'a> {
    let odata_id = format!(
        "/redfish/v1/Chassis/{chassis_id}/NetworkAdapters/{network_adapter_id}/NetworkDeviceFunctions/{function_id}"
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#NetworkDeviceFunction.v1_7_0.NetworkDeviceFunction"),
        id: Cow::Borrowed(function_id),
        name: Cow::Borrowed("NetworkDeviceFunction"),
    }
}

/// Get builder of the network device function.
pub(crate) fn builder(resource: &redfish::Resource) -> NetworkDeviceFunctionBuilder {
    NetworkDeviceFunctionBuilder {
        id: Cow::Owned(resource.id.to_string()),
        value: resource.json_patch(),
    }
}

pub(crate) struct NetworkDeviceFunction {
    pub(crate) id: Cow<'static, str>,
    value: serde_json::Value,
}

impl NetworkDeviceFunction {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }
}

pub(crate) struct NetworkDeviceFunctionBuilder {
    id: Cow<'static, str>,
    value: serde_json::Value,
}

impl Builder for NetworkDeviceFunctionBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            id: self.id,
        }
    }
}

impl NetworkDeviceFunctionBuilder {
    pub(crate) fn ethernet(self, v: serde_json::Value) -> Self {
        self.apply_patch(json!({ "Ethernet": v }))
    }

    pub(crate) fn oem(self, v: serde_json::Value) -> Self {
        self.apply_patch(json!({ "Oem": v }))
    }

    pub(crate) fn build(self) -> NetworkDeviceFunction {
        NetworkDeviceFunction {
            id: self.id,
            value: self.value,
        }
    }
}
