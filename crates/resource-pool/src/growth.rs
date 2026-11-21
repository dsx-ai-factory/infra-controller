// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Apply explicit additive growth through the pool's saved backend selection.

use std::collections::HashMap;

use db::resource_pool::{DefineResourcePoolError, binding};
use db::{DatabaseError, Transaction, TransactionVending as _};
use model::resource_pool::binding::INTEGRATED_AUTHORITY_ID;
use model::resource_pool::{
    ResourcePoolBackendKind, ResourcePoolDef, ResourcePoolSelection, ValueType,
};
use sqlx::PgPool;

use crate::registration::{self, RegistrationError};
use crate::value::{ResourcePoolEligibility, ValueConversionError};
use crate::{PoolDefinitionSource, ResourcePoolRegistry, ResourcePoolRegistryError};

/// `ResourcePoolGrowthError` preserves native definition errors and selected
/// backend failures so API callers retain their existing error mappings.
#[derive(Debug, thiserror::Error)]
pub enum ResourcePoolGrowthError {
    /// A database operation or growth precondition failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// A saved pool selection is invalid or unavailable.
    #[error(transparent)]
    Registry(#[from] ResourcePoolRegistryError),
    /// The Integrated definition could not be applied.
    #[error(transparent)]
    Definition(#[from] DefineResourcePoolError),
    /// Remote eligibility could not be acknowledged.
    #[error(transparent)]
    Registration(#[from] RegistrationError),
    /// The submitted declaration is invalid.
    #[error(transparent)]
    Value(#[from] ValueConversionError),
}

impl ResourcePoolRegistry {
    /// `grow` adds values using each pool's saved backend. New pools select
    /// Integrated; existing pools retain their authority and value domain.
    /// An all-Integrated batch commits bindings and inventory atomically.
    /// Remote growth supports one existing remote pool per request; mixed and
    /// multiple-remote batches fail without committed changes or provider calls.
    ///
    /// Remote declarations append to acknowledged eligibility without changing
    /// earlier precedence. Identical declarations reuse the accepted revision;
    /// matching pending growth replays the same saved Define. Different pending
    /// input or a competing successor returns the registration conflict.
    /// Native SQL retains ordinary transaction limits. Remote registration has
    /// its shared 30-second budget and never holds this transaction across RPC.
    pub async fn grow(
        &self,
        database: &PgPool,
        pools: &HashMap<String, ResourcePoolDef>,
    ) -> Result<(), ResourcePoolGrowthError> {
        let mut txn = database.txn_begin().await?;
        let mut selections = HashMap::new();
        for (name, definition) in pools {
            if name.trim().is_empty() {
                return Err(DefineResourcePoolError::InvalidArgument(
                    "resource pool name must not be empty".to_owned(),
                )
                .into());
            }
            let stored = binding::load(txn.as_pgconn(), name)
                .await
                .map_err(ResourcePoolRegistryError::from)?;
            let value_domain = ValueType::from(definition.pool_type).into();
            let selection = match stored {
                Some(stored) => ResourcePoolSelection {
                    pool_name: name.clone(),
                    value_domain,
                    backend_name: stored.backend_name,
                    backend_kind: stored.backend_kind,
                    authority_id: stored.authority_id,
                    remote_pool: stored.remote_pool,
                },
                None => ResourcePoolSelection {
                    pool_name: name.clone(),
                    value_domain,
                    backend_name: None,
                    backend_kind: ResourcePoolBackendKind::Integrated,
                    authority_id: INTEGRATED_AUTHORITY_ID.to_owned(),
                    remote_pool: name.clone(),
                },
            };
            selections.insert(name.clone(), selection);
        }
        let remotes = self
            .pools()
            .filter_map(|selected| {
                selected
                    .backend
                    .remote()
                    .map(|remote| (remote.authority_id().to_owned(), remote.clone()))
            })
            .collect();
        let resolved = Self::resolve_with_remotes(
            PoolDefinitionSource::GrowthRequest,
            txn.as_pgconn(),
            &selections,
            &remotes,
        )
        .await?;
        // Dispatch from the binding we resolved, not an earlier READ COMMITTED
        // lookup that may have run before another caller created the binding.
        resolved.apply_growth(database, pools, txn).await
    }

    /// `grow_resolved` adds values to pools whose bindings are already committed
    /// and resolved in this registry, without scanning unrelated pools.
    /// Empty input does no database work. Missing pools, empty names, and changes
    /// to a binding's value domain fail before inventory changes.
    /// Integrated batches, remote single-pool requests, and registration retries
    /// follow the same transaction and replay rules as [`Self::grow`].
    pub async fn grow_resolved(
        &self,
        database: &PgPool,
        pools: &HashMap<String, ResourcePoolDef>,
    ) -> Result<(), ResourcePoolGrowthError> {
        if pools.is_empty() {
            return Ok(());
        }
        for (name, definition) in pools {
            if name.trim().is_empty() {
                return Err(DefineResourcePoolError::InvalidArgument(
                    "resource pool name must not be empty".to_owned(),
                )
                .into());
            }
            let selected = self
                .get(name)
                .ok_or_else(|| ResourcePoolRegistryError::MissingPool { name: name.clone() })?;
            let value_domain = ValueType::from(definition.pool_type).into();
            if value_domain != selected.binding.value_domain {
                return Err(ResourcePoolRegistryError::ValueDomainConflict {
                    name: name.clone(),
                    stored: selected.binding.value_domain,
                    configured: value_domain,
                }
                .into());
            }
        }
        let txn = database.txn_begin().await?;
        self.apply_growth(database, pools, txn).await
    }

    async fn apply_growth(
        &self,
        database: &PgPool,
        pools: &HashMap<String, ResourcePoolDef>,
        mut txn: Transaction<'_>,
    ) -> Result<(), ResourcePoolGrowthError> {
        let remote_pool = pools.keys().find(|name| {
            self.get(name)
                .is_some_and(|selected| selected.backend.remote().is_some())
        });
        if remote_pool.is_some() && pools.len() != 1 {
            return Err(DatabaseError::FailedPrecondition(
                "remote resource pool growth requires a single-pool request".to_owned(),
            )
            .into());
        }
        let Some(name) = remote_pool else {
            db::resource_pool::define_all_from(txn.as_pgconn(), pools).await?;
            txn.commit().await?;
            return Ok(());
        };
        // Registration owns its binding lock and saved request. Release this
        // preflight transaction before reading eligibility or contacting a provider.
        txn.rollback().await?;
        let selected = self
            .get(name)
            .ok_or_else(|| ResourcePoolRegistryError::MissingPool { name: name.clone() })?;
        let remote = selected.backend.remote().ok_or_else(|| {
            ResourcePoolRegistryError::UnsupportedAuthority { name: name.clone() }
        })?;
        let accepted = registration::latest_accepted(database, &selected.binding)
            .await?
            .ok_or_else(|| {
                DatabaseError::FailedPrecondition(format!(
                    "resource pool `{name}` has no accepted definition for growth",
                ))
            })?;
        let previous_revision = accepted
            .eligibility_revision
            .parse()
            .map_err(|_| RegistrationError::Invalid("accepted revision is invalid"))?;
        let delta = ResourcePoolEligibility::try_from(pools[name].clone())?;
        let mut eligibility = ResourcePoolEligibility {
            domain: selected.binding.value_domain,
            entries: accepted.eligibility,
        };
        for entry in delta.entries {
            if !eligibility.entries.contains(&entry) {
                eligibility.entries.push(entry);
            }
        }
        registration::define(
            database,
            remote.provider(),
            &selected.binding,
            Some(previous_revision),
            &eligibility,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use crate::ResourcePoolRegistry;

    #[tokio::test]
    async fn empty_resolved_growth_does_not_access_database() {
        let database = PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new());
        database.close().await;
        ResourcePoolRegistry::default()
            .grow_resolved(&database, &HashMap::new())
            .await
            .expect("empty growth does not need a database connection");
    }
}
