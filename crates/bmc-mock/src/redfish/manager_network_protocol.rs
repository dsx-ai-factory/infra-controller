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

pub(super) fn manager_resource<'a>(manager_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Managers/{manager_id}/NetworkProtocol");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#ManagerNetworkProtocol.v1_5_0.ManagerNetworkProtocol"),
        id: Cow::Borrowed("NetworkProtocol"),
        name: Cow::Borrowed("Manager Network Protocol"),
    }
}

/// Get builder of the network adapter.
pub(super) fn builder(resource: &redfish::Resource) -> ManagerNetworkProtocolBuilder {
    ManagerNetworkProtocolBuilder {
        value: resource.json_patch(),
    }
}

pub(super) struct ManagerNetworkProtocolBuilder {
    value: serde_json::Value,
}

impl Builder for ManagerNetworkProtocolBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl ManagerNetworkProtocolBuilder {
    pub(super) fn ipmi(self, enabled: bool, port: Option<u16>) -> Self {
        let value = self.apply_patch(json!({"IPMI": { "ProtocolEnabled": enabled }}));
        match port {
            Some(port) => value.apply_patch(json!({"IPMI": { "Port": port }})),
            None => value,
        }
    }

    pub(super) fn ntp(self, protocol_enabled: bool, servers: &[impl AsRef<str>]) -> Self {
        let servers = servers.iter().map(AsRef::as_ref).collect::<Vec<_>>();
        self.apply_patch(json!({
            "NTP": {
                "NTPServers": servers,
                "ProtocolEnabled": protocol_enabled,
            },
        }))
    }

    pub(super) fn build(self) -> serde_json::Value {
        self.value
    }
}
