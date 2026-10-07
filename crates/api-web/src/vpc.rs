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
#[template(path = "vpc_show.html")]
struct VpcShow {
    vpcs: Vec<VpcRowDisplay>,
}

struct VpcRowDisplay {
    id: String,
    metadata: rpc::forge::Metadata,
    tenant_organization_id: String,
    tenant_keyset_id: String,
    network_virtualization_type: String,
    routing_profile_type: String,
    vni: String,
}

fn vpc_allocated_vni_display(vpc: &forgerpc::Vpc) -> String {
    vpc.status
        .as_ref()
        .and_then(|status| status.vni)
        .map(|vni| vni.to_string())
        .unwrap_or_default()
}

fn vpc_virtualization_type_display(config: &forgerpc::VpcConfig) -> String {
    format!(
        "{:?}",
        forgerpc::VpcVirtualizationType::try_from(
            config.network_virtualization_type.unwrap_or_default(),
        )
        .unwrap_or_default()
    )
}

impl From<forgerpc::Vpc> for VpcRowDisplay {
    fn from(vpc: forgerpc::Vpc) -> Self {
        let vni = vpc_allocated_vni_display(&vpc);
        let config = vpc.config.unwrap_or_default();
        let network_virtualization_type = vpc_virtualization_type_display(&config);

        Self {
            network_virtualization_type,
            id: vpc.id.unwrap_or_default().to_string(),
            metadata: vpc.metadata.unwrap_or_default(),
            tenant_organization_id: config.tenant_organization_id,
            tenant_keyset_id: config.tenant_keyset_id.unwrap_or_default(),
            routing_profile_type: config.routing_profile_type.unwrap_or("None".to_string()),
            vni,
        }
    }
}

/// List VPCs
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let vpcs = match fetch_vpcs(state.clone()).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_vpcs");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading VPCs").into_response();
        }
    };

    let tmpl = VpcShow {
        vpcs: vpcs.into_iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let vpcs = match fetch_vpcs(state).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_vpcs");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading VPCs").into_response();
        }
    };
    let list = forgerpc::VpcList { vpcs };
    serde_json::to_string(&list).unwrap();
    (StatusCode::OK, Json(list)).into_response()
}

async fn fetch_vpcs(api: Arc<Api>) -> Result<Vec<forgerpc::Vpc>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::VpcSearchFilter::default());

    let vpc_ids = api.find_vpc_ids(request).await?.into_inner().vpc_ids;

    let mut vpcs = Vec::new();
    let mut offset = 0;
    while offset != vpc_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(vpc_ids.len() - offset);
        let next_ids = &vpc_ids[offset..offset + page_size];
        let next_vpcs = api
            .find_vpcs_by_ids(tonic::Request::new(forgerpc::VpcsByIdsRequest {
                vpc_ids: next_ids.to_vec(),
            }))
            .await?
            .into_inner();

        vpcs.extend(next_vpcs.vpcs);
        offset += page_size;
    }

    vpcs.sort_unstable_by(|vpc1, vpc2| {
        // Order by name first, and ID second
        let vpc1_name = vpc1
            .metadata
            .as_ref()
            .map(|x| x.name.as_str())
            .unwrap_or_default();
        let vpc2_name = vpc2
            .metadata
            .as_ref()
            .map(|x| x.name.as_str())
            .unwrap_or_default();
        let ord = vpc1_name.cmp(vpc2_name);

        if !ord.is_eq() {
            return ord;
        }

        vpc1.id
            .as_ref()
            .map(|id| id.to_string())
            .cmp(&vpc2.id.as_ref().map(|id| id.to_string()))
    });
    Ok(vpcs)
}

struct VpcPeeringRow {
    id: String,
    peer_vpc_id: String,
}

#[derive(Template)]
#[template(path = "vpc_detail.html")]
struct VpcDetail {
    id: String,
    tenant_organization_id: String,
    tenant_keyset_id: String,
    network_virtualization_type: String,
    routing_profile_type: String,
    has_routing_profile_overrides: bool,
    routing_profile_overrides: String,
    has_effective_routing_profile: bool,
    effective_routing_profile: String,
    vni: String,
    metadata_detail: super::MetadataDetail,
    peerings: Vec<VpcPeeringRow>,
}

impl From<forgerpc::Vpc> for VpcDetail {
    fn from(vpc: forgerpc::Vpc) -> Self {
        let vni = vpc_allocated_vni_display(&vpc);
        let config = vpc.config.unwrap_or_default();
        let network_virtualization_type = vpc_virtualization_type_display(&config);
        let routing_profile_overrides = config
            .routing_profile_overrides
            .as_ref()
            .and_then(|overrides| serde_json::to_string_pretty(overrides).ok());
        let effective_routing_profile = vpc
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref())
            .and_then(|profile| serde_json::to_string_pretty(profile).ok());
        Self {
            network_virtualization_type,
            id: vpc.id.unwrap_or_default().to_string(),
            tenant_organization_id: config.tenant_organization_id,
            tenant_keyset_id: config.tenant_keyset_id.unwrap_or_default(),
            routing_profile_type: config.routing_profile_type.unwrap_or("None".to_string()),
            vni,
            has_routing_profile_overrides: routing_profile_overrides.is_some(),
            routing_profile_overrides: routing_profile_overrides.unwrap_or_default(),
            has_effective_routing_profile: effective_routing_profile.is_some(),
            effective_routing_profile: effective_routing_profile.unwrap_or_default(),
            metadata_detail: super::MetadataDetail {
                metadata: vpc.metadata.unwrap_or_default(),
                metadata_version: vpc.version,
            },
            peerings: Vec::new(),
        }
    }
}

