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
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

pub(crate) fn resource<'a>() -> redfish::Resource<'a> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1"),
        odata_type: Cow::Borrowed("#ServiceRoot.v1_10_0.ServiceRoot"),
        id: Cow::Borrowed("RootService"),
        name: Cow::Borrowed("Root Service"),
    }
}

pub(crate) fn add_routes(r: Router<BmcState>) -> Router<BmcState> {
    r.route(&resource().odata_id, get(get_service_root))
}

fn builder(resource: &redfish::Resource) -> ServiceRootBuilder {
    ServiceRootBuilder {
        value: resource.json_patch().patch(json!({
            "Links": {
                "Sessions": {
                    "@odata.id": "/redfish/v1/SessionService/Sessions"
                }
            },
        })),
    }
}

async fn get_service_root(State(state): State<BmcState>) -> Response {
    let builder = builder(&resource())
        .redfish_version(state.bmc_redfish_version)
        .maybe_with(
            ServiceRootBuilder::vendor,
            &state.bmc_vendor.service_root_value(),
        )
        .maybe_with(ServiceRootBuilder::product, &state.bmc_product)
        .account_service(&redfish::account_service::resource())
        .session_service(&redfish::session_service::service_resource())
        .chassis_collection(&redfish::chassis::collection());
    // Delta power shelves advertise no `Systems` collection (see `BmcState`).
    let builder = if state.exposes_computer_systems {
        builder.system_collection(&redfish::computer_system::collection())
    } else {
        builder
    };
    let builder = if state.event_service.is_some() {
        builder.event_service(&redfish::event_service::resource())
    } else {
        builder
    };
    builder
        .manager_collection(&redfish::manager::collection())
        .update_service(&redfish::update_service::resource())
        .telemetry_service(&redfish::telemetry_service::resource())
        .build()
        .into_ok_response()
}

struct ServiceRootBuilder {
    value: serde_json::Value,
}

impl Builder for ServiceRootBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl ServiceRootBuilder {
    fn build(self) -> serde_json::Value {
        self.value
    }

    fn redfish_version(self, v: &str) -> Self {
        self.add_str_field("RedfishVersion", v)
    }

    fn vendor(self, v: &str) -> Self {
        self.add_str_field("Vendor", v)
    }

    fn product(self, v: &str) -> Self {
        self.add_str_field("Product", v)
    }

    fn account_service(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("AccountService"))
    }

    fn session_service(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("SessionService"))
    }

    fn chassis_collection(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("Chassis"))
    }

    fn system_collection(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("Systems"))
    }

    fn manager_collection(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("Managers"))
    }

    fn update_service(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("UpdateService"))
    }

    fn event_service(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("EventService"))
    }

    fn telemetry_service(self, v: &redfish::Resource<'_>) -> Self {
        self.apply_patch(v.nav_property("TelemetryService"))
    }
}
