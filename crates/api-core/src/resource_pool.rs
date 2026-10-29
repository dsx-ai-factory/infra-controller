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

//! Resource pool startup and observations using shared provider connections.

pub(crate) mod metrics;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use ::rpc::forge as rpc;
use carbide_resource_pool::grpc::{GrpcResourcePoolBackend, GrpcResourcePoolError};
use carbide_resource_pool::remote::{RemoteResourcePool, start_cleanup_metrics};
use carbide_resource_pool::value::ResourcePoolEligibility;
use carbide_resource_pool::{PoolDefinitionSource, ResourcePoolRegistry, registration};
use carbide_resource_pool_api::v1;
use chrono::{DateTime, Utc};
use db::work_lock_manager::WorkLockManagerHandle;
use eyre::WrapErr;
use futures::{Stream, StreamExt, stream};
use model::resource_pool::binding::{
    INTEGRATED_AUTHORITY_ID, ResourcePoolBackendKind, ResourcePoolBinding, ResourcePoolSelection,
};
use opentelemetry::metrics::Meter;
use sqlx::{PgConnection, PgPool};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::CarbideResult;
use crate::cfg::resource_pool::ResourcePoolBackendConfig;

// Each pass observes at most eight remote pools at once. Each shared client
// allows 32 calls across all users, including concurrent admin requests.
const MAX_CONCURRENT_POOL_OBSERVATIONS: usize = 8;

