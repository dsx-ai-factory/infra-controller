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
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::{Base, filters};

#[derive(Template)]
#[template(path = "ib_partition_show.html")]
struct IbPartitionShow {
    partitions: Vec<IbPartitionRowDisplay>,
}

struct IbPartitionRowDisplay {
    id: String,
    tenant_organization_id: String,
    metadata: rpc::forge::Metadata,
    state: String,
    time_in_state_above_sla: bool,
    pkey: String,
}

impl From<forgerpc::IbPartition> for IbPartitionRowDisplay {
    fn from(partition: forgerpc::IbPartition) -> Self {
        Self {
            id: partition.id.map(|id| id.to_string()).unwrap_or_default(),
            tenant_organization_id: partition.config.unwrap_or_default().tenant_organization_id,
            metadata: partition.metadata.unwrap_or_default(),
            state: partition
                .status
                .as_ref()
                .and_then(|status| forgerpc::TenantState::try_from(status.state).ok())
                .map(|state| format!("{state:?}"))
                .unwrap_or_default(),
            time_in_state_above_sla: partition
                .status
                .as_ref()
                .and_then(|status| status.state_sla.as_ref())
                .as_ref()
                .map(|sla| sla.time_in_state_above_sla)
                .unwrap_or_default(),
            pkey: partition
                .status
                .as_ref()
                .and_then(|status| status.pkey.clone())
                .unwrap_or_default(),
        }
    }
}

/// List partitions
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let partitions = match fetch_ib_partitions(state.clone()).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_ib_partitions");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading IB partitions",
            )
                .into_response();
        }
    };

    let tmpl = IbPartitionShow {
        partitions: partitions.into_iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let partitions = match fetch_ib_partitions(state).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_ib_partitions");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading IB partitions",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(partitions)).into_response()
}

async fn fetch_ib_partitions(api: Arc<Api>) -> Result<Vec<forgerpc::IbPartition>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::IbPartitionSearchFilter::default());

    let ib_partition_ids = api
        .find_ib_partition_ids(request)
        .await?
        .into_inner()
        .ib_partition_ids;

    let mut partitions = Vec::new();
    let mut offset = 0;
    while offset != ib_partition_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(ib_partition_ids.len() - offset);
        let next_ids = &ib_partition_ids[offset..offset + page_size];
        let request = tonic::Request::new(forgerpc::IbPartitionsByIdsRequest {
            ib_partition_ids: next_ids.to_vec(),
            include_history: false,
        });
        let next_partitions = api
            .find_ib_partitions_by_ids(request)
            .await
            .map(|response| response.into_inner())?;

        partitions.extend(next_partitions.ib_partitions);
        offset += page_size;
    }

    partitions.sort_unstable_by(|p1, p2: &rpc::IbPartition| {
        // Sort by tenant_org and name
        // Otherwise fall back to ID
        if let (Some(pc1), Some(pc2)) = (p1.config.as_ref(), p2.config.as_ref()) {
            let ord = pc1.tenant_organization_id.cmp(&pc2.tenant_organization_id);
            if ord.is_ne() {
                return ord;
            }
            let ord = p1
                .metadata
                .as_ref()
                .map(|m| &m.name)
                .cmp(&p2.metadata.as_ref().map(|m| &m.name));
            if ord.is_ne() {
                return ord;
            }
        }
        if let (Some(id1), Some(id2)) = (p1.id.as_ref(), p2.id.as_ref()) {
            return id1.cmp(id2);
        }
        // This path should never be taken, since ID is always set
        (p1 as *const rpc::IbPartition).cmp(&(p2 as *const rpc::IbPartition))
    });
    Ok(partitions)
}

#[derive(Template)]
#[template(path = "ib_partition_detail.html")]
struct IbPartitionDetail {
    id: String,
    config_version: String,
    tenant_organization_id: String,
    metadata: rpc::forge::Metadata,
    state_display: super::StateDisplay,
    state_sla_detail: super::StateSlaDetail,
    pkey: String,
    service_level: String,
    rate_limit: String,
    mtu: String,
    enable_sharp: String,
}

