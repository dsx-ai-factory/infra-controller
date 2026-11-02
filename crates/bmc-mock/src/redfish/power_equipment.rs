// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Configured power-equipment inventory, including shelf distribution resources.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::redfish::ModelExt;
use crate::{Callbacks, http, redfish};

pub(super) const RESOURCE: &str = "/redfish/v1/PowerEquipment";
const SHELVES: &str = "/redfish/v1/PowerEquipment/PowerShelves";

pub(crate) struct Config {
    pub(crate) power_shelves: Vec<redfish::schema::power_distribution::PowerDistribution>,
}

pub(crate) fn add_routes<C: Callbacks>(router: Router<BmcState<C>>) -> Router<BmcState<C>> {
    router
        .route(RESOURCE, get(get_equipment::<C>))
        .route(SHELVES, get(get_shelves::<C>))
        .route(
            "/redfish/v1/PowerEquipment/PowerShelves/{shelf_id}",
            get(get_shelf::<C>),
        )
}

async fn get_equipment<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    if state.power_equipment.is_none() {
        return http::not_found();
    }
    redfish::schema::power_equipment::PowerEquipment::new(
        RESOURCE,
        "PowerEquipment",
        "Power Equipment",
    )
    .power_shelves(SHELVES)
    .into_json()
    .into_ok_response()
}

async fn get_shelves<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    let Some(config) = state.power_equipment.as_ref() else {
        return http::not_found();
    };
    let members = config
        .power_shelves
        .iter()
        .map(|shelf| {
            redfish::Resource {
                odata_id: shelf.odata_id.clone().into(),
                odata_type: shelf.odata_type.into(),
                id: shelf.id.clone().into(),
                name: shelf.name.clone().into(),
            }
            .entity_ref()
        })
        .collect::<Vec<_>>();
    redfish::Collection {
        odata_id: SHELVES.into(),
        odata_type: "#PowerDistributionCollection.PowerDistributionCollection".into(),
        name: "PowerShelf Collection".into(),
    }
    .with_members(&members)
    .into_ok_response()
}

async fn get_shelf<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(id): Path<String>,
) -> Response {
    state
        .power_equipment
        .as_ref()
        .and_then(|config| config.power_shelves.iter().find(|shelf| shelf.id == id))
        .map(|shelf| {
            serde_json::to_value(shelf)
                .expect("generated PowerDistribution serializes")
                .into_ok_response()
        })
        .unwrap_or_else(http::not_found)
}
