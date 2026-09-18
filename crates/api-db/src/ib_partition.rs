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

use carbide_uuid::infiniband::IBPartitionId;
use config_version::ConfigVersion;
use futures::StreamExt;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::ib_partition::{
    IBPartition, IBPartitionControllerState, IBPartitionStatus, NewIBPartition,
};
use model::metadata::Metadata;
use sqlx::{FromRow, PgConnection};

use crate::db_read::DbReader;
use crate::{
    ColumnInfo, ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult,
    FilterableQueryBuilder, ObjectColumnFilter,
};

#[derive(Copy, Clone)]
pub struct IdColumn;
impl ColumnInfo<'_> for IdColumn {
    type TableType = IBPartition;
    type ColumnType = IBPartitionId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

pub async fn create(
    value: NewIBPartition,
    txn: &mut PgConnection,
    max_partition_per_tenant: i32,
    status: IBPartitionStatus,
) -> Result<IBPartition, DatabaseError> {
    value.metadata.validate(true).map_err(|e| {
        DatabaseError::InvalidArgument(format!("Invalid metadata for IBPartition: {}", e))
    })?;

    let version = ConfigVersion::initial();
    let state = IBPartitionControllerState::Provisioning;
    let conf = &value.config;

    let query = "INSERT INTO ib_partitions (
                id,
                name,
                labels,
                description,
                pkey,
                organization_id,
                mtu,
                rate_limit,
                service_level,
                config_version,
                controller_state_version,
                controller_state,
                status)
            SELECT $1, $2, $3::json, $4, $5, $6, $7, $8, $9, $10, $11, $12, $14
            WHERE (SELECT COUNT(*) FROM ib_partitions WHERE organization_id = $6) < $13
            RETURNING *";
    let segment: IBPartition = sqlx::query_as(query)
        .bind(value.id)
        .bind(&value.metadata.name)
        .bind(sqlx::types::Json(&value.metadata.labels))
        .bind(&value.metadata.description)
        .bind(status.pkey.map(|k| u16::from(k) as i32))
        .bind(conf.tenant_organization_id.to_string())
        .bind::<i32>(conf.mtu.clone().unwrap_or_default().into())
        .bind::<i32>(conf.rate_limit.clone().unwrap_or_default().into())
        .bind::<i32>(conf.service_level.clone().unwrap_or_default().into())
        .bind(version)
        .bind(version)
        .bind(sqlx::types::Json(state))
        .bind(max_partition_per_tenant)
        .bind(sqlx::types::Json(&status))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(segment)
}

/// Retrieves the IDs of all IB partition
///
/// * `txn` - A reference to a currently open database transaction
pub async fn list_segment_ids(txn: &mut PgConnection) -> Result<Vec<IBPartitionId>, DatabaseError> {
    let query = "SELECT id FROM ib_partitions";
    let mut results = Vec::new();
    let mut segment_id_stream = sqlx::query_as(query).fetch(txn);
    while let Some(maybe_id) = segment_id_stream.next().await {
        let id = maybe_id.map_err(|e| DatabaseError::query(query, e))?;
        results.push(id);
    }

    Ok(results)
}

pub async fn for_tenant(
    txn: impl DbReader<'_>,
    tenant_organization_id: String,
) -> Result<Vec<IBPartition>, DatabaseError> {
    let results: Vec<IBPartition> = {
        let query = "SELECT * FROM ib_partitions WHERE organization_id=$1";
        sqlx::query_as(query)
            .bind(tenant_organization_id)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
    };

    Ok(results)
}

pub async fn find_ids(
    txn: impl DbReader<'_>,
    filter: model::ib_partition::IbPartitionSearchFilter,
) -> Result<Vec<IBPartitionId>, DatabaseError> {
    // build query
    let mut builder = sqlx::QueryBuilder::new("SELECT id FROM ib_partitions");
    let mut has_filter = false;
    if let Some(tenant_org_id) = &filter.tenant_org_id {
        builder.push(" WHERE organization_id = ");
        builder.push_bind(tenant_org_id);
        has_filter = true;
    }
    if let Some(name) = &filter.name {
        if has_filter {
            builder.push(" AND name = ");
        } else {
            builder.push(" WHERE name = ");
        }
        builder.push_bind(name);
    }

    let query = builder.build_query_as();
    let ids: Vec<IBPartitionId> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("ib_partition::find_ids", e))?;

    Ok(ids)
}

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = IBPartition>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<IBPartition>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new("SELECT * FROM ib_partitions").filter(&filter);

    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

