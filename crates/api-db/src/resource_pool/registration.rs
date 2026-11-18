// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Save remote pool registrations before contacting their provider.
//!
//! Writes use the caller's transaction. Lock the binding before checking its
//! latest revision and inserting a successor, then commit before provider I/O.
//! Request encoding and provider validation belong to the caller.

use model::resource_pool::ResourcePoolBinding;
use model::resource_pool::registration::ResourcePoolRegistration;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::DatabaseError;
use crate::db_read::DbReader;

/// `lock_binding` returns a binding locked until the caller's transaction ends.
/// The lock serializes registration preparation while allowing foreign-key reads.
/// Missing bindings return `NotFoundError`; callers validate the loaded binding.
pub async fn lock_binding(
    txn: &mut PgConnection,
    binding_id: Uuid,
) -> Result<ResourcePoolBinding, DatabaseError> {
    let query = "SELECT binding_id, pool_name, value_domain, backend_name, backend_kind,
        authority_id, remote_pool FROM resource_pool_binding
        WHERE binding_id = $1 FOR NO KEY UPDATE";
    sqlx::query_as(query)
        .bind(binding_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "resource pool binding",
            id: binding_id.to_string(),
        })
}

/// `latest` returns the highest saved revision, including a pending request.
/// It returns `None` when this binding has no registration history.
pub async fn latest(
    db: impl DbReader<'_>,
    binding_id: Uuid,
) -> Result<Option<ResourcePoolRegistration>, DatabaseError> {
    let query = "SELECT binding_id, revision, request, accepted FROM resource_pool_registration
        WHERE binding_id = $1 ORDER BY revision DESC LIMIT 1";
    sqlx::query_as(query)
        .bind(binding_id)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `latest_accepted` returns the newest revision acknowledged by the provider.
/// A pending successor does not hide that revision. `None` means no revision
/// has been acknowledged for this binding.
pub async fn latest_accepted(
    db: impl DbReader<'_>,
    binding_id: Uuid,
) -> Result<Option<ResourcePoolRegistration>, DatabaseError> {
    let query = "SELECT binding_id, revision, request, accepted FROM resource_pool_registration
        WHERE binding_id = $1 AND accepted ORDER BY revision DESC LIMIT 1";
    sqlx::query_as(query)
        .bind(binding_id)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `load` returns the exact saved revision, or `None` when it does not exist.
pub async fn load(
    db: impl DbReader<'_>,
    binding_id: Uuid,
    revision: i64,
) -> Result<Option<ResourcePoolRegistration>, DatabaseError> {
    let query = "SELECT binding_id, revision, request, accepted FROM resource_pool_registration
        WHERE binding_id = $1 AND revision = $2";
    sqlx::query_as(query)
        .bind(binding_id)
        .bind(revision)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `insert` saves a registration in the caller's transaction. The caller must
/// lock and validate its binding and predecessor first. Duplicate revisions,
/// nonpositive revisions, missing bindings, and a second pending request fail.
/// The request bytes are stored without decoding or changing them.
pub async fn insert(
    txn: &mut PgConnection,
    registration: &ResourcePoolRegistration,
) -> Result<(), DatabaseError> {
    let query = "INSERT INTO resource_pool_registration (binding_id, revision, request, accepted)
        VALUES ($1, $2, $3, $4)";
    sqlx::query(query)
        .bind(registration.binding_id)
        .bind(registration.revision)
        .bind(&registration.request)
        .bind(registration.accepted)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// `acknowledge` records provider acceptance without changing the saved request.
/// Repeated acknowledgements succeed; a missing revision returns `NotFoundError`.
pub async fn acknowledge(
    txn: &mut PgConnection,
    binding_id: Uuid,
    revision: i64,
) -> Result<(), DatabaseError> {
    let query = "UPDATE resource_pool_registration SET accepted = true
        WHERE binding_id = $1 AND revision = $2";
    let result = sqlx::query(query)
        .bind(binding_id)
        .bind(revision)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "resource pool registration",
            id: format!("{binding_id}/{revision}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
