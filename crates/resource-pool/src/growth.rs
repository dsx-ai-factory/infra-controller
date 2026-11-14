// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Apply explicit additive growth through the pool's saved backend selection.

use std::collections::HashMap;

use db::resource_pool::{DefineResourcePoolError, binding};
use db::{DatabaseError, Transaction};
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
    /// multiple-remote batches fail before any writes or provider calls.
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
        let mut txn = Transaction::begin(database).await?;
        let mut selections = HashMap::new();
        let mut remote_pool = None;
        for (name, definition) in pools {
            if name.trim().is_empty() {
                return Err(DefineResourcePoolError::InvalidArgument(
                    "resource pool name must not be empty".to_owned(),
                )
                .into());
            }
            let backend = self.get_or_load_backend(txn.as_pgconn(), name).await?;
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
            if backend.and_then(|backend| backend.remote()).is_some() {
                if pools.len() != 1 {
                    return Err(DatabaseError::FailedPrecondition(
                        "remote resource pool growth requires a single-pool request".to_owned(),
                    )
                    .into());
                }
                remote_pool = Some(name);
            }
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
        let Some(name) = remote_pool else {
            db::resource_pool::define_all_from(txn.as_pgconn(), pools).await?;
            txn.commit().await?;
            return Ok(());
        };
        // Registration owns its binding lock and saved request. Release this
        // preflight transaction before reading eligibility or contacting a provider.
        txn.rollback().await?;
        let selected = resolved
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
