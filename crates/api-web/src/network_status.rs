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
// Flat `rpc::forge::Machine` fields are deprecated in favour of `status`/`config`
// sub-messages, but this module must still read them until the REST API is migrated.
// See https://github.com/NVIDIA/infra-controller/issues/2793
#![allow(deprecated)]

use std::cmp::min;
use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_uuid::machine::MachineId;
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::{Base, filters};

const DEFAULT_PAGE_RECORD_LIMIT: usize = 50;

#[derive(Template)]
#[template(path = "network_status.html")]
struct NetworkStatus {
    dpus: Vec<NetworkStatusDisplay>,
    active_filter: String,
    all_count: usize,
    healthy_count: usize,
    unhealthy_count: usize,
    outdated_count: usize,
    current_page: usize,
    previous: usize,
    next: usize,
    pages: usize,
    page_range_start: usize,
    page_range_end: usize,
    limit: usize,
}

#[derive(Clone, serde::Serialize)]
struct NetworkStatusDisplay {
    observed_at: String,
    dpu_machine_id: String,
    network_config_version: String,
    is_healthy: bool,
    health: health_report::HealthReport,
    agent_version: String,
    is_agent_updated: bool,
}

pub(super) async fn show_html(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let filter = params.get("filter").cloned().unwrap_or("all".to_string());

    let current_page = params
        .get("current_page")
        .map_or(0, |s| s.parse::<usize>().unwrap_or(0));

    let limit: usize = params.get("limit").map_or(DEFAULT_PAGE_RECORD_LIMIT, |s| {
        s.parse::<usize>().map_or(DEFAULT_PAGE_RECORD_LIMIT, |s| {
            min(s, DEFAULT_PAGE_RECORD_LIMIT)
        })
    });

    let (pages, all_status) = match fetch_network_status(state, current_page, limit).await {
        Ok(all) => all,
        Err(err) => {
            tracing::error!(error = %err, "fetch_network_status");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading network status",
            )
                .into_response();
        }
    };
    let all_count = all_status.len();
    let mut dpus = Vec::with_capacity(all_status.len());
    let (mut healthy_count, mut unhealthy_count, mut outdated_count) = (0, 0, 0);
    for st in all_status.into_iter() {
        let display: NetworkStatusDisplay = st;
        if display.is_healthy {
            healthy_count += 1;
        } else {
            unhealthy_count += 1;
        }
        if !display.is_agent_updated {
            outdated_count += 1;
        }
        match filter.as_str() {
            "all" => dpus.push(display),
            "healthy" => {
                if display.is_healthy {
                    dpus.push(display);
                }
            }
            "unhealthy" => {
                if !display.is_healthy {
                    dpus.push(display);
                }
            }
            "outdated" => {
                if !display.is_agent_updated {
                    dpus.push(display);
                }
            }
            _ => {
                return (StatusCode::BAD_REQUEST, "Unknown filter").into_response();
            }
        }
    }

    let tmpl = NetworkStatus {
        dpus,
        active_filter: filter,
        all_count,
        healthy_count,
        unhealthy_count,
        outdated_count,
        current_page,
        previous: current_page.saturating_sub(1),
        next: current_page.saturating_add(1),
        pages,
        page_range_start: current_page.saturating_sub(3),
        page_range_end: min(current_page.saturating_add(4), pages),
        limit,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let current_page = params
        .get("current_page")
        .map_or(0, |s| s.parse::<usize>().unwrap_or(0));

    let limit: usize = params.get("limit").map_or(DEFAULT_PAGE_RECORD_LIMIT, |s| {
        s.parse::<usize>().map_or(DEFAULT_PAGE_RECORD_LIMIT, |s| {
            min(s, DEFAULT_PAGE_RECORD_LIMIT)
        })
    });

    let (_, all_status) = match fetch_network_status(state, current_page, limit).await {
        Ok(all) => all,
        Err(err) => {
            tracing::error!(error = %err, "fetch_network_status");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading network status",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(all_status)).into_response()
}

async fn fetch_network_status(
    api: Arc<Api>,
    current_page: usize,
    limit: usize,
) -> Result<(usize, Vec<NetworkStatusDisplay>), tonic::Status> {
    let request: tonic::Request<forgerpc::ManagedHostNetworkStatusRequest> =
        tonic::Request::new(forgerpc::ManagedHostNetworkStatusRequest {});
    // The only reason we require the get_all_managed_host_network_status
    // API here is for retrieving the actually applied network_config_version
    // and the time of last contact (observed_at).
    // Everything else is available via the Machine API.

    let all_status = api
        .get_all_managed_host_network_status(request)
        .await
        .map(|response| response.into_inner())?
        .all;

    let all_ids = all_status
        .iter()
        .filter_map(|status| status.dpu_machine_id)
        .collect::<Vec<_>>();

    // Handling the case of getting a nonsensical limit.
    let limit = if limit == 0 {
        DEFAULT_PAGE_RECORD_LIMIT
    } else {
        limit
    };

    let pages = all_ids.len().div_ceil(limit);

    let current_record_cnt_seen = current_page.saturating_mul(limit);

    // Just handles the other case of someone messing around with the
    // query params and suddenly setting a limit that makes
    // current_record_cnt_seen no longer make sense.
    if current_record_cnt_seen > all_ids.len() {
        return Ok((pages, vec![]));
    }

    let ids_for_page = all_ids
        .into_iter()
        .skip(current_record_cnt_seen)
        .take(limit)
        .map(Into::into)
        .collect();

    let all_dpus = api
        .find_machines_by_ids(tonic::Request::new(forgerpc::MachinesByIdsRequest {
            machine_ids: ids_for_page,
            include_history: false,
        }))
        .await
        .map(|response| response.into_inner())?
        .machines;
    let mut dpus_by_id = HashMap::new();
    for dpu in all_dpus.into_iter() {
        if let Some(id) = dpu.id {
            dpus_by_id.insert(id, dpu);
        }
    }

    let mut result = Vec::new();

    for status in all_status.into_iter() {
        let Some(dpu_id) = status.dpu_machine_id else {
            continue;
        };
        let Some(dpu) = dpus_by_id.get(&MachineId::from(dpu_id)) else {
            continue;
        };

        let agent_version = dpu
            .inventory
            .as_ref()
            .and_then(|inventory| {
                inventory
                    .components
                    .iter()
                    .find(|c| c.name == "carbide-dpu-agent" || c.name == "forge-dpu-agent")
                    .map(|c| c.version.clone())
            })
            .unwrap_or_default();
        let health = dpu
            .health
            .as_ref()
            .map(|h| {
                health_report::HealthReport::try_from(h.clone())
                    .unwrap_or_else(health_report::HealthReport::malformed_report)
            })
            .unwrap_or_else(health_report::HealthReport::missing_report);

        result.push(NetworkStatusDisplay {
            observed_at: status
                .observed_at
                .map(|o| {
                    let dt: chrono::DateTime<chrono::Utc> = o.try_into().unwrap_or_default();
                    dt.format("%Y-%m-%d %H:%M:%S.%3f").to_string()
                })
                .unwrap_or_default(),
            dpu_machine_id: dpu_id.to_string(),
            network_config_version: status.network_config_version.unwrap_or_default(),
            is_healthy: health.alerts.is_empty(),
            health,
            is_agent_updated: agent_version == carbide_version::v!(build_version),
            agent_version,
        });
    }

    Ok((pages, result))
}

impl super::Base for NetworkStatus {}
