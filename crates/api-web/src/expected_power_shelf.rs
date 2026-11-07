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

use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::State as AxumState;
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use hyper::http::StatusCode;
use rpc::forge::forge_server::Forge;

use super::{Base, filters};

#[derive(Template)]
#[template(path = "expected_power_shelf.html")]
struct ExpectedPowerShelves {
    power_shelves: Vec<ExpectedPowerShelfRow>,
}

#[derive(Debug, serde::Serialize)]
struct ExpectedPowerShelfRow {
    serial_number: String,
    bmc_mac_address: String,
    rack_id: String,
    explored_bmc_ip: String,
    power_shelf_id: String,
}

/// Show all expected power shelves.
pub(super) async fn show_html(state: AxumState<Arc<Api>>) -> Response {
    let power_shelves = match fetch_expected_power_shelves(&state).await {
        Ok(shelves) => shelves,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let display = ExpectedPowerShelves { power_shelves };
    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

/// Show all expected power shelves as JSON.
pub(super) async fn show_json(state: AxumState<Arc<Api>>) -> Response {
    let power_shelves = match fetch_expected_power_shelves(&state).await {
        Ok(shelves) => shelves,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    (StatusCode::OK, Json(power_shelves)).into_response()
}

async fn fetch_expected_power_shelves(
    api: &Api,
) -> Result<Vec<ExpectedPowerShelfRow>, (http::StatusCode, String)> {
    let response = match api
        .get_all_expected_power_shelves_linked(tonic::Request::new(()))
        .await
    {
        Ok(response) => response.into_inner(),
        Err(err) => {
            tracing::error!(error = %err, "get_all_expected_power_shelves_linked");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to list expected power shelves".to_string(),
            ));
        }
    };

    let power_shelves = response
        .expected_power_shelves
        .into_iter()
        .map(|eps| ExpectedPowerShelfRow {
            serial_number: eps.shelf_serial_number,
            bmc_mac_address: eps.bmc_mac_address,
            rack_id: eps.rack_id.map(|id| id.to_string()).unwrap_or_default(),
            explored_bmc_ip: eps.explored_endpoint_address.unwrap_or_default(),
            power_shelf_id: eps
                .power_shelf_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "Unlinked".to_string()),
        })
        .collect();

    Ok(power_shelves)
}

impl super::Base for ExpectedPowerShelves {}
