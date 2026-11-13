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

use std::str::FromStr;
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_uuid::machine::MachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use hyper::http::StatusCode;

use super::Base;
use super::health::{
    HealthHistoryRecord, HealthHistoryTable, fetch_machine_health_history,
    fetch_power_shelf_health_history, fetch_rack_health_history, fetch_switch_health_history,
};

#[derive(Template)]
#[template(path = "health_history.html")]
struct HealthHistory {
    id: String,
    /// The type of object that the history is for in human readable form.
    /// E.g. Host, Switch, Power Shelf, Rack.
    object_type: String,
    /// The base URL segment used for this type of object. E.g. `machine`,
    /// `switch`, `power-shelf`, `rack`.
    object_url_path: String,
    history: HealthHistoryTable,
}

impl super::Base for HealthHistory {}

/// Parses `id_str` into the typed id, then loads its health-history records via
/// `fetch`. On failure it returns the HTTP response to send back (a `400` for an
/// unparseable id, a `500` for a transport error), so callers can `?`-style
/// short-circuit with `Err(response) => response`.
async fn load_records<Id>(
    api: &Api,
    id_str: &str,
    fetch: impl AsyncFn(&Api, &Id) -> Result<Vec<HealthHistoryRecord>, tonic::Status>,
) -> Result<(Id, Vec<HealthHistoryRecord>), Response>
where
    Id: FromStr + std::fmt::Display,
{
    let Ok(id) = Id::from_str(id_str) else {
        return Err((StatusCode::BAD_REQUEST, "invalid id").into_response());
    };

    match fetch(api, &id).await {
        Ok(records) => Ok((id, records)),
        Err(err) => {
            tracing::error!(error = %err, id = %id, "find health histories");
            Err((StatusCode::INTERNAL_SERVER_ERROR, String::new()).into_response())
        }
    }
}

/// Renders the health-history page for one object.
fn render_page(
    id: String,
    object_type: &str,
    object_url_path: &str,
    records: Vec<HealthHistoryRecord>,
) -> Response {
    let display = HealthHistory {
        id,
        object_type: object_type.to_string(),
        object_url_path: object_url_path.to_string(),
        history: HealthHistoryTable { records },
    };
    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

pub(super) async fn show_machine_health_history(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_records::<MachineId>(&state, &id, fetch_machine_health_history).await {
        Ok((id, records)) => render_page(id.to_string(), "Host", "machine", records),
        Err(response) => response,
    }
}

pub(super) async fn show_machine_health_history_json(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_records::<MachineId>(&state, &id, fetch_machine_health_history).await {
        Ok((_id, records)) => (StatusCode::OK, Json(records)).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn show_switch_health_history(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_records::<SwitchId>(&state, &id, fetch_switch_health_history).await {
        Ok((id, records)) => render_page(id.to_string(), "Switch", "switch", records),
        Err(response) => response,
    }
}

pub(super) async fn show_switch_health_history_json(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_records::<SwitchId>(&state, &id, fetch_switch_health_history).await {
        Ok((_id, records)) => (StatusCode::OK, Json(records)).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn show_rack_health_history(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_records::<RackId>(&state, &id, fetch_rack_health_history).await {
        Ok((id, records)) => render_page(id.to_string(), "Rack", "rack", records),
        Err(response) => response,
    }
}

pub(super) async fn show_rack_health_history_json(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_records::<RackId>(&state, &id, fetch_rack_health_history).await {
        Ok((_id, records)) => (StatusCode::OK, Json(records)).into_response(),
        Err(response) => response,
    }
}

pub(super) async fn show_power_shelf_health_history(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_records::<PowerShelfId>(&state, &id, fetch_power_shelf_health_history).await {
        Ok((id, records)) => render_page(id.to_string(), "Power Shelf", "power-shelf", records),
        Err(response) => response,
    }
}

pub(super) async fn show_power_shelf_health_history_json(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match load_records::<PowerShelfId>(&state, &id, fetch_power_shelf_health_history).await {
        Ok((_id, records)) => (StatusCode::OK, Json(records)).into_response(),
        Err(response) => response,
    }
}
