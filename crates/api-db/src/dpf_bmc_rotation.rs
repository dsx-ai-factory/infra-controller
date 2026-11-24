// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use mac_address::MacAddress;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// This record survives deletion of the machine and DPF objects. The operation
/// ID identifies snapshots held separately by the credential manager.
#[derive(Debug, sqlx::FromRow)]
pub struct Recovery {
    pub device_mac: MacAddress,
    pub operation_id: Uuid,
    pub host_machine_id: String,
    pub host_dpf_id: String,
    pub previous_version: Option<i32>,
    pub target_version: i32,
    pub patch_pending: bool,
    pub detached: bool,
}

/// The rotation, force-delete, and recovery paths share this work lease.
pub fn work_key(host_id: impl std::fmt::Display) -> String {
    format!("dpf-bmc-rotation:{host_id}")
}

pub async fn get(
    conn: impl DbReader<'_>,
    device_mac: MacAddress,
) -> DatabaseResult<Option<Recovery>> {
    let query = "SELECT device_mac, operation_id, host_machine_id, host_dpf_id, previous_version, \
                        target_version, patch_pending, detached \
                 FROM dpf_bmc_rotation_recovery WHERE device_mac = $1";
    sqlx::query_as(query)
        .bind(device_mac)
        .fetch_optional(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

pub async fn insert(conn: &mut PgConnection, recovery: &Recovery) -> DatabaseResult<()> {
    let query = "INSERT INTO dpf_bmc_rotation_recovery \
                 (device_mac, operation_id, host_machine_id, host_dpf_id, previous_version, \
                  target_version, patch_pending, detached) \
                 VALUES ($1, $2, $3, $4, $5, $6, false, false)";
    sqlx::query(query)
        .bind(recovery.device_mac)
        .bind(recovery.operation_id)
        .bind(&recovery.host_machine_id)
        .bind(&recovery.host_dpf_id)
        .bind(recovery.previous_version)
        .bind(recovery.target_version)
        .execute(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

pub async fn set_patch_pending(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    operation_id: Uuid,
    pending: bool,
) -> DatabaseResult<()> {
    let query = "UPDATE dpf_bmc_rotation_recovery SET patch_pending = $3 \
                 WHERE device_mac = $1 AND operation_id = $2";
    let rows = sqlx::query(query)
        .bind(device_mac)
        .bind(operation_id)
        .bind(pending)
        .execute(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .rows_affected();
    if rows == 0 {
        return Err(DatabaseError::FailedPrecondition(format!(
            "BMC {device_mac} has no matching DPF rotation recovery record"
        )));
    }
    Ok(())
}

pub async fn detach(
    conn: &mut PgConnection,
    host_id: &str,
    device_macs: &[MacAddress],
) -> DatabaseResult<()> {
    let query = "UPDATE dpf_bmc_rotation_recovery SET detached = true \
                 WHERE host_machine_id = $1 AND device_mac = ANY($2)";
    sqlx::query(query)
        .bind(host_id)
        .bind(device_macs)
        .execute(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

pub async fn detached_macs(
    conn: impl DbReader<'_>,
    macs: &[MacAddress],
) -> DatabaseResult<Vec<MacAddress>> {
    let query = "SELECT device_mac FROM dpf_bmc_rotation_recovery \
                 WHERE device_mac = ANY($1) AND detached";
    sqlx::query_scalar(query)
        .bind(macs)
        .fetch_all(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

pub async fn record_applied_version(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    version: Option<i32>,
) -> DatabaseResult<()> {
    let query = "INSERT INTO device_credential_rotation (device_mac, credential_type, current_version) \
                 VALUES ($1, 'bmc', $2) ON CONFLICT (device_mac, credential_type) DO UPDATE \
                 SET current_version = EXCLUDED.current_version, rotating_to_version = NULL, \
                     rotate_attempts = 0, rotate_quarantined_until = NULL, rotate_last_error_redacted = NULL";
    sqlx::query(query)
        .bind(device_mac)
        .bind(version)
        .execute(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

pub async fn remove(
    conn: &mut PgConnection,
    device_mac: MacAddress,
    operation_id: Uuid,
) -> DatabaseResult<()> {
    let query = "DELETE FROM dpf_bmc_rotation_recovery WHERE device_mac = $1 AND operation_id = $2";
    sqlx::query(query)
        .bind(device_mac)
        .bind(operation_id)
        .execute(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}
