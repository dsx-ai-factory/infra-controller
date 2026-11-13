// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Find an entity's attached allocation and release it in the entity transaction.

use std::str::FromStr;

use db::resource_pool::ResourcePoolAllocationNotOwned;
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::allocation::{AllocationSource, AllocationState};
use model::resource_pool::remote::AllocationResult;
use model::resource_pool::{OwnerType, ResourcePool};
use sqlx::{PgConnection, PgTransaction};
use uuid::Uuid;

use crate::remote::{RemoteResourcePoolError, wire};
use crate::value::value_from_wire;
use crate::{
    AllocationCleanup, IntegratedResourcePoolBackend, ResourcePoolBackend,
    ResourcePoolBackendError, ResourcePoolRegistry, ResourcePoolRegistryError,
};

/// `AllocationCleanupStatus` reports cleanup still outstanding for an owner slot,
/// including retained assignments from older generations or another pool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AllocationCleanupStatus {
    /// An unused attempt is awaiting confirmed cancellation.
    pub cancellation_requested: bool,
    /// A previously attached assignment is awaiting confirmed release.
    pub release_requested: bool,
}

impl AllocationCleanupStatus {
    /// `is_pending` reports whether either kind of cleanup needs confirmation.
    pub fn is_pending(&self) -> bool {
        self.cancellation_requested || self.release_requested
    }
}

/// `AllocationStatus` describes unfinished work for one pool, owner and slot.
/// Only attached assignments and their releases expose an owned value. Pending
/// reservations and cancellation results are handled by the shared recovery path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocationStatus<T> {
    /// An allocation attempt has not attached to its entity.
    Pending,
    /// The entity owns a value, including an assignment retained for later reuse.
    Attached {
        /// The value held by this pool for the owner.
        value: T,
    },
    /// An unused attempt is awaiting confirmed cancellation.
    CancelRequested,
    /// A previously attached assignment is awaiting confirmed release.
    ReleaseRequested {
        /// The original value that may still exist at the provider.
        value: T,
    },
}

