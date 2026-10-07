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
use axum::extract::State as AxumState;
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::Base;

#[derive(Template)]
#[template(path = "spx_partition_show.html")]
struct SpxPartitionShow {
    partitions: Vec<SpxPartitionRowDisplay>,
}

struct SpxPartitionRowDisplay {
    id: String,
    name: String,
    tenant_organization_id: String,
    vni: i32,
}

impl From<forgerpc::SpxPartition> for SpxPartitionRowDisplay {
    fn from(partition: forgerpc::SpxPartition) -> Self {
        Self {
            id: partition.id.map(|id| id.to_string()).unwrap_or_default(),
            tenant_organization_id: partition.tenant_organization_id,
            name: partition
                .metadata
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_default(),
            vni: partition.vni as i32,
        }
    }
}

/// List partitions
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let partitions = match fetch_spx_partitions(state.clone()).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_spx_partitions");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading SPX partitions",
            )
                .into_response();
        }
    };

    let tmpl = SpxPartitionShow {
        partitions: partitions.into_iter().map(Into::into).collect(),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let partitions = match fetch_spx_partitions(state).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_spx_partitions");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading SPX partitions",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(partitions)).into_response()
}

async fn fetch_spx_partitions(api: Arc<Api>) -> Result<Vec<forgerpc::SpxPartition>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::SpxPartitionSearchFilter::default());

    let spx_partition_ids = api
        .find_spx_partition_ids(request)
        .await?
        .into_inner()
        .spx_partition_ids;

    let mut partitions = Vec::new();
    let mut offset = 0;
    while offset != spx_partition_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(spx_partition_ids.len() - offset);
        let next_ids = &spx_partition_ids[offset..offset + page_size];
        let request = tonic::Request::new(forgerpc::SpxPartitionsByIdsRequest {
            spx_partition_ids: next_ids.to_vec(),
        });
        let next_partitions = api
            .find_spx_partitions_by_ids(request)
            .await
            .map(|response| response.into_inner())?;

        partitions.extend(next_partitions.spx_partitions);
        offset += page_size;
    }

    Ok(partitions)
}

impl super::Base for SpxPartitionShow {}
