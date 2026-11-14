/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Persistence and membership admission for NVLink domains.

use carbide_uuid::rack::RackGroupId;
use model::nvlink_domain::NvLinkDomain;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// Require an existing rack group and atomically claim positions in the caller's transaction.
/// The caller must roll back on failure, including a concurrent membership conflict.
pub async fn create(txn: &mut PgConnection, domain: &NvLinkDomain) -> DatabaseResult<()> {
    // Serialize creation with the rack group's dependent-domain update guard.
    let query = "SELECT rack_group_id FROM expected_rack_groups WHERE rack_group_id=$1 FOR SHARE";
    let group: Option<RackGroupId> = sqlx::query_scalar(query)
        .bind(&domain.rack_group_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    group.ok_or_else(|| DatabaseError::NotFoundError {
        kind: "expected_rack_group",
        id: domain.rack_group_id.to_string(),
    })?;

    let query = "INSERT INTO nvlink_domains (id, rack_group_id, name) VALUES ($1, $2, $3)";
    sqlx::query(query)
        .bind(domain.id)
        .bind(&domain.rack_group_id)
        .bind(&domain.name)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    // Sorted insertion order prevents deadlocks between overlapping requests.
    let mut members: Vec<_> = domain.members.iter().collect();
    members.sort_by_key(|m| (m.rack_index, m.slot_index, m.component_type.to_string()));
    for member in members {
        let query = "INSERT INTO nvlink_domain_members (domain_id, rack_group_id, rack_index, slot_index, component_type) VALUES ($1, $2, $3, $4, $5)";
        sqlx::query(query)
            .bind(domain.id)
            .bind(&domain.rack_group_id)
            .bind(i64::from(member.rack_index))
            .bind(i64::from(member.slot_index))
            .bind(member.component_type.to_string())
            .execute(&mut *txn)
            .await
            .map_err(|e| {
                if e.as_database_error()
                    .is_some_and(|e| e.constraint() == Some("nvlink_domain_members_pkey"))
                {
                    DatabaseError::AlreadyFoundError {
                        kind: "NVLink domain member",
                        id: "one or more positions are already claimed".into(),
                    }
                } else {
                    DatabaseError::query(query, e)
                }
            })?;
    }
    Ok(())
}

/// List declaration IDs in UUID order, optionally restricted to one rack group.
pub async fn find_ids(
    txn: impl DbReader<'_>,
    rack_group_id: Option<&RackGroupId>,
) -> DatabaseResult<Vec<Uuid>> {
    let query =
        "SELECT id FROM nvlink_domains WHERE ($1::varchar IS NULL OR rack_group_id=$1) ORDER BY id";
    sqlx::query_scalar(query)
        .bind(rack_group_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Load declarations in UUID order with membership from the same database snapshot.
pub async fn find_by_ids(
    txn: impl DbReader<'_>,
    ids: &[Uuid],
) -> DatabaseResult<Vec<NvLinkDomain>> {
    let query = "SELECT d.id, d.rack_group_id, d.name, COALESCE((SELECT jsonb_agg(jsonb_build_object('rack_index', m.rack_index, 'slot_index', m.slot_index, 'component_type', m.component_type) ORDER BY m.rack_index, m.slot_index, m.component_type) FROM nvlink_domain_members m WHERE m.domain_id=d.id), '[]'::jsonb) AS members FROM nvlink_domains d WHERE d.id = ANY($1) ORDER BY d.id";
    sqlx::query_as(query)
        .bind(ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Delete a declaration and its membership claims, without touching hardware or observed domains.
pub async fn delete(txn: &mut PgConnection, id: Uuid) -> DatabaseResult<()> {
    let query = "DELETE FROM nvlink_domains WHERE id=$1";
    let result = sqlx::query(query)
        .bind(id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "nvlink_domain",
            id: id.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
