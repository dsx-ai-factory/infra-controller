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

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch};
use crate::redfish::Builder;
use crate::{Callbacks, http, redfish};

pub(crate) fn collection() -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Borrowed("/redfish/v1/ComponentIntegrity"),
        odata_type: Cow::Borrowed("#ComponentIntegrityCollection.ComponentIntegrityCollection"),
        name: Cow::Borrowed("Component Integrity Collection"),
    }
}

pub(crate) fn resource<'a>(component_integrity_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("{}/{component_integrity_id}", collection().odata_id);
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#ComponentIntegrity.v1_2_0.ComponentIntegrity"),
        id: Cow::Borrowed(component_integrity_id),
        name: Cow::Borrowed("Component Integrity"),
    }
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    const COMPONENT_INTEGRITY_ID_PARAM: &str = "{component_integrity_id}";
    r.route(&collection().odata_id, get(get_component_integrities))
        .route(
            &resource(COMPONENT_INTEGRITY_ID_PARAM).odata_id,
            get(get_component_integrity),
        )
}

/// One device the BMC says it can attest.
#[derive(Debug, Clone)]
pub(crate) struct ComponentIntegrity {
    pub(crate) id: Cow<'static, str>,
    /// The attestation protocol, `SPDM` or `TPM`.
    pub(crate) integrity_type: Cow<'static, str>,
    /// Read-write on real hardware, so a configured device can be present and
    /// switched off.
    pub(crate) enabled: bool,
}

impl ComponentIntegrity {
    fn to_json(&self) -> serde_json::Value {
        builder(&resource(&self.id))
            .component_integrity_type(&self.integrity_type)
            .component_integrity_enabled(self.enabled)
            .build()
    }
}

fn builder(resource: &redfish::Resource) -> ComponentIntegrityBuilder {
    ComponentIntegrityBuilder {
        value: resource.json_patch().patch(json!({
            "Status": redfish::resource::Status::Ok.into_json(),
            // Required of the resource and read by no NICo code. Clients
            // deserialize it as non-optional, so it has to be served.
            "ComponentIntegrityTypeVersion": "1.1.0",
        })),
    }
}

struct ComponentIntegrityBuilder {
    value: serde_json::Value,
}

impl Builder for ComponentIntegrityBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl ComponentIntegrityBuilder {
    fn component_integrity_type(self, value: &str) -> Self {
        self.add_str_field("ComponentIntegrityType", value)
    }

    fn component_integrity_enabled(self, value: bool) -> Self {
        self.apply_patch(json!({"ComponentIntegrityEnabled": value}))
    }

    fn build(self) -> serde_json::Value {
        self.value
    }
}

async fn get_component_integrities<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    let Some(component_integrities) = &state.component_integrities else {
        return http::not_found();
    };
    let members = component_integrities
        .iter()
        .map(|component_integrity| resource(&component_integrity.id).entity_ref())
        .collect::<Vec<_>>();
    collection().with_members(&members).into_ok_response()
}

async fn get_component_integrity<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(component_integrity_id): Path<String>,
) -> Response {
    state
        .component_integrities
        .iter()
        .flatten()
        .find(|component_integrity| component_integrity.id == component_integrity_id)
        .map(|component_integrity| component_integrity.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}
