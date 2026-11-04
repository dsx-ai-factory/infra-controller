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
//! Allocate and release typed values through each pool's selected backend.
//!
//! [`ResourcePoolRegistry::select`] looks up a typed pool ready for use. Call its
//! [`prepare_allocation`](ResourcePoolWithBackend::prepare_allocation) before
//! starting the entity transaction, then [`Allocation::allocate`] in that
//! transaction and save the entity. If creation fails, roll back and call
//! [`Allocation::cancel`]. The allocation owns its saved request identity and
//! result.
//!
//! Controllers that save creation IDs with allocation requests use
//! [`ResourcePoolWithBackend::prepare_request`] in their preparation transaction instead.
//! After committing, [`PreparedAllocationRequest::resolve`] returns the same allocation.
//! Integrated reservation stays in the entity transaction; remote requests retain
//! their identity so recovery can finish cancellation or release after a restart.
//! Before forgetting creation IDs, capture cleanup with
//! [`ResourcePoolRegistry::request_cancellation`] under the creation lock, commit,
//! then call [`AllocationCleanup::wait`]. Reacquire that lock to recheck the
//! consumer's current identity and desire and [`AllocationCleanup::is_complete`].
//!
//! [`ResourcePoolRegistry`] saves and checks backend selections. API startup owns
//! remote recovery and cleanup metrics. Production configuration rejects
//! remote selection; tests exercise the shared API below that gate.
//! Shared allocation supports automatic and requested manual values. Exact
//! selection across both partitions remains on the Integrated adapter.

#![cfg_attr(test, allow(txn_held_across_await, txn_without_commit))]

#[cfg(test)]
mod registry_tests;
#[cfg(test)]
mod tests;

mod allocation;
mod cleanup;
pub mod grpc;
mod owned;
pub mod registration;
mod registry;
pub mod remote;
mod transaction;
pub mod value;

use std::str::FromStr;

pub use allocation::{Allocation, PreparedAllocationRequest};
pub use cleanup::AllocationCleanup;
use db::resource_pool::{ResourcePoolAllocationNotOwned, ResourcePoolDatabaseError};
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::{OwnerType, ResourcePool};
pub use owned::OwnedAllocation;
use registration::RegistrationError;
pub use registry::{
    PoolDefinitionSource, ResolvedResourcePool, ResourcePoolRegistry, ResourcePoolRegistryError,
};
use remote::{RemoteResourcePool, RemoteResourcePoolError};
use sqlx::PgConnection;
pub use transaction::database_transaction;
use value::ValueConversionError;

/// `ResourcePoolBackend` selects the allocator for a typed pool.
/// Use [`ResourcePoolWithBackend::prepare_allocation`] for allocations through either
/// backend. The Integrated adapter also supports existing transaction-only
/// operations; runtime setup and cleanup can access the remote coordinator with
/// [`Self::remote`].
#[derive(Clone)]
pub enum ResourcePoolBackend {
    /// Use the existing SQL allocator in the caller's entity transaction.
    Integrated(IntegratedResourcePoolBackend),
    /// Coordinate provider calls separately from the entity transaction.
    Grpc(RemoteResourcePool),
}

/// `ResourcePoolWithBackend` borrows a typed pool and its selected allocator.
/// [`ResourcePoolRegistry::select`] returns this handle. Call
/// [`Self::prepare_allocation`] before starting the entity transaction, then
/// allocate and save the entity together. Controllers can use [`Self::prepare_request`]
/// to save an allocation request alongside their own creation record.
pub struct ResourcePoolWithBackend<'a, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    <T as FromStr>::Err: std::error::Error,
{
    pool: &'a ResourcePool<T>,
    backend: &'a ResourcePoolBackend,
}

/// `IntegratedResourcePoolBackend` represents the SQL allocator in
/// [`ResourcePoolBackend::Integrated`].
///
/// Its transaction-only methods forward to the existing SQL allocator. The
/// caller supplies the connection and owns commit or rollback.
#[derive(Clone)]
pub struct IntegratedResourcePoolBackend;

impl ResourcePoolBackend {
    /// `remote` returns the coordinator for calls made outside an entity transaction.
    /// `None` means allocation and release belong in the Integrated transaction.
    pub fn remote(&self) -> Option<&RemoteResourcePool> {
        match self {
            Self::Integrated(_) => None,
            Self::Grpc(remote) => Some(remote),
        }
    }

