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

use carbide_uuid::nvlink::NvLinkPartitionId;
use model::nvl_partition::{NewNvlPartition, NvlPartition, NvlPartitionSnapshotPgJson};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{
    ColumnInfo, DatabaseError, DatabaseResult, FilterableQueryBuilder, ObjectColumnFilter,
};

#[derive(Copy, Clone)]
pub struct IdColumn;
impl ColumnInfo<'_> for IdColumn {
    type TableType = NvlPartition;
    type ColumnType = NvLinkPartitionId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

pub async fn create(
    value: &NewNvlPartition,
    txn: &mut PgConnection,
) -> Result<NvlPartition, DatabaseError> {
    let query = "INSERT INTO nvlink_partitions (
                id,
                nmx_c_partition_id,
                name,
                domain_uuid,
                logical_partition_id)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING row_to_json(nvlink_partitions.*)";

    let partition: NvlPartitionSnapshotPgJson = sqlx::query_as(query)
        .bind(value.id)
        .bind(value.nmx_c_partition_id)
        .bind(value.name.as_str())
        .bind(value.domain_uuid)
        .bind(value.logical_partition_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;
    partition
        .try_into()
        .map_err(|e| DatabaseError::new(query, e))
}

/// Retrieves the IDs of all NvLink partitions
///
/// * `txn` - A reference to a currently open database transaction
pub async fn for_tenant(
    txn: impl DbReader<'_>,
    tenant_organization_id: String,
) -> Result<Vec<NvlPartition>, DatabaseError> {
    let results: Vec<NvlPartition> = {
        let query = "SELECT * FROM nvlink_partitions WHERE config->>'tenant_organization_id' = $1";
        let partitions: Vec<NvlPartitionSnapshotPgJson> = sqlx::query_as(query)
            .bind(tenant_organization_id)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::new(query, e))?;

        partitions
            .into_iter()
            .map(|p| p.try_into())
            .collect::<Result<Vec<NvlPartition>, sqlx::Error>>()
            .map_err(|e| DatabaseError::new(query, e))?
    };

    Ok(results)
}

pub async fn find_ids(
    txn: impl DbReader<'_>,
    filter: model::nvl_partition::NvLinkPartitionSearchFilter,
) -> Result<Vec<NvLinkPartitionId>, DatabaseError> {
    // build query
    let mut builder = sqlx::QueryBuilder::new("SELECT id FROM nvlink_partitions");
    let tenant_org_id = &filter.tenant_organization_id;
    let name = &filter.name;

    if name.is_some() {
        builder.push(" WHERE config->>'name' = ");
        builder.push_bind(name);

        if tenant_org_id.is_some() {
            builder.push(" AND config->>'tenant_organization_id' = ");
            builder.push_bind(tenant_org_id);
        }
    } else if tenant_org_id.is_some() {
        builder.push(" WHERE config->>'tenant_organization_id' = ");
        builder.push_bind(tenant_org_id);
    }

    let query = builder.build_query_as();
    let ids: Vec<NvLinkPartitionId> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("nvl_partition::find_ids", e))?;

    Ok(ids)
}

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = NvlPartition>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<NvlPartition>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new(
        "SELECT row_to_json(p.*) FROM (SELECT * FROM nvlink_partitions) p",
    )
    .filter(&filter);
    let partitions: Vec<NvlPartitionSnapshotPgJson> =
        query
            .build_query_as()
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::new(query.sql(), e))?;

    partitions
        .into_iter()
        .map(|p| p.try_into())
        .collect::<Result<Vec<NvlPartition>, sqlx::Error>>()
        .map_err(|e| DatabaseError::new(query.sql(), e))
}

pub async fn mark_as_deleted(
    partition: &NvlPartition,
    txn: &mut PgConnection,
) -> DatabaseResult<NvlPartition> {
    let query = "UPDATE nvlink_partitions SET updated=NOW(), deleted=NOW() WHERE id=$1 RETURNING *";
    let partition: NvlPartitionSnapshotPgJson = sqlx::query_as(query)
        .bind(partition.id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    partition
        .try_into()
        .map_err(|e| DatabaseError::new(query, e))
}

pub async fn final_delete(
    partition_id: NvLinkPartitionId,
    txn: &mut PgConnection,
) -> Result<NvLinkPartitionId, DatabaseError> {
    let query = "DELETE FROM nvlink_partitions WHERE id=$1::uuid RETURNING id";
    let partition: NvLinkPartitionId = sqlx::query_as(query)
        .bind(partition_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(partition)
}
