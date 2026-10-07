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
use crate::redfish::Builder;
use crate::{BootOptionKind, redfish};

pub(super) fn collection(system_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!(
        "{}/BootOptions",
        redfish::computer_system::resource(system_id).odata_id
    );
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#BootOptionCollection.BootOptionCollection"),
        name: Cow::Borrowed("Boot Options Collection"),
    }
}

pub(crate) fn resource<'a>(system_id: &str, boot_option_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("{}/{boot_option_id}", collection(system_id).odata_id);
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#BootOption.v1_0_4.BootOption"),
        name: Cow::Borrowed("Uefi Boot Option"),
        id: Cow::Borrowed(boot_option_id),
    }
}

pub(crate) fn builder(resource: &redfish::Resource, kind: BootOptionKind) -> BootOptionBuilder {
    BootOptionBuilder {
        id: Cow::Owned(resource.id.to_string()),
        value: resource.json_patch(),
        reference: None,
        kind,
    }
}

pub(crate) struct BootOption {
    pub(crate) id: Cow<'static, str>,
    pub(crate) reference: Option<String>,
    pub(crate) kind: BootOptionKind,
    value: serde_json::Value,
}

impl BootOption {
    pub(crate) fn boot_reference(&self) -> &str {
        self.reference.as_deref().unwrap_or(&self.id)
    }
    pub(crate) fn to_json(&self) -> serde_json::Value {
        self.value.clone()
    }
}

pub(crate) struct BootOptionBuilder {
    id: Cow<'static, str>,
    reference: Option<String>,
    value: serde_json::Value,
    kind: BootOptionKind,
}

impl Builder for BootOptionBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            id: self.id,
            reference: self.reference,
            kind: self.kind,
        }
    }
}

impl BootOptionBuilder {
    pub(crate) fn display_name(self, value: &str) -> Self {
        self.add_str_field("DisplayName", value)
    }

    pub(crate) fn boot_option_reference(self, value: &str) -> Self {
        let mut result = self.add_str_field("BootOptionReference", value);
        result.reference = Some(value.to_string());
        result
    }

    pub(crate) fn uefi_device_path(self, value: &str) -> Self {
        self.add_str_field("UefiDevicePath", value)
    }

    pub(crate) fn alias(self, value: &str) -> Self {
        self.add_str_field("Alias", value)
    }

    pub(crate) fn odata_etag(self, value: &str) -> Self {
        self.add_str_field("@odata.etag", value)
    }

    pub(crate) fn build(self) -> BootOption {
        BootOption {
            id: self.id,
            reference: self.reference,
            value: self.value,
            kind: self.kind,
        }
    }
}
