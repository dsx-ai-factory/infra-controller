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

use std::future::{Future, ready};
use std::time::Duration;

use carbide_uuid::infiniband::IBPartitionId;
use config_version::ConfigVersion;
use futures::future::Either;
use futures::{FutureExt, StreamExt};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::ib_partition::{
    IBPartition, IBPartitionControllerState, IBPartitionStatus, NewIBPartition, PartitionKey,
};
use model::metadata::Metadata;
use model::resource_pool::{OwnerType, ResourcePool, ResourcePoolEntryState};
use sqlx::{FromRow, PgConnection};

use crate::db_read::DbReader;
use crate::resource_pool::ResourcePoolAllocationNotOwned;
use crate::{
    ColumnInfo, ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult,
    FilterableQueryBuilder, ObjectColumnFilter, Transaction,
};

#[cfg(test)]
mod test_explicit_columns;

// Match the prerequisite database's two-minute lock limit, but cap the whole
// admission attempt rather than restarting that budget for every row lock.
const ADMISSION_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(120);

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
            RETURNING id, name, labels, description, pkey, organization_id, mtu, rate_limit,
                      service_level, config_version, controller_state_version, controller_state,
                      controller_state_outcome, status, deleted";
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
        let query = "SELECT id, name, labels, description, pkey, organization_id, mtu, rate_limit,
                            service_level, config_version, controller_state_version, controller_state,
                            controller_state_outcome, status, deleted
                     FROM ib_partitions WHERE organization_id=$1";
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
    let mut query = FilterableQueryBuilder::new(
        "SELECT id, name, labels, description, pkey, organization_id, mtu, rate_limit,
                service_level, config_version, controller_state_version, controller_state,
                controller_state_outcome, status, deleted
         FROM ib_partitions",
    )
    .filter(&filter);

    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

/// `find_active_for_share` locks active partitions for Instance admission.
/// Missing and deleted IDs are omitted; duplicate IDs are locked once, in ID
/// order. Keep the transaction open through the Instance write so deletion
/// cannot release a PKey between validation and persistence. `mark_as_deleted`
/// also waits for these locks; admission waiting behind its UPDATE
/// rechecks `deleted IS NULL` after it commits.
///
/// A cumulative two-minute client deadline covers setting the statement cap,
/// the locking query, and successful restoration of the prior cap. The borrowed
/// connection is already acquired. PostgreSQL separately caps each statement
/// after setup at the smaller of two minutes and a nonzero operator
/// `statement_timeout`; that per-statement budget can outlive the client
/// deadline. Success restores the prior setting. On any error, abandon the
/// caller's transaction: client cancellation alone does not cancel SQL, and
/// rollback can wait for the server cap or communication to recover.
pub async fn find_active_for_share(
    txn: &mut PgConnection,
    partition_ids: &[IBPartitionId],
) -> DatabaseResult<Vec<IBPartition>> {
    find_active_for_share_with_timeout(txn, partition_ids, ADMISSION_ATTEMPT_TIMEOUT).await
}

fn find_active_for_share_with_timeout<'a>(
    txn: &'a mut PgConnection,
    partition_ids: &'a [IBPartitionId],
    attempt_timeout: Duration,
) -> impl Future<Output = DatabaseResult<Vec<IBPartition>>> + Send + 'a {
    if partition_ids.is_empty() {
        return Either::Left(ready(Ok(Vec::new())));
    }

    let timeout_ms = attempt_timeout.as_millis();
    if !(1..=i32::MAX as u128).contains(&timeout_ms) {
        return Either::Left(ready(Err(DatabaseError::InvalidArgument(
            "statement timeout must be between 1 and 2147483647 milliseconds".into(),
        ))));
    }

    Either::Right(
        tokio::time::timeout(attempt_timeout, async move {
            // `pg_settings` reports milliseconds. Capture the prior setting while
            // installing the cap, using NULLIF so zero does not disable it.
            let query = "SELECT setting, set_config('statement_timeout',
                         LEAST(NULLIF(setting::bigint, 0), $1::text::bigint)::text, true)
                     FROM pg_settings WHERE name = 'statement_timeout'";
            let (prior_timeout, _): (String, String) = sqlx::query_as(query)
                .bind(timeout_ms.to_string())
                .fetch_one(&mut *txn)
                .await
                .map_err(|error| DatabaseError::query(query, error))?;

            let query =
                "SELECT id, name, labels, description, pkey, organization_id, mtu, rate_limit,
                        service_level, config_version, controller_state_version, controller_state,
                        controller_state_outcome, status, deleted
                 FROM ib_partitions
                 WHERE id = ANY($1) AND deleted IS NULL
                 ORDER BY id FOR SHARE";
            let partitions = sqlx::query_as(query)
                .bind(partition_ids)
                .fetch_all(&mut *txn)
                .await
                .map_err(|error| DatabaseError::query(query, error))?;

            let query = "SELECT set_config('statement_timeout', $1, true)";
            sqlx::query(query)
                .bind(prior_timeout)
                .execute(&mut *txn)
                .await
                .map_err(|error| DatabaseError::query(query, error))?;
            Ok(partitions)
        })
        .map(|result| {
            result.map_err(|_| {
                DatabaseError::query(
                    "ib_partition::find_active_for_share",
                    sqlx::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "IB partition admission attempt timed out",
                    )),
                )
            })?
        }),
    )
}

