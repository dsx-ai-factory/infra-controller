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
#[template(path = "tenant_keyset_show.html")]
struct KeysetShow {
    keysets: Vec<KeysetDisplay>,
}

struct KeysetDisplay {
    organization_id: String,
    keyset_id: String,
    version: String,
    num_keys: usize,
    keys: Vec<rpc::forge::TenantPublicKey>,
}

impl From<forgerpc::TenantKeyset> for KeysetDisplay {
    fn from(ks: forgerpc::TenantKeyset) -> Self {
        let default_keyset_content = rpc::forge::TenantKeysetContent {
            public_keys: Vec::new(),
        };
        let content = ks.keyset_content.unwrap_or(default_keyset_content);

        Self {
            organization_id: ks
                .keyset_identifier
                .as_ref()
                .map(|id| id.organization_id.clone())
                .unwrap_or_default(),
            keyset_id: ks
                .keyset_identifier
                .as_ref()
                .map(|id| id.keyset_id.clone())
                .unwrap_or_default(),
            num_keys: content.public_keys.len(),
            keys: content.public_keys,
            version: ks.version,
        }
    }
}

/// List tenants
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let out = match fetch_keysets(state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_keysets");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading keysets").into_response();
        }
    };

    let tmpl = KeysetShow {
        keysets: out.keyset.into_iter().map(|ks| ks.into()).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let out: forgerpc::TenantKeySetList = match fetch_keysets(state).await {
        Ok(ks) => ks,
        Err(err) => {
            tracing::error!(error = %err, "fetch_keysets");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading keysets").into_response();
        }
    };
    (StatusCode::OK, Json(out)).into_response()
}

async fn fetch_keysets(api: Arc<Api>) -> Result<forgerpc::TenantKeySetList, tonic::Status> {
    let request = tonic::Request::new(forgerpc::TenantKeysetSearchFilter {
        tenant_org_id: None,
    });

    let keyset_ids = api
        .find_tenant_keyset_ids(request)
        .await?
        .into_inner()
        .keyset_ids;

    let mut keyset = Vec::new();
    let mut offset = 0;
    while offset != keyset_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(keyset_ids.len() - offset);
        let next_ids = &keyset_ids[offset..offset + page_size];
        let next_keysets = api
            .find_tenant_keysets_by_ids(tonic::Request::new(forgerpc::TenantKeysetsByIdsRequest {
                keyset_ids: next_ids.to_vec(),
                include_key_data: true,
            }))
            .await?
            .into_inner();

        keyset.extend(next_keysets.keyset);
        offset += page_size;
    }

    keyset.sort_unstable_by(|ks1, ks2| {
        let default_keyset_id = rpc::forge::TenantKeysetIdentifier {
            organization_id: String::new(),
            keyset_id: String::new(),
        };

        let id1 = ks1.keyset_identifier.as_ref().unwrap_or(&default_keyset_id);
        let id2 = ks2.keyset_identifier.as_ref().unwrap_or(&default_keyset_id);

        // Order by tenant org first, then keyset ID
        let ord = id1.organization_id.cmp(&id2.organization_id);
        if !ord.is_eq() {
            return ord;
        }

        id1.keyset_id.cmp(&id2.keyset_id)
    });

    Ok(forgerpc::TenantKeySetList { keyset })
}

#[derive(Template)]
#[template(path = "tenant_keyset_detail.html")]
struct TenantKeysetDetail {
    keyset: KeysetDisplay,
}

impl From<forgerpc::TenantKeyset> for TenantKeysetDetail {
    fn from(keyset: forgerpc::TenantKeyset) -> Self {
        Self {
            keyset: keyset.into(),
        }
    }
}

/// View keyset
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath((organization_id, keyset_id)): AxumPath<(String, String)>,
) -> Response {
    let (show_json, keyset_id) = match keyset_id.strip_suffix(".json") {
        Some(keyset_id) => (true, keyset_id.to_string()),
        None => (false, keyset_id),
    };

    let request = tonic::Request::new(forgerpc::TenantKeysetsByIdsRequest {
        keyset_ids: vec![forgerpc::TenantKeysetIdentifier {
            organization_id: organization_id.clone(),
            keyset_id: keyset_id.clone(),
        }],
        include_key_data: true,
    });
    let keyset = match state
        .find_tenant_keysets_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(x) if x.keyset.is_empty() => {
            return super::not_found_response(format!("{organization_id}/{keyset_id}"));
        }
        Ok(x) if x.keyset.len() != 1 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Keyset list for {organization_id}/{keyset_id} returned {} keysets",
                    x.keyset.len()
                ),
            )
                .into_response();
        }
        Ok(mut x) => x.keyset.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return super::not_found_response(format!("{organization_id}/{keyset_id}"));
        }
        Err(err) => {
            tracing::error!(error = %err, %organization_id, "find_tenant_keysets_by_ids");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading keyset").into_response();
        }
    };

    if show_json {
        return (StatusCode::OK, Json(keyset)).into_response();
    }

    let keyset_detail: TenantKeysetDetail = keyset.into();
    (StatusCode::OK, Html(keyset_detail.render().unwrap())).into_response()
}

impl super::Base for KeysetShow {}
impl super::Base for TenantKeysetDetail {}
