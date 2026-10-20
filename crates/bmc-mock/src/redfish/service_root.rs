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
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;

use super::schema::service_root::{
    Links, ProtocolFeaturesSupported, ServiceRoot, ServiceRootRequired,
};
use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::redfish::ModelExt;
use crate::{Callbacks, redfish};

pub(crate) fn resource<'a>() -> redfish::Resource<'a> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1"),
        odata_type: Cow::Borrowed(redfish::schema::service_root::ServiceRoot::ODATA_TYPE),
        id: Cow::Borrowed("RootService"),
        name: Cow::Borrowed("Root Service"),
    }
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(&resource().odata_id, get(get_service_root::<C>))
}

fn builder(resource: &redfish::Resource) -> ServiceRoot {
    ServiceRoot::new(ServiceRootRequired {
        odata_id: resource.odata_id.to_string(),
        id: resource.id.to_string(),
        name: resource.name.to_string(),
        links: Links::new("/redfish/v1/SessionService/Sessions"),
    })
}

async fn get_service_root<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    let builder = builder(&resource())
        .redfish_version(state.bmc_redfish_version)
        .protocol_features_supported(ProtocolFeaturesSupported::new().filter_query(true))
        .maybe_with(
            |root, vendor: &str| root.vendor(vendor),
            &state.bmc_vendor.service_root_value(),
        )
        .maybe_with(
            |root, product: &str| root.product(product),
            &state.bmc_product,
        )
        .account_service(redfish::account_service::resource().odata_id.as_ref())
        .session_service(
            redfish::session_service::service_resource()
                .odata_id
                .as_ref(),
        )
        .chassis(redfish::chassis::collection().odata_id.as_ref());
    // Delta power shelves advertise no `Systems` collection (see `BmcState`).
    let builder = if state.exposes_computer_systems {
        builder.systems(redfish::computer_system::collection().odata_id.as_ref())
    } else {
        builder
    };
    let builder = if state.event_service.is_some() {
        builder.event_service(redfish::event_service::resource().odata_id.as_ref())
    } else {
        builder
    };
    // Attestation clients read this link to decide whether the BMC has
    // anything to attest, so only a configured collection is advertised.
    let builder = if state.component_integrities.is_some() {
        builder.component_integrity(redfish::component_integrity::collection().odata_id.as_ref())
    } else {
        builder
    };
    builder
        .managers(redfish::manager::collection().odata_id.as_ref())
        .update_service(redfish::update_service::resource().odata_id.as_ref())
        .telemetry_service(redfish::telemetry_service::resource().odata_id.as_ref())
        .into_json()
        .into_ok_response()
}
