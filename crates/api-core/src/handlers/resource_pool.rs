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

use std::collections::{BTreeMap, HashMap};

use ::rpc::forge as rpc;
use carbide_resource_pool::grpc::{GrpcResourcePoolBackend, GrpcResourcePoolError};
use carbide_resource_pool::{PoolDefinitionSource, ResourcePoolRegistry, registration};
use carbide_resource_pool_api::v1;
use chrono::{DateTime, Utc};
use futures::{StreamExt, stream};
use model::resource_pool::binding::{
    INTEGRATED_AUTHORITY_ID, ResourcePoolBackendKind, ResourcePoolBinding, ResourcePoolSelection,
};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;
use crate::cfg::resource_pool::ResourcePoolBackendConfig;

// Each listing may observe up to eight remote pools at once. This keeps slow
// providers from serializing the listing while limiting the work one request starts.
const MAX_CONCURRENT_POOL_OBSERVATIONS: usize = 8;

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
    let selections = pools
        .iter()
        .map(|(name, definition)| {
            (
                name.clone(),
                ResourcePoolSelection {
                    pool_name: name.clone(),
                    value_domain: model::resource_pool::ValueType::from(definition.pool_type)
                        .into(),
                    backend_name: None,
                    backend_kind: ResourcePoolBackendKind::Integrated,
                    authority_id: INTEGRATED_AUTHORITY_ID.to_owned(),
                    remote_pool: name.clone(),
                },
            )
        })
        .collect();
    // Save bindings in the growth transaction so a rejected batch leaves both
    // the bindings and available values unchanged.
    let mut txn = api.txn_begin().await?;
    ResourcePoolRegistry::resolve(PoolDefinitionSource::GrowthRequest, &mut txn, &selections)
        .await
        .map_err(CarbideError::from)?;
    let reconcile_dpu_loopbacks = pools.contains_key(model::resource_pool::common::LOOPBACK_IP_V6);
    use db::resource_pool::DefineResourcePoolError as DE;
    match db::resource_pool::define_all_from(&mut txn, &pools).await {
        Ok(()) => {
            txn.commit().await?;
            if reconcile_dpu_loopbacks {
                // The backfill opens one transaction per DPU, so publish the
                // new pool rows before it starts reconciliation.
                db::machine::update_dpu_loopback_ips_v6(
                    &api.database_connection,
                    &api.common_pools,
                )
                .await
                .map_err(CarbideError::from)?;
            }
            Ok(Response::new(rpc::GrowResourcePoolResponse {}))
        }
        Err(DE::InvalidArgument(msg)) => Err(CarbideError::InvalidArgument(msg).into()),
        Err(DE::ResourcePoolError(msg)) => Err(CarbideError::Internal {
            message: msg.to_string(),
        }
        .into()),
        Err(DE::DatabaseError(err)) => Err(CarbideError::Internal {
            message: err.to_string(),
        }
        .into()),
        Err(err @ DE::TooBig(_, _)) => Err(tonic::Status::out_of_range(err.to_string())),
    }
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

