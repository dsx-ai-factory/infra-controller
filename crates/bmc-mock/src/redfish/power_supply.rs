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

pub(crate) fn resource<'a>(chassis_id: &str, supply_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!(
        "{}/PowerSubsystem/PowerSupplies/{supply_id}",
        redfish::chassis::resource(chassis_id).odata_id
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(redfish::schema::power_supply::PowerSupply::ODATA_TYPE),
        id: Cow::Borrowed(supply_id),
        name: Cow::Borrowed("Power Supply"),
    }
}

pub(super) fn collection(chassis_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!(
        "{}/PowerSubsystem/PowerSupplies",
        redfish::chassis::resource(chassis_id).odata_id
    );
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#PowerSupplyCollection.PowerSupplyCollection"),
        name: Cow::Borrowed("Power Supply"),
    }
}

pub(crate) struct PowerSupply {
    pub(crate) id: Cow<'static, str>,
    value: serde_json::Value,
}

impl PowerSupply {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }
}

impl PowerSupply {
    /// LiteOn reports non-standard boolean power state and string capacity,
    /// omitting the standard PowerCapacityWatts field (PF-1333-7R r1.3.8).
    pub(crate) fn liteon(
        resource: &redfish::Resource<'_>,
        powered_on: bool,
        capacity: &str,
    ) -> Self {
        let model = super::schema::power_supply::PowerSupply::new(
            resource.odata_id.as_ref(),
            resource.id.as_ref(),
            resource.name.as_ref(),
        )
        .status(redfish::resource::Status::Ok.into());
        Self {
            id: resource.id.to_string().into(),
            value: model
                .into_json()
                .patch(json!({"PowerState": powered_on, "CapacityWatts": capacity})),
        }
    }

    /// Delta exposes power state and fan control in its OEM extension.
    pub(crate) fn delta(
        resource: &redfish::Resource<'_>,
        powered_on: bool,
        capacity: f64,
        fan_speed_target: i64,
    ) -> Self {
        let model = super::schema::power_supply::PowerSupply::new(
            resource.odata_id.as_ref(),
            resource.id.as_ref(),
            resource.name.as_ref(),
        )
        .power_capacity_watts(
            nv_redfish_server_core::FiniteF64::try_from(capacity).expect("finite PSU capacity"),
        )
        .status(redfish::resource::Status::Ok.into())
        .oem(redfish::resource::oem(
            [(
                "deltaenergysystems".to_string(),
                redfish::oem::object(
                    "#DeltaEnergySystemsPowerSupply.v1_0_0.PowerSupply",
                    super::schema::delta_energy_systems_power_supply::PowerSupply::new()
                        .power(powered_on)
                        .fan_speed_target(fan_speed_target),
                ),
            )]
            .into(),
        ));

        Self {
            id: resource.id.to_string().into(),
            value: model.into_json(),
        }
    }
}
