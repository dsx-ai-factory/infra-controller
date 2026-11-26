// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Remember an unfinished VPC's tenant and whether creation was cancelled.
//! Successful creation removes this record in the VPC transaction. Cancellation
//! keeps it so a request delayed before admission cannot create the VPC later.

use carbide_uuid::vpc::VpcId;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

/// Serialize admission, allocation preparation, attachment and cancellation for
/// this VPC ID. Commit before provider I/O; cancellation uses the same lock to
/// prevent any later preparation or attachment once its decision is saved.
pub async fn lock(txn: &mut PgConnection, id: VpcId) -> DatabaseResult<()> {
    let query = "SELECT pg_advisory_xact_lock(hashtextextended('vpc-creation:' || $1, 0))";
    sqlx::query(query)
        .bind(id.to_string())
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// Save the tenant before preparing an allocation, rejecting a cancelled ID or
/// a competing tenant. Hold [`lock`] until this transaction commits.
pub async fn begin(txn: &mut PgConnection, id: VpcId, tenant: &str) -> DatabaseResult<()> {
    if record(txn, id, tenant).await? {
        return Err(DatabaseError::FailedPrecondition(format!(
            "creation of VPC `{id}` was cancelled; use a new VPC ID",
        )));
    }
    Ok(())
}

/// Save cancellation even when the original request has not reached Core yet.
/// Hold [`lock`] and check the VPC row before calling; a committed VPC must be
/// returned to the caller rather than cancelled here. Commit before cleanup I/O.
pub async fn cancel(txn: &mut PgConnection, id: VpcId, tenant: &str) -> DatabaseResult<()> {
    record(txn, id, tenant).await?;
    let query = "UPDATE vpc_creation_requests SET cancelled = true WHERE vpc_id = $1";
    sqlx::query(query)
        .bind(id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// Remove the unfinished request when its VPC is inserted, or when configured
/// startup discards an unused ID. Hold [`lock`] and commit that decision together.
/// An inserted VPC keeps its ID reserved, including after ordinary soft deletion.
pub async fn complete(txn: &mut PgConnection, id: VpcId) -> DatabaseResult<()> {
    let query = "DELETE FROM vpc_creation_requests WHERE vpc_id = $1";
    sqlx::query(query)
        .bind(id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// Record the first tenant under the creation lock. A different tenant must not
/// cancel or attach an allocation belonging to an in-flight VPC with this ID.
/// Returns whether that tenant has already cancelled creation.
async fn record(txn: &mut PgConnection, id: VpcId, tenant: &str) -> DatabaseResult<bool> {
    let query = "INSERT INTO vpc_creation_requests (vpc_id, tenant_organization_id)
                 VALUES ($1, $2) ON CONFLICT (vpc_id) DO NOTHING";
    sqlx::query(query)
        .bind(id)
        .bind(tenant)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let query =
        "SELECT tenant_organization_id, cancelled FROM vpc_creation_requests WHERE vpc_id = $1";
    let (saved_tenant, cancelled): (String, bool) = sqlx::query_as(query)
        .bind(id)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if saved_tenant != tenant {
        return Err(DatabaseError::FailedPrecondition(format!(
            "VPC `{id}` creation belongs to another tenant",
        )));
    }
    Ok(cancelled)
}
