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
        "{}/ThermalSubsystem",
        redfish::chassis::resource(chassis_id).odata_id
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#ThermalSubsystem.v1_4_0.ThermalSubsystem"),
        id: Cow::Borrowed("ThermalSubsystem"),
        name: Cow::Borrowed("Thermal Subsystem"),
    }
}

pub(super) fn leak_detection_resource(chassis_id: &str) -> redfish::Resource<'static> {
    let odata_id = format!("{}/LeakDetection", resource(chassis_id).odata_id);
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#LeakDetection.v1_0_0.LeakDetection"),
        id: Cow::Borrowed("LeakDetection"),
        name: Cow::Borrowed("Leak Detection"),
    }
}

pub(super) fn builder(resource: &redfish::Resource) -> ThermalSubsystemBuilder {
    ThermalSubsystemBuilder {
        value: resource.json_patch(),
    }
}

pub(super) fn leak_detection_builder(resource: &redfish::Resource) -> LeakDetectionBuilder {
    LeakDetectionBuilder {
        value: resource.json_patch(),
    }
}

pub(super) struct ThermalSubsystemBuilder {
    value: serde_json::Value,
}

impl Builder for ThermalSubsystemBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl ThermalSubsystemBuilder {
    pub(super) fn leak_detection(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("LeakDetection"))
    }

    pub(super) fn build(self) -> serde_json::Value {
        self.value
    }
}

pub(super) struct LeakDetectionBuilder {
    value: serde_json::Value,
}

impl Builder for LeakDetectionBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl LeakDetectionBuilder {
    pub(super) fn leak_detectors(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("LeakDetectors"))
    }

    pub(super) fn build(self) -> serde_json::Value {
        self.value
    }
}
