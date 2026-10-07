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

use super::Base;

fn sanitize_os(os: &mut forgerpc::OperatingSystem) {
    for artifact in &mut os.ipxe_template_artifacts {
        artifact.auth_token = None;
    }
}

#[derive(Template)]
#[template(path = "operating_system_show.html")]
struct OperatingSystemShow {
    operating_systems: Vec<OsRowDisplay>,
}

struct OsRowDisplay {
    id: String,
    name: String,
    os_type: String,
    status: String,
    tenant_organization_id: String,
    template_id: String,
    is_active: bool,
}

impl From<&forgerpc::OperatingSystem> for OsRowDisplay {
    fn from(os: &forgerpc::OperatingSystem) -> Self {
        let os_type = forgerpc::OperatingSystemType::try_from(os.r#type)
            .map(|t| format!("{t:?}"))
            .unwrap_or_else(|_| "Unknown".to_string());
        let status = forgerpc::TenantState::try_from(os.status)
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|_| "Unknown".to_string());
        Self {
            id: os.id.as_ref().map(|u| u.to_string()).unwrap_or_default(),
            name: os.name.clone(),
            os_type,
            status,
            tenant_organization_id: os.tenant_organization_id.clone().unwrap_or_default(),
            template_id: os
                .ipxe_template_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            is_active: os.is_active,
        }
    }
}

pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let oss = match fetch_operating_systems(state).await {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(error = %err, "fetch_operating_systems");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading operating systems",
            )
                .into_response();
        }
    };

    let tmpl = OperatingSystemShow {
        operating_systems: oss.iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let mut oss = match fetch_operating_systems(state).await {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(error = %err, "fetch_operating_systems");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading operating systems",
            )
                .into_response();
        }
    };
    oss.iter_mut().for_each(sanitize_os);
    (StatusCode::OK, Json(oss)).into_response()
}

async fn fetch_operating_systems(
    api: Arc<Api>,
) -> Result<Vec<forgerpc::OperatingSystem>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::OperatingSystemSearchFilter {
        tenant_organization_id: None,
    });
    let id_list = api
        .find_operating_system_ids(request)
        .await?
        .into_inner()
        .ids;

    if id_list.is_empty() {
        return Ok(Vec::new());
    }

    let request = tonic::Request::new(forgerpc::OperatingSystemsByIdsRequest { ids: id_list });
    let mut oss = api
        .find_operating_systems_by_ids(request)
        .await?
        .into_inner()
        .operating_systems;

    oss.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(oss)
}

#[derive(Template)]
#[template(path = "operating_system_detail.html")]
struct OsDetail {
    id: String,
    name: String,
    description: String,
    os_type: String,
    status: String,
    tenant_organization_id: String,
    is_active: bool,
    allow_override: bool,
    phone_home_enabled: bool,
    created: String,
    updated: String,
    ipxe_script: String,
    template_id: String,
    definition_hash: String,
    parameters: Vec<OsParameter>,
    artifacts: Vec<OsArtifact>,
}

struct OsParameter {
    name: String,
    value: String,
}

struct OsArtifact {
    name: String,
    url: String,
    cached_url: String,
    sha: String,
    cache_strategy: String,
}

impl From<forgerpc::OperatingSystem> for OsDetail {
    fn from(os: forgerpc::OperatingSystem) -> Self {
        let os_type = forgerpc::OperatingSystemType::try_from(os.r#type)
            .map(|t| format!("{t:?}"))
            .unwrap_or_else(|_| "Unknown".to_string());
        let status = forgerpc::TenantState::try_from(os.status)
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|_| "Unknown".to_string());

        let parameters = os
            .ipxe_template_parameters
            .iter()
            .map(|p| OsParameter {
                name: p.name.clone(),
                value: p.value.clone(),
            })
            .collect();

        let artifacts = os
            .ipxe_template_artifacts
            .iter()
            .map(|a| {
                let cache_strategy =
                    forgerpc::IpxeTemplateArtifactCacheStrategy::try_from(a.cache_strategy)
                        .map(|s| format!("{s:?}"))
                        .unwrap_or_else(|_| "Unknown".to_string());
                OsArtifact {
                    name: a.name.clone(),
                    url: a.url.clone(),
                    cached_url: a.cached_url.clone().unwrap_or_default(),
                    sha: a.sha.clone().unwrap_or_default(),
                    cache_strategy,
                }
            })
            .collect();

        Self {
            id: os.id.as_ref().map(|u| u.to_string()).unwrap_or_default(),
            name: os.name,
            description: os.description.unwrap_or_default(),
            os_type,
            status,
            tenant_organization_id: os.tenant_organization_id.unwrap_or_default(),
            is_active: os.is_active,
            allow_override: os.allow_override,
            phone_home_enabled: os.phone_home_enabled,
            created: os.created,
            updated: os.updated,
            ipxe_script: os.ipxe_script.unwrap_or_default(),
            template_id: os
                .ipxe_template_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            definition_hash: os.ipxe_template_definition_hash.unwrap_or_default(),
            parameters,
            artifacts,
        }
    }
}

pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(os_id): AxumPath<String>,
) -> Response {
    let (show_json, os_id) = match os_id.strip_suffix(".json") {
        Some(id) => (true, id.to_string()),
        None => (false, os_id),
    };

    let os_id_msg: carbide_uuid::operating_system::OperatingSystemId = match os_id.parse() {
        Ok(id) => id,
        Err(_) => return super::not_found_response(os_id),
    };

    let request = tonic::Request::new(os_id_msg);
    let os = match state.get_operating_system(request).await {
        Ok(resp) => resp.into_inner(),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return super::not_found_response(os_id);
        }
        Err(err) => {
            tracing::error!(error = %err, "get_operating_system");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading operating system",
            )
                .into_response();
        }
    };

    if show_json {
        let mut os = os;
        sanitize_os(&mut os);
        return (StatusCode::OK, Json(os)).into_response();
    }

    let detail: OsDetail = os.into();
    (StatusCode::OK, Html(detail.render().unwrap())).into_response()
}

impl super::Base for OperatingSystemShow {}
impl super::Base for OsDetail {}
