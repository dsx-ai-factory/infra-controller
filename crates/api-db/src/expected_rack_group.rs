/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::HashMap;

use carbide_uuid::rack::{RackGroupId, RackId};
use model::expected_rack_group::{ExpectedRackGroup, ExpectedRackGroupMember, RackGroupTopology};
use model::metadata::Metadata;
use sqlx::{FromRow, PgConnection};

use crate::{DatabaseError, DatabaseResult};

#[derive(FromRow)]
struct GroupRow {
    rack_group_id: RackGroupId,
    topology: String,
    rack_ids: sqlx::types::Json<Vec<RackId>>,
    members: sqlx::types::Json<Vec<ExpectedRackGroupMember>>,
    metadata_name: String,
    metadata_description: String,
    metadata_labels: sqlx::types::Json<HashMap<String, String>>,
}

impl GroupRow {
    fn into_group(self) -> ExpectedRackGroup {
        ExpectedRackGroup {
            rack_group_id: self.rack_group_id,
            topology: RackGroupTopology::new(self.topology),
            rack_ids: self.rack_ids.0,
            members: self.members.0,
            metadata: Metadata {
                name: self.metadata_name,
                description: self.metadata_description,
                labels: self.metadata_labels.0,
            },
        }
    }
}

pub async fn find_by_rack_group_id(
    txn: &mut PgConnection,
    rack_group_id: &RackGroupId,
) -> DatabaseResult<Option<ExpectedRackGroup>> {
    let query = "SELECT * FROM expected_rack_groups WHERE rack_group_id=$1";
    let Some(row): Option<GroupRow> = sqlx::query_as(query)
        .bind(rack_group_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?
    else {
        return Ok(None);
    };
    Ok(Some(row.into_group()))
}

pub async fn find_all(txn: &mut PgConnection) -> DatabaseResult<Vec<ExpectedRackGroup>> {
    let query = "SELECT * FROM expected_rack_groups ORDER BY rack_group_id";
    let rows: Vec<GroupRow> = sqlx::query_as(query)
        .fetch_all(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    Ok(rows.into_iter().map(GroupRow::into_group).collect())
}

pub async fn create(
    txn: &mut PgConnection,
    group: &ExpectedRackGroup,
) -> DatabaseResult<ExpectedRackGroup> {
    let query = "INSERT INTO expected_rack_groups
        (rack_group_id, topology, rack_ids, members, metadata_name, metadata_description, metadata_labels)
        VALUES ($1, $2, $3, $4, $5, $6, $7)";
    sqlx::query(query)
        .bind(&group.rack_group_id)
        .bind(group.topology.as_str())
        .bind(sqlx::types::Json(&group.rack_ids))
        .bind(sqlx::types::Json(&group.members))
        .bind(&group.metadata.name)
        .bind(&group.metadata.description)
        .bind(sqlx::types::Json(&group.metadata.labels))
        .execute(&mut *txn)
        .await
        .map_err(|err| match err {
            sqlx::Error::Database(ref error)
                if error.constraint() == Some("expected_rack_groups_pkey") =>
            {
                DatabaseError::AlreadyFoundError {
                    kind: "expected_rack_group",
                    id: group.rack_group_id.to_string(),
                }
            }
            _ => DatabaseError::query(query, err),
        })?;
    Ok(group.clone())
}

pub async fn update(txn: &mut PgConnection, group: &ExpectedRackGroup) -> DatabaseResult<()> {
    let query = "UPDATE expected_rack_groups SET topology=$1, rack_ids=$2, members=$3, metadata_name=$4, metadata_description=$5, metadata_labels=$6 WHERE rack_group_id=$7";
    let result = sqlx::query(query)
        .bind(group.topology.as_str())
        .bind(sqlx::types::Json(&group.rack_ids))
        .bind(sqlx::types::Json(&group.members))
        .bind(&group.metadata.name)
        .bind(&group.metadata.description)
        .bind(sqlx::types::Json(&group.metadata.labels))
        .bind(&group.rack_group_id)
        .execute(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_rack_group",
            id: group.rack_group_id.to_string(),
        });
    }

    Ok(())
}

pub async fn delete(txn: &mut PgConnection, rack_group_id: &RackGroupId) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_rack_groups WHERE rack_group_id=$1";
    let result = sqlx::query(query)
        .bind(rack_group_id)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_rack_group",
            id: rack_group_id.to_string(),
        });
    }
    Ok(())
}

pub async fn clear(txn: &mut PgConnection) -> DatabaseResult<()> {
    // Like expected-machine replacement, exclude all writers until the replacement
    // commits. Ordinary reads remain available. Call before any replacement writes.
    let lock = "LOCK TABLE expected_rack_groups IN SHARE ROW EXCLUSIVE MODE";
    sqlx::query(lock)
        .execute(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(lock, err))?;
    let query = "DELETE FROM expected_rack_groups";
    sqlx::query(query)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(query, err))
}

#[cfg(test)]
mod tests;