/// `lock_for_delete` locks the partition through the final recount, removal,
/// and PKey release, including deleted rows. Before final deletion,
/// `mark_as_deleted` has excluded new admissions; cleanup can remove references.
/// A missing partition returns `NotApplied(ControllerStateNotCurrent)`; SQL
/// failures remain errors. Keep the caller's transaction open through deletion.
pub async fn lock_for_delete(
    txn: &mut PgConnection,
    partition_id: IBPartitionId,
) -> DatabaseResult<ConditionalWrite<(), ControllerStateNotCurrent>> {
    let query = "SELECT id FROM ib_partitions WHERE id = $1 FOR UPDATE";
    let partition = sqlx::query_scalar::<_, IBPartitionId>(query)
        .bind(partition_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(match partition {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(ControllerStateNotCurrent),
    })
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
    let query = "UPDATE ib_partitions SET updated=NOW(), deleted=NOW() WHERE id=$1
                 RETURNING id, name, labels, description, pkey, organization_id, mtu, rate_limit,
                           service_level, config_version, controller_state_version, controller_state,
                           controller_state_outcome, status, deleted";
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
        Some(sqlx::types::Json(ResourcePoolEntryState::Allocated { owner, owner_type })) => {
            if owner_type != OwnerType::IBPartition.to_string() {
                return Err(DatabaseError::FailedPrecondition(format!(
                    "PKey {expected_pkey} in pool `{}` is allocated to {owner_type}, not an IB partition",
                    pkey_pool.name()
                )));
            }
            match crate::resource_pool::release(
                pkey_pool,
                &mut inner_txn,
                expected_pkey.into(),
                OwnerType::IBPartition,
                &owner,
            )
            .await?
            {
                ConditionalWrite::Applied(()) => {}
                ConditionalWrite::NotApplied(ResourcePoolAllocationNotOwned) => {
                    return Err(DatabaseError::FailedPrecondition(format!(
                        "PKey {expected_pkey} in pool `{}` did not match the locked IB reservation for owner `{owner}`",
                        pkey_pool.name()
                    )));
                }
            }
        }
        None | Some(sqlx::types::Json(ResourcePoolEntryState::Free)) => {}
    }
    inner_txn.commit().await?;
    Ok(ConditionalWrite::Applied(partition))
}