/// `inspect` combines local inventory with saved bindings. A provider failure
/// belongs to its pool's row, so operators can still inspect the other pools.
pub(crate) async fn inspect(
    api: &Api,
    request: Request<rpc::InspectResourcePoolsRequest>,
) -> Result<Response<rpc::InspectResourcePoolsResponse>, Status> {
    crate::api::log_request_data(&request);
    let mut txn = api.txn_begin().await?;
    let inventory = db::resource_pool::all(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    let bindings = db::resource_pool::binding::all(txn.as_pgconn())
        .await
        .map_err(CarbideError::from)?;
    // Release the database transaction before waiting for any provider.
    txn.commit().await?;

    // Local rows remain visible even when no configuration has created a binding.
    // Their bounds describe stored inventory, not the ranges allowed by TOML.
    let observed_at = Utc::now();
    let mut pools = BTreeMap::new();
    for snapshot in inventory {
        let mut pool = integrated_inspection(snapshot.name, observed_at);
        pool.min = Some(snapshot.min);
        pool.max = Some(snapshot.max);
        pool.stats = Some(snapshot.stats.into());
        pools.insert(pool.name.clone(), pool);
    }

    let mut remote = Vec::new();
    for binding in bindings {
        if binding.backend_kind == ResourcePoolBackendKind::Grpc {
            remote.push(binding);
            continue;
        }
        // A bound Integrated pool with no rows is empty. This is a known count,
        // unlike a provider that cannot report its inventory.
        let pool = pools
            .entry(binding.pool_name.clone())
            .or_insert_with(|| integrated_inspection(binding.pool_name, observed_at));
        pool.binding_id = Some(binding.binding_id.to_string());
        pool.backend_name = binding.backend_name;
    }

    let mut observations = stream::iter(remote)
        .map(|binding| inspect_remote(api, binding))
        .buffer_unordered(MAX_CONCURRENT_POOL_OBSERVATIONS);
    while let Some(pool) = observations.next().await {
        pools.insert(pool.name.clone(), pool);
    }
    Ok(Response::new(rpc::InspectResourcePoolsResponse {
        pools: pools.into_values().collect(),
    }))
}

/// `integrated_inspection` starts with the known empty state of a local pool.
/// Inventory observations replace its counts and add bounds when rows exist.
fn integrated_inspection(name: String, observed_at: DateTime<Utc>) -> rpc::ResourcePoolInspection {
    rpc::ResourcePoolInspection {
        remote_pool: name.clone(),
        name,
        backend_kind: rpc::ResourcePoolBackendKind::Integrated as i32,
        authority_id: INTEGRATED_AUTHORITY_ID.to_owned(),
        stats: Some(rpc::ResourcePoolStats::default()),
        stats_status: rpc::ResourcePoolStatsStatus::Known as i32,
        observed_at: Some(observed_at.into()),
        ..Default::default()
    }
}

/// `inspect_remote` uses the acknowledged definition's exact provider identity
/// and revision. The configured alias is only a label here; resolving it again
/// could report a different provider pool after the alias changes.
async fn inspect_remote(api: &Api, binding: ResourcePoolBinding) -> rpc::ResourcePoolInspection {
    let mut pool = rpc::ResourcePoolInspection {
        name: binding.pool_name.clone(),
        backend_kind: rpc::ResourcePoolBackendKind::Grpc as i32,
        backend_name: binding.backend_name.clone(),
        binding_id: Some(binding.binding_id.to_string()),
        authority_id: binding.authority_id.clone(),
        remote_pool: binding.remote_pool.clone(),
        stats_status: rpc::ResourcePoolStatsStatus::Unavailable as i32,
        ..Default::default()
    };
    let definition = match registration::latest_accepted(&api.database_connection, &binding).await {
        Ok(Some(definition)) => definition,
        Ok(None) => {
            pool.stats_message = Some("no accepted pool definition".to_owned());
            return pool;
        }
        Err(error) => {
            tracing::warn!(pool_name = %binding.pool_name, %error, "could not read resource pool definition");
            pool.stats_message = Some("could not read accepted pool definition".to_owned());
            return pool;
        }
    };
    pool.provider_pool_id = definition
        .pool
        .as_ref()
        .map(|identity| identity.pool_id.clone());
    pool.accepted_revision = Some(definition.eligibility_revision.clone());
    let Some(ResourcePoolBackendConfig::Grpc { endpoint, tls }) = binding
        .backend_name
        .as_ref()
        .and_then(|name| api.runtime_config.resource_pool_backends.get(name))
    else {
        pool.stats_message = Some("saved gRPC backend is not configured".to_owned());
        return pool;
    };
    let stats = async {
        // Production remote selection remains gated. Reuse the runtime's shared
        // clients during https://github.com/dsx-ai-factory/infra-controller/issues/7140,
        // before production activation.
        let backend = GrpcResourcePoolBackend::new(endpoint.clone(), tls.clone())?;
        backend
            .stats(v1::StatsRequest {
                pool: definition.pool,
                eligibility_revision: definition.eligibility_revision,
            })
            .await
    }
    .await;
    match stats {
        Ok(response) => {
            // The client validates all six counts and their partitions. Check
            // total capacity too, since numeric consumers add used and free.
            if let Some(stats) = response
                .stats
                .filter(|stats| stats.used.checked_add(stats.free).is_some())
            {
                pool.stats = Some(rpc::ResourcePoolStats {
                    used: stats.used,
                    free: stats.free,
                    auto_assign_used: stats.auto_assign_used,
                    auto_assign_free: stats.auto_assign_free,
                    non_auto_assign_used: stats.non_auto_assign_used,
                    non_auto_assign_free: stats.non_auto_assign_free,
                });
                pool.stats_status = rpc::ResourcePoolStatsStatus::Known as i32;
                pool.observed_at = Some(Utc::now().into());
            } else {
                pool.stats_message =
                    Some("provider capacity exceeds the supported numeric range".to_owned());
            }
        }
        Err(error) => {
            if matches!(&error, GrpcResourcePoolError::Rpc(status) if status.code() == tonic::Code::Unimplemented)
            {
                pool.stats_status = rpc::ResourcePoolStatsStatus::Unknown as i32;
            }
            // The client error's Display intentionally omits endpoint, credential,
            // and provider-supplied text. Keep that redaction at the public API.
            pool.stats_message = Some(error.to_string());
            tracing::warn!(pool_name = %binding.pool_name, %error, "could not inspect resource pool statistics");
        }
    }
    pool
}
