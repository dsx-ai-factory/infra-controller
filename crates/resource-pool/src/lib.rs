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
//! Explicit backend selection and execution for typed resource pools.
//!
//! Typed pool descriptors remain in `model`; SQL mutations remain in `db`.
//! [`ResourcePoolRegistry`] saves and checks each pool's backend selection.
//! [`ResourcePoolWithBackend`] borrows a descriptor and its selected backend.
//! This association does not resolve configuration or persist which backend is
//! authoritative for allocations. [`ResourcePoolBackend`] executes Integrated
//! allocations in the caller's transaction. [`grpc::GrpcResourcePoolBackend`]
//! independently calls a provider using identities and inputs saved by its caller;
//! remote selection remains separate from the Integrated backend enum.
//! [`value`] converts Core definitions and typed values to and from provider messages.
//! [`registration`] saves remote pool definitions and retries their registration
//! when provider acceptance has not been recorded locally.
//! [`remote::RemoteResourcePool`] saves remote requests, recovers lost replies,
//! and finishes cancellation or release. Consumers attach assignments and request
//! release in their own entity transactions.
//! Production startup of the coordinator and its cleanup metric follows in
//! [P11](https://github.com/dsx-ai-factory/infra-controller/issues/7140);
//! v2.4 configuration still rejects remote pool selection.

#![cfg_attr(test, allow(txn_held_across_await, txn_without_commit))]

#[cfg(test)]
mod registry_tests;
#[cfg(test)]
mod tests;

pub mod grpc;
pub mod registration;
mod registry;
pub mod remote;
mod transaction;
pub mod value;

use std::str::FromStr;

use db::resource_pool::{ResourcePoolAllocationNotOwned, ResourcePoolDatabaseError};
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::{OwnerType, ResourcePool};
pub use registry::{
    PoolDefinitionSource, ResolvedResourcePool, ResourcePoolRegistry, ResourcePoolRegistryError,
};
use sqlx::PgConnection;

/// `ResourcePoolBackend` selects how a pool performs allocation and release.
///
/// Integrated uses a connection from the caller's transaction; the caller owns
/// commit or rollback. It forwards to the SQL allocator without acquiring
/// connections or committing independently. Existing allocations need no
/// conversion to be used or released through the Integrated backend.
#[derive(Clone)]
pub enum ResourcePoolBackend {
    /// Use the existing SQL allocator in the caller's entity transaction.
    Integrated(IntegratedResourcePoolBackend),
}

/// `ResourcePoolWithBackend` pairs a typed pool with an explicitly selected backend.
///
/// Both values remain owned by the caller. The association lasts for the borrow;
/// constructing it performs no I/O and does not establish a durable backend
/// binding. Allocation and release methods belong to [`ResourcePoolBackend`].
///
/// Each pool can select a separate backend instance. The pools must already be
/// populated in SQL; the caller supplies a transaction to the selected backend:
///
/// ```rust,no_run
/// use std::net::Ipv4Addr;
///
/// use carbide_resource_pool::{
///     IntegratedResourcePoolBackend, ResourcePoolBackend, ResourcePoolWithBackend,
/// };
/// use model::resource_pool::{OwnerType, ResourcePool, ValueType};
/// use sqlx::PgConnection;
///
/// async fn allocate_from_selected_pools(
///     txn: &mut PgConnection,
/// ) -> Result<(Ipv4Addr, i32), Box<dyn std::error::Error>> {
///     let loopback = ResourcePool::<Ipv4Addr>::new("loopback_ip".into(), ValueType::Ipv4);
///     let vni = ResourcePool::<i32>::new("vni".into(), ValueType::Integer);
///     let machine_backend = ResourcePoolBackend::Integrated(IntegratedResourcePoolBackend);
///     let network_backend = ResourcePoolBackend::Integrated(IntegratedResourcePoolBackend);
///     let loopback = ResourcePoolWithBackend::new(&loopback, &machine_backend);
///     let vni = ResourcePoolWithBackend::new(&vni, &network_backend);
///
///     let address = loopback
///         .backend()
///         .allocate(loopback.pool(), txn, OwnerType::Machine, "machine-1", None)
///         .await?;
///     let vni = vni
///         .backend()
///         .allocate(
///             vni.pool(),
///             txn,
///             OwnerType::NetworkSegment,
///             "segment-1",
///             Some(42),
///         )
///         .await?;
///     Ok((address, vni))
/// }
/// ```
pub struct ResourcePoolWithBackend<'a, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    <T as FromStr>::Err: std::error::Error,
{
    pool: &'a ResourcePool<T>,
    backend: &'a ResourcePoolBackend,
}

impl<'a, T> ResourcePoolWithBackend<'a, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    <T as FromStr>::Err: std::error::Error,
{
    /// `new` borrows the descriptor and backend selected by the caller for this pool.
    pub fn new(pool: &'a ResourcePool<T>, backend: &'a ResourcePoolBackend) -> Self {
        Self { pool, backend }
    }

    /// `pool` returns the typed descriptor supplied when this association was created.
    pub fn pool(&self) -> &'a ResourcePool<T> {
        self.pool
    }

    /// `backend` returns the selected backend for allocation and release.
    pub fn backend(&self) -> &'a ResourcePoolBackend {
        self.backend
    }
}

/// `IntegratedResourcePoolBackend` represents the SQL allocator in
/// [`ResourcePoolBackend::Integrated`].
///
/// It is stateless; the connection supplied to [`ResourcePoolBackend`] methods
/// determines the database.
#[derive(Clone)]
pub struct IntegratedResourcePoolBackend;

impl ResourcePoolBackend {
    /// `allocate` reserves a value and returns it as the pool's Rust type.
    ///
    /// `None` selects from free automatic entries. `Some(value)` requests that
    /// value from the manual partition. Exhaustion, unavailable requested values,
    /// database failures, and value-conversion errors retain the SQL allocator's
    /// error types and precedence. Integrated performs one SQL query on success.
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
        <T as FromStr>::Err: std::error::Error,
    {
        match self {
            Self::Integrated(_) => {
                db::resource_pool::allocate(pool, txn, owner_type, owner_id, requested_value).await
            }
        }
    }

    /// `allocate_exact` reserves the requested value from either assignment partition.
    ///
    /// Assignment mode is preserved. Missing or allocated values return
    /// [`DatabaseError::FailedPrecondition`], including allocations belonging to
    /// the same owner; callers must handle retained allocations separately.
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
        <T as FromStr>::Err: std::error::Error,
    {
        match self {
            Self::Integrated(_) => {
                db::resource_pool::allocate_exact(pool, txn, owner_type, owner_id, requested_value)
                    .await
            }
        }
    }

    /// `release` frees a value only when its owner type and ID match.
    ///
    /// Missing, free, and differently owned values return
    /// [`ConditionalWrite::NotApplied`] without mutation. Ownership does not
    /// identify an allocation generation: callers must prevent stale cleanup
    /// from reaching a later allocation for the same owner.
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
        <T as FromStr>::Err: std::error::Error,
    {
        match self {
            Self::Integrated(_) => {
                db::resource_pool::release(pool, txn, value, owner_type, owner_id).await
            }
        }
    }
}