    /// `require_integrated` selects the SQL adapter for operations whose caller
    /// still depends on Integrated inventory. Remote selection fails before I/O.
    pub fn require_integrated(&self) -> Result<&IntegratedResourcePoolBackend, DatabaseError> {
        match self {
            Self::Integrated(integrated) => Ok(integrated),
            Self::Grpc(_) => Err(DatabaseError::FailedPrecondition(
                "operation requires an integrated resource pool".to_owned(),
            )),
        }
    }
}

impl IntegratedResourcePoolBackend {
    /// `allocate` reserves a value in the caller's transaction.
    /// `None` chooses free automatic inventory; `Some(value)` requests that value
    /// from the manual partition. SQL errors and their precedence are preserved.
    pub async fn allocate<T>(
        &self,
        pool: &ResourcePool<T>,
        txn: &mut PgConnection,
        owner_type: OwnerType,
        owner_id: &str,
        requested_value: Option<T>,
    ) -> Result<T, ResourcePoolDatabaseError>
    where
        T: ToString + FromStr + Send + Sync + 'static,
        T::Err: std::error::Error,
    {
        db::resource_pool::allocate(pool, txn, owner_type, owner_id, requested_value).await
    }

    /// `allocate_exact` reserves a value from either assignment partition.
    /// Missing or already allocated values return `FailedPrecondition`, even
    /// when the allocation belongs to the same owner. Assignment mode is preserved.
    pub async fn allocate_exact<T>(
        &self,
        pool: &ResourcePool<T>,
        txn: &mut PgConnection,
        owner_type: OwnerType,
        owner_id: &str,
        requested_value: T,
    ) -> Result<T, DatabaseError>
    where
        T: ToString + FromStr + Send + Sync + 'static,
        T::Err: std::error::Error,
    {
        db::resource_pool::allocate_exact(pool, txn, owner_type, owner_id, requested_value).await
    }

    /// `release` frees a value only when its owner type and ID match.
    /// Missing, free, or differently owned values return `NotApplied`. Ownership
    /// does not identify a generation; callers must exclude stale cleanup that
    /// could reach a later allocation for the same owner.
    pub async fn release<T>(
        &self,
        pool: &ResourcePool<T>,
        txn: &mut PgConnection,
        value: T,
        owner_type: OwnerType,
        owner_id: &str,
    ) -> Result<ConditionalWrite<(), ResourcePoolAllocationNotOwned>, DatabaseError>
    where
        T: ToString + FromStr + Send + Sync + 'static,
        T::Err: std::error::Error,
    {
        db::resource_pool::release(pool, txn, value, owner_type, owner_id).await
    }
}

/// `ResourcePoolBackendError` preserves failures from the selected allocator
/// while identifying when a controller must clean up its previous request.
#[derive(Debug, thiserror::Error)]
pub enum ResourcePoolBackendError {
    /// A database read, write, or transaction failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The pool's backend binding could not be loaded or validated.
    #[error(transparent)]
    Registry(#[from] ResourcePoolRegistryError),
    /// The Integrated allocator rejected or could not complete the allocation.
    #[error(transparent)]
    Integrated(#[from] ResourcePoolDatabaseError),
    /// The remote coordinator could not allocate, attach, or finish cleanup.
    #[error(transparent)]
    Remote(#[from] RemoteResourcePoolError),
    /// The accepted pool definition could not be loaded or validated.
    #[error(transparent)]
    Registration(#[from] RegistrationError),
    /// A requested or assigned value does not fit the pool or its Rust type.
    #[error(transparent)]
    Value(#[from] ValueConversionError),
    /// The same controller request has different pool selection or inputs.
    /// Cancel its previous attempt before preparing the replacement.
    #[error("resource pool request changed; cancel the previous attempt before retrying")]
    RequestChanged,
    /// The previous attempt is still cancelling or releasing its allocation.
    #[error("resource pool allocation cleanup is still pending")]
    CleanupPending,
}

impl ResourcePoolBackendError {
    /// `is_not_current` reports that the saved allocation attempt changed before
    /// this operation completed. A controller can recheck whether another worker
    /// satisfied its request.
    pub fn is_not_current(&self) -> bool {
        matches!(self, Self::Remote(RemoteResourcePoolError::NotCurrent))
    }
}
