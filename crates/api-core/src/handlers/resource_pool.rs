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

use ::rpc::forge as rpc;
use carbide_resource_pool::{ResourcePoolBackendError, ResourcePoolGrowthError};
use db::resource_pool::DefineResourcePoolError as DE;
use futures::StreamExt;
use model::resource_pool::binding::ResourcePoolBackendKind;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;

pub(crate) async fn grow(
    api: &Api,
    request: Request<rpc::GrowResourcePoolRequest>,
) -> Result<Response<rpc::GrowResourcePoolResponse>, Status> {
    crate::api::log_request_data(&request);

    let toml_text = request.into_inner().text;

    let mut pools = HashMap::new();
    let table: toml::Table = toml_text
        .parse()
        .map_err(|e: toml::de::Error| CarbideError::InvalidArgument(e.to_string()))?;
    for (name, def) in table {
        if name.trim().is_empty() {
            return Err(CarbideError::InvalidArgument(
                "resource pool name must not be empty".to_owned(),
            )
            .into());
        }
        let d: model::resource_pool::ResourcePoolDef = def
            .try_into()
            .map_err(|e: toml::de::Error| CarbideError::InvalidArgument(e.to_string()))?;
        pools.insert(name, d);
    }
    match api
        .resource_pool_registry
        .grow(&api.database_connection, &pools)
        .await
    {
        Ok(()) => {}
        Err(ResourcePoolGrowthError::Definition(error)) => {
            return Err(match error {
                DE::InvalidArgument(message) => CarbideError::InvalidArgument(message).into(),
                DE::ResourcePoolError(error) => CarbideError::Internal {
                    message: error.to_string(),
                }
                .into(),
                DE::DatabaseError(error) => CarbideError::Internal {
                    message: error.to_string(),
                }
                .into(),
                error @ DE::TooBig(_, _) => Status::out_of_range(error.to_string()),
            });
        }
        Err(ResourcePoolGrowthError::Database(error)) => {
            return Err(CarbideError::from(error).into());
        }
        Err(ResourcePoolGrowthError::Registry(error)) => {
            return Err(CarbideError::from(error).into());
        }
        Err(ResourcePoolGrowthError::Registration(error)) => {
            return Err(CarbideError::from(error).into());
        }
        Err(ResourcePoolGrowthError::Value(error)) => {
            return Err(CarbideError::InvalidArgument(error.to_string()).into());
        }
    }
    if pools.contains_key(model::resource_pool::common::LOOPBACK_IP_V6) {
        // Backfill reads eligibility in its own transactions, so growth must be
        // committed or acknowledged before those reads begin.
        carbide_site_explorer::dpu_loopback::update_dpu_loopback_ips_v6(
            &api.database_connection,
            &api.resource_pool_registry,
            &api.common_pools,
            &api.cancel_token,
&api.instrumentation,
)
        .await
        .map_err(|error| match error {
            ResourcePoolBackendError::Database(source) if source.is_lock_not_available() => {
                CarbideError::Ipv6LoopbackBackfillContended {
                    source: Box::new(source),
                }
                .into()
            }
            error => {
                tracing::error!(%error, "Pool growth committed; IPv6 loopback backfill is incomplete");
                let mut status: Status = CarbideError::from(error).into();
                let previous = status
                    .metadata()
                    .get("nico-error-mitigation")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                let context = concat!(
                    "Pool growth has committed or been acknowledged. ",
                    "Check the current pool configuration and remaining IPv6 ",
                    "loopback assignments before retrying growth.",
                );
                let mitigation = format!("{context} {previous}");
                match mitigation.trim_end().parse() {
                    Ok(value) => {
                        status.metadata_mut().insert("nico-error-mitigation", value);
                    }
                    Err(error) => tracing::error!(%error, "Could not add committed-growth mitigation"),
                }
                status
            }
        })?;
    }
    Ok(Response::new(rpc::GrowResourcePoolResponse {}))
}

pub(crate) async fn list(
    api: &Api,
    request: Request<rpc::ListResourcePoolsRequest>,
) -> Result<tonic::Response<rpc::ResourcePools>, tonic::Status> {
    crate::api::log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    // This response cannot represent unknown counts. Reject the whole listing
    // when a remote binding exists instead of reporting a misleading zero.
    if db::resource_pool::binding::all(txn.as_pgconn())
        .await
        .map_err(CarbideError::from)?
        .iter()
        .any(|binding| binding.backend_kind == ResourcePoolBackendKind::Grpc)
    {
        return Err(CarbideError::FailedPrecondition(
            "remote resource pools require AdminInspectResourcePools".to_owned(),
        )
        .into());
    }

    let snapshot = db::resource_pool::all(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    Ok(Response::new(rpc::ResourcePools {
        pools: snapshot.into_iter().map(|s| s.into()).collect(),
    }))
}

/// `inspect` reads current backend statistics through the shared monitor,
/// sorted by pool name for a stable admin listing. Unlike metrics, the listing
/// omits local pools that have neither inventory nor a saved binding.
pub(crate) async fn inspect(
    api: &Api,
    request: Request<rpc::InspectResourcePoolsRequest>,
) -> Result<Response<rpc::InspectResourcePoolsResponse>, Status> {
    crate::api::log_request_data(&request);
    let mut pools: Vec<_> = api
        .resource_pool_monitor
        .observe(&[])
        .await?
        .collect()
        .await;
    pools.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(Response::new(rpc::InspectResourcePoolsResponse { pools }))
}
