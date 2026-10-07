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

use super::Base;

#[derive(Template)]
#[template(path = "expected_switch.html")]
struct ExpectedSwitches {
    switches: Vec<ExpectedSwitchRow>,
}

#[derive(Debug, serde::Serialize)]
struct ExpectedSwitchRow {
    serial_number: String,
    bmc_mac_address: String,
    rack_id: String,
    explored_bmc_ip: String,
    switch_id: String,
}

/// Show all expected switches.
pub(super) async fn show_html(state: AxumState<Arc<Api>>) -> Response {
    let switches = match fetch_expected_switches(&state).await {
        Ok(switches) => switches,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let display = ExpectedSwitches { switches };
    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

/// Show all expected switches as JSON.
pub(super) async fn show_json(state: AxumState<Arc<Api>>) -> Response {
    let switches = match fetch_expected_switches(&state).await {
        Ok(switches) => switches,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    (StatusCode::OK, Json(switches)).into_response()
}

async fn fetch_expected_switches(
    api: &Api,
) -> Result<Vec<ExpectedSwitchRow>, (http::StatusCode, String)> {
    let response = match api
        .get_all_expected_switches_linked(tonic::Request::new(()))
        .await
    {
        Ok(response) => response.into_inner(),
        Err(err) => {
            tracing::error!(error = %err, "get_all_expected_switches_linked");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to list expected switches".to_string(),
            ));
        }
    };

    let switches = response
        .expected_switches
        .into_iter()
        .map(|es| ExpectedSwitchRow {
            serial_number: es.switch_serial_number,
            bmc_mac_address: es.bmc_mac_address,
            rack_id: es.rack_id.map(|id| id.to_string()).unwrap_or_default(),
            explored_bmc_ip: es.explored_endpoint_address.unwrap_or_default(),
            switch_id: es
                .switch_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "Unlinked".to_string()),
        })
        .collect();

    Ok(switches)
}

impl super::Base for ExpectedSwitches {}