// This read budget includes connection acquisition. Provider calls separately
// use the shared client's ten-second deadline.
const DEFINITION_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// `ResourcePoolMonitor` resolves the registry, registers remote pools, and
/// starts recovery and cleanup sampling. It also reads local inventory and
/// remote statistics for admin inspection and metrics, reusing the same
/// provider connections.
#[derive(Clone)]
pub(crate) struct ResourcePoolMonitor {
    database: PgPool,
    /// Configured connections, keyed by the backend alias used in TOML.
    clients: HashMap<String, GrpcResourcePoolBackend>,
    /// Resolved connections, keyed by provider authority so observations keep
    /// working when a saved binding still names an old configuration alias.
    authority_clients: HashMap<String, GrpcResourcePoolBackend>,
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
        Ok(Self {
            database,
            clients,
            authority_clients: HashMap::new(),
        })
    }

    /// `resolve_registry` uses database state to select one shared coordinator
    /// per provider authority, without contacting providers. The caller commits
    /// bindings and seed definitions before calling `register_pools`.
    pub(crate) async fn resolve_registry(
        &mut self,
        source: PoolDefinitionSource,
        txn: &mut PgConnection,
        configured: &HashMap<String, ResourcePoolSelection>,
        work_locks: &WorkLockManagerHandle,
    ) -> eyre::Result<ResourcePoolRegistry> {
        let mut remotes = HashMap::new();
        let bindings = db::resource_pool::binding::all(&mut *txn).await?;
        // Prefer the current alias for an authority. The saved binding keeps
        // its original alias even when configuration renames that connection.
        // Logical pool order makes the choice stable when aliases share an authority.
        let configured_pools: BTreeMap<_, _> = configured.iter().collect();
        let providers = configured_pools
            .values()
            .map(|pool| {
                (
                    &pool.pool_name,
                    pool.backend_kind,
                    &pool.authority_id,
                    pool.backend_name.as_ref(),
                )
            })
            .chain(bindings.iter().map(|pool| {
                (
                    &pool.pool_name,
                    pool.backend_kind,
                    &pool.authority_id,
                    pool.backend_name.as_ref(),
                )
            }));
        for (pool_name, kind, authority_id, backend_name) in providers {
            if kind != ResourcePoolBackendKind::Grpc || remotes.contains_key(authority_id) {
                continue;
            }
            let client = backend_name
                .and_then(|name| self.clients.get(name))
                .ok_or_else(|| {
                    eyre::eyre!(
                        "resource pool `{pool_name}` has no configured gRPC backend matching alias {backend_name:?} for authority `{authority_id}`"
                    )
                })?;
            remotes.insert(
                authority_id.clone(),
                RemoteResourcePool::new(
                    self.database.clone(),
                    work_locks.clone(),
                    authority_id.clone(),
                    client.clone(),
                ),
            );
        }
        let registry =
            ResourcePoolRegistry::resolve_with_remotes(source, txn, configured, &remotes).await?;
        self.use_registry_clients(&registry);
        Ok(registry)
    }

    /// `use_registry_clients` shares the resolved provider connections with
    /// status reads, including when a saved binding retains an old backend alias.
    pub(crate) fn use_registry_clients(&mut self, registry: &ResourcePoolRegistry) {
        self.authority_clients = registry
            .pools()
            .filter_map(|selected| selected.backend.remote())
            .map(|remote| (remote.authority_id().to_owned(), remote.provider().clone()))
            .collect();
    }

    /// `register_pools` finishes saved registrations before pool consumers start.
    /// New registrations use the stored seed, so restarting with edited TOML
    /// cannot grow a pool. Listen-only replicas require an accepted definition
    /// without submitting or acknowledging one. Statistics are not readiness.
    pub(crate) async fn register_pools(
        &self,
        registry: &ResourcePoolRegistry,
        source: PoolDefinitionSource,
        cancel: &CancellationToken,
    ) -> eyre::Result<()> {
        for selected in registry.pools() {
            let Some(remote) = selected.backend.remote() else {
                continue;
            };
            let result = cancel
                .run_until_cancelled(self.register_pool(
                    remote.provider(),
                    &selected.binding,
                    source,
                ))
                .await
                .unwrap_or_else(|| Err(eyre::eyre!("resource pool registration cancelled")));
            result.wrap_err_with(|| {
                format!(
                    "registering resource pool `{}` (binding {})",
                    selected.binding.pool_name, selected.binding.binding_id,
                )
            })?;
        }
        Ok(())
    }

    /// `register_pool` resumes the saved request, or defines a pool from its
    /// original seed. Accepted revisions still need a current protocol check.
    async fn register_pool(
        &self,
        backend: &GrpcResourcePoolBackend,
        binding: &ResourcePoolBinding,
        source: PoolDefinitionSource,
    ) -> eyre::Result<()> {
        let latest = tokio::time::timeout(DEFINITION_READ_TIMEOUT, async {
            if matches!(source, PoolDefinitionSource::ListenOnly) {
                db::resource_pool::registration::latest_accepted(&self.database, binding.binding_id)
                    .await
            } else {
                db::resource_pool::registration::latest(&self.database, binding.binding_id).await
            }
        })
        .await??;
        let Some(latest) = latest else {
            eyre::ensure!(
                !matches!(source, PoolDefinitionSource::ListenOnly),
                "resource pool `{}` has no accepted definition",
                binding.pool_name,
            );
            let seed = tokio::time::timeout(
                DEFINITION_READ_TIMEOUT,
                db::resource_pool::stored_def(&self.database, &binding.pool_name),
            )
            .await??
            .ok_or_else(|| {
                eyre::eyre!(
                    "resource pool `{}` has no seed definition",
                    binding.pool_name,
                )
            })?;
            // `define` checks Describe before saving this pool's first request.
            registration::define(
                &self.database,
                backend,
                binding,
                None,
                &ResourcePoolEligibility::try_from(seed)?,
            )
            .await?;
            return Ok(());
        };
        let definition =
            registration::resume(&self.database, backend, binding, latest.revision).await?;
        // A saved acknowledgement does not contact the provider running now.
        // Its alias must still identify the saved authority, pool ID, and value domain.
        let description = Box::pin(backend.describe(v1::DescribeRequest {
            pool_alias: binding.remote_pool.clone(),
        }))
        .await?;
        let pool = definition
            .pool
            .as_ref()
            .ok_or_else(|| eyre::eyre!("resource pool definition has no provider identity"))?;
        eyre::ensure!(
            description.authority_id == pool.authority_id
                && description.pool_id == pool.pool_id
                && description.value_domain == definition.value_domain,
            "resource pool `{}` provider differs from its accepted definition",
            binding.pool_name,
        );
        Ok(())
    }

    /// `start_recovery` joins provider recovery and one cleanup sampler to the
    /// API's task set. Start before consumers allocate so saved attempts can be
    /// reconciled during startup; cancellation and joining belong to the API.
    pub(crate) fn start_recovery(
        &self,
        registry: &ResourcePoolRegistry,
        meter: Meter,
        tasks: &mut JoinSet<()>,
        cancel: CancellationToken,
    ) -> std::io::Result<()> {
        let mut started = HashSet::new();
        for selected in registry.pools() {
            if let Some(remote) = selected.backend.remote()
                && started.insert(remote.authority_id())
            {
                remote.clone().start(tasks, cancel.clone())?;
            }
        }
        start_cleanup_metrics(self.database.clone(), meter, tasks, cancel)
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
        let Some(backend) = self
            .authority_clients
            .get(&binding.authority_id)
            .or_else(|| {
                binding
                    .backend_name
                    .as_ref()
                    .and_then(|name| self.clients.get(name))
            })
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
