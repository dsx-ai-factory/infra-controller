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

pub(super) fn resource<'a>(system_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!(
        "{}/SecureBoot",
        redfish::computer_system::resource(system_id).odata_id
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#SecureBoot.v1_1_0.SecureBoot"),
        id: Cow::Borrowed("SecureBoot"),
        name: Cow::Borrowed("UEFI Secure Boot"),
    }
}

pub(super) fn builder(resource: &redfish::Resource) -> SecureBootBuilder {
    SecureBootBuilder {
        value: resource.json_patch(),
    }
}

pub(super) struct SecureBootBuilder {
    value: serde_json::Value,
}

impl Builder for SecureBootBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl SecureBootBuilder {
    pub(super) fn secure_boot_enable(self, v: bool) -> Self {
        self.apply_patch(json!({"SecureBootEnable": v}))
    }

    pub(super) fn secure_boot_current_boot(self, enabled: bool) -> Self {
        if enabled {
            self.add_str_field("SecureBootCurrentBoot", "Enabled")
        } else {
            self.add_str_field("SecureBootCurrentBoot", "Disabled")
        }
    }

    pub(super) fn build(self) -> serde_json::Value {
        self.value
    }
}
