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
use axum::extract::{OriginalUri, Path as AxumPath, Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_uuid::nvlink::{NvLinkDomainId, NvLinkPartitionId};
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;
use uuid::Uuid;

use super::pagination::{self, PageContext, PaginationParams};
use super::{Base, health};

#[derive(serde::Serialize, Template)]
#[template(path = "nvlink_partition_show.html")]
struct LogicalPartitionShow {
    partitions: Vec<LogicalPartitionRowDisplay>,
}

#[derive(serde::Serialize)]
struct LogicalPartitionRowDisplay {
    id: String,
    name: String,
    state: String,
    physical_partitions: usize,
}

#[derive(Template)]
#[template(path = "nvlink_domain_health_show.html")]
struct NvLinkDomainHealthShow {
    domains: Vec<NvLinkDomainHealthRow>,
    json_path: String,
    search_query: String,
    page: PageContext,
}

#[derive(serde::Serialize)]
struct NvLinkDomainHealthRow {
    id: String,
    health_url: String,
}

#[derive(serde::Deserialize, Debug, Default)]
pub(super) struct NvLinkDomainHealthParams {
    #[serde(flatten)]
    pagination: PaginationParams,
    #[serde(default)]
    q: String,
}

#[derive(serde::Serialize, Clone)]
struct ShowLogicalPartition {
    partition: forgerpc::NvLinkLogicalPartition,
    physical_partitions: Vec<ShowPhysicalPartition>,
}
#[derive(serde::Serialize, Clone)]
struct ShowPhysicalPartition {
    partition: forgerpc::NvLinkPartition,
    members: Vec<ShowPartitionMember>,
}
#[derive(serde::Serialize, Clone)]
struct ShowPartitionMember {
    machine_id: String,
    gpu_id: String,
}

impl From<ShowLogicalPartition> for LogicalPartitionRowDisplay {
    fn from(show: ShowLogicalPartition) -> Self {
        Self {
            id: show.partition.id.map(|i| i.to_string()).unwrap_or_default(),
            name: show
                .partition
                .config
                .unwrap_or_default()
                .metadata
                .unwrap_or_default()
                .name,
            state: forgerpc::TenantState::try_from(show.partition.status.unwrap_or_default().state)
                .unwrap_or_default()
                .as_str_name()
                .to_string(),
            physical_partitions: show.physical_partitions.len(),
        }
    }
}

impl From<NvLinkDomainId> for NvLinkDomainHealthRow {
    fn from(id: NvLinkDomainId) -> Self {
        Self {
            id: id.to_string(),
            health_url: health::nvlink_domain_health_url(&id),
        }
    }
}

#[derive(serde::Serialize, Clone)]
struct ShowPhysicalPartitionDetail {
    id: String,
    domain_uuid: String,
    domain_health_url: String,
    name: String,
    nmx_c_partition_id: String,
    members: Vec<ShowPartitionMember>,
}

#[derive(Template)]
#[template(path = "logical_partition_detail.html")]
struct LogicalPartitionDetail {
    id: String,
    name: String,
    state: String,
    created: String,
    physical_partitions: Vec<ShowPhysicalPartitionDetail>,
}

impl From<ShowLogicalPartition> for LogicalPartitionDetail {
    fn from(show: ShowLogicalPartition) -> Self {
        let mut physical_partitions = Vec::new();
        for s in show.physical_partitions {
            let domain_uuid = s.partition.domain_uuid;
            let domain_health_url = domain_uuid
                .as_ref()
                .map(health::nvlink_domain_health_url)
                .unwrap_or_default();

            let pp = ShowPhysicalPartitionDetail {
                id: s.partition.id.map(|i| i.to_string()).unwrap_or_default(),
                domain_uuid: domain_uuid.map(|id| id.to_string()).unwrap_or_default(),
                domain_health_url,
                name: s.partition.name,
                nmx_c_partition_id: s.partition.nmx_m_id,
                members: s.members,
            };

            physical_partitions.push(pp);
        }

        let created = show
            .partition
            .created
            .map(|c| c.to_string())
            .unwrap_or_default();
        Self {
            id: show.partition.id.map(|i| i.to_string()).unwrap_or_default(),
            name: show
                .partition
                .config
                .unwrap_or_default()
                .metadata
                .unwrap_or_default()
                .name,
            state: forgerpc::TenantState::try_from(show.partition.status.unwrap_or_default().state)
                .unwrap_or_default()
                .as_str_name()
                .to_string(),
            created,
            physical_partitions,
        }
    }
}

/// List logical partitions
pub(super) async fn show_nvlink_logical_partitions_html(
    AxumState(state): AxumState<Arc<Api>>,
) -> Response {
    let partitions = match fetch_logical_partitions(state.clone(), false, None).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_logical_partitions");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading logical partitions",
            )
                .into_response();
        }
    };

    let tmpl = LogicalPartitionShow {
        partitions: partitions.into_iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_nvlink_logical_partitions_json(
    AxumState(state): AxumState<Arc<Api>>,
) -> impl IntoResponse {
    let partitions = match fetch_logical_partitions(state, false, None).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_logical_partitions");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json("Error loading logical_partitions".to_string()),
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(partitions)).into_response()
}

