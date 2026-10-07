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
#[template(path = "tenant_show.html")]
struct TenantShow {
    tenants: Vec<TenantDisplay>,
}

struct TenantDisplay {
    organization_id: String,
    routing_profile_type: String,
    metadata: rpc::forge::Metadata,
}

impl From<forgerpc::Tenant> for TenantDisplay {
    fn from(tenant: forgerpc::Tenant) -> Self {
        Self {
            routing_profile_type: tenant
                .routing_profile_type
                .unwrap_or_else(|| "None".to_string()),
            organization_id: tenant.organization_id,
            metadata: tenant.metadata.unwrap_or_default(),
        }
    }
}

/// List tenants
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let out = match fetch_tenants(state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_tenants");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading tenants").into_response();
        }
    };

    let tmpl = TenantShow {
        tenants: out.tenants.into_iter().map(|t| t.into()).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let out: forgerpc::TenantList = match fetch_tenants(state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_tenants");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading tenants").into_response();
        }
    };
    (StatusCode::OK, Json(out)).into_response()
}

async fn fetch_tenants(api: Arc<Api>) -> Result<forgerpc::TenantList, tonic::Status> {
    let request = tonic::Request::new(forgerpc::TenantSearchFilter {
        tenant_organization_name: None,
    });

    let tenant_ids = api
        .find_tenant_organization_ids(request)
        .await?
        .into_inner()
        .tenant_organization_ids;

    let mut tenants = Vec::new();
    let mut offset = 0;
    while offset != tenant_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(tenant_ids.len() - offset);
        let next_ids = &tenant_ids[offset..offset + page_size];
        let next_vpcs = api
            .find_tenants_by_organization_ids(tonic::Request::new(
                forgerpc::TenantByOrganizationIdsRequest {
                    organization_ids: next_ids.to_vec(),
                },
            ))
            .await?
            .into_inner();

        tenants.extend(next_vpcs.tenants);
        offset += page_size;
    }

    tenants.sort_by(|t1, t2| t1.organization_id.cmp(&t2.organization_id));

    Ok(forgerpc::TenantList { tenants })
}

#[derive(Template)]
#[template(path = "tenant_detail.html")]
struct TenantDetail {
    tenant: TenantDisplay,
    metadata_detail: super::MetadataDetail,
}

impl From<forgerpc::Tenant> for TenantDetail {
    fn from(tenant: forgerpc::Tenant) -> Self {
        let metadata_detail = super::MetadataDetail {
            metadata: tenant.metadata.clone().unwrap_or_default(),
            metadata_version: tenant.version.clone(),
        };
        Self {
            tenant: tenant.into(),
            metadata_detail,
        }
    }
}

/// View tenant
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(organization_id): AxumPath<String>,
) -> Response {
    let (show_json, organization_id) = match organization_id.strip_suffix(".json") {
        Some(organization_id) => (true, organization_id.to_string()),
        None => (false, organization_id),
    };

    let request = tonic::Request::new(forgerpc::TenantByOrganizationIdsRequest {
        organization_ids: vec![organization_id.clone()],
    });
    let tenant = match state
        .find_tenants_by_organization_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(x) if x.tenants.is_empty() => {
            return super::not_found_response(organization_id);
        }
        Ok(x) if x.tenants.len() != 1 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Tenant list for {organization_id} returned {} tenants",
                    x.tenants.len()
                ),
            )
                .into_response();
        }
        Ok(mut x) => x.tenants.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return super::not_found_response(organization_id);
        }
        Err(err) => {
            tracing::error!(error = %err, %organization_id, "find_tenants");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading tenants").into_response();
        }
    };

    if show_json {
        return (StatusCode::OK, Json(tenant)).into_response();
    }

    let tenant_detail: TenantDetail = tenant.into();
    (StatusCode::OK, Html(tenant_detail.render().unwrap())).into_response()
}

impl super::Base for TenantShow {}
impl super::Base for TenantDetail {}
