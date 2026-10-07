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

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

pub(super) fn resource(chassis_id: &str) -> redfish::Resource<'static> {
    let odata_id = format!(
        "{}/PowerSubsystem",
        redfish::chassis::resource(chassis_id).odata_id
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#PowerSubsystem.v1_1_3.PowerSubsystem"),
        id: Cow::Borrowed("PowerSubsystem"),
        name: Cow::Borrowed("Power Subsystem"),
    }
}

pub(super) fn builder(resource: &redfish::Resource) -> PowerSubsystemBuilder {
    PowerSubsystemBuilder {
        value: resource.json_patch(),
    }
}

pub(super) struct PowerSubsystemBuilder {
    value: serde_json::Value,
}

impl Builder for PowerSubsystemBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl PowerSubsystemBuilder {
    pub(super) fn power_supplies(self, v: redfish::Collection) -> Self {
        self.apply_patch(v.nav_property("PowerSupplies"))
    }

    pub(super) fn build(self) -> serde_json::Value {
        self.value
    }
}
