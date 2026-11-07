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

use super::schema::pcie_device::PcieDevice;
use crate::{hw, redfish};

const PCIE_DEVICE_TYPE: &str = redfish::schema::pcie_device::PcieDevice::ODATA_TYPE;

pub(crate) fn chassis_resource(chassis_id: &str, dev_id: &str) -> redfish::Resource<'static> {
    let odata_id = format!("/redfish/v1/Chassis/{chassis_id}/PCIeDevices/{dev_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(PCIE_DEVICE_TYPE),
        id: Cow::Owned(dev_id.into()),
        name: Cow::Borrowed("PCIe Device"),
    }
}

pub(super) fn chassis_collection(chassis_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Chassis/{chassis_id}/PCIeDevices");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#PCIeDeviceCollection.PCIeDeviceCollection"),
        name: Cow::Borrowed("PCIeDevice Collection"),
    }
}

/// Generate resource bound to chassis.
pub(crate) fn builder(resource: &redfish::Resource) -> PcieDevice {
    PcieDevice::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
}

pub(crate) fn builder_from_nic(resource: &redfish::Resource, nic: &hw::nic::Nic) -> PcieDevice {
    PcieDevice {
        serial_number: nic.serial_number.as_ref().map(|v| Some(v.to_string())),
        description: nic.description.as_ref().map(|v| Some(v.to_string())),
        manufacturer: nic.manufacturer.as_ref().map(|v| Some(v.to_string())),
        model: nic.model.as_ref().map(|v| Some(v.to_string())),
        part_number: nic.part_number.as_ref().map(|v| Some(v.to_string())),
        firmware_version: nic.firmware_version.as_ref().map(|v| Some(v.to_string())),
        ..builder(resource)
    }
}

pub(crate) struct PCIeDevice {
    pub(crate) id: Cow<'static, str>,
    value: serde_json::Value,
}

impl PCIeDevice {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }
}

impl PcieDevice {
    pub(crate) fn build(self) -> PCIeDevice {
        PCIeDevice {
            id: Cow::Owned(self.id.clone()),
            value: serde_json::to_value(self).expect("generated Redfish model is serializable"),
        }
    }
}
