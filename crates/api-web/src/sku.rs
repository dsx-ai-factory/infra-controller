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
use crate::filters;

#[derive(Template)]
#[template(path = "sku_show.html")]
struct SkuShow {
    skus: Vec<SkuRowDisplay>,
}

struct SkuRowDisplay {
    id: String,
    architecture: String,
    model: String,
    vendor: String,
    num_cpus: usize,
    num_gpus: usize,
    num_ib_devices: usize,
    memory_capacity: String,
    associated_machine_count: usize,
}

impl From<forgerpc::Sku> for SkuRowDisplay {
    fn from(sku: forgerpc::Sku) -> Self {
        let components = sku.components.as_ref();
        let chassis = components.and_then(|c| c.chassis.as_ref());
        Self {
            id: sku.id,
            architecture: chassis.map(|c| c.architecture.clone()).unwrap_or_default(),
            model: chassis.map(|c| c.model.clone()).unwrap_or_default(),
            vendor: chassis.map(|c| c.vendor.clone()).unwrap_or_default(),
            num_cpus: components
                .map(|c| c.cpus.iter().map(|c| c.count as usize).sum::<usize>())
                .unwrap_or_default(),
            num_gpus: components
                .map(|c| c.gpus.iter().map(|g| g.count as usize).sum::<usize>())
                .unwrap_or_default(),
            num_ib_devices: components
                .map(|c| {
                    c.infiniband_devices
                        .iter()
                        .map(|ib| ib.count as usize)
                        .sum::<usize>()
                })
                .unwrap_or_default(),
            memory_capacity: components
                .map(|c| {
                    c.memory
                        .iter()
                        .map(|m| m.capacity_mb as u64 * m.count as u64)
                        .sum::<u64>()
                })
                .map(|cap_mb| format!("{} GiB", cap_mb as f64 / 1024.0))
                .unwrap_or_default(),
            associated_machine_count: sku.associated_machine_ids.len(),
        }
    }
}

/// List SKUs
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let skus = match fetch_skus(state.clone()).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_skus");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading skus").into_response();
        }
    };

    let tmpl = SkuShow {
        skus: skus.into_iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let skus = match fetch_skus(state).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_skus");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading SKUs").into_response();
        }
    };
    (StatusCode::OK, Json(skus)).into_response()
}

async fn fetch_skus(api: Arc<Api>) -> Result<Vec<forgerpc::Sku>, tonic::Status> {
    let request = tonic::Request::new(());

    let sku_ids = api.get_all_sku_ids(request).await?.into_inner().ids;

    let mut skus = Vec::new();
    let mut offset = 0;
    while offset != sku_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(sku_ids.len() - offset);
        let next_ids = &sku_ids[offset..offset + page_size];
        let request = tonic::Request::new(forgerpc::SkusByIdsRequest {
            ids: next_ids.to_vec(),
        });
        let next_skus = api
            .find_skus_by_ids(request)
            .await
            .map(|response| response.into_inner())?;

        skus.extend(next_skus.skus);
        offset += page_size;
    }

    skus.sort_unstable_by(|sku1, sku2| sku1.id.cmp(&sku2.id));

    Ok(skus)
}

#[derive(Template)]
#[template(path = "sku_detail.html")]
struct SkuDetail {
    id: String,
    description: String,
    created: String,
    components_json: String,
    associated_machines: Vec<String>,
}

impl From<forgerpc::Sku> for SkuDetail {
    fn from(sku: forgerpc::Sku) -> Self {
        Self {
            id: sku.id,
            description: sku.description.unwrap_or_default(),
            created: sku.created.map(|c| c.to_string()).unwrap_or_default(),
            components_json: sku
                .components
                .map(|c| {
                    serde_json::to_string_pretty(&c).unwrap_or_else(|_e| "Invalid JSON".to_string())
                })
                .unwrap_or_default(),
            associated_machines: sku
                .associated_machine_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect::<Vec<String>>(),
        }
    }
}

/// View SKU details
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(sku_id): AxumPath<String>,
) -> Response {
    let (show_json, sku_id) = match sku_id.strip_suffix(".json") {
        Some(sku_id) => (true, sku_id.to_string()),
        None => (false, sku_id),
    };

    let request = tonic::Request::new(forgerpc::SkusByIdsRequest {
        ids: vec![sku_id.clone()],
    });
    let sku = match state
        .find_skus_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(l) if l.skus.is_empty() => {
            return super::not_found_response(sku_id);
        }
        Ok(l) if l.skus.len() != 1 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("SKU list for {sku_id} returned {} SKUs", l.skus.len()),
            )
                .into_response();
        }
        Ok(mut l) => l.skus.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return super::not_found_response(sku_id);
        }
        Err(err) => {
            tracing::error!(error = %err, "find_skus_by_ids");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading SKUs").into_response();
        }
    };

    if show_json {
        return (StatusCode::OK, Json(sku)).into_response();
    }

    let tmpl: SkuDetail = sku.into();
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

impl super::Base for SkuShow {}
impl super::Base for SkuDetail {}