/// List NVLink domains with health reports.
pub(super) async fn show_nvlink_domain_health_html(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<NvLinkDomainHealthParams>,
    uri: OriginalUri,
) -> Response {
    let rows = match fetch_nvlink_domain_health_rows(&state).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, "fetch_nvlink_domain_health_rows");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading NVLink domain health",
            )
                .into_response();
        }
    };

    let search_query = params.q.trim().to_string();
    let rows = filter_nvlink_domain_health_rows(rows, &search_query);
    let extra_query_params = domain_health_extra_query_params(&search_query);
    let (info, domains) = pagination::paginate_vec(rows, &params.pagination);
    let path = uri.path();

    let tmpl = NvLinkDomainHealthShow {
        domains,
        json_path: format!("{path}.json"),
        search_query,
        page: PageContext::new(info, path).with_extra_params(extra_query_params),
    };

    match tmpl.render() {
        Ok(html) => (StatusCode::OK, Html(html)).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "render_nvlink_domain_health");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error rendering NVLink domain health",
            )
                .into_response()
        }
    }
}

/// List NVLink domains with health reports as JSON.
pub(super) async fn show_nvlink_domain_health_json(
    AxumState(state): AxumState<Arc<Api>>,
) -> Response {
    let rows = match fetch_nvlink_domain_health_rows(&state).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, "fetch_nvlink_domain_health_rows");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json("Error loading NVLink domain health".to_string()),
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(rows)).into_response()
}

/// View Logical Partition details
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(partition_id): AxumPath<String>,
) -> Response {
    let (show_json, partition_id) = match partition_id.strip_suffix(".json") {
        Some(partition_id) => (true, partition_id.to_string()),
        None => (false, partition_id),
    };

    let partitionid = match Uuid::parse_str(&partition_id) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Logical Partition id {partition_id} could not be parsed into UUID Err {e}"
                ),
            )
                .into_response();
        }
    };
    let partitions = match fetch_logical_partitions(state.clone(), true, Some(partitionid)).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_logical_partitions");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading logical partitions",
            )
                .into_response();
        }
    };

    if show_json {
        return (StatusCode::OK, Json(partitions[0].clone())).into_response();
    }

    let tmpl: LogicalPartitionDetail = partitions[0].clone().into();
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

/// Fetches NVLink domain rows from the health-report table.
async fn fetch_nvlink_domain_health_rows(
    api: &Api,
) -> Result<Vec<NvLinkDomainHealthRow>, db::DatabaseError> {
    let ids = db::nvlink_domain_health_report::list_domain_ids(api.db_reader().as_mut()).await?;

    Ok(ids.into_iter().map(Into::into).collect())
}

/// Filters NVLink domain rows by case-insensitive domain ID substring.
fn filter_nvlink_domain_health_rows(
    rows: Vec<NvLinkDomainHealthRow>,
    search_query: &str,
) -> Vec<NvLinkDomainHealthRow> {
    if search_query.is_empty() {
        return rows;
    }

    let search_query = search_query.to_ascii_lowercase();

    rows.into_iter()
        .filter(|row| row.id.contains(&search_query))
        .collect()
}

/// Builds query parameters that pagination links must preserve.
fn domain_health_extra_query_params(search_query: &str) -> String {
    if search_query.is_empty() {
        String::new()
    } else {
        format!("&q={}", urlencoding::encode(search_query))
    }
}

