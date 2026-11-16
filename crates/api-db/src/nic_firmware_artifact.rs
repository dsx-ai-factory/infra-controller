// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Coordinate frozen NIC firmware membership with cache eviction.

use carbide_uuid::machine::{HostMachineId, MachineType};
use model::machine::ManagedHostState;
use model::machine::nic_firmware::NicFirmwareOperation;
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::{DatabaseError, DatabaseResult};

/// `ArtifactSnapshot` keeps authorization and retention from one SQL statement.
/// Retention decoding is delayed until authorization succeeds, so an unrelated
/// corrupt operation cannot turn a host's normal 404 denial into a 503.
pub struct ArtifactSnapshot {
    /// The requested host's current controller state.
    pub host_state: ManagedHostState,
    host_operations: String,
}

impl ArtifactSnapshot {
    /// `into_host_operations` decodes every matching host's frozen state.
    /// Malformed membership fails the entire read, without a partial result.
    /// Results are unordered; an empty list means no current host operations.
    pub fn into_host_operations(
        self,
    ) -> DatabaseResult<Vec<(HostMachineId, NicFirmwareOperation)>> {
        serde_json::from_str(&self.host_operations).map_err(|error| {
            DatabaseError::query(
                "decode NIC firmware retention snapshot",
                sqlx::Error::Decode(error.into()),
            )
        })
    }
}

/// `lock_retention_for_update` excludes artifact snapshots while new frozen
/// membership is committed. Acquire it before history or host row locks, in
/// the transaction that makes the new operation active; commit releases it.
/// Bound the wait and keep the writer's critical section short. Every transition
/// adding active membership must participate, including retries from a phase
/// that did not retain artifacts. Existing writers only preserve or shrink it.
/// Readers and writers must use the same writable database and preserve the
/// backend throughout each transaction, including through any pooling proxy.
/// The production writer is tracked in
/// <https://github.com/dsx-ai-factory/infra-controller/issues/8943>.
pub async fn lock_retention_for_update(txn: &mut PgConnection) -> DatabaseResult<()> {
    let query =
        "SELECT pg_advisory_xact_lock(hashtextextended('nic_firmware_artifact_retention', 0))";
    sqlx::query(query)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// `lock_retention` acquires shared exclusion before reading membership.
/// Use read-committed isolation and call `snapshot` after this returns, so its
/// statement snapshot includes a writer that committed while the lock waited.
/// Keep the transaction through physical cache open/eviction, then release it
/// before a source download or response stream. Callers bound the SQL wait.
pub async fn lock_retention(txn: &mut PgConnection) -> DatabaseResult<()> {
    let query = "SELECT pg_advisory_xact_lock_shared(hashtextextended('nic_firmware_artifact_retention', 0))";
    sqlx::query(query)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// `snapshot` reads host authorization and all current stable/predicted-host
/// operations in one statement after `lock_retention`. Missing hosts return
/// `None`; malformed host state fails the read. Decode retention only after
/// authorization, then validate and select retaining phases. No rows are locked.
pub async fn snapshot(
    txn: &mut PgConnection,
    host_id: HostMachineId,
) -> DatabaseResult<Option<ArtifactSnapshot>> {
    let query = r#"
        SELECT controller_state, COALESCE((
            SELECT jsonb_agg(jsonb_build_array(id, controller_state->'operation'))
            FROM machines
            WHERE (id LIKE $2 OR id LIKE $3)
              AND controller_state->>'state' = 'nicfirmwareupdating'
        ), '[]'::jsonb)::text
        FROM machines WHERE id = $1
    "#;
    type Row = (Json<ManagedHostState>, String);
    let row: Option<Row> = sqlx::query_as(query)
        .bind(host_id)
        .bind(format!("{}%", MachineType::Host.id_prefix()))
        .bind(format!("{}%", MachineType::PredictedHost.id_prefix()))
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(row.map(|(state, operations)| ArtifactSnapshot {
        host_state: state.0,
        host_operations: operations,
    }))
}
