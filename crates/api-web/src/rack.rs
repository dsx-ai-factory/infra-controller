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

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Path as AxumPath, Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_uuid::rack::RackId;
use hyper::http::StatusCode;
use rpc::forge::forge_server::Forge;

use super::state_history::StateHistoryTable;
use super::{Base, filters};

#[derive(Template)]
#[template(path = "rack_show.html")]
struct Racks {
    racks: Vec<RackRecord>,
}

struct RackRecord {
    id: String,
    state_display: super::StateDisplay,
}

struct RackSwitchRecord {
    id: String,
    is_primary: bool,
}

impl From<rpc::forge::Rack> for RackRecord {
    fn from(rack: rpc::forge::Rack) -> Self {
        let lifecycle = rack
            .status
            .as_ref()
            .and_then(|status| status.lifecycle.as_ref());

        Self {
            id: rack.id.map(|id| id.to_string()).unwrap_or_default(),
            state_display: super::StateDisplay::from_lifecycle(lifecycle),
        }
    }
}

#[derive(Template)]
#[template(path = "rack_detail.html")]
struct RackDetail {
    id: String,
    lifecycle_detail: super::LifecycleDetail,
    version: String,
    health_detail: super::HealthDetail,
    associated_machines: Vec<String>,
    associated_switches: Vec<RackSwitchRecord>,
    associated_power_shelves: Vec<String>,
    metadata_detail: super::MetadataDetail,
    history: StateHistoryTable,
}

/// Show all racks
pub(super) async fn show_html(state: AxumState<Arc<Api>>) -> Response {
    let racks = match fetch_racks(&state).await {
        Ok(racks) => racks,
        Err(err) => {
            tracing::error!(error = %err, "fetch_racks");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading racks").into_response();
        }
    };

    let display = Racks {
        racks: racks.racks.into_iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

/// Show all racks as JSON
pub(super) async fn show_json(state: AxumState<Arc<Api>>) -> Response {
    let racks = match fetch_racks(&state).await {
        Ok(racks) => racks,
        Err(err) => {
            tracing::error!(error = %err, "fetch_racks");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading racks").into_response();
        }
    };
    (StatusCode::OK, Json(racks)).into_response()
}

async fn fetch_racks(api: &Api) -> Result<rpc::forge::RackList, tonic::Status> {
    let request = tonic::Request::new(rpc::forge::RackSearchFilter::default());

    let rack_ids = api.find_rack_ids(request).await?.into_inner().rack_ids;

    let mut racks = Vec::new();
    let mut offset = 0;
    while offset != rack_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(rack_ids.len() - offset);
        let next_ids = &rack_ids[offset..offset + page_size];
        let next_racks = api
            .find_racks_by_ids(tonic::Request::new(rpc::forge::RacksByIdsRequest {
                rack_ids: next_ids.to_vec(),
            }))
            .await?
            .into_inner();

        racks.extend(next_racks.racks);
        offset += page_size;
    }

    Ok(rpc::forge::RackList { racks })
}

async fn fetch_rack(api: &Api, rack_id: &RackId) -> Result<Option<::rpc::forge::Rack>, Response> {
    let request = tonic::Request::new(rpc::forge::RacksByIdsRequest {
        rack_ids: vec![rack_id.clone()],
    });

    let rack = match api
        .find_racks_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(r) if r.racks.is_empty() => {
            return Ok(None);
        }
        Ok(r) if r.racks.len() != 1 => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Rack list for {rack_id} returned {} racks", r.racks.len()),
            )
                .into_response());
        }
        Ok(mut r) => Some(r.racks.remove(0)),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return Ok(None);
        }
        Err(err) => {
            tracing::error!(error = %err, %rack_id, "find_racks_by_ids");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(rack)
}

