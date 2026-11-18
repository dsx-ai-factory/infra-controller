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

use serde_json::json;

use crate::json::JsonExt;
use crate::redfish;
use crate::redfish::ModelExt;

pub(crate) fn chassis_resource(chassis_id: &str) -> redfish::Resource<'static> {
    let odata_id = format!("/redfish/v1/Chassis/{chassis_id}/Assembly");
    redfish::Resource {
        odata_id: odata_id.into(),
        odata_type: redfish::schema::assembly::Assembly::ODATA_TYPE.into(),
        id: "Assembly".into(),
        name: format!("Assembly data for {chassis_id}").into(),
    }
}

pub(crate) fn response(
    chassis_id: &str,
    member_id: &str,
    serial_number: &str,
) -> serde_json::Value {
    use super::schema::assembly::{Assembly, AssemblyData};
    let resource = chassis_resource(chassis_id);
    let member_uri = format!("{}#/Assemblies/{member_id}", resource.odata_id);
    let model = Assembly::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
    .assemblies([member_uri.as_str()]);
    let assembly = AssemblyData::new(member_uri, member_id).serial_number(serial_number);
    // The generator represents navigation properties as links; assembly data
    // is embedded in the containing resource by Redfish.
    model.into_json().patch(json!({"Assemblies": [assembly]}))
}
