/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Persistence shared by Domain ingestion and maintenance handlers.

use carbide_uuid::rack::RackGroupId;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::nvlink_domain::{
    NvLinkDomain, NvLinkDomainConfig, NvLinkDomainMember, NvLinkDomainSnapshot, NvLinkDomainState,
};
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult};

#[derive(sqlx::FromRow)]
struct ControllerRow {
    id: Uuid,
    rack_group_id: RackGroupId,
    name: Option<String>,
    members: Json<Vec<NvLinkDomainMember>>,
    config: Json<NvLinkDomainConfig>,
    version: ConfigVersion,
    controller_state: Json<NvLinkDomainState>,
    controller_state_version: ConfigVersion,
    controller_state_outcome: Option<Json<PersistentStateHandlerOutcome>>,
    deleted: Option<DateTime<Utc>>,
}

/// Create a controller-owned Domain and initial history in the caller's transaction.
/// The caller must roll back the entire transaction on failure.
/// Lifecycle callers are introduced by the ingestion work in https://github.com/dsx-ai-factory/infra-controller/issues/7426.
pub async fn create(txn: &mut PgConnection, domain: &NvLinkDomain) -> DatabaseResult<()> {
    super::create(txn, domain).await?;
    let version = ConfigVersion::initial();
    let query = "UPDATE nvlink_domains SET controller_state=$2,
        controller_state_version=$3, version=$3 WHERE id=$1";
    let result = sqlx::query(query)
        .bind(domain.id)
        .bind(Json(NvLinkDomainState::Created))
        .bind(version)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if result.rows_affected() != 1 {
        return Err(DatabaseError::NotFoundError {
            kind: "nvlink_domain",
            id: domain.id.to_string(),
        });
    }
    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::NvLinkDomain,
        &domain.id,
        &NvLinkDomainState::Created,
        version,
    )
    .await?;
    Ok(())
}

/// List adopted Domains, including deletion requests that still hold member positions.
pub async fn find_ids(reader: impl DbReader<'_>) -> DatabaseResult<Vec<Uuid>> {
    let query = "SELECT id FROM nvlink_domains WHERE controller_state IS NOT NULL
        AND controller_state_version IS NOT NULL AND version IS NOT NULL ORDER BY id";
    sqlx::query_scalar(query)
        .fetch_all(reader)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Load declaration, member positions, and runtime fields from one database snapshot.
/// Missing or unadopted declarations return None.
pub async fn find_snapshot(
    reader: impl DbReader<'_>,
    id: Uuid,
) -> DatabaseResult<Option<NvLinkDomainSnapshot>> {
    let query = "SELECT d.id, d.rack_group_id, d.name, d.config, d.version,
        d.controller_state, d.controller_state_version, d.controller_state_outcome, d.deleted,
        COALESCE((SELECT jsonb_agg(jsonb_build_object('rack_index', m.rack_index,
        'slot_index', m.slot_index, 'component_type', m.component_type)
        ORDER BY m.rack_index, m.slot_index, m.component_type)
        FROM nvlink_domain_members m WHERE m.domain_id=d.id), '[]'::jsonb) AS members
        FROM nvlink_domains d WHERE d.id=$1 AND d.controller_state IS NOT NULL
        AND d.controller_state_version IS NOT NULL AND d.version IS NOT NULL";
    let row: Option<ControllerRow> = sqlx::query_as(query)
        .bind(id)
        .fetch_optional(reader)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(row.map(|row| NvLinkDomainSnapshot {
        domain: NvLinkDomain {
            id: row.id,
            rack_group_id: row.rack_group_id,
            name: row.name,
            members: row.members.0,
        },
        config: Versioned::new(row.config.0, row.version),
        controller_state: Versioned::new(row.controller_state.0, row.controller_state_version),
        controller_state_outcome: row.controller_state_outcome.map(|outcome| outcome.0),
        deleted: row.deleted,
    }))
}

/// Lock a current snapshot until the iteration's state, history, and outcome commit.
/// Configuration or state changes invalidate the entire iteration.
pub async fn lock_snapshot(
    txn: &mut PgConnection,
    snapshot: &NvLinkDomainSnapshot,
) -> DatabaseResult<ConditionalWrite<(), ControllerStateNotCurrent>> {
    let query = "SELECT id FROM nvlink_domains WHERE id=$1 AND version=$2
        AND controller_state_version=$3 FOR UPDATE";
    let id: Option<Uuid> = sqlx::query_scalar(query)
        .bind(snapshot.domain.id)
        .bind(snapshot.config.version)
        .bind(snapshot.controller_state.version)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(if id.is_some() {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    })
}

/// Apply a state transition only at the expected controller version.
/// A deletion request prevents transitions other than Deleting.
pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    id: Uuid,
    old_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &NvLinkDomainState,
) -> DatabaseResult<ConditionalWrite<(), ControllerStateNotCurrent>> {
    let query = "UPDATE nvlink_domains SET controller_state=$4, controller_state_version=$3
        WHERE id=$1 AND controller_state_version=$2
        AND (deleted IS NULL OR $5)";
    let result = sqlx::query(query)
        .bind(id)
        .bind(old_version)
        .bind(new_version)
        .bind(Json(new_state))
        .bind(matches!(new_state, NvLinkDomainState::Deleting))
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(if result.rows_affected() == 1 {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    })
}

/// Save the last iteration's result in the caller's transaction.
pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    id: Uuid,
    outcome: PersistentStateHandlerOutcome,
) -> DatabaseResult<()> {
    let query = "UPDATE nvlink_domains SET controller_state_outcome=$2 WHERE id=$1";
    let result = sqlx::query(query)
        .bind(id)
        .bind(Json(outcome))
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if result.rows_affected() != 1 {
        return Err(DatabaseError::NotFoundError {
            kind: "nvlink_domain",
            id: id.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
