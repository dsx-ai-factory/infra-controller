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

use super::schema::network_adapter::NetworkAdapter as NetworkAdapterModel;
use crate::{hw, redfish};

const NETWORK_ADAPTER_TYPE: &str = redfish::schema::network_adapter::NetworkAdapter::ODATA_TYPE;
const NETWORK_ADAPTER_NAME: &str = "Network Adapter";

pub(crate) fn chassis_resource(chassis_id: &str, adapter_id: &str) -> redfish::Resource<'static> {
    let odata_id = format!("/redfish/v1/Chassis/{chassis_id}/NetworkAdapters/{adapter_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(NETWORK_ADAPTER_TYPE),
        id: Cow::Owned(adapter_id.into()),
        name: Cow::Borrowed(NETWORK_ADAPTER_NAME),
    }
}

pub(super) fn chassis_collection(chassis_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Chassis/{chassis_id}/NetworkAdapters");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#NetworkAdapterCollection.NetworkAdapterCollection"),
        name: Cow::Borrowed("Network Adapter Collection"),
    }
}

pub(crate) struct NetworkAdapter {
    pub(crate) id: Cow<'static, str>,
    value: serde_json::Value,
    pub(crate) functions: Vec<redfish::network_device_function::NetworkDeviceFunction>,
}

impl NetworkAdapter {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }
    pub(crate) fn find_function(
        &self,
        function_id: &str,
    ) -> Option<&redfish::network_device_function::NetworkDeviceFunction> {
        self.functions.iter().find(|f| f.id.as_ref() == function_id)
    }
}

pub(crate) fn builder(resource: &redfish::Resource) -> NetworkAdapterModel {
    NetworkAdapterModel::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
}

pub(crate) fn builder_from_nic(
    resource: &redfish::Resource,
    nic: &hw::nic::Nic,
) -> NetworkAdapterModel {
    NetworkAdapterModel {
        serial_number: nic.serial_number.as_ref().map(|v| Some(v.to_string())),
        description: nic.description.as_ref().map(|v| Some(v.to_string())),
        manufacturer: nic.manufacturer.as_ref().map(|v| Some(v.to_string())),
        model: nic.model.as_ref().map(|v| Some(v.to_string())),
        part_number: nic.part_number.as_ref().map(|v| Some(v.to_string())),
        ..builder(resource)
    }
}

impl NetworkAdapterModel {
    pub(crate) fn build(self) -> NetworkAdapter {
        NetworkAdapter {
            id: Cow::Owned(self.id.clone()),
            value: serde_json::to_value(self).expect("generated Redfish model is serializable"),
            functions: Vec::new(),
        }
    }

    pub(crate) fn build_with_functions(
        self,
        collection: &redfish::Collection<'_>,
        functions: Vec<redfish::network_device_function::NetworkDeviceFunction>,
    ) -> NetworkAdapter {
        NetworkAdapter {
            functions,
            ..self
                .network_device_functions(collection.odata_id.as_ref())
                .build()
        }
    }
}