pub async fn find_pkey_by_partition_id(
    txn: &mut PgConnection,
    id: IBPartitionId,
) -> Result<Option<u16>, DatabaseError> {
    #[derive(Debug, Clone, FromRow)]
    struct Pkey(String);

    let mut query = FilterableQueryBuilder::new("SELECT status->>'pkey' FROM ib_partitions")
        .filter(&ObjectColumnFilter::One(IdColumn, &id));
    let pkey = query
        .build_query_as::<Pkey>()
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))?;

    pkey.map(|id| u16::from_str_radix(id.0.trim_start_matches("0x"), 16))
        .transpose()
        .map_err(|e| DatabaseError::Internal {
            message: e.to_string(),
        })
}

/// `try_update_controller_state` writes the IB partition state and `new_version`
/// when the version matches `expected_version`.
///
/// A missing partition or changed version returns
/// `NotApplied(ControllerStateNotCurrent)`.
/// `Applied(())` leaves the write in the caller's transaction; database failures
/// remain errors.
pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    partition_id: IBPartitionId,
    expected_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &IBPartitionControllerState,
) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
    let query = "UPDATE ib_partitions SET controller_state_version=$1, controller_state=$2::json where id=$3::uuid AND controller_state_version=$4 returning id";
    let result = sqlx::query_as::<_, IBPartitionId>(query)
        .bind(new_version)
        .bind(sqlx::types::Json(new_state))
        .bind(partition_id)
        .bind(expected_version)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match result {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(ControllerStateNotCurrent),
    })
}

pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    partition_id: IBPartitionId,
    outcome: PersistentStateHandlerOutcome,
) -> Result<(), DatabaseError> {
    let query = "UPDATE ib_partitions SET controller_state_outcome=$1::json WHERE id=$2::uuid";
    sqlx::query(query)
        .bind(sqlx::types::Json(outcome))
        .bind(partition_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn mark_as_deleted(
    value: &IBPartition,
    txn: &mut PgConnection,
) -> DatabaseResult<IBPartition> {
    let query = "UPDATE ib_partitions SET updated=NOW(), deleted=NOW() WHERE id=$1 RETURNING *";
    let segment: IBPartition = sqlx::query_as(query)
        .bind(value.id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(segment)
}

pub async fn final_delete(
    partition_id: IBPartitionId,
    txn: &mut PgConnection,
) -> Result<IBPartitionId, DatabaseError> {
    let query = "DELETE FROM ib_partitions WHERE id=$1::uuid RETURNING id";
    let partition: IBPartitionId = sqlx::query_as(query)
        .bind(partition_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(partition)
}

/// Counts the number of instances that reference a given IB partition in their ib_config.
pub async fn count_instances_referencing_partition(
    txn: impl DbReader<'_>,
    partition_id: IBPartitionId,
) -> Result<i64, DatabaseError> {
    let query = "
        SELECT count(*) FROM instances
        WHERE (ib_config -> 'ib_interfaces')
              @> jsonb_build_array(jsonb_build_object('ib_partition_id', $1::text))
    ";
    let (count,): (i64,) = sqlx::query_as(query)
        .bind(partition_id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(count)
}

/// `PartitionNotCurrent` means the partition is missing or its config version
/// no longer matches the snapshot. The write does not distinguish these cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionNotCurrent;

/// `update_metadata` replaces metadata and advances the config version only
/// while the partition has `expected_version`, leaving controller status intact.
/// A partition marked for deletion remains eligible until final deletion.
///
/// Returns `Applied(partition)` with the updated row, or
/// `NotApplied(PartitionNotCurrent)` for a missing or changed partition. Database
/// failures remain errors; the caller must commit the transaction.
pub async fn update_metadata(
    partition_id: IBPartitionId,
    expected_version: ConfigVersion,
    metadata: &Metadata,
    txn: &mut PgConnection,
) -> Result<ConditionalWrite<IBPartition, PartitionNotCurrent>, DatabaseError> {
    metadata.validate(true).map_err(|e| {
        DatabaseError::InvalidArgument(format!("Invalid metadata for IBPartition: {}", e))
    })?;

    let query = "UPDATE ib_partitions SET name=$1, labels=$2::json, description=$3, config_version=$4, updated=NOW()
                 WHERE id=$5::uuid AND config_version=$6 RETURNING *";

    let partition: Option<IBPartition> = sqlx::query_as(query)
        .bind(&metadata.name)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(&metadata.description)
        .bind(expected_version.increment())
        .bind(partition_id)
        .bind(expected_version)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match partition {
        Some(partition) => ConditionalWrite::Applied(partition),
        None => ConditionalWrite::NotApplied(PartitionNotCurrent),
    })
}

/// `update_status` stores the controller's observed status without changing
/// metadata or either version. Returns the updated partition; a missing row or
/// database failure remains an error. The caller must commit the transaction.
pub async fn update_status(
    partition_id: IBPartitionId,
    status: &Option<IBPartitionStatus>,
    txn: &mut PgConnection,
) -> Result<IBPartition, DatabaseError> {
    let query = "UPDATE ib_partitions SET status=$1::json, updated=NOW()
                 WHERE id=$2::uuid RETURNING *";

    sqlx::query_as(query)
        .bind(sqlx::types::Json(status))
        .bind(partition_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

#[cfg(test)]
mod tests {
    use model::ib_partition::IBPartitionConfig;

    use super::*;

    #[crate::sqlx_test]
    async fn metadata_and_status_updates_preserve_each_other(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let snapshot = create(
            NewIBPartition {
                id: IBPartitionId::new(),
                config: IBPartitionConfig {
                    name: "initial".to_string(),
                    pkey: None,
                    tenant_organization_id: "example".parse().unwrap(),
                    mtu: None,
                    rate_limit: None,
                    service_level: None,
                },
                metadata: Metadata {
                    name: "initial".to_string(),
                    ..Default::default()
                },
            },
            &mut txn,
            10,
            IBPartitionStatus {
                partition: None,
                mtu: None,
                rate_limit: None,
                service_level: None,
                pkey: Some(42.try_into().unwrap()),
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();

        let mut status = snapshot.status.clone();
        status.as_mut().unwrap().partition = Some("first observation".to_string());
        let mut txn = pool.begin().await.unwrap();
        update_status(snapshot.id, &status, &mut txn).await.unwrap();
        txn.commit().await.unwrap();

        let metadata = Metadata {
            name: "edited".to_string(),
            description: "new description".to_string(),
            labels: [("owner".to_string(), "tenant".to_string())].into(),
        };
        let mut txn = pool.begin().await.unwrap();
        let ConditionalWrite::Applied(edited) =
            update_metadata(snapshot.id, snapshot.version, &metadata, &mut txn)
                .await
                .unwrap()
        else {
            panic!("a controller status write must not invalidate the metadata revision");
        };
        txn.commit().await.unwrap();
        assert_eq!(edited.id, snapshot.id);
        assert_eq!(
            edited.version.version_nr(),
            snapshot.version.version_nr() + 1
        );
        assert_eq!(edited.status, status);

        // This controller snapshot predates the metadata edit. Saving its next
        // observation must not restore the metadata or its old version.
        let mut status = snapshot.status.clone();
        status.as_mut().unwrap().partition = Some("later observation".to_string());
        let mut txn = pool.begin().await.unwrap();
        update_status(snapshot.id, &status, &mut txn).await.unwrap();
        txn.commit().await.unwrap();

        let persisted = find_by(&pool, ObjectColumnFilter::One(IdColumn, &snapshot.id))
            .await
            .unwrap()
            .remove(0);
        assert_eq!(persisted.metadata, metadata);
        assert_eq!(persisted.version, edited.version);
        assert_eq!(persisted.status, status);
        assert_eq!(
            persisted.controller_state.version,
            snapshot.controller_state.version
        );
        assert_eq!(
            persisted.config.tenant_organization_id,
            snapshot.config.tenant_organization_id
        );
        assert_eq!(persisted.config.pkey, snapshot.config.pkey);
    }
}
