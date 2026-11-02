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

use super::schema::boot_option::{BootOption as BootOptionModel, BootOptionRequired};
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
        odata_type: Cow::Borrowed(redfish::schema::boot_option::BootOption::ODATA_TYPE),
        name: Cow::Borrowed("Uefi Boot Option"),
        id: Cow::Borrowed(boot_option_id),
    }
}

pub(crate) fn builder(resource: &redfish::Resource) -> BootOptionModel {
    BootOptionModel::new(BootOptionRequired {
        odata_id: resource.odata_id.to_string(),
        id: resource.id.to_string(),
        name: resource.name.to_string(),
        boot_option_reference: None,
    })
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

impl BootOptionModel {
    pub(crate) fn build(self, kind: BootOptionKind) -> BootOption {
        BootOption {
            id: self.id.clone().into(),
            reference: self.boot_option_reference.clone(),
            kind,
            value: serde_json::to_value(self).expect("generated boot option is serializable"),
        }
    }
}