async fn fetch_logical_partitions(
    api: Arc<Api>,
    detail: bool,
    pid: Option<Uuid>,
) -> Result<Vec<ShowLogicalPartition>, tonic::Status> {
    let request =
        tonic::Request::new(rpc::forge::NvLinkLogicalPartitionSearchFilter { name: None });
    let mut show_partitions = Vec::<ShowLogicalPartition>::new();

    let partition_ids = api
        .find_nv_link_logical_partition_ids(request)
        .await?
        .into_inner()
        .partition_ids;
    if partition_ids.is_empty() {
        return Ok(show_partitions);
    }

    let mut partitions = Vec::new();
    if let Some(pid) = pid {
        let request_partitions =
            tonic::Request::new(rpc::forge::NvLinkLogicalPartitionsByIdsRequest {
                partition_ids: vec![pid.into()],
                include_history: false,
            });
        let next_partitions = api
            .find_nv_link_logical_partitions_by_ids(request_partitions)
            .await
            .map(|response| response.into_inner())?;
        partitions.extend(next_partitions.partitions);
    } else {
        let mut offset = 0;
        while offset != partition_ids.len() {
            const PAGE_SIZE: usize = 100;
            let page_size = PAGE_SIZE.min(partition_ids.len() - offset);
            let next_ids = &partition_ids[offset..offset + page_size];
            let request_partitions =
                tonic::Request::new(rpc::forge::NvLinkLogicalPartitionsByIdsRequest {
                    partition_ids: next_ids.to_vec(),
                    include_history: false,
                });
            let next_partitions = api
                .find_nv_link_logical_partitions_by_ids(request_partitions)
                .await
                .map(|response| response.into_inner())?;

            partitions.extend(next_partitions.partitions);
            offset += page_size;
        }
    }

    let request = tonic::Request::new(rpc::forge::NvLinkPartitionSearchFilter {
        name: None,
        tenant_organization_id: None,
    });

    let mut map: HashMap<_, Vec<forgerpc::NvLinkPartition>> = HashMap::new();
    let mut member_map: HashMap<NvLinkPartitionId, Vec<ShowPartitionMember>> = HashMap::new();

    let ids = api
        .find_nv_link_partition_ids(request)
        .await
        .map(|response| response.into_inner())
        .unwrap();

    if !ids.partition_ids.is_empty() {
        let request = tonic::Request::new(forgerpc::NvLinkPartitionsByIdsRequest {
            partition_ids: ids.partition_ids,
            include_history: false,
        });

        let physical_partitions = api
            .find_nv_link_partitions_by_ids(request)
            .await
            .map(|response| response.into_inner())
            .unwrap();

        if detail {
            let request = tonic::Request::new(forgerpc::MachineSearchConfig {
                mnnvl_only: true,
                include_predicted_host: true,
                ..Default::default()
            });

            let machine_ids = api
                .find_machine_ids(request)
                .await?
                .into_inner()
                .machine_ids;

            let mut machines = Vec::new();
            let mut offset = 0;
            while offset != machine_ids.len() {
                const PAGE_SIZE: usize = 100;
                let page_size = PAGE_SIZE.min(machine_ids.len() - offset);
                let next_ids = &machine_ids[offset..offset + page_size];
                let next_vpcs = api
                    .find_machines_by_ids(tonic::Request::new(forgerpc::MachinesByIdsRequest {
                        machine_ids: next_ids.to_vec(),
                        include_history: false,
                    }))
                    .await?
                    .into_inner();

                machines.extend(next_vpcs.machines);
                offset += page_size;
            }

            for m in machines {
                if let Some(status) = m.status.and_then(|status| status.nvlink) {
                    for gpu in &status.gpu_status {
                        if let Some(partition_id) = &gpu.partition_id {
                            member_map.entry(*partition_id).or_default().push(
                                ShowPartitionMember {
                                    machine_id: m.id.unwrap_or_default().to_string(),
                                    gpu_id: gpu.guid.to_string(),
                                },
                            );
                        }
                    }
                }
            }
        }

        for lp in &partitions {
            if let Some(ref lp_id) = lp.id {
                let matching_partitions: Vec<forgerpc::NvLinkPartition> = physical_partitions
                    .partitions
                    .iter()
                    .filter(|p| p.logical_partition_id.as_ref() == Some(lp_id))
                    .cloned()
                    .collect();
                map.insert(*lp_id, matching_partitions);
            }
        }
    }

    for lp in partitions {
        let ph_p = lp.id.and_then(|id| map.get(&id)).cloned();
        let mut show_physical_partitions = Vec::new();
        if let Some(ph_p) = ph_p {
            for p in ph_p {
                let m = p.id.and_then(|id| member_map.get(&id));
                if let Some(m) = m {
                    show_physical_partitions.push(ShowPhysicalPartition {
                        partition: p,
                        members: m.to_vec(),
                    })
                } else {
                    show_physical_partitions.push(ShowPhysicalPartition {
                        partition: p,
                        members: vec![],
                    })
                }
            }
        }

        let show_lp = ShowLogicalPartition {
            partition: lp.clone(),
            physical_partitions: show_physical_partitions,
        };
        show_partitions.push(show_lp);
    }

    Ok(show_partitions)
}

impl super::Base for LogicalPartitionShow {}
impl super::Base for NvLinkDomainHealthShow {}
impl super::Base for LogicalPartitionDetail {}
