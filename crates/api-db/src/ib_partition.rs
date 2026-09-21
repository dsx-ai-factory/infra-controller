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
    IBPartition, IBPartitionControllerState, IBPartitionStatus, NewIBPartition, PartitionKey,
};
use model::metadata::Metadata;
use model::resource_pool::{OwnerType, ResourcePool, ResourcePoolEntryState};
use sqlx::{FromRow, PgConnection};

use crate::db_read::DbReader;
use crate::{
    ColumnInfo, ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult,
    FilterableQueryBuilder, ObjectColumnFilter, Transaction,
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

/// `delete_and_release_pkey` deletes the expected partition and releases any
/// allocated PKey reservation in the same transaction. A missing partition or
/// changed status PKey returns `NotApplied(ControllerStateNotCurrent)` without
/// touching the reservation.
///
/// A missing or free reservation needs no release. An allocation belonging to
/// another owner type is an error. Any error discards this operation's changes;
/// `Applied(id)` still requires the caller to commit its outer transaction.
pub async fn delete_and_release_pkey(
    partition_id: IBPartitionId,
    expected_pkey: PartitionKey,
    pkey_pool: &ResourcePool<u16>,
    txn: &mut PgConnection,
) -> Result<ConditionalWrite<IBPartitionId, ControllerStateNotCurrent>, DatabaseError> {
    let mut inner_txn = Transaction::begin_inner(txn).await?;
    // Older migrations cleared the separate `pkey` column after moving the
    // value into `status`. Compare the same PKey the controller reads.
    let query = "DELETE FROM ib_partitions
                 WHERE id=$1::uuid AND status->>'pkey'=$2 RETURNING id";
    let partition: Option<IBPartitionId> = sqlx::query_as(query)
        .bind(partition_id)
        .bind(expected_pkey.to_string())
        .fetch_optional(&mut inner_txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    let Some(partition) = partition else {
        return Ok(ConditionalWrite::NotApplied(ControllerStateNotCurrent));
    };

    // Reservations keep the creation-time name even after a partition rename.
    // Deleting the exact UUID/PKey above authorizes this release, not a match
    // against today's name. The unique status PKey prevents another partition
    // from committing that PKey until this deletion commits.
    // The pool stores decimal values, unlike the hex PKey in `status`.
    let query = "SELECT state FROM resource_pool
                 WHERE name=$1 AND value=$2 FOR UPDATE";
    let allocation: Option<sqlx::types::Json<ResourcePoolEntryState>> = sqlx::query_scalar(query)
        .bind(pkey_pool.name())
        .bind(u16::from(expected_pkey).to_string())
        .fetch_optional(&mut inner_txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    match allocation {
        Some(sqlx::types::Json(ResourcePoolEntryState::Allocated { owner_type, .. })) => {
            if owner_type != OwnerType::IBPartition.to_string() {
                return Err(DatabaseError::FailedPrecondition(format!(
                    "PKey {expected_pkey} in pool `{}` is allocated to {owner_type}, not an IB partition",
                    pkey_pool.name()
                )));
            }
            crate::resource_pool::release(pkey_pool, &mut inner_txn, expected_pkey.into()).await?;
        }
        None | Some(sqlx::types::Json(ResourcePoolEntryState::Free)) => {}
    }
    inner_txn.commit().await?;
    Ok(ConditionalWrite::Applied(partition))
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
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use model::ib_partition::IBPartitionConfig;
    use model::resource_pool::{ResourcePoolError, ValueType};

    use super::*;
    use crate::resource_pool::ResourcePoolDatabaseError;

    async fn create_reserved_partition(
        txn: &mut PgConnection,
        pkey_pool: &ResourcePool<u16>,
        name: &str,
        pkey: PartitionKey,
    ) -> IBPartition {
        crate::resource_pool::populate(pkey_pool, txn, vec![pkey.into()], false)
            .await
            .unwrap();
        crate::resource_pool::allocate(
            pkey_pool,
            txn,
            OwnerType::IBPartition,
            name,
            Some(pkey.into()),
        )
        .await
        .unwrap();
        create(
            NewIBPartition {
                id: IBPartitionId::new(),
                config: IBPartitionConfig {
                    name: name.to_string(),
                    pkey: None,
                    tenant_organization_id: "example".parse().unwrap(),
                    mtu: None,
                    rate_limit: None,
                    service_level: None,
                },
                metadata: Metadata {
                    name: name.to_string(),
                    ..Default::default()
                },
            },
            txn,
            10,
            IBPartitionStatus {
                partition: None,
                mtu: None,
                rate_limit: None,
                service_level: None,
                pkey: Some(pkey),
            },
        )
        .await
        .unwrap()
    }

    async fn persisted_cleanup_records(
        pool: &sqlx::PgPool,
    ) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let partitions =
            sqlx::query_scalar("SELECT to_jsonb(ib_partitions) FROM ib_partitions ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        let allocations = sqlx::query_scalar(
            "SELECT to_jsonb(resource_pool) FROM resource_pool ORDER BY name, value",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        (partitions, allocations)
    }

    #[crate::sqlx_test]
    async fn renamed_partition_cleanup_preserves_reassigned_pkey(pool: sqlx::PgPool) {
        let pkey_pool = ResourcePool::new("ib-cleanup".to_string(), ValueType::Integer);
        let pkey = 42.try_into().unwrap();
        let mut txn = pool.begin().await.unwrap();
        let partition = create_reserved_partition(&mut txn, &pkey_pool, "original", pkey).await;
        // The migration to `status.pkey` cleared this column on older rows.
        sqlx::query("UPDATE ib_partitions SET pkey = NULL WHERE id = $1")
            .bind(partition.id)
            .execute(&mut *txn)
            .await
            .unwrap();
        let renamed_metadata = Metadata {
            name: "renamed".to_string(),
            ..Default::default()
        };
        let ConditionalWrite::Applied(renamed) =
            update_metadata(partition.id, partition.version, &renamed_metadata, &mut txn)
                .await
                .unwrap()
        else {
            panic!("the partition metadata must be current");
        };
        assert_eq!(renamed.metadata, renamed_metadata);
        assert_eq!(renamed.config.pkey, None);
        assert_eq!(renamed.status.as_ref().unwrap().pkey, Some(pkey));
        txn.commit().await.unwrap();

        let mut txn = pool.begin().await.unwrap();
        assert_eq!(
            delete_and_release_pkey(partition.id, pkey, &pkey_pool, &mut txn)
                .await
                .unwrap(),
            ConditionalWrite::Applied(partition.id),
        );

        // `Applied` does not make the PKey reusable until the caller commits.
        let mut allocation_txn = pool.begin().await.unwrap();
        let error = crate::resource_pool::allocate(
            &pkey_pool,
            &mut allocation_txn,
            OwnerType::IBPartition,
            "replacement",
            Some(pkey.into()),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ResourcePoolDatabaseError::ResourcePool(ResourcePoolError::Empty)
        ));
        allocation_txn.rollback().await.unwrap();
        txn.commit().await.unwrap();
        assert!(
            find_by(&pool, ObjectColumnFilter::One(IdColumn, &partition.id))
                .await
                .unwrap()
                .is_empty()
        );
        let allocation = crate::resource_pool::find_value(&pool, &u16::from(pkey).to_string())
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.pool_name == pkey_pool.name())
            .unwrap();
        assert_eq!(allocation.state.0, ResourcePoolEntryState::Free);
        assert_eq!(allocation.allocated, None);

        let mut txn = pool.begin().await.unwrap();
        let replacement =
            create_reserved_partition(&mut txn, &pkey_pool, "replacement", pkey).await;
        assert_ne!(replacement.id, partition.id);
        txn.commit().await.unwrap();
        let before = persisted_cleanup_records(&pool).await;

        let mut txn = pool.begin().await.unwrap();
        assert_eq!(
            delete_and_release_pkey(partition.id, pkey, &pkey_pool, &mut txn)
                .await
                .unwrap(),
            ConditionalWrite::NotApplied(ControllerStateNotCurrent),
        );
        txn.commit().await.unwrap();
        assert_eq!(persisted_cleanup_records(&pool).await, before);
    }

    #[crate::sqlx_test]
    async fn cleanup_without_an_allocated_reservation_preserves_other_records(pool: sqlx::PgPool) {
        let pkey_pool = ResourcePool::new("ib-cleanup".to_string(), ValueType::Integer);
        let mut txn = pool.begin().await.unwrap();
        create_reserved_partition(&mut txn, &pkey_pool, "other", 43.try_into().unwrap()).await;
        txn.commit().await.unwrap();
        check_cases_async(
            [
                Case {
                    scenario: "free reservation",
                    input: Some(ResourcePoolEntryState::Free),
                    expect: Yields(()),
                },
                Case {
                    scenario: "missing reservation",
                    input: None,
                    expect: Yields(()),
                },
            ],
            |reservation| {
                let pool = pool.clone();
                async move {
                    let pkey_pool = ResourcePool::new("ib-cleanup".to_string(), ValueType::Integer);
                    let pkey = 42.try_into().unwrap();
                    let mut txn = pool.begin().await.unwrap();
                    let partition =
                        create_reserved_partition(&mut txn, &pkey_pool, "original", pkey).await;
                    match reservation {
                        Some(state) => {
                            sqlx::query("UPDATE resource_pool SET state = $1, allocated = NULL WHERE name = $2 AND value = '42'")
                                .bind(sqlx::types::Json(state))
                                .bind(pkey_pool.name())
                                .execute(&mut *txn)
                                .await
                                .unwrap();
                        }
                        None => {
                            sqlx::query("DELETE FROM resource_pool WHERE name = $1 AND value = '42'")
                                .bind(pkey_pool.name())
                                .execute(&mut *txn)
                                .await
                                .unwrap();
                        }
                    }
                    txn.commit().await.unwrap();
                    let (mut partitions, allocations) = persisted_cleanup_records(&pool).await;
                    partitions.retain(|row| row["id"] != partition.id.to_string());

                    let mut txn = pool.begin().await.unwrap();
                    let result = delete_and_release_pkey(partition.id, pkey, &pkey_pool, &mut txn)
                        .await
                        .map_err(|error| error.to_string())?;
                    assert_eq!(result, ConditionalWrite::Applied(partition.id));
                    txn.commit().await.unwrap();
                    assert_eq!(persisted_cleanup_records(&pool).await, (partitions, allocations));
                    Ok::<(), String>(())
                }
            },
        )
        .await;
    }

    #[crate::sqlx_test]
    async fn cleanup_rejects_another_partitions_pkey(pool: sqlx::PgPool) {
        let pkey_pool = ResourcePool::new("ib-cleanup".to_string(), ValueType::Integer);
        let mut txn = pool.begin().await.unwrap();
        let first =
            create_reserved_partition(&mut txn, &pkey_pool, "first", 42.try_into().unwrap()).await;
        let other_pkey = 43.try_into().unwrap();
        create_reserved_partition(&mut txn, &pkey_pool, "other", other_pkey).await;
        txn.commit().await.unwrap();
        let before = persisted_cleanup_records(&pool).await;

        let mut txn = pool.begin().await.unwrap();
        assert_eq!(
            delete_and_release_pkey(first.id, other_pkey, &pkey_pool, &mut txn)
                .await
                .unwrap(),
            ConditionalWrite::NotApplied(ControllerStateNotCurrent),
        );
        txn.commit().await.unwrap();
        assert_eq!(persisted_cleanup_records(&pool).await, before);
    }

    #[crate::sqlx_test]
    async fn cleanup_release_failure_preserves_partition_after_commit(pool: sqlx::PgPool) {
        let pkey_pool = ResourcePool::new("ib-cleanup".to_string(), ValueType::Integer);
        let pkey = 42.try_into().unwrap();
        let mut txn = pool.begin().await.unwrap();
        let partition = create_reserved_partition(&mut txn, &pkey_pool, "original", pkey).await;
        sqlx::query(
            "ALTER TABLE resource_pool ADD CONSTRAINT reject_pkey_release \
             CHECK (allocated IS NOT NULL) NOT VALID",
        )
        .execute(&mut *txn)
        .await
        .unwrap();
        txn.commit().await.unwrap();
        let before = persisted_cleanup_records(&pool).await;

        let mut txn = pool.begin().await.unwrap();
        let error = delete_and_release_pkey(partition.id, pkey, &pkey_pool, &mut txn)
            .await
            .unwrap_err();
        let DatabaseError::Sqlx(error) = error else {
            panic!("expected the injected release constraint failure, got {error:?}");
        };
        assert_eq!(
            error.source.as_database_error().unwrap().constraint(),
            Some("reject_pkey_release"),
        );
        // Commit instead of rolling back: the helper's savepoint must undo
        // the partition deletion even when the caller keeps its transaction.
        txn.commit().await.unwrap();
        assert_eq!(persisted_cleanup_records(&pool).await, before);
    }

    #[crate::sqlx_test]
    async fn cleanup_wrong_owner_type_preserves_partition_after_commit(pool: sqlx::PgPool) {
        let pkey_pool = ResourcePool::new("ib-cleanup".to_string(), ValueType::Integer);
        let pkey = 42.try_into().unwrap();
        let mut txn = pool.begin().await.unwrap();
        let partition = create_reserved_partition(&mut txn, &pkey_pool, "original", pkey).await;
        sqlx::query("UPDATE resource_pool SET state = $1 WHERE name = $2 AND value = $3")
            .bind(sqlx::types::Json(ResourcePoolEntryState::Allocated {
                owner: "original".to_string(),
                owner_type: OwnerType::Machine.to_string(),
            }))
            .bind(pkey_pool.name())
            .bind(u16::from(pkey).to_string())
            .execute(&mut *txn)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let before = persisted_cleanup_records(&pool).await;

        let mut txn = pool.begin().await.unwrap();
        let error = delete_and_release_pkey(partition.id, pkey, &pkey_pool, &mut txn)
            .await
            .unwrap_err();
        assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
        txn.commit().await.unwrap();
        assert_eq!(persisted_cleanup_records(&pool).await, before);
    }

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