impl From<forgerpc::IbPartition> for IbPartitionDetail {
    fn from(partition: forgerpc::IbPartition) -> Self {
        Self {
            id: partition.id.map(|id| id.to_string()).unwrap_or_default(),
            config_version: partition.config_version,
            tenant_organization_id: partition.config.unwrap_or_default().tenant_organization_id,
            metadata: partition.metadata.unwrap_or_default(),
            state_display: super::StateDisplay {
                state: partition
                    .status
                    .as_ref()
                    .and_then(|status| forgerpc::TenantState::try_from(status.state).ok())
                    .map(|state| format!("{state:?}"))
                    .unwrap_or_default(),
                time_in_state_above_sla: partition
                    .status
                    .as_ref()
                    .and_then(|status| status.state_sla.as_ref())
                    .map(|sla| sla.time_in_state_above_sla)
                    .unwrap_or_default(),
            },
            state_sla_detail: super::StateSlaDetail {
                state_sla: partition
                    .status
                    .as_ref()
                    .and_then(|status| status.state_sla.as_ref())
                    .and_then(|sla| sla.sla)
                    .map(|sla| {
                        config_version::format_duration(
                            chrono::TimeDelta::try_from(sla).unwrap_or(chrono::TimeDelta::MAX),
                        )
                    })
                    .unwrap_or_default(),
                time_in_state_above_sla: partition
                    .status
                    .as_ref()
                    .and_then(|status| status.state_sla.as_ref())
                    .map(|sla| sla.time_in_state_above_sla)
                    .unwrap_or_default(),
                state_reason: partition
                    .status
                    .as_ref()
                    .and_then(|s| s.state_reason.clone()),
            },
            pkey: partition
                .status
                .as_ref()
                .and_then(|status| status.pkey.clone())
                .unwrap_or_default(),
            service_level: partition
                .status
                .as_ref()
                .and_then(|status| status.service_level)
                .map(|service_level| service_level.to_string())
                .unwrap_or_default(),
            rate_limit: partition
                .status
                .as_ref()
                .and_then(|status| status.rate_limit)
                .map(|rate_limit| rate_limit.to_string())
                .unwrap_or_default(),
            mtu: partition
                .status
                .as_ref()
                .and_then(|status| status.mtu)
                .map(|mtu| mtu.to_string())
                .unwrap_or_default(),
            enable_sharp: partition
                .status
                .as_ref()
                .and_then(|status| status.enable_sharp)
                .map(|enable_sharp| enable_sharp.to_string())
                .unwrap_or_default(),
        }
    }
}

/// View partition details
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(partition_id): AxumPath<String>,
) -> Response {
    let (show_json, partition_id_string) = match partition_id.strip_suffix(".json") {
        Some(partition_id) => (true, partition_id.to_string()),
        None => (false, partition_id),
    };

    let partition_id = match partition_id_string.parse() {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid IBPartitionId {partition_id_string}: {e}"),
            )
                .into_response();
        }
    };

    let request = tonic::Request::new(forgerpc::IbPartitionsByIdsRequest {
        ib_partition_ids: vec![partition_id],
        include_history: true,
    });
    let partition = match state
        .find_ib_partitions_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(p) if p.ib_partitions.is_empty() => {
            return super::not_found_response(partition_id_string);
        }
        Ok(p) if p.ib_partitions.len() != 1 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Partition list for {partition_id} returned {} partitions",
                    p.ib_partitions.len()
                ),
            )
                .into_response();
        }
        Ok(mut p) => p.ib_partitions.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return super::not_found_response(partition_id_string);
        }
        Err(err) => {
            tracing::error!(error = %err, "find_ib_partitions");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading IB partitions",
            )
                .into_response();
        }
    };

    if show_json {
        return (StatusCode::OK, Json(partition)).into_response();
    }

    let tmpl: IbPartitionDetail = partition.into();
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

impl super::Base for IbPartitionShow {}
impl super::Base for IbPartitionDetail {}