/// View VPC details
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(vpc_id): AxumPath<String>,
) -> Response {
    let (show_json, vpc_id_string) = match vpc_id.strip_suffix(".json") {
        Some(vpc_id) => (true, vpc_id.to_string()),
        None => (false, vpc_id),
    };

    let vpc_id = match vpc_id_string.parse() {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid VPC ID {vpc_id_string}: {e}"),
            )
                .into_response();
        }
    };

    let request = tonic::Request::new(forgerpc::VpcsByIdsRequest {
        vpc_ids: vec![vpc_id],
    });
    let vpc = match state
        .find_vpcs_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(x) if x.vpcs.is_empty() => {
            return super::not_found_response(vpc_id_string);
        }
        Ok(x) if x.vpcs.len() != 1 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("VPC list for {vpc_id} returned {} VPCs", x.vpcs.len()),
            )
                .into_response();
        }
        Ok(mut x) => x.vpcs.remove(0),
        Err(err) => {
            tracing::error!(error = %err, "find_vpcs");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading VPCs").into_response();
        }
    };

    if show_json {
        return (StatusCode::OK, Json(vpc)).into_response();
    }

    let mut tmpl: VpcDetail = vpc.into();

    let peerings = fetch_vpc_peerings(state, vpc_id_string).await;
    tmpl.peerings = peerings;

    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

async fn fetch_vpc_peerings(state: Arc<Api>, vpc_id_string: String) -> Vec<VpcPeeringRow> {
    let Ok(vpc_id) = vpc_id_string.parse() else {
        return Vec::new();
    };

    // Get the peering IDs for the VPC
    let peering_ids = match state
        .find_vpc_peering_ids(tonic::Request::new(forgerpc::VpcPeeringSearchFilter {
            vpc_id: Some(vpc_id),
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(id_list) => id_list.vpc_peering_ids,
        Err(err) => {
            tracing::error!(error = %err, "find_vpc_peering_ids");
            return Vec::new();
        }
    };

    // Short circuit if there are no peerings
    if peering_ids.is_empty() {
        return Vec::new();
    }

    // Get the peerings by IDs
    let peerings = match state
        .find_vpc_peerings_by_ids(tonic::Request::new(forgerpc::VpcPeeringsByIdsRequest {
            vpc_peering_ids: peering_ids,
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(peerings) => peerings.vpc_peerings,
        Err(err) => {
            tracing::error!(error = %err, "find_vpc_peerings_by_ids");
            Vec::new()
        }
    };

    peerings
        .into_iter()
        .map(|p| {
            let vpc_id_str = p
                .vpc_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default();
            let peer_vpc_id_str = p
                .peer_vpc_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default();
            // The search filter may return peerings where the current VPC is on
            // either side; resolve to whichever side is not the current VPC.
            let peer_vpc_id = if vpc_id_str == vpc_id_string {
                peer_vpc_id_str
            } else {
                vpc_id_str
            };
            VpcPeeringRow {
                id: p.id.map(|id| id.to_string()).unwrap_or_default(),
                peer_vpc_id,
            }
        })
        .collect()
}

impl super::Base for VpcShow {}
impl super::Base for VpcDetail {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_profile_display_shows_raw_and_effective_values() {
        let vpc = forgerpc::Vpc {
            config: Some(forgerpc::VpcConfig {
                routing_profile_overrides: Some(forgerpc::VpcRoutingProfileOverrides {
                    leak_default_route_from_underlay: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            status: Some(forgerpc::VpcStatus {
                effective_routing_profile: Some(forgerpc::VpcEffectiveRoutingProfile {
                    allowed_anycast_prefixes: vec![forgerpc::PrefixFilterPolicyEntry {
                        prefix: "198.51.100.0/24".to_string(),
                    }],
                    internal: true,
                    access_tier: 2,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let detail = VpcDetail::from(vpc);
        assert!(detail.has_routing_profile_overrides);
        let overrides: serde_json::Value =
            serde_json::from_str(&detail.routing_profile_overrides).unwrap();
        assert_eq!(overrides["leak_default_route_from_underlay"], false);
        assert!(overrides.get("internal").is_none());
        assert!(overrides.get("access_tier").is_none());

        assert!(detail.has_effective_routing_profile);
        let effective: serde_json::Value =
            serde_json::from_str(&detail.effective_routing_profile).unwrap();
        assert_eq!(
            effective["allowed_anycast_prefixes"][0]["prefix"],
            "198.51.100.0/24"
        );
        assert_eq!(effective["internal"], true);
        assert_eq!(effective["access_tier"], 2);
    }

    #[test]
    fn routing_profile_display_tracks_absent_values() {
        let detail = VpcDetail::from(forgerpc::Vpc::default());

        assert!(!detail.has_routing_profile_overrides);
        assert!(detail.routing_profile_overrides.is_empty());
        assert!(!detail.has_effective_routing_profile);
        assert!(detail.effective_routing_profile.is_empty());
    }
}
