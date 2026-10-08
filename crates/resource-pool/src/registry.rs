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

use db::DatabaseError;
use db::resource_pool::binding::{self, BindingError};
use model::resource_pool::binding::{
    INTEGRATED_AUTHORITY_ID, ResourcePoolBackendKind, ResourcePoolBinding, ResourcePoolSelection,
    ResourcePoolValueDomain,
};
use sqlx::PgConnection;

use crate::{IntegratedResourcePoolBackend, ResourcePoolBackend};

/// `ResolvedResourcePool` pairs a pool's saved binding with its allocation backend.
pub struct ResolvedResourcePool {
    /// The pool's saved backend selection and stable binding UUID.
    pub binding: ResourcePoolBinding,
    /// The backend selected by the saved binding.
    pub backend: ResourcePoolBackend,
}

/// `ResourcePoolRegistry` resolves each pool to its stored backend association.
///
/// Every pool has its own binding. Named Integrated aliases use the same local
/// database, and removing a pool from configuration does not remove its saved binding.
#[derive(Default)]
pub struct ResourcePoolRegistry {
    pools: HashMap<String, ResolvedResourcePool>,
}

/// `PoolDefinitionSource` tells the registry why it is resolving pool definitions.
#[derive(Clone, Copy)]
pub enum PoolDefinitionSource {
    /// Load seed configuration using each existing pool's stored value family.
    /// The caller's reconciliation reports differences from the saved definition.
    /// Pools with only a binding reject changes to their value family.
    SeedConfiguration,
    /// Check an explicit grow request; existing pools must keep their stored
    /// value family.
    GrowthRequest,
    /// Register pools already in the database and retain saved bindings,
    /// leaving pools that exist only in configuration unbound.
    ListenOnly,
}

/// `ResourcePoolRegistryError` reports failures to load or select a pool's backend.
#[derive(Debug, thiserror::Error)]
pub enum ResourcePoolRegistryError {
    /// Reading, saving, or checking a pool's binding or stored values failed.
    #[error(transparent)]
    Binding(#[from] BindingError),
    /// The database could not load the saved bindings.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// v2.4 supports only the database-local Integrated authority.
    #[error(
        "pool {name:?} selects an unsupported allocation authority; v2.4 requires integrated in the local database"
    )]
    UnsupportedAuthority {
        /// Pool whose selected allocator is unsupported.
        name: String,
    },
    /// Integrated allocations use the logical pool name in SQL.
    #[error("integrated pool {name:?} cannot use a different remote_pool")]
    IntegratedAlias {
        /// Pool whose remote name differs from its NICo name.
        name: String,
    },
    /// The requested value family differs from the pool's stored values or definition.
    #[error(
        "pool {name:?} has stored value domain {stored} but configured value domain {configured}"
    )]
    ValueDomainConflict {
        /// Pool whose requested value family differs from the stored one.
        name: String,
        /// Value family from the pool's stored values or definition.
        stored: ResourcePoolValueDomain,
        /// Value family requested by configuration or growth.
        configured: ResourcePoolValueDomain,
    },
    /// The map key must identify the same logical pool as its selection.
    #[error("pool selection key {name:?} does not match its logical name")]
    NameMismatch {
        /// The key used to submit the selection.
        name: String,
    },
}

impl ResourcePoolRegistry {
    /// `get` looks up a pool's backend and saved binding.
    ///
    /// `None` means this registry has no binding for the name.
    pub fn get(&self, name: &str) -> Option<&ResolvedResourcePool> {
        self.pools.get(name)
    }

