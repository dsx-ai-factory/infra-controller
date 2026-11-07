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
use carbide_uuid::domain::DomainId;
use carbide_uuid::network::NetworkSegmentId;
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::{Base, filters};

#[derive(Template)]
#[template(path = "network_segment_show.html")]
struct NetworkSegmentShow {
    admin: Vec<NetworkSegmentRowDisplay>,
    host_inband: Vec<NetworkSegmentRowDisplay>,
    tenant: Vec<NetworkSegmentRowDisplay>,
    underlay: Vec<NetworkSegmentRowDisplay>,
}

struct NetworkSegmentRowDisplay {
    name: String,
    id: String,
    vpc_id: String,
    created: String,
    state_display: super::StateDisplay,
    sub_domain: String,
    mtu: i32,
    prefixes: String,
    version: String,
}

impl TryFrom<forgerpc::NetworkSegment> for NetworkSegmentRowDisplay {
    type Error = &'static str;

    fn try_from(segment: forgerpc::NetworkSegment) -> Result<Self, Self::Error> {
        let name = segment
            .metadata
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_default();

        let config = segment.config.ok_or("network segment missing config")?;
        let status = segment.status.unwrap_or_default();
        let lifecycle = status.lifecycle.as_ref();

        Ok(Self {
            id: segment.id.unwrap_or_default().to_string(),
            name,
            vpc_id: config.vpc_id.map(|id| id.to_string()).unwrap_or_default(),
            created: segment.created.unwrap_or_default().to_string(),
            state_display: super::StateDisplay::from_lifecycle(lifecycle),
            sub_domain: String::new(), // filled in later
            mtu: config.mtu.unwrap_or(-1),
            prefixes: config
                .prefixes
                .iter()
                .map(|x| x.prefix.to_string())
                .collect::<Vec<String>>()
                .join(", "),
            version: lifecycle.map(|lc| lc.version.clone()).unwrap_or_default(),
        })
    }
}

/// List network segments
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let networks = match fetch_network_segments(state.clone()).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_network_segments");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading network segments",
            )
                .into_response();
        }
    };

    let mut admin = Vec::new();
    let mut host_inband = Vec::new();
    let mut underlay = Vec::new();
    let mut tenant = Vec::new();
    for n in networks.into_iter() {
        let mut domain_name = String::new();
        if let Some(config) = n.config.as_ref()
            && let Some(domain_id) = config.subdomain_id.as_ref()
            && let Ok(name) = get_domain_name(state.clone(), domain_id).await
        {
            domain_name = name;
        };
        let segment_type = n
            .config
            .as_ref()
            .map(|c| c.segment_type)
            .unwrap_or_default();

        let mut display: NetworkSegmentRowDisplay = match n.try_into() {
            Ok(d) => d,
            Err(err) => {
                tracing::error!(error = err, "skipping malformed network segment");
                continue;
            }
        };
        display.sub_domain = domain_name;
        match forgerpc::NetworkSegmentType::try_from(segment_type) {
            Ok(forgerpc::NetworkSegmentType::Admin) => admin.push(display),
            Ok(forgerpc::NetworkSegmentType::HostInband) => host_inband.push(display),
            Ok(forgerpc::NetworkSegmentType::Underlay) => underlay.push(display),
            Ok(forgerpc::NetworkSegmentType::Tenant) => tenant.push(display),
            _ => {
                tracing::error!(segment_type, "Invalid NetworkSegmentType, skipping");
            }
        }
    }

    let tmpl = NetworkSegmentShow {
        admin,
        host_inband,
        underlay,
        tenant,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let networks = match fetch_network_segments(state).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_network_segments");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading network segments",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(networks)).into_response()
}

async fn fetch_network_segments(
    api: Arc<Api>,
) -> Result<Vec<forgerpc::NetworkSegment>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::NetworkSegmentSearchFilter::default());

    let network_segments_ids = api
        .find_network_segment_ids(request)
        .await?
        .into_inner()
        .network_segments_ids;

    let mut segments = Vec::new();
    let mut offset = 0;
    while offset != network_segments_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(network_segments_ids.len() - offset);
        let next_ids = &network_segments_ids[offset..offset + page_size];
        let next_vpcs = api
            .find_network_segments_by_ids(tonic::Request::new(
                forgerpc::NetworkSegmentsByIdsRequest {
                    network_segments_ids: next_ids.to_vec(),
                    include_history: false,
                    include_num_free_ips: false,
                },
            ))
            .await?
            .into_inner();

        segments.extend(next_vpcs.network_segments);
        offset += page_size;
    }

    segments.sort_unstable_by(|ns1, ns2| {
        let n1 = ns1
            .metadata
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or_default();
        let n2 = ns2
            .metadata
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or_default();
        n1.cmp(n2)
    });
    Ok(segments)
}

async fn get_domain_name(state: Arc<Api>, domain_id: &DomainId) -> eyre::Result<String> {
    let request = tonic::Request::new(rpc::protos::dns::DomainSearchQuery {
        id: Some(*domain_id),
        name: None,
    });
    let domain_list = state
        .find_domain(request)
        .await
        .map(|response| response.into_inner())?;

    if domain_list.domains.len() != 1 {
        eyre::bail!(
            "expected one domain matching {domain_id}, found {}",
            domain_list.domains.len()
        );
    }
    Ok(domain_list.domains[0].name.clone())
}

