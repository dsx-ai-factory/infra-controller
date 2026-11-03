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

use super::schema::component_integrity::ComponentIntegrityType;
use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::redfish::ModelExt;
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
        odata_type: Cow::Borrowed(
            redfish::schema::component_integrity::ComponentIntegrity::ODATA_TYPE,
        ),
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
    pub(crate) target_component_uri: Cow<'static, str>,
    /// The attestation protocol.
    pub(crate) integrity_type: ComponentIntegrityType,
    /// Read-write on real hardware, so a configured device can be present and
    /// switched off.
    pub(crate) enabled: bool,
}

impl ComponentIntegrity {
    fn to_json(&self) -> serde_json::Value {
        use super::schema::component_integrity::{ComponentIntegrity, ComponentIntegrityRequired};
        let resource = resource(&self.id);
        ComponentIntegrity::new(ComponentIntegrityRequired {
            odata_id: resource.odata_id.into_owned(),
            id: resource.id.into_owned(),
            name: resource.name.into_owned(),
            component_integrity_type: self.integrity_type,
            component_integrity_type_version: "1.1.0".to_string(),
            target_component_uri: self.target_component_uri.to_string(),
        })
        .component_integrity_enabled(self.enabled)
        .status(redfish::resource::Status::Ok.into())
        .into_json()
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
