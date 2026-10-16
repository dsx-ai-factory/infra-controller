// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Read active allocation records across Integrated and remote pools.
//!
//! Database triggers keep Integrated inventory and its allocation records in sync
//! within the same statement. If an allocation record cannot be written, the
//! inventory write rolls back too, including on an autocommit connection. This covers
//! allocation, release, and older binaries writing inventory directly. Deleting
//! inventory removes its allocation record through the foreign key.
//!
//! Remote result and confirmed cleanup writes maintain their allocation records
//! atomically with the operation. These reads join the source to derive current
//! state; a terminal remote operation with an active allocation record is an error.

use std::num::NonZeroU16;

use model::resource_pool::allocation::ResourcePoolAllocation;
use uuid::Uuid;

use crate::DatabaseError;
use crate::db_read::DbReader;

/// `load` returns one active allocation record, or `None` for an unknown ID,
/// an Integrated release, or confirmed remote cleanup. Invalid stored values or
/// an allocation record without an active source return a decode error; they must
/// not look like an allocation that was successfully released.
pub async fn load(
    db: impl DbReader<'_>,
    allocation_id: Uuid,
) -> Result<Option<ResourcePoolAllocation>, DatabaseError> {
    let query = "SELECT allocation.allocation_id, allocation.pool_name,
        allocation.value, allocation.value_type, allocation.owner_type,
        allocation.owner_id, allocation.auto_assign, allocation.allocated_at,
        allocation.recorded_at, allocation.integrated_resource_id,
        allocation.remote_request_id, inventory.state AS integrated_state,
        operation.state AS remote_state
        FROM resource_pool_allocations AS allocation
        LEFT JOIN resource_pool AS inventory
          ON inventory.id = allocation.integrated_resource_id
        LEFT JOIN resource_pool_operations AS operation
          ON operation.request_id = allocation.remote_request_id
        WHERE allocation.allocation_id = $1";
    sqlx::query_as(query)
        .bind(allocation_id)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `list` reads at most `limit` active allocations for one pool in UUID order.
/// Pass the last returned `allocation_id` as `after`; `None` starts a new scan.
/// Allocations can change between pages, so this does not promise a snapshot
/// across separate calls. Invalid stored allocations fail the page, as in [`load`].
pub async fn list(
    db: impl DbReader<'_>,
    pool_name: &str,
    after: Option<Uuid>,
    limit: NonZeroU16,
) -> Result<Vec<ResourcePoolAllocation>, DatabaseError> {
    let mut query = sqlx::QueryBuilder::new(
        "SELECT allocation.allocation_id, allocation.pool_name,
        allocation.value, allocation.value_type, allocation.owner_type,
        allocation.owner_id, allocation.auto_assign, allocation.allocated_at,
        allocation.recorded_at, allocation.integrated_resource_id,
        allocation.remote_request_id, inventory.state AS integrated_state,
        operation.state AS remote_state
        FROM resource_pool_allocations AS allocation
        LEFT JOIN resource_pool AS inventory
          ON inventory.id = allocation.integrated_resource_id
        LEFT JOIN resource_pool_operations AS operation
          ON operation.request_id = allocation.remote_request_id
        WHERE allocation.pool_name = ",
    );
    query.push_bind(pool_name);
    // A direct cursor comparison lets prepared plans seek into the pool's index.
    if let Some(after) = after {
        query.push(" AND allocation.allocation_id > ");
        query.push_bind(after);
    }
    query.push(" ORDER BY allocation.allocation_id LIMIT ");
    query.push_bind(i64::from(limit.get()));
    query
        .build_query_as()
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query.sql(), error))
}

#[cfg(test)]
mod tests;
