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

//! Resource pool observations shared by admin inspection and background metrics.

pub(crate) mod metrics;

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use ::rpc::forge as rpc;
use carbide_resource_pool::grpc::{GrpcResourcePoolBackend, GrpcResourcePoolError};
use carbide_resource_pool::registration;
use carbide_resource_pool_api::v1;
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt, stream};
use model::resource_pool::binding::{
    INTEGRATED_AUTHORITY_ID, ResourcePoolBackendKind, ResourcePoolBinding,
};
use sqlx::PgPool;

use crate::CarbideResult;
use crate::cfg::resource_pool::ResourcePoolBackendConfig;

// Each pass observes at most eight remote pools at once. Each shared client
// allows 32 calls across all users, including concurrent admin requests.
const MAX_CONCURRENT_POOL_OBSERVATIONS: usize = 8;

// This read budget includes connection acquisition. Provider calls separately
// use the shared client's ten-second deadline.
const DEFINITION_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// `ResourcePoolMonitor` reads local inventory and remote statistics for both
/// admin inspection and metrics, reusing the same provider connections.
#[derive(Clone)]
pub(crate) struct ResourcePoolMonitor {
    database: PgPool,
    clients: HashMap<String, GrpcResourcePoolBackend>,
}

impl ResourcePoolMonitor {
    /// `new` prepares one client per configured gRPC backend without connecting
    /// or reading credentials. Integrated observations use the database directly.
    pub(crate) fn new(
        database: PgPool,
        backends: &HashMap<String, ResourcePoolBackendConfig>,
    ) -> Result<Self, GrpcResourcePoolError> {
        let mut clients = HashMap::new();
        for (name, config) in backends {
            if let ResourcePoolBackendConfig::Grpc { endpoint, tls } = config {
                clients.insert(
                    name.clone(),
                    GrpcResourcePoolBackend::new(endpoint.clone(), tls.clone())?,
                );
            }
        }
        Ok(Self { database, clients })
    }

    /// `observe` yields Integrated rows first, then remote rows as calls finish.
    /// Expected local pools with no inventory get zero counts. Saved remote
    /// bindings replace those defaults before any observation is returned.
    /// Provider failures belong to their own rows. The stream owns the pending
    /// read and RPC futures, so dropping it stops the pass.
    pub(crate) async fn observe<'a>(
        &'a self,
        expected_local_pools: &'a [String],
    ) -> CarbideResult<impl Stream<Item = rpc::ResourcePoolInspection> + 'a> {
        let mut txn = carbide_resource_pool::database_transaction(&self.database).await?;
        let inventory = db::resource_pool::all(&mut txn).await?;
        let bindings = db::resource_pool::binding::all(txn.as_pgconn()).await?;
        // Release the database transaction before waiting for any provider.
        txn.commit().await?;

        // Local rows remain visible even when no configuration has created a binding.
        // Their bounds describe stored inventory, not the ranges allowed by TOML.
        let observed_at = Utc::now();
        let mut pools: BTreeMap<_, _> = expected_local_pools
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    integrated_inspection(name.clone(), observed_at),
                )
            })
            .collect();
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
                // A remote binding replaces any local inventory with the same name.
                // Remove it before yielding so callers never see local counts for it.
                pools.remove(&binding.pool_name);
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

        let observations = stream::iter(remote)
            .map(|binding| self.inspect_remote(binding))
            .buffer_unordered(MAX_CONCURRENT_POOL_OBSERVATIONS);
        Ok(stream::iter(pools.into_values()).chain(observations))
    }

    /// `inspect_remote` uses the acknowledged definition's exact provider identity
    /// and revision. The configured alias is only a label here; resolving it again
    /// could report a different provider pool after the alias changes.
    async fn inspect_remote(&self, binding: ResourcePoolBinding) -> rpc::ResourcePoolInspection {
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
        let definition = match tokio::time::timeout(
            DEFINITION_READ_TIMEOUT,
            registration::latest_accepted(&self.database, &binding),
        )
        .await
        {
            Ok(Ok(Some(definition))) => definition,
            Ok(Ok(None)) => {
                pool.stats_message = Some("no accepted pool definition".to_owned());
                return pool;
            }
            Ok(Err(error)) => {
                tracing::warn!(pool_name = %binding.pool_name, %error, "could not read resource pool definition");
                pool.stats_message = Some("could not read accepted pool definition".to_owned());
                return pool;
            }
            Err(_) => {
                tracing::warn!(pool_name = %binding.pool_name, "reading accepted resource pool definition timed out");
                pool.stats_message = Some("reading accepted pool definition timed out".to_owned());
                return pool;
            }
        };
        pool.provider_pool_id = definition
            .pool
            .as_ref()
            .map(|identity| identity.pool_id.clone());
        pool.accepted_revision = Some(definition.eligibility_revision.clone());
        let Some(backend) = binding
            .backend_name
            .as_ref()
            .and_then(|name| self.clients.get(name))
        else {
            pool.stats_message = Some("saved gRPC backend is not configured".to_owned());
            return pool;
        };
        let stats = backend
            .stats(v1::StatsRequest {
                pool: definition.pool,
                eligibility_revision: definition.eligibility_revision,
            })
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