impl<T> AllocationStatus<T> {
    /// `value` returns the attached value or original value awaiting release.
    /// Unattached attempts do not establish ownership, even if a reply is saved.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Attached { value } | Self::ReleaseRequested { value } => Some(value),
            Self::Pending | Self::CancelRequested => None,
        }
    }

    /// `is_cleanup_pending` reports cancellation or release awaiting confirmation.
    pub fn is_cleanup_pending(&self) -> bool {
        matches!(self, Self::CancelRequested | Self::ReleaseRequested { .. })
    }
}

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
    cleanup: AllocationCleanup,
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

    /// `cleanup` captures a handle whose `wait` confirms this exact assignment,
    /// even after another generation uses the owner slot. Integrated waits return
    /// immediately. Use `wait` to confirm release; `is_complete`
    /// checks the latest generation and can reject an older release
    /// handle even after its cleanup is complete.
    /// Commit [`Self::release`] before waiting through the returned handle.
    pub fn cleanup(&self) -> AllocationCleanup {
        self.cleanup.clone()
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
    /// `cleanup_status` locks unfinished remote generations for this owner slot
    /// and reports cancellation and release separately. It includes every pool,
    /// so a newer attached assignment cannot hide an older release. Integrated
    /// release completes in SQL and adds no outstanding cleanup.
    /// Hold the entity lock through this check and dependent writes. This performs
    /// no provider I/O; database and errors decoding saved records remain errors.
    pub async fn cleanup_status(
        &self,
        txn: &mut PgConnection,
        owner_type: OwnerType,
        owner_id: &str,
        slot: &str,
    ) -> Result<AllocationCleanupStatus, ResourcePoolBackendError> {
        let records =
            db::resource_pool::remote::active_for_owner_slot(txn, owner_type, owner_id, slot)
                .await?;
        Ok(AllocationCleanupStatus {
            cancellation_requested: records
                .iter()
                .any(|record| record.state == AllocationState::CancelRequested),
            release_requested: records
                .iter()
                .any(|record| record.state == AllocationState::ReleaseRequested),
        })
    }

    /// `inspect_allocations` returns unfinished allocation state for this typed
    /// pool and owner. Confirmed cancellation and release are omitted. Integrated
    /// returns its current attached inventory value, even without an allocation record. Its `slot` argument is ignored.
    /// Remote values are checked against the selected authority and Rust type;
    /// invalid bindings, malformed assignments and duplicate attachments fail.
    /// An absent pool returns an empty list. This performs no provider I/O and
    /// does not allocate or request cleanup.
    ///
    /// Hold the entity lock through inspection and dependent writes. Unfinished
    /// remote generations are locked before inspecting this pool's ownership,
    /// so inspections of several pools take those locks in the same order.
    /// Ordering of returned states is not a public contract.
    pub async fn inspect_allocations<T>(
        &self,
        pool: &ResourcePool<T>,
        txn: &mut PgConnection,
        owner_type: OwnerType,
        owner_id: &str,
        slot: &str,
    ) -> Result<Vec<AllocationStatus<T>>, ResourcePoolBackendError>
    where
        T: ToString + FromStr + Send + Sync + 'static,
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        let records =
            db::resource_pool::remote::active_for_owner_slot(txn, owner_type, owner_id, slot)
                .await?;
        let Some(backend) = self.get_or_load_backend(txn, pool.name()).await? else {
            return Ok(Vec::new());
        };
        let Some(remote) = backend.remote() else {
            return Ok(
                db::resource_pool::find_owned_allocation(pool, txn, owner_type, owner_id)
                    .await
                    .map_err(DatabaseError::from)?
                    .map(|value| AllocationStatus::Attached { value })
                    .into_iter()
                    .collect(),
            );
        };
        let mut states = Vec::new();
        let mut attached = false;
        for record in records
            .into_iter()
            .filter(|record| record.pool_name == pool.name())
        {
            let AllocationSource::Remote {
                request, result, ..
            } = record.source
            else {
                return Err(RemoteResourcePoolError::InvalidRecord(
                    "remote inspection found an Integrated allocation",
                )
                .into());
            };
            if request.authority_id != remote.authority_id() {
                return Err(RemoteResourcePoolError::InvalidRecord(
                    "allocation belongs to a different provider authority",
                )
                .into());
            }
            let state = match record.state {
                AllocationState::Pending => AllocationStatus::Pending,
                AllocationState::CancelRequested => AllocationStatus::CancelRequested,
                AllocationState::Attached | AllocationState::ReleaseRequested => {
                    let Some(AllocationResult::Allocated(assignment)) = result else {
                        return Err(RemoteResourcePoolError::InvalidRecord(
                            "owned allocation has no original assignment",
                        )
                        .into());
                    };
                    let value =
                        value_from_wire(pool.value_type.into(), &wire::value(&assignment.value)?)?;
                    if record.state == AllocationState::ReleaseRequested {
                        AllocationStatus::ReleaseRequested { value }
                    } else {
                        if attached {
                            return Err(RemoteResourcePoolError::InvalidRecord(
                                "pool has multiple attached allocations for one owner slot",
                            )
                            .into());
                        }
                        attached = true;
                        AllocationStatus::Attached { value }
                    }
                }
                AllocationState::Cancelled | AllocationState::Released => {
                    return Err(RemoteResourcePoolError::InvalidRecord(
                        "unfinished allocation query returned completed cleanup",
                    )
                    .into());
                }
            };
            states.push(state);
        }
        Ok(states)
    }

    /// `find_owned` locks an owner's attached allocation in the caller's transaction.
    /// This performs no provider I/O. For a pool absent from the registry, lookup
    /// locks Integrated inventory. Found ownership defers binding validation
    /// to [`OwnedAllocation::release`], after the caller's entity checks. If no
    /// Integrated ownership exists, lookup checks the saved binding before returning
    /// absence: an unresolved remote binding is an error because empty local
    /// inventory cannot prove the owner has no remote allocation.
    /// Remote lookup also matches `slot` and excludes pending and cleanup records.
    /// Integrated uses pool and owner and works without an allocation record.
    /// Multiple matches and invalid saved values are errors, rather than absence.
    ///
    /// Hold the entity lock and transaction until all dependent changes and any
    /// release finish. Unfinished remote generations for the owner slot are locked
    /// first, so lookup can wait for an attachment or cleanup in another pool.
    /// The caller commits or rolls back the changes together.
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
        // A retained assignment can belong to another pool. Lock unfinished
        // remote generations before inventory, matching inspection's order.
        db::resource_pool::remote::active_for_owner_slot(txn, owner_type, owner_id, slot).await?;
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
                    cleanup: AllocationCleanup::for_request(owner_type, owner_id, slot, None),
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
                    cleanup: AllocationCleanup::for_request(
                        owner_type,
                        owner_id,
                        slot,
                        Some(allocation.allocation_id),
                    ),
                }))
            }
        }
    }
}
