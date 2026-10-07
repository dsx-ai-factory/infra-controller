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
#[template(path = "expected_rack.html")]
struct ExpectedRacks {
    racks: Vec<ExpectedRackRow>,
}

#[derive(Debug, serde::Serialize)]
struct ExpectedRackRow {
    rack_id: String,
    rack_profile_id: String,
    compute_trays: String,
    switches: String,
    power_shelves: String,
}

/// Show all expected racks.
pub(super) async fn show_html(state: AxumState<Arc<Api>>) -> Response {
    let racks = match fetch_expected_racks(&state).await {
        Ok(racks) => racks,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let display = ExpectedRacks { racks };
    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

/// Show all expected racks as JSON.
pub(super) async fn show_json(state: AxumState<Arc<Api>>) -> Response {
    let racks = match fetch_expected_racks(&state).await {
        Ok(racks) => racks,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    (StatusCode::OK, Json(racks)).into_response()
}

async fn fetch_expected_racks(
    api: &Api,
) -> Result<Vec<ExpectedRackRow>, (http::StatusCode, String)> {
    let expected_response = match api.get_all_expected_racks(tonic::Request::new(())).await {
        Ok(response) => response.into_inner(),
        Err(err) => {
            tracing::error!(error = %err, "get_all_expected_racks");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to list expected racks".to_string(),
            ));
        }
    };

    let rows = expected_response
        .expected_racks
        .into_iter()
        .map(|er| {
            let rack_id = er
                .rack_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default();
            let rack_profile_id = er
                .rack_profile_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default();

            // Look up capabilities from the rack profile config.
            let profile = api.runtime_config.rack_profiles.get(&rack_profile_id);

            // Expected rack profile capabilities for compute trays, switches, and power shelves
            let (compute_trays, switches, power_shelves) = match profile {
                Some(rack_profile) => (
                    rack_profile.rack_capabilities.compute.count,
                    rack_profile.rack_capabilities.switch.count,
                    rack_profile.rack_capabilities.power_shelf.count,
                ),
                None => (0, 0, 0),
            };

            ExpectedRackRow {
                rack_id,
                rack_profile_id,
                compute_trays: compute_trays.to_string(),
                switches: switches.to_string(),
                power_shelves: power_shelves.to_string(),
            }
        })
        .collect();

    Ok(rows)
}

impl super::Base for ExpectedRacks {}
