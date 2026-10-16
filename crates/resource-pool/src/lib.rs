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
//! [`ResourcePoolWithBackend`] borrows a descriptor and its selected backend.
//! This association does not resolve configuration or persist which backend is
//! authoritative for allocations. Callers match the backend before supplying
//! the execution context its adapter requires.

#![cfg_attr(test, allow(txn_held_across_await, txn_without_commit))]

#[cfg(test)]
mod tests;

use std::str::FromStr;

use db::resource_pool::{ResourcePoolAllocationNotOwned, ResourcePoolDatabaseError};
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::{OwnerType, ResourcePool};
use sqlx::PgConnection;

/// `ResourcePoolBackend` selects how a pool performs allocation and release.
///
/// Matching the execution variant keeps Integrated's borrowed transaction
/// explicit at the caller.
pub enum ResourcePoolBackend {
    /// Use the existing SQL allocator in the caller's entity transaction.
    Integrated(IntegratedResourcePoolBackend),
}

/// `ResourcePoolWithBackend` pairs a typed pool with an explicitly selected backend.
///
/// Both values remain owned by the caller. The association lasts for the borrow;
/// constructing it performs no I/O and does not establish a durable backend
/// binding. Allocation and release methods belong to the selected adapter, so
/// Integrated's SQL connection requirement stays specific to that backend.
///
/// Each pool can select a separate backend instance. The pools must already be
/// populated in SQL; the caller supplies a transaction after matching the
/// selected variant:
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
///     let address = match loopback.backend() {
///         ResourcePoolBackend::Integrated(backend) => {
///             backend
///                 .allocate(loopback.pool(), txn, OwnerType::Machine, "machine-1", None)
///                 .await?
///         }
///     };
///     let vni = match vni.backend() {
///         ResourcePoolBackend::Integrated(backend) => {
///             backend
///                 .allocate(
///                     vni.pool(),
///                     txn,
///                     OwnerType::NetworkSegment,
///                     "segment-1",
///                     Some(42),
///                 )
///                 .await?
///         }
///     };
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

    /// `backend` returns the selected backend for matching before execution.
    pub fn backend(&self) -> &'a ResourcePoolBackend {
        self.backend
    }
}

/// `IntegratedResourcePoolBackend` forwards mutations to the existing SQL allocator.
///
/// The caller supplies a connection from its transaction and owns commit or
/// rollback. The adapter does not acquire connections or commit independently;
/// existing allocations need no conversion to be used or released through it.
/// The adapter is stateless; the supplied connection determines the database.
pub struct IntegratedResourcePoolBackend;

impl IntegratedResourcePoolBackend {
    /// `allocate` reserves a value and returns it as the pool's Rust type.
    ///
    /// `None` selects from free automatic entries. `Some(value)` requests that
    /// value from the manual partition. Exhaustion, unavailable requested values,
    /// database failures, and value-conversion errors retain the SQL allocator's
    /// error types and precedence. Success performs the same single SQL query.
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
        db::resource_pool::allocate(pool, txn, owner_type, owner_id, requested_value).await
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
        db::resource_pool::allocate_exact(pool, txn, owner_type, owner_id, requested_value).await
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
        db::resource_pool::release(pool, txn, value, owner_type, owner_id).await
    }
}
