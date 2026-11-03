// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Read active allocations from Integrated inventory and remote providers.
//!
//! Both sources use `resource_pool_allocations`. Pending remote requests appear
//! here once the provider assigns a value. Confirmed remote cleanup keeps the
//! record for retries but removes it from these active-allocation reads.

use std::num::NonZeroU16;

use model::resource_pool::allocation::ResourcePoolAllocation;
use uuid::Uuid;

use crate::DatabaseError;
use crate::db_read::DbReader;

/// `load` returns one active allocation, or `None` if the ID is unknown,
/// the provider has not assigned a value, or cleanup is confirmed. Rejected
/// requests and confirmed cancellations are also excluded.
/// Invalid stored values return a decode error.
pub async fn load(
    db: impl DbReader<'_>,
    allocation_id: Uuid,
) -> Result<Option<ResourcePoolAllocation>, DatabaseError> {
    let query = "SELECT allocation_id, pool_name, owner_type, owner_id, state,
        created_at, updated_at, integrated_resource_id, value, value_type,
        auto_assign, allocated_at, binding_id, remote_request, result
        FROM resource_pool_allocations
        WHERE allocation_id = $1
          AND state IN ('pending', 'attached', 'cancel_requested', 'release_requested')
          AND (integrated_resource_id IS NOT NULL OR result->>'status' = 'allocated')";
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
        "SELECT allocation_id, pool_name, owner_type, owner_id, state,
        created_at, updated_at, integrated_resource_id, value, value_type,
        auto_assign, allocated_at, binding_id, remote_request, result
        FROM resource_pool_allocations
        WHERE state IN ('pending', 'attached', 'cancel_requested', 'release_requested')
          AND (integrated_resource_id IS NOT NULL OR result->>'status' = 'allocated')
          AND pool_name = ",
    );
    query.push_bind(pool_name);
    // A direct cursor comparison lets prepared plans seek into the pool's index.
    if let Some(after) = after {
        query.push(" AND allocation_id > ");
        query.push_bind(after);
    }
    query.push(" ORDER BY allocation_id LIMIT ");
    query.push_bind(i64::from(limit.get()));
    query
        .build_query_as()
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query.sql(), error))
}

#[cfg(test)]
mod tests;