/// View details about a Rack
pub(super) async fn detail(
    AxumState(api): AxumState<Arc<Api>>,
    AxumPath(rack_id): AxumPath<String>,
    Query(_params): Query<HashMap<String, String>>,
) -> Response {
    let (show_json, rack_id) = match rack_id.strip_suffix(".json") {
        Some(rack_id) => (true, rack_id.to_string()),
        None => (false, rack_id),
    };

    let rack_id = match rack_id.parse::<RackId>() {
        Ok(rack_id) => rack_id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let rack = match fetch_rack(&api, &rack_id).await {
        Ok(Some(rack)) => rack,
        Ok(None) => {
            return super::not_found_response(rack_id.to_string());
        }
        Err(response) => return response,
    };

    if show_json {
        return (StatusCode::OK, Json(rack)).into_response();
    };

    let associated_machines = match fetch_machine_ids(api.clone(), rack_id.clone()).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_machine_ids");
            vec![]
        }
    };

    let associated_switches = match fetch_switches(&api, &rack_id).await {
        Ok(switches) => switches,
        Err(err) => {
            tracing::error!(error = %err, "fetch_switches");
            vec![]
        }
    };

    let associated_power_shelves = match fetch_power_shelf_ids(&api, &rack_id).await {
        Ok(ids) => ids,
        Err(err) => {
            tracing::error!(error = %err, "fetch_power_shelf_ids");
            vec![]
        }
    };

    let version = rack.version.clone();

    let lifecycle = rack
        .status
        .as_ref()
        .and_then(|s| s.lifecycle.clone())
        .unwrap_or_default();

    let metadata_detail = super::MetadataDetail {
        metadata: rack.metadata.clone().unwrap_or_default(),
        metadata_version: version.clone(),
    };

    let rack_status = rack.status.as_ref();
    let health_url = format!("/admin/rack/{rack_id}/health");
    let health_detail = super::HealthDetail::new(
        health_url,
        "Go to Rack health reports",
        rack_status.and_then(|status| status.health.clone()),
        rack_status
            .map(|status| status.health_sources.clone())
            .unwrap_or_default(),
    );

    let history = match super::state_history::fetch_rack_state_history_records(
        &api,
        rack_id.as_ref(),
    )
    .await
    {
        Ok((_rack_id, records)) => StateHistoryTable {
            records: records.into_iter().map(Into::into).collect(),
        },
        Err((code, err)) => {
            tracing::error!(http_status = %code, error = %err, %rack_id, "fetch_rack_state_history_records");
            StateHistoryTable { records: vec![] }
        }
    };

    let display = RackDetail {
        id: rack_id.to_string(),
        lifecycle_detail: lifecycle.into(),
        version,
        health_detail,
        associated_machines,
        associated_switches,
        associated_power_shelves,
        metadata_detail,
        history,
    };

    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

async fn fetch_machine_ids(api: Arc<Api>, rack_id: RackId) -> Result<Vec<String>, tonic::Status> {
    let request = tonic::Request::new(rpc::forge::MachineSearchConfig {
        include_predicted_host: true,
        rack_id: Some(rack_id.clone()),
        ..Default::default()
    });

    Ok(api
        .find_machine_ids(request)
        .await?
        .into_inner()
        .machine_ids
        .into_iter()
        .map(|id| id.to_string())
        .collect())
}

async fn fetch_switches(
    api: &Api,
    rack_id: &RackId,
) -> Result<Vec<RackSwitchRecord>, tonic::Status> {
    let request = tonic::Request::new(rpc::forge::SwitchSearchFilter {
        rack_id: Some(rack_id.clone()),
        ..Default::default()
    });

    let switch_ids = api.find_switch_ids(request).await?.into_inner().ids;

    if switch_ids.is_empty() {
        return Ok(vec![]);
    }

    // find_switches_by_ids rejects a request with more IDs than the server's
    // configured max_find_by_ids, so page the lookup to that limit. A zero/unset
    // cap means the server enforces no limit.
    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    let chunk_size = if max_find_by_ids == 0 {
        switch_ids.len()
    } else {
        max_find_by_ids
    };

    let mut switches = Vec::with_capacity(switch_ids.len());
    for chunk in switch_ids.chunks(chunk_size) {
        let page = api
            .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
                switch_ids: chunk.to_vec(),
            }))
            .await?
            .into_inner();
        switches.extend(page.switches);
    }

    Ok(switches
        .into_iter()
        .map(|switch| RackSwitchRecord {
            id: switch.id.map(|id| id.to_string()).unwrap_or_default(),
            is_primary: switch.is_primary,
        })
        .collect())
}

async fn fetch_power_shelf_ids(api: &Api, rack_id: &RackId) -> Result<Vec<String>, tonic::Status> {
    let request = tonic::Request::new(rpc::forge::PowerShelfSearchFilter {
        rack_id: Some(rack_id.clone()),
        ..Default::default()
    });

    Ok(api
        .find_power_shelf_ids(request)
        .await?
        .into_inner()
        .ids
        .into_iter()
        .map(|id| id.to_string())
        .collect())
}

impl super::Base for Racks {}
impl super::Base for RackDetail {}