/// Counts Instances referencing a partition in `ib_config`, including deleted
/// Instances whose cleanup has not removed their row. Final deletion must hold
/// `lock_for_delete` and count in a separate READ COMMITTED statement after the
/// lock wait, so cleanup committed during that wait is visible.
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
                 WHERE id=$5::uuid AND config_version=$6
                 RETURNING id, name, labels, description, pkey, organization_id, mtu, rate_limit,
                           service_level, config_version, controller_state_version, controller_state,
                           controller_state_outcome, status, deleted";

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
                 WHERE id=$2::uuid
                 RETURNING id, name, labels, description, pkey, organization_id, mtu, rate_limit,
                           service_level, config_version, controller_state_version, controller_state,
                           controller_state_outcome, status, deleted";

    sqlx::query_as(query)
        .bind(sqlx::types::Json(status))
        .bind(partition_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use model::ib_partition::IBPartitionConfig;
    use model::resource_pool::{ResourcePoolError, ValueType};

    use super::*;
    use crate::resource_pool::ResourcePoolDatabaseError;
    use crate::test_support::postgres::{count_blocked_queries, wait_for_blocked_query};

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
    async fn missing_delete_lock_invalidates_the_iteration(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        assert_eq!(
            lock_for_delete(&mut txn, IBPartitionId::new())
                .await
                .unwrap(),
            ConditionalWrite::NotApplied(ControllerStateNotCurrent),
        );
        txn.commit().await.unwrap();
    }

    /// Timeout must end the blocked SQL and roll back earlier work while the
    /// conflicting partition is still locked. A shorter operator cap stays in
    /// force, and successful admission restores the caller's prior setting.
    #[crate::sqlx_test]
    async fn admission_deadline_rolls_back_and_preserves_statement_policy(pool: sqlx::PgPool) {
        use sqlx::Connection;

        let pkey_pool = ResourcePool::new("ib-admission".to_string(), ValueType::Integer);
        let mut txn = pool.begin().await.unwrap();
        let target =
            create_reserved_partition(&mut txn, &pkey_pool, "target", 42.try_into().unwrap()).await;
        let sentinel =
            create_reserved_partition(&mut txn, &pkey_pool, "sentinel", 43.try_into().unwrap())
                .await;
        txn.commit().await.unwrap();

        for (scenario, operator_ms, attempt_timeout, completion_timeout) in [
            (
                "internal cap",
                0_i64,
                Duration::from_secs(2),
                Duration::from_secs(10),
            ),
            (
                "tighter operator cap",
                2000,
                Duration::from_secs(10),
                Duration::from_secs(5),
            ),
        ] {
            let mut successful = pool.begin().await.unwrap();
            sqlx::query("SELECT set_config('statement_timeout', $1, true)")
                .bind(operator_ms.to_string())
                .execute(successful.as_mut())
                .await
                .unwrap();
            let admitted =
                find_active_for_share_with_timeout(&mut successful, &[target.id], attempt_timeout)
                    .await
                    .unwrap();
            assert_eq!(admitted.len(), 1);
            let restored_ms: i64 = sqlx::query_scalar(
                "SELECT setting::bigint FROM pg_settings WHERE name = 'statement_timeout'",
            )
            .fetch_one(successful.as_mut())
            .await
            .unwrap();
            assert_eq!(
                restored_ms, operator_ms,
                "{scenario}: successful admission restores policy"
            );
            successful.commit().await.unwrap();

            let mut blocker = pool.begin().await.unwrap();
            assert_eq!(
                lock_for_delete(&mut blocker, target.id).await.unwrap(),
                ConditionalWrite::Applied(()),
            );
            let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(blocker.as_mut())
                .await
                .unwrap();
            let admission = async {
                let mut connection = pool
                    .acquire()
                    .await
                    .expect("acquire bounded admission connection");
                let mut txn = connection.begin().await.expect("begin bounded admission");
                sqlx::query("SELECT set_config('statement_timeout', $1, true)")
                    .bind(operator_ms.to_string())
                    .execute(txn.as_mut())
                    .await
                    .unwrap();
                sqlx::query(
                    "UPDATE ib_partitions SET name = 'uncommitted-admission' WHERE id = $1",
                )
                .bind(sentinel.id)
                .execute(txn.as_mut())
                .await
                .expect("stage an unrelated write before admission");
                let error =
                    find_active_for_share_with_timeout(&mut txn, &[target.id], attempt_timeout)
                        .await
                        .expect_err("blocked admission must respect its deadline");
                // If the client timer wins, rollback can first receive the
                // pending statement-timeout error. SQLx then queues ROLLBACK
                // on transaction drop; retain the connection and flush it.
                if let Err(rollback_error) = txn.rollback().await {
                    assert!(
                        matches!(&error, DatabaseError::Sqlx(error)
                            if matches!(&error.source, sqlx::Error::Io(error)
                                if error.kind() == std::io::ErrorKind::TimedOut)),
                        "only a client timeout may leave a pending SQL cancellation: {rollback_error:?}",
                    );
                    let source = rollback_error
                        .as_database_error()
                        .expect("rollback only tolerates the pending PostgreSQL cancellation");
                    assert_eq!(source.code().as_deref(), Some("57014"));
                    connection
                        .ping()
                        .await
                        .expect("flush the queued rollback while the blocker remains held");
                }
                error
            };
            tokio::pin!(admission);
            tokio::select! {
                error = &mut admission => panic!("{scenario}: admission did not reach its lock wait: {error:?}"),
                _ = wait_for_blocked_query(&pool, blocker_pid, "FOR SHARE") => {}
            }
            let error = tokio::time::timeout(completion_timeout, admission)
                .await
                .expect("admission and rollback finish while the blocker remains held");
            match error {
                DatabaseError::Sqlx(error) => match error.source {
                    sqlx::Error::Io(error) => {
                        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{scenario}");
                    }
                    sqlx::Error::Database(error) => {
                        assert_eq!(error.code().as_deref(), Some("57014"), "{scenario}");
                    }
                    error => panic!("{scenario}: unexpected SQLx error: {error:?}"),
                },
                error => panic!("{scenario}: unexpected admission error: {error:?}"),
            }
            assert_eq!(
                count_blocked_queries(&pool, blocker_pid, "FOR SHARE").await,
                0
            );
            let persisted = find_by(&pool, ObjectColumnFilter::One(IdColumn, &sentinel.id))
                .await
                .unwrap()
                .remove(0);
            assert_eq!(persisted.metadata.name, "sentinel");
            sqlx::query("SELECT id FROM ib_partitions WHERE id = $1 FOR UPDATE NOWAIT")
                .bind(sentinel.id)
                .fetch_one(&pool)
                .await
                .expect("rollback released the earlier unrelated row lock");
            assert_eq!(
                count_instances_referencing_partition(&pool, target.id)
                    .await
                    .unwrap(),
                0
            );

            blocker.rollback().await.unwrap();
            let mut retry = pool.begin().await.unwrap();
            let admitted = find_active_for_share(&mut retry, &[target.id])
                .await
                .unwrap();
            assert_eq!(admitted.len(), 1);
            assert_eq!(admitted[0].id, target.id);
            retry.commit().await.unwrap();
        }
    }

    /// Admission waits for an uncommitted deletion, then omits either the
    /// updated tombstone or the physically removed row after deletion commits.
    #[crate::sqlx_test]
    async fn deletion_before_admission_omits_partition(pool: sqlx::PgPool) {
        #[derive(Debug)]
        enum DeletionKind {
            Soft,
            Physical,
        }

        let pkey_pool = ResourcePool::new("ib-admission".to_string(), ValueType::Integer);
        for (kind, name, pkey) in [
            (DeletionKind::Soft, "soft-delete", 42u16),
            (DeletionKind::Physical, "physical-delete", 43),
        ] {
            let pkey = pkey.try_into().unwrap();
            let mut txn = pool.begin().await.unwrap();
            let partition = create_reserved_partition(&mut txn, &pkey_pool, name, pkey).await;
            txn.commit().await.unwrap();

            let mut deletion = pool.begin().await.unwrap();
            match kind {
                DeletionKind::Soft => {
                    mark_as_deleted(&partition, &mut deletion).await.unwrap();
                }
                DeletionKind::Physical => {
                    assert_eq!(
                        lock_for_delete(&mut deletion, partition.id).await.unwrap(),
                        ConditionalWrite::Applied(()),
                    );
                    assert_eq!(
                        delete_and_release_pkey(partition.id, pkey, &pkey_pool, &mut deletion)
                            .await
                            .unwrap(),
                        ConditionalWrite::Applied(partition.id),
                    );
                }
            }
            let deletion_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(deletion.as_mut())
                .await
                .unwrap();
            let admission = async {
                let mut txn = pool.begin().await.expect("begin waiting admission");
                let admitted = find_active_for_share(&mut txn, &[partition.id])
                    .await
                    .expect("read partition after the deletion wait");
                txn.commit().await.expect("commit waiting admission");
                admitted
            };
            tokio::pin!(admission);
            tokio::select! {
                result = &mut admission => panic!("admission passed {kind:?} deletion: {result:?}"),
                _ = wait_for_blocked_query(&pool, deletion_pid, "FOR SHARE") => {}
            }

            deletion.commit().await.unwrap();
            let admitted = tokio::time::timeout(Duration::from_secs(30), admission)
                .await
                .expect("admission resumes after deletion commits");
            assert!(admitted.is_empty(), "{kind:?} deletion rejects admission");
        }
    }

    /// Admission locks the lower UUID before waiting on the higher UUID,
    /// regardless of input order, while omitted rows remain unlocked.
    #[crate::sqlx_test]
    async fn admission_locks_exact_active_partitions_in_id_order(pool: sqlx::PgPool) {
        let [high_id, low_id, deleted_id, unrelated_id, missing_id] =
            [5, 4, 3, 2, 1].map(|value| IBPartitionId::from(uuid::Uuid::from_u128(value)));
        let mut txn = pool.begin().await.unwrap();
        for (id, name, pkey, deleted) in [
            (high_id, "higher", 42u16, false),
            (deleted_id, "deleted", 44, true),
            (unrelated_id, "unrelated", 45, false),
            (low_id, "lower", 43, false),
        ] {
            let partition = create(
                NewIBPartition {
                    id,
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
                &mut txn,
                10,
                IBPartitionStatus {
                    partition: None,
                    mtu: None,
                    rate_limit: None,
                    service_level: None,
                    pkey: Some(pkey.try_into().unwrap()),
                },
            )
            .await
            .unwrap();
            if deleted {
                mark_as_deleted(&partition, &mut txn).await.unwrap();
            }
        }
        txn.commit().await.unwrap();

        let mut guard = pool.begin().await.unwrap();
        assert_eq!(
            lock_for_delete(&mut guard, high_id).await.unwrap(),
            ConditionalWrite::Applied(()),
        );
        let guard_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(guard.as_mut())
            .await
            .unwrap();
        let admission = async {
            let mut txn = pool.begin().await.expect("begin ordered admission");
            // The higher UUID was deliberately inserted first. Force a
            // sequential scan so only ORDER BY can lock the lower UUID first.
            sqlx::raw_sql("SET LOCAL enable_indexscan = off; SET LOCAL enable_bitmapscan = off")
                .execute(txn.as_mut())
                .await
                .expect("force sequential scan for the ordering proof");
            let partitions =
                find_active_for_share(&mut txn, &[high_id, low_id, low_id, missing_id, deleted_id])
                    .await
                    .expect("read partitions in lock order");
            txn.commit().await.expect("commit ordered admission");
            partitions
                .into_iter()
                .map(|partition| partition.id)
                .collect::<Vec<_>>()
        };
        tokio::pin!(admission);
        tokio::select! {
            result = &mut admission => panic!("admission passed the higher ID lock: {result:?}"),
            _ = wait_for_blocked_query(&pool, guard_pid, "FOR SHARE") => {}
        }

        sqlx::query("SELECT id FROM ib_partitions WHERE id = $1 FOR SHARE NOWAIT")
            .bind(low_id)
            .fetch_one(&pool)
            .await
            .expect("another share lock is compatible with admission's lower UUID lock");

        // The request lists the higher ID first, but must already hold the
        // lower ID lock when it waits. Omitted rows must remain unlocked.
        for (id, expected_locked) in [(low_id, true), (deleted_id, false), (unrelated_id, false)] {
            let mut probe = pool.begin().await.unwrap();
            let result =
                sqlx::query("SELECT id FROM ib_partitions WHERE id = $1 FOR UPDATE NOWAIT")
                    .bind(id)
                    .fetch_one(probe.as_mut())
                    .await;
            if expected_locked {
                let error = result.expect_err("the lower ID is locked before the higher ID wait");
                assert_eq!(
                    error.as_database_error().unwrap().code().as_deref(),
                    Some("55P03")
                );
            } else {
                result.expect("unrequested and deleted partitions remain unlocked");
            }
            probe.rollback().await.unwrap();
        }

        guard.commit().await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(30), admission)
                .await
                .expect("admission resumes after the higher ID lock is released"),
            [low_id, high_id]
        );
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
                        Some(_) => {
                            assert_eq!(
                                crate::resource_pool::release(
                                    &pkey_pool,
                                    &mut txn,
                                    pkey.into(),
                                    OwnerType::IBPartition,
                                    "original",
                                )
                                .await
                                .unwrap(),
                                ConditionalWrite::Applied(())
                            );
                        }
                        None => {
                            sqlx::query(
                                "DELETE FROM resource_pool WHERE name = $1 AND value = '42'",
                            )
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
                    assert_eq!(
                        persisted_cleanup_records(&pool).await,
                        (partitions, allocations)
                    );
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