    /// `resolve` checks each pool's backend selection and saves new bindings in
    /// the caller's transaction.
    ///
    /// Pools with resource rows or a saved definition get an Integrated binding
    /// if they do not have one. Their values and allocations stay unchanged.
    /// Saved bindings keep the same allocator even after a pool becomes empty
    /// or is removed from configuration.
    ///
    /// All selections and saved bindings are checked before the first insert;
    /// the caller commits or rolls back the batch. No provider connection is
    /// opened. Unsupported saved backends are rejected before reading pool values.
    ///
    /// `SeedConfiguration` uses the value family from stored values, or the saved
    /// definition when there are no values. The caller's reconciliation reports
    /// configuration differences. `GrowthRequest` must match that stored family.
    /// `ListenOnly` registers existing pools and retains saved bindings, but skips
    /// pools that exist only in configuration. In every mode, a pool with only a
    /// binding rejects changes to its value family. IPv4 and IPv6 share one family,
    /// so adding either to an address pool keeps the same binding.
    pub async fn resolve(
        source: PoolDefinitionSource,
        txn: &mut PgConnection,
        configured: &HashMap<String, ResourcePoolSelection>,
    ) -> Result<Self, ResourcePoolRegistryError> {
        for (name, selection) in configured {
            if *name != selection.pool_name {
                return Err(ResourcePoolRegistryError::NameMismatch { name: name.clone() });
            }
            validate_integrated(
                name,
                selection.backend_kind,
                &selection.authority_id,
                &selection.remote_pool,
            )?;
        }

        let bindings = binding::all(&mut *txn).await?;
        for stored in &bindings {
            validate_integrated(
                &stored.pool_name,
                stored.backend_kind,
                &stored.authority_id,
                &stored.remote_pool,
            )?;
        }

        let existing_domains = binding::existing_pool_domains(&mut *txn).await?;
        let mut selections = configured.clone();
        for (name, &value_domain) in &existing_domains {
            if let Some(selection) = selections.get_mut(name) {
                if matches!(source, PoolDefinitionSource::GrowthRequest)
                    && selection.value_domain != value_domain
                {
                    return Err(ResourcePoolRegistryError::ValueDomainConflict {
                        name: name.clone(),
                        stored: value_domain,
                        configured: selection.value_domain,
                    });
                }
                selection.value_domain = value_domain;
            } else {
                selections.insert(
                    name.clone(),
                    ResourcePoolSelection {
                        remote_pool: name.clone(),
                        pool_name: name.clone(),
                        value_domain,
                        backend_name: None,
                        backend_kind: ResourcePoolBackendKind::Integrated,
                        authority_id: INTEGRATED_AUTHORITY_ID.to_owned(),
                    },
                );
            }
        }

        let mut pools = bindings
            .into_iter()
            .map(|binding| {
                if let Some(selection) = selections.remove(&binding.pool_name)
                    && !binding.matches(&selection)
                {
                    return Err(BindingError::Conflict {
                        name: binding.pool_name.clone(),
                        stored: Box::new(binding),
                        requested: Box::new(selection),
                    });
                }
                Ok((
                    binding.pool_name.clone(),
                    ResolvedResourcePool {
                        binding,
                        backend: ResourcePoolBackend::Integrated(IntegratedResourcePoolBackend),
                    },
                ))
            })
            .collect::<Result<HashMap<_, _>, BindingError>>()?;

        // Insert pools in name order so instances registering the same pools
        // do not deadlock by locking them in opposite orders.
        let mut pending_selections: Vec<_> = selections.into_values().collect();
        pending_selections.sort_unstable_by(|left, right| left.pool_name.cmp(&right.pool_name));
        for selection in pending_selections {
            if matches!(source, PoolDefinitionSource::ListenOnly)
                && !existing_domains.contains_key(&selection.pool_name)
            {
                continue;
            }
            let binding = binding::bind_or_verify(txn, &selection).await?;
            pools.insert(
                binding.pool_name.clone(),
                ResolvedResourcePool {
                    binding,
                    backend: ResourcePoolBackend::Integrated(IntegratedResourcePoolBackend),
                },
            );
        }
        Ok(Self { pools })
    }
}

fn validate_integrated(
    name: &str,
    backend_kind: ResourcePoolBackendKind,
    authority_id: &str,
    remote_pool: &str,
) -> Result<(), ResourcePoolRegistryError> {
    if backend_kind != ResourcePoolBackendKind::Integrated
        || authority_id != INTEGRATED_AUTHORITY_ID
    {
        return Err(ResourcePoolRegistryError::UnsupportedAuthority {
            name: name.to_owned(),
        });
    }
    if remote_pool != name {
        return Err(ResourcePoolRegistryError::IntegratedAlias {
            name: name.to_owned(),
        });
    }
    Ok(())
}