#[derive(Template)]
#[template(path = "network_segment_detail.html")]
struct NetworkSegmentDetail {
    id: String,
    name: String,
    vpc_id: String,
    version: String,
    created: String,
    updated: String,
    deleted: String,
    lifecycle_detail: super::LifecycleDetail,
    domain_id: String,
    domain_name: String,
    segment_type: String,
    infer_slaac_eui64_addresses: bool,
    prefixes: Vec<NetworkSegmentPrefix>,
    history: Vec<NetworkSegmentHistory>,
}

struct NetworkSegmentPrefix {
    index: usize,
    id: String,
    prefix: String,
    gateway: String,
    reserve_first: i32,
}

struct NetworkSegmentHistory {
    state: String,
    version: String,
}

impl TryFrom<forgerpc::NetworkSegment> for NetworkSegmentDetail {
    type Error = &'static str;

    fn try_from(segment: forgerpc::NetworkSegment) -> Result<Self, Self::Error> {
        let name = segment
            .metadata
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_default();

        let config = segment.config.ok_or("network segment missing config")?;
        let status = segment.status.unwrap_or_default();

        let mut prefixes = Vec::new();
        for (i, p) in config.prefixes.into_iter().enumerate() {
            prefixes.push(NetworkSegmentPrefix {
                index: i,
                id: p.id.unwrap_or_default().to_string(),
                prefix: p.prefix,
                gateway: p.gateway.unwrap_or_else(|| "Unknown".to_string()),
                reserve_first: p.reserve_first,
            });
        }

        let lifecycle_detail = status
            .lifecycle
            .map(super::LifecycleDetail::from)
            .unwrap_or_else(|| {
                super::LifecycleDetail::new(String::new(), String::new(), None, None)
            });

        Ok(Self {
            id: segment.id.unwrap_or_default().to_string(),
            name,
            version: lifecycle_detail.version.clone(),
            vpc_id: config.vpc_id.map(|id| id.to_string()).unwrap_or_default(),
            created: segment.created.unwrap_or_default().to_string(),
            updated: segment.updated.unwrap_or_default().to_string(),
            deleted: segment
                .deleted
                .map(|x| x.to_string())
                .unwrap_or("Not Deleted".to_string()),
            lifecycle_detail,
            domain_id: config.subdomain_id.unwrap_or_default().to_string(),
            domain_name: String::new(), // filled in later
            segment_type: format!(
                "{:?}",
                forgerpc::NetworkSegmentType::try_from(config.segment_type).unwrap_or_default()
            ),
            infer_slaac_eui64_addresses: config.infer_slaac_eui64_addresses,
            prefixes,
            // History is fetched separately via FindNetworkSegmentStateHistories
            // and set on the template after conversion.
            history: Vec::new(),
        })
    }
}

/// View networks segment details
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(segment_id): AxumPath<String>,
) -> Response {
    let (show_json, segment_id_string) = match segment_id.strip_suffix(".json") {
        Some(segment_id) => (true, segment_id.to_string()),
        None => (false, segment_id),
    };

    let segment_id = match segment_id_string.parse::<NetworkSegmentId>() {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid network segment ID {segment_id_string}: {e}"),
            )
                .into_response();
        }
    };

    let request = tonic::Request::new(forgerpc::NetworkSegmentsByIdsRequest {
        network_segments_ids: vec![segment_id],
        include_history: false, // deprecated; fetched separately below
        include_num_free_ips: true,
    });

    let segment = match state
        .find_network_segments_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(n) if n.network_segments.is_empty() => {
            return super::not_found_response(segment_id_string);
        }
        Ok(n) if n.network_segments.len() != 1 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Network Segment list for {segment_id} returned {} segments",
                    n.network_segments.len()
                ),
            )
                .into_response();
        }
        Ok(mut n) => n.network_segments.remove(0),
        Err(err) => {
            tracing::error!(error = %err, "find_network_segments");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading network segments",
            )
                .into_response();
        }
    };

    if show_json {
        return (StatusCode::OK, Json(segment)).into_response();
    }

    let mut domain_name = String::new();
    if let Some(domain_id) = segment
        .config
        .as_ref()
        .and_then(|c| c.subdomain_id.as_ref())
        && let Ok(name) = get_domain_name(state.clone(), domain_id).await
    {
        domain_name = name;
    };
    let mut tmpl: NetworkSegmentDetail = match segment.try_into() {
        Ok(t) => t,
        Err(err) => {
            tracing::error!(error = err, "malformed network segment");
            return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response();
        }
    };

    tmpl.domain_name = domain_name;

    if let Ok(mut histories) = state
        .find_network_segment_state_histories(tonic::Request::new(
            forgerpc::NetworkSegmentStateHistoriesRequest {
                network_segment_ids: vec![segment_id],
            },
        ))
        .await
        .map(|r| r.into_inner().histories)
        && let Some(records) = histories.remove(&segment_id.to_string())
    {
        tmpl.history = records
            .records
            .into_iter()
            .map(|h| NetworkSegmentHistory {
                state: h.state,
                version: h.version,
            })
            .collect();
    }

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

impl super::Base for NetworkSegmentShow {}
impl super::Base for NetworkSegmentDetail {}
