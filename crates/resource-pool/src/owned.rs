// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Find an entity's attached allocation and release it in the entity transaction.

use std::str::FromStr;

use db::resource_pool::ResourcePoolAllocationNotOwned;
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::allocation::AllocationSource;
use model::resource_pool::remote::AllocationResult;
use model::resource_pool::{OwnerType, ResourcePool};
use sqlx::{PgConnection, PgTransaction};
use uuid::Uuid;

use crate::remote::{RemoteResourcePoolError, wire};
use crate::value::value_from_wire;
use crate::{
    IntegratedResourcePoolBackend, ResourcePoolBackend, ResourcePoolBackendError,
    ResourcePoolRegistry, ResourcePoolRegistryError,
};

/// `OwnedAllocation` identifies an attached allocation found under the caller's
/// database locks.
/// Use its value and release it in the same transaction as the lookup. Integrated
/// ownership does not distinguish later allocations to the same owner and value.
/// Remote ownership retains the original request and provider allocation identity.
pub struct OwnedAllocation<'pool, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    T::Err: std::error::Error,
{
    pool: &'pool ResourcePool<T>,
    registry: &'pool ResourcePoolRegistry,
    value: T,
    source: OwnedAllocationSource,
}

/// Records the identity needed to release the allocation that lookup found.
#[derive(Debug)]
enum OwnedAllocationSource {
    Integrated {
        owner_type: OwnerType,
        owner_id: String,
    },
    Remote {
        request_id: Uuid,
        provider_allocation_id: String,
    },
}

impl<T> OwnedAllocation<'_, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    T::Err: std::error::Error,
{
    /// `value` returns the value held by this allocation without changing its identity.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// `release` ends entity ownership in the transaction that found this allocation.
    /// Checks the backend selection before changing ownership. Missing pools,
    /// invalid bindings, and database errors prevent release.
    /// Integrated inventory becomes free; remote allocations save release intent
    /// for recovery to finish after commit. This performs no provider I/O and does
    /// not confirm remote cleanup. The caller must satisfy its entity's drain and
    /// retention checks, and commit or roll back both changes together.
    /// An allocation that no longer permits release returns `NotApplied`.
    pub async fn release(
        self,
        txn: &mut PgTransaction<'_>,
    ) -> Result<ConditionalWrite<(), ResourcePoolAllocationNotOwned>, ResourcePoolBackendError>
    {
        self.registry
            .get_or_load_backend(txn, self.pool.name())
            .await?
            .ok_or_else(|| ResourcePoolRegistryError::MissingPool {
                name: self.pool.name().to_owned(),
            })?;
        match self.source {
            OwnedAllocationSource::Integrated {
                owner_type,
                owner_id,
            } => db::resource_pool::release(self.pool, txn, self.value, owner_type, &owner_id)
                .await
                .map_err(Into::into),
            OwnedAllocationSource::Remote {
                request_id,
                provider_allocation_id,
            } => Ok(
                match db::resource_pool::remote::request_release(
                    txn,
                    request_id,
                    &provider_allocation_id,
                )
                .await?
                {
                    ConditionalWrite::Applied(()) => ConditionalWrite::Applied(()),
                    ConditionalWrite::NotApplied(_) => {
                        ConditionalWrite::NotApplied(ResourcePoolAllocationNotOwned)
                    }
                },
            ),
        }
    }
}

impl ResourcePoolRegistry {
    /// `find_owned` locks an owner's attached allocation in the caller's transaction.
    /// This performs no provider I/O. For a pool absent from the registry, lookup
    /// first locks Integrated inventory. Found ownership defers binding validation
    /// to [`OwnedAllocation::release`], after the caller's entity checks. If no
    /// Integrated ownership exists, lookup checks the saved binding before returning
    /// absence: an unresolved remote binding is an error because empty local
    /// inventory cannot prove the owner has no remote allocation.
    /// Remote lookup also matches `slot` and excludes pending and cleanup records.
    /// Integrated uses pool and owner and works without an allocation record.
    /// Multiple matches and invalid saved values are errors, rather than absence.
    ///
    /// Hold the entity lock and transaction until all dependent changes and any
    /// release finish. The caller commits or rolls back the changes together.
    pub async fn find_owned<'pool, T>(
        &'pool self,
        pool: &'pool ResourcePool<T>,
        txn: &mut PgConnection,
        owner_type: OwnerType,
        owner_id: &str,
        slot: &str,
    ) -> Result<Option<OwnedAllocation<'pool, T>>, ResourcePoolBackendError>
    where
        T: ToString + FromStr + Send + Sync + 'static,
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        let integrated = ResourcePoolBackend::Integrated(IntegratedResourcePoolBackend);
        let selected = self.get(pool.name());
        let backend = selected.map_or(&integrated, |selected| &selected.backend);
        match backend {
            ResourcePoolBackend::Integrated(_) => {
                let Some(value) =
                    db::resource_pool::find_owned_allocation(pool, txn, owner_type, owner_id)
                        .await
                        .map_err(DatabaseError::from)?
                else {
                    if selected.is_none() {
                        self.get_or_load_backend(txn, pool.name()).await?;
                    }
                    return Ok(None);
                };
                Ok(Some(OwnedAllocation {
                    pool,
                    registry: self,
                    value,
                    source: OwnedAllocationSource::Integrated {
                        owner_type,
                        owner_id: owner_id.to_owned(),
                    },
                }))
            }
            ResourcePoolBackend::Grpc(remote) => {
                let Some(allocation) = db::resource_pool::remote::attached_for_owner_slot(
                    txn,
                    pool.name(),
                    owner_type,
                    owner_id,
                    slot,
                )
                .await?
                else {
                    return Ok(None);
                };
                let AllocationSource::Remote {
                    request,
                    result: Some(AllocationResult::Allocated(assignment)),
                    ..
                } = allocation.source
                else {
                    return Err(RemoteResourcePoolError::InvalidRecord(
                        "attached remote allocation has no assignment",
                    )
                    .into());
                };
                if request.authority_id != remote.authority_id() {
                    return Err(RemoteResourcePoolError::InvalidRecord(
                        "attached allocation belongs to a different provider authority",
                    )
                    .into());
                }
                let value =
                    value_from_wire(pool.value_type.into(), &wire::value(&assignment.value)?)?;
                Ok(Some(OwnedAllocation {
                    pool,
                    registry: self,
                    value,
                    source: OwnedAllocationSource::Remote {
                        request_id: allocation.allocation_id,
                        provider_allocation_id: assignment.allocation_id,
                    },
                }))
            }
        }
    }
}
