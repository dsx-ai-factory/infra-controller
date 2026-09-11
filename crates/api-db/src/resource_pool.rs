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
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use config_version::ConfigVersion;
use ipnetwork::Ipv6Network;
use model::resource_pool;
use model::resource_pool::common::{
    CommonPools, DPA_VNI, EXTERNAL_VPC_VNI, EthernetPools, FNN_ASN, IbPools, LOOPBACK_IP,
    LOOPBACK_IP_V6, VLANID, VNI, VPC_DPU_LOOPBACK, VPC_VNI,
};
use model::resource_pool::define::{ResourcePoolDef, ResourcePoolType};
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolEntry, ResourcePoolEntryState, ResourcePoolError,
    ResourcePoolSnapshot, ResourcePoolStats, ValueType,
};
use sqlx::{PgConnection, Postgres};
use tokio::sync::oneshot;

use super::BIND_LIMIT;
use crate::DatabaseError;
use crate::config_drift::{ConfigDefinitionDrifted, ConfigDriftKind, ConfigResourceKind};
use crate::db_read::DbReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum ResourcePoolOperation {
    Allocate,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum ResourcePoolFailure {
    Exhausted,
    RequestedValueUnavailable,
    Parse,
    Database,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum ResourcePoolFailurePolicy {
    Required,
    BestEffort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum ResourcePoolAllocationMode {
    Automatic,
    Requested,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum ResourcePoolValueType {
    Integer,
    Ipv4,
    Ipv6,
    Ipv6Prefix,
}

impl From<ValueType> for ResourcePoolValueType {
    fn from(value: ValueType) -> Self {
        match value {
            ValueType::Integer => Self::Integer,
            ValueType::Ipv4 => Self::Ipv4,
            ValueType::Ipv6 => Self::Ipv6,
            ValueType::Ipv6Prefix => Self::Ipv6Prefix,
        }
    }
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_resource_pool_lifecycle_failures_total",
    kind = counter,
    component = "nico-api",
    describe = "Number of resource pool lifecycle failures, by operation, failure, failure policy, allocation mode, and value type."
)]
struct ResourcePoolLifecycleFailures {
    operation: ResourcePoolOperation,
    failure: ResourcePoolFailure,
    failure_policy: ResourcePoolFailurePolicy,
    allocation_mode: ResourcePoolAllocationMode,
    value_type: ResourcePoolValueType,
}

// These Events record the counter the family above declares. Existing
// allocation diagnostics retain their messages and context; `release` gains
// its first diagnostic at this database boundary.
#[derive(Event)]
#[event(
    event_name = "resource_pool_exhausted",
    metric_family = ResourcePoolLifecycleFailures,
    log = error,
    message = "Pool exhausted, cannot allocate"
)]
struct ResourcePoolExhausted {
    #[label]
    operation: ResourcePoolOperation,
    #[label]
    failure: ResourcePoolFailure,
    #[label]
    failure_policy: ResourcePoolFailurePolicy,
    #[label]
    allocation_mode: ResourcePoolAllocationMode,
    #[label]
    value_type: ResourcePoolValueType,
    #[context]
    owner_id: String,
    #[context]
    pool: String,
}

#[derive(Event)]
#[event(
    event_name = "resource_pool_requested_vni_unavailable",
    metric_family = ResourcePoolLifecycleFailures,
    log = error,
    message = "invalid pool value requested, cannot allocate"
)]
struct ResourcePoolRequestedVniUnavailable {
    #[label]
    operation: ResourcePoolOperation,
    #[label]
    failure: ResourcePoolFailure,
    #[label]
    failure_policy: ResourcePoolFailurePolicy,
    #[label]
    allocation_mode: ResourcePoolAllocationMode,
    #[label]
    value_type: ResourcePoolValueType,
    #[context]
    owner_id: String,
    #[context]
    pool: String,
    #[context(value)]
    requested_vni: i64,
}

#[derive(Event)]
#[event(
    event_name = "resource_pool_allocation_failed",
    metric_family = ResourcePoolLifecycleFailures,
    log = error,
    message = "Error allocating from resource pool"
)]
struct ResourcePoolAllocationFailed {
    #[label]
    operation: ResourcePoolOperation,
    #[label]
    failure: ResourcePoolFailure,
    #[label]
    failure_policy: ResourcePoolFailurePolicy,
    #[label]
    allocation_mode: ResourcePoolAllocationMode,
    #[label]
    value_type: ResourcePoolValueType,
    #[context]
    owner_id: String,
    #[context]
    error: String,
    #[context]
    pool: String,
}

#[derive(Event)]
#[event(
    event_name = "resource_pool_release_failed",
    metric_family = ResourcePoolLifecycleFailures,
    log = error,
    message = "Error releasing value to resource pool"
)]
struct ResourcePoolReleaseFailed {
    #[label]
    operation: ResourcePoolOperation,
    #[label]
    failure: ResourcePoolFailure,
    #[label]
    failure_policy: ResourcePoolFailurePolicy,
    #[label]
    allocation_mode: ResourcePoolAllocationMode,
    #[label]
    value_type: ResourcePoolValueType,
    #[context]
    error: String,
    #[context]
    pool: String,
    #[context]
    value: String,
}

#[derive(Event)]
#[event(
    event_name = "dpu_asn_allocation_failed",
    metric_family = ResourcePoolLifecycleFailures,
    log = info,
    message = "Failed to allocate asn for dpu"
)]
struct DpuAsnAllocationFailed {
    #[label]
    operation: ResourcePoolOperation,
    #[label]
    failure: ResourcePoolFailure,
    #[label]
    failure_policy: ResourcePoolFailurePolicy,
    #[label]
    allocation_mode: ResourcePoolAllocationMode,
    #[label]
    value_type: ResourcePoolValueType,
    #[context]
    stable_machine_id: String,
    #[context]
    error: String,
}

/// Put some resources into the pool, so they can be allocated later.
/// This needs to be called before `allocate` can return anything.
pub async fn populate<T>(
    value: &ResourcePool<T>,
    txn: &mut PgConnection,
    all_values: Vec<T>,
    auto_assign: bool,
) -> Result<(), DatabaseError>
where
    T: ToString + FromStr + Send + Sync + 'static,
    <T as FromStr>::Err: std::error::Error,
{
    let free_state = ResourcePoolEntryState::Free;
    let initial_version = ConfigVersion::initial();

    // Divide the bind limit by the number of parameters we're inserting in each tuple (currently 6)
    for vals in all_values.chunks(BIND_LIMIT / 6) {
        let query = "INSERT INTO resource_pool(name, value, value_type, state, state_version, auto_assign) ";
        let mut qb = sqlx::QueryBuilder::new(query);
        qb.push_values(vals.iter(), |mut b, v| {
            b.push_bind(&value.name)
                .push_bind(v.to_string())
                .push_bind(value.value_type)
                .push_bind(sqlx::types::Json(&free_state))
                .push_bind(initial_version)
                .push_bind(auto_assign);
        });
        qb.push("ON CONFLICT (name, value) DO NOTHING");
        let q = qb.build();
        q.execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
    }
    Ok(())
}

/// `allocate` reserves a value from the automatic or explicit assignment partition.
/// An omitted `requested_value` requires `auto_assign=true`; an exact request
/// requires `auto_assign=false`. Use [`allocate_exact`] when an exact request
/// may claim a free value from either partition.
pub async fn allocate<T>(
    value: &ResourcePool<T>,
    txn: &mut PgConnection,
    owner_type: OwnerType,
    owner_id: &str,
    requested_value: Option<T>,
) -> Result<T, ResourcePoolDatabaseError>
where
    T: ToString + FromStr + Send + Sync + 'static,
    <T as FromStr>::Err: std::error::Error,
{
    let auto_assign = requested_value.is_none();

    let query = "
WITH allocate AS (
 SELECT id, value FROM resource_pool
    WHERE
        name = $1
        AND state = $2
        AND auto_assign=$4
        -- Either auto_assign is true or we only want
        -- the requested value.
        AND (auto_assign='t' OR value=$5)
    ORDER BY random()
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
UPDATE resource_pool SET
    state=$3,
    allocated=NOW()
FROM allocate
WHERE resource_pool.id = allocate.id
RETURNING allocate.value
";
    let free_state = ResourcePoolEntryState::Free;
    let allocated_state = ResourcePoolEntryState::Allocated {
        owner: owner_id.to_string(),
        owner_type: owner_type.to_string(),
    };

    let req = requested_value.map(|v| v.to_string());
    // TODO: We should probably update the `state_version` field too. But
    // it's hard to do this inside the SQL query.
    let allocation = sqlx::query_scalar::<_, String>(query)
        .bind(&value.name)
        .bind(sqlx::types::Json(&free_state))
        .bind(sqlx::types::Json(&allocated_state))
        .bind(auto_assign)
        .bind(&req)
        .fetch_one(&mut *txn)
        .await;

    // A successful allocation is a single round-trip. The pool-state accounting
    // (`stats`) is consulted only when the allocation statement returns no row,
    // so it never burdens the common success path — while the error it produces
    // is exactly what an up-front pre-scan would have returned.
    //
    // Concurrency note: the accounting now reads pool state at failure time
    // rather than before the allocation attempt, so a caller losing a race
    // against a concurrent drain reports the pool as it actually is when the
    // attempt fails (Empty) rather than as it looked a moment earlier
    // (FailedPrecondition or a generic no-row error). Which racer sees which
    // error was never deterministic; the at-failure reading is the more
    // truthful of the two orderings.
    let allocated: String = match allocation {
        Ok(row) => row,
        Err(sqlx::Error::RowNotFound) => {
            let stats = stats(&mut *txn, value.name()).await?;
            let free = if auto_assign {
                stats.auto_assign_free
            } else {
                stats.non_auto_assign_free
            };
            if free == 0 {
                // The relevant partition of the pool is exhausted.
                return Err(ResourcePoolError::Empty.into());
            }
            if !auto_assign {
                // A specific value was requested and the pool has free
                // non-auto-assign entries, but this value is not one of them:
                // it was either already allocated or it is not a value that is
                // allowed to be explicitly requested.
                return Err(DatabaseError::FailedPrecondition(format!(
                    "`{}` not an available value for resource-pool `{}`",
                    req.unwrap_or_default(),
                    value.name()
                ))
                .into());
            }
            // Auto-assign with free entries still matched no row (for example,
            // every free row was locked by a concurrent allocation and skipped
            // by `FOR UPDATE SKIP LOCKED`): surface it as a generic query error.
            return Err(DatabaseError::query(query, sqlx::Error::RowNotFound).into());
        }
        Err(e) => return Err(DatabaseError::query(query, e).into()),
    };
    let out = allocated
        .parse()
        .map_err(|e: <T as FromStr>::Err| ResourcePoolError::Parse {
            e: e.to_string(),
            v: allocated,
            pool_name: value.name.clone(),
            owner_type: owner_type.to_string(),
            owner_id: owner_id.to_string(),
        })?;
    Ok(out)
}

/// `allocate_exact` reserves a specific free value from either assignment partition.
///
/// Unlike [`allocate`], this accepts and preserves either `auto_assign` setting.
/// It records the owner and allocation time, then returns the requested value.
/// Missing or allocated values return [`DatabaseError::FailedPrecondition`],
/// including values already reserved by the same owner. Callers must handle
/// retained allocations separately. The claim participates in the supplied
/// transaction and waits for conflicting row locks.
pub async fn allocate_exact<T>(
    pool: &ResourcePool<T>,
    txn: &mut PgConnection,
    owner_type: OwnerType,
    owner_id: &str,
    requested_value: T,
) -> Result<T, DatabaseError>
where
    T: ToString + FromStr + Send + Sync + 'static,
    <T as FromStr>::Err: std::error::Error,
{
    let requested_value_text = requested_value.to_string();
    let allocated_state = ResourcePoolEntryState::Allocated {
        owner: owner_id.to_string(),
        owner_type: owner_type.to_string(),
    };
    let query = "UPDATE resource_pool SET state = $1, allocated = NOW()
        WHERE name = $2 AND value = $3 AND state = $4";
    let result = sqlx::query(query)
        .bind(sqlx::types::Json(&allocated_state))
        .bind(pool.name())
        .bind(&requested_value_text)
        .bind(sqlx::types::Json(ResourcePoolEntryState::Free))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::FailedPrecondition(format!(
            "`{requested_value_text}` not an available value for resource-pool `{}`",
            pool.name()
        )));
    }

    Ok(requested_value)
}

/// Returns the value already reserved by one owner in this pool.
///
/// A duplicate reservation is treated as corrupted pool state. Callers cannot
/// safely choose one value while leaving the other assigned to the same owner.
pub async fn find_owned_allocation<T>(
    pool: &ResourcePool<T>,
    txn: &mut PgConnection,
    owner_type: OwnerType,
    owner_id: &str,
) -> Result<Option<T>, ResourcePoolDatabaseError>
where
    T: ToString + FromStr + Send + Sync + 'static,
    <T as FromStr>::Err: std::error::Error,
{
    let allocated_state = ResourcePoolEntryState::Allocated {
        owner: owner_id.to_string(),
        owner_type: owner_type.to_string(),
    };
    let query = "SELECT value FROM resource_pool
        WHERE name = $1 AND state = $2
        ORDER BY value
        LIMIT 2
        FOR UPDATE";
    let values: Vec<String> = sqlx::query_scalar(query)
        .bind(pool.name())
        .bind(sqlx::types::Json(&allocated_state))
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    if values.len() > 1 {
        return Err(DatabaseError::FailedPrecondition(format!(
            "resource pool `{}` has multiple values allocated to {} `{owner_id}`",
            pool.name(),
            owner_type
        ))
        .into());
    }

    values
        .into_iter()
        .next()
        .map(|value| {
            value
                .parse()
                .map_err(|error: <T as FromStr>::Err| ResourcePoolError::Parse {
                    e: error.to_string(),
                    v: value,
                    pool_name: pool.name.clone(),
                    owner_type: owner_type.to_string(),
                    owner_id: owner_id.to_string(),
                })
        })
        .transpose()
        .map_err(Into::into)
}

/// Returns a value shared by two integer pools, if any.
///
/// This compares materialized entries, including allocated and manually
/// assigned values, rather than the configured range definitions. It does not
/// lock the pools or prevent their definitions from changing afterward.
pub async fn find_pool_overlap(
    txn: &mut PgConnection,
    first: &ResourcePool<i32>,
    second: &ResourcePool<i32>,
) -> Result<Option<i32>, DatabaseError> {
    let query = "SELECT first_pool.value::integer FROM resource_pool first_pool
        JOIN resource_pool second_pool ON first_pool.value = second_pool.value
        WHERE first_pool.name = $1 AND second_pool.name = $2
        LIMIT 1";
    sqlx::query_scalar(query)
        .bind(first.name())
        .bind(second.name())
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Return a resource to the pool
pub async fn release<T>(
    pool: &ResourcePool<T>,
    txn: &mut PgConnection,
    value: T,
) -> Result<(), DatabaseError>
where
    T: ToString + FromStr + Send + Sync + 'static,
    <T as FromStr>::Err: std::error::Error,
{
    // TODO: If we would get passed the current owner, we could guard on that
    // so that nothing else could release the value
    let value = value.to_string();
    let query = "
UPDATE resource_pool SET
  allocated = NULL,
  state = $1
WHERE name = $2 AND value = $3
";
    if let Err(source) = sqlx::query(query)
        .bind(sqlx::types::Json(ResourcePoolEntryState::Free))
        .bind(&pool.name)
        .bind(&value)
        .execute(txn)
        .await
    {
        let event_error = source.to_string();
        let error = DatabaseError::query(query, source);
        emit(ResourcePoolReleaseFailed {
            operation: ResourcePoolOperation::Release,
            failure: ResourcePoolFailure::Database,
            failure_policy: ResourcePoolFailurePolicy::Required,
            allocation_mode: ResourcePoolAllocationMode::NotApplicable,
            value_type: pool.value_type.into(),
            error: event_error,
            pool: pool.name.clone(),
            value,
        });
        return Err(error);
    }
    Ok(())
}

pub async fn stats<'c, E>(executor: E, name: &str) -> Result<ResourcePoolStats, DatabaseError>
where
    E: sqlx::Executor<'c, Database = Postgres>,
{
    // Will do an index scan on idx_resource_pools_name, same as without the FILTER, so doing
    // all at once is faster than two queries.
    let free_state = ResourcePoolEntryState::Free;
    let query = "SELECT
                            auto_assign_used,
                            auto_assign_free,
                            non_auto_assign_used,
                            non_auto_assign_free,
                            (auto_assign_used + non_auto_assign_used) AS used,
                            (auto_assign_free + non_auto_assign_free) AS free
                        FROM
                            (
                                SELECT
                                COUNT(*) FILTER (WHERE state != $1 AND auto_assign) AS auto_assign_used,
                                COUNT(*) FILTER (WHERE state = $1 AND auto_assign) AS auto_assign_free,
                                COUNT(*) FILTER (WHERE state != $1 AND NOT auto_assign) AS non_auto_assign_used,
                                COUNT(*) FILTER (WHERE state = $1 AND NOT auto_assign) AS non_auto_assign_free
                                FROM resource_pool WHERE NAME = $2
                        ) split_stats";
    let s: ResourcePoolStats = sqlx::query_as(query)
        .bind(sqlx::types::Json(free_state))
        .bind(name)
        .fetch_one(executor)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(s)
}

/// Insert the `ResourcePoolDef` snapshot for a pool that has never been
/// seeded. **Fails** with a unique-constraint violation if the row already
/// exists — callers must check with `stored_def` / `all_stored_defs` before
/// calling this, and skip the insert when a snapshot is present.
pub async fn insert_pool_def(
    txn: &mut PgConnection,
    name: &str,
    def: &ResourcePoolDef,
) -> Result<(), DatabaseError> {
    let query = "
        INSERT INTO resource_pool_def (name, definition, seeded_at)
        VALUES ($1, $2, NOW())
    ";

    let definition = serde_json::to_value(def).map_err(|e| {
        DatabaseError::InvalidArgument(format!(
            "ResourcePool: {def:?} could not be serialized to JSON: {e}"
        ))
    })?;

    sqlx::query(query)
        .bind(name)
        .bind(definition)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Fetch the stored definition for a single pool, or `None` if never seeded.
pub async fn stored_def(
    txn: impl DbReader<'_>,
    name: &str,
) -> Result<Option<ResourcePoolDef>, DatabaseError> {
    let query = "SELECT definition FROM resource_pool_def WHERE name = $1";
    let row: Option<(sqlx::types::Json<ResourcePoolDef>,)> = sqlx::query_as(query)
        .bind(name)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(row.map(|(json,)| json.0))
}

/// Fetch every stored definition as a `HashMap<name, def>`.
pub async fn all_stored_defs(
    txn: impl DbReader<'_>,
) -> Result<HashMap<String, ResourcePoolDef>, DatabaseError> {
    let query = "SELECT name, definition FROM resource_pool_def";
    let rows: Vec<(String, sqlx::types::Json<ResourcePoolDef>)> = sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(rows
        .into_iter()
        .map(|(name, json)| (name, json.0))
        .collect())
}

pub async fn all(txn: &mut PgConnection) -> Result<Vec<ResourcePoolSnapshot>, DatabaseError> {
    let mut out = Vec::with_capacity(4);

    let query_int = "
            SELECT
                name,
                min,
                max,
                auto_assign_free,
                auto_assign_used,
                non_auto_assign_free,
                non_auto_assign_used,
                (auto_assign_used + non_auto_assign_used) AS used,
                (auto_assign_free + non_auto_assign_free) AS free
            FROM
            (
                SELECT name, CAST(min(value::bigint) AS text), CAST(max(value::bigint) AS text),
                    count(*) FILTER (WHERE state = '{\"state\": \"free\"}' AND auto_assign) AS auto_assign_free,
                    count(*) FILTER (WHERE state != '{\"state\": \"free\"}' AND auto_assign) AS auto_assign_used,
                    count(*) FILTER (WHERE state = '{\"state\": \"free\"}' AND NOT auto_assign) AS non_auto_assign_free,
                    count(*) FILTER (WHERE state != '{\"state\": \"free\"}' AND NOT auto_assign) AS non_auto_assign_used
                FROM resource_pool WHERE value_type = 'integer' GROUP BY name
            ) snapshot";

    let query_ip_address = "
            SELECT
                name,
                min,
                max,
                auto_assign_free,
                auto_assign_used,
                non_auto_assign_free,
                non_auto_assign_used,
                (auto_assign_used + non_auto_assign_used) AS used,
                (auto_assign_free + non_auto_assign_free) AS free
            FROM
            (
                SELECT name, host(min(value::inet)) AS min, host(max(value::inet)) AS max,
                    count(*) FILTER (WHERE state = '{\"state\": \"free\"}' AND auto_assign) AS auto_assign_free,
                    count(*) FILTER (WHERE state != '{\"state\": \"free\"}' AND auto_assign) AS auto_assign_used,
                    count(*) FILTER (WHERE state = '{\"state\": \"free\"}' AND NOT auto_assign) AS non_auto_assign_free,
                    count(*) FILTER (WHERE state != '{\"state\": \"free\"}' AND NOT auto_assign) AS non_auto_assign_used
                FROM resource_pool WHERE value_type IN ('ipv4', 'ipv6') GROUP BY name
            ) snapshot";

    for query in [query_int, query_ip_address] {
        let mut rows: Vec<ResourcePoolSnapshot> = sqlx::query_as(query)
            .fetch_all(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
        out.append(&mut rows);
    }
    out.sort_unstable_by(|a, b| a.name.cmp(&b.name));

    Ok(out)
}

/// All the resource pool entries for the given value
pub async fn find_value(
    txn: impl DbReader<'_>,
    value: &str,
) -> Result<Vec<ResourcePoolEntry>, DatabaseError> {
    let query =
        "SELECT name, value, value_type, state, allocated FROM resource_pool WHERE value = $1";
    let entry: Vec<ResourcePoolEntry> = sqlx::query_as(query)
        .bind(value)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(entry)
}

/// Used for functions that may return a database error or may return a ResourcePoolError. This
/// keeps this DatabaseError out of the ResourcePoolError variants, so they can live in separate
/// crates.
#[derive(thiserror::Error, Debug)]
pub enum ResourcePoolDatabaseError {
    #[error(transparent)]
    ResourcePool(#[from] ResourcePoolError),
    #[error(transparent)]
    Database(#[from] Box<DatabaseError>),
}

impl From<DatabaseError> for ResourcePoolDatabaseError {
    fn from(e: DatabaseError) -> Self {
        ResourcePoolDatabaseError::Database(Box::new(e))
    }
}

pub(crate) fn classify_resource_pool_failure(
    error: &ResourcePoolDatabaseError,
) -> ResourcePoolFailure {
    match error {
        ResourcePoolDatabaseError::ResourcePool(ResourcePoolError::Empty) => {
            ResourcePoolFailure::Exhausted
        }
        ResourcePoolDatabaseError::ResourcePool(ResourcePoolError::Parse { .. }) => {
            ResourcePoolFailure::Parse
        }
        ResourcePoolDatabaseError::Database(error) => classify_database_failure(error),
    }
}

fn classify_database_failure(error: &DatabaseError) -> ResourcePoolFailure {
    if matches!(error, DatabaseError::FailedPrecondition(_)) {
        ResourcePoolFailure::RequestedValueUnavailable
    } else {
        ResourcePoolFailure::Database
    }
}

/// Whether an allocation error represents an unavailable requested value.
pub fn is_requested_value_unavailable(error: &ResourcePoolDatabaseError) -> bool {
    classify_resource_pool_failure(error) == ResourcePoolFailure::RequestedValueUnavailable
}

/// Emit the non-fatal diagnostic used when DPU creation cannot allocate an ASN.
pub(crate) fn emit_best_effort_dpu_asn_allocation_failure(
    value_type: ValueType,
    stable_machine_id: &str,
    error: &ResourcePoolDatabaseError,
) {
    emit(DpuAsnAllocationFailed {
        operation: ResourcePoolOperation::Allocate,
        failure: classify_resource_pool_failure(error),
        failure_policy: ResourcePoolFailurePolicy::BestEffort,
        allocation_mode: ResourcePoolAllocationMode::Automatic,
        value_type: value_type.into(),
        stable_machine_id: stable_machine_id.to_string(),
        error: error.to_string(),
    });
}

/// Emit a generic resource-pool allocation failure at its API mapping boundary.
///
/// `allocate` returns a deliberately detailed error so each caller can retain
/// its existing status mapping. Recording it here keeps that boundary as the
/// single log and metric owner without counting the same failed query again in
/// the database helper.
///
/// `diagnostic_pool` is deliberately supplied by the caller. Some existing
/// diagnostics use a stable alias such as `lo-ip` instead of the configured
/// pool name, and adopting an Event must preserve those structured log fields.
pub fn emit_allocation_failure(
    value_type: ValueType,
    owner_id: &str,
    requested: bool,
    diagnostic_pool: &str,
    error: &ResourcePoolDatabaseError,
) {
    let failure = classify_resource_pool_failure(error);
    emit_classified_allocation_failure(
        value_type,
        owner_id,
        requested,
        diagnostic_pool,
        failure,
        error.to_string(),
    );
}

/// Emit an allocation failure after a caller has unwrapped its database error.
///
/// VPC and SPX allocation preserve their existing database-error status
/// mapping, so those callers retain ownership of the inner [`DatabaseError`].
pub fn emit_database_allocation_failure(
    value_type: ValueType,
    owner_id: &str,
    requested: bool,
    diagnostic_pool: &str,
    error: &DatabaseError,
) {
    emit_classified_allocation_failure(
        value_type,
        owner_id,
        requested,
        diagnostic_pool,
        classify_database_failure(error),
        error.to_string(),
    );
}

fn emit_classified_allocation_failure(
    value_type: ValueType,
    owner_id: &str,
    requested: bool,
    diagnostic_pool: &str,
    failure: ResourcePoolFailure,
    error: String,
) {
    let allocation_mode = if requested {
        ResourcePoolAllocationMode::Requested
    } else {
        ResourcePoolAllocationMode::Automatic
    };
    let value_type = value_type.into();

    match failure {
        ResourcePoolFailure::Exhausted => emit(ResourcePoolExhausted {
            operation: ResourcePoolOperation::Allocate,
            failure,
            failure_policy: ResourcePoolFailurePolicy::Required,
            allocation_mode,
            value_type,
            owner_id: owner_id.to_string(),
            pool: diagnostic_pool.to_string(),
        }),
        ResourcePoolFailure::RequestedValueUnavailable
        | ResourcePoolFailure::Parse
        | ResourcePoolFailure::Database => {
            emit(ResourcePoolAllocationFailed {
                operation: ResourcePoolOperation::Allocate,
                failure,
                failure_policy: ResourcePoolFailurePolicy::Required,
                allocation_mode,
                value_type,
                owner_id: owner_id.to_string(),
                error,
                pool: diagnostic_pool.to_string(),
            });
        }
    }
}

/// Emit the requested-VNI diagnostic retained by VPC and SPX allocation APIs.
///
/// This Event shares the normalized lifecycle counter labels with generic pool
/// failures while preserving the APIs' existing message and `requested_vni`
/// structured field.
pub fn emit_requested_vni_unavailable(
    value_type: ValueType,
    owner_id: &str,
    requested_vni: i32,
    diagnostic_pool: &str,
) {
    emit(ResourcePoolRequestedVniUnavailable {
        operation: ResourcePoolOperation::Allocate,
        failure: ResourcePoolFailure::RequestedValueUnavailable,
        failure_policy: ResourcePoolFailurePolicy::Required,
        allocation_mode: ResourcePoolAllocationMode::Requested,
        value_type: value_type.into(),
        owner_id: owner_id.to_string(),
        pool: diagnostic_pool.to_string(),
        requested_vni: i64::from(requested_vni),
    });
}

/// A pool bigger than this is very likely a mistake
const MAX_POOL_SIZE: usize = 250_000;

#[derive(thiserror::Error, Debug)]
pub enum DefineResourcePoolError {
    #[error("{0}")]
    InvalidArgument(String),

    #[error("resource pool error: {0}")]
    ResourcePoolError(#[from] model::resource_pool::ResourcePoolError),

    #[error("max pool size exceeded. {0} > {1}")]
    TooBig(usize, usize),

    #[error("database error: {0}")]
    DatabaseError(#[from] DatabaseError),
}

/// Create or edit the resource pools, making them match the given toml string.
/// Does not delete or shrink pools, is only additive.
pub async fn define_all_from(
    txn: &mut PgConnection,
    pools: &HashMap<String, ResourcePoolDef>,
) -> Result<(), DefineResourcePoolError> {
    for (ref name, def) in pools {
        define(txn, name, def).await?;
        tracing::info!(pool_name = name, "Pool populated");
    }
    Ok(())
}
pub async fn pool_has_rows(txn: &mut PgConnection, name: &str) -> Result<bool, DatabaseError> {
    let query: &str = "SELECT EXISTS(SELECT 1 FROM resource_pool WHERE name = $1)";
    let (exists,): (bool,) = sqlx::query_as(query)
        .bind(name)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(exists)
}

/// Reconcile declared pool definitions against what was previously seeded.
/// Each declared pool is classified independently by its `(snapshot, pool)`
/// state.
///
///   1. **New** (no snapshot, pool absent from `resource_pool`): seed
///      and snapshot.
///   2. **Backfill** (no snapshot, pool present): record the snapshot only.
///   3. **In sync** (snapshot matches declaration): no-op.
///   4. **Drift** (snapshot differs from declaration, pool present): warn
///      and leave both in place. Operator must reconcile by hand.
///   5. **Anomaly** (snapshot present, pool absent): error-log and skip.
///      Indicates a partial restore or manual deletion; auto-healing here
///      would risk re-seeding a pool an operator deliberately cleared.
///
/// Pools that appear in the snapshot table but are no longer declared
/// ("dropped" from `InitialObjectsConfig.pools`) are warned about, but
/// not removed.
pub async fn reconcile_pool_defs(
    txn: &mut PgConnection,
    declared: &HashMap<String, ResourcePoolDef>,
) -> Result<(), DefineResourcePoolError> {
    let stored = all_stored_defs(&mut *txn).await?;

    for (name, def) in declared {
        let pool_exists = pool_has_rows(&mut *txn, name).await?;
        match (stored.get(name), pool_exists) {
            // Snapshot exists but pool is missing from resource_pool.
            // Anomaly: e.g., partial DB restore. Don't auto-heal — operator
            // must reconcile by hand.
            (Some(stored_def), false) => {
                tracing::error!(
                    pool_name = name,
                    stored = ?stored_def,
                    "Resource Pool snapshot exists but resource_pool has no rows for it; \
                     manual recovery required"
                );
            }
            // Already seeded with the current declaration.
            (Some(stored_def), true) if stored_def == def => {}
            // Declaration has drifted since seed. Warn, don't reapply.
            (Some(stored_def), true) => {
                emit(ConfigDefinitionDrifted {
                    resource_kind: ConfigResourceKind::ResourcePool,
                    drift_kind: ConfigDriftKind::Changed,
                    name: name.clone(),
                    stored: Some(format!("{stored_def:?}")),
                    declared: Some(format!("{def:?}")),
                });
            }
            // Pool exists in resource_pool but has no snapshot yet.
            // Pre-migration deployment, or a pool re-added after a snapshot
            // was manually deleted. Record the snapshot only.
            (None, true) => {
                insert_pool_def(txn, name, def).await?;
                tracing::info!(
                    pool_name = name,
                    "Backfilled pool definition snapshot for pre-existing pool"
                );
            }
            // New — seed and snapshot.
            (None, false) => {
                define(txn, name, def).await?;
                insert_pool_def(txn, name, def).await?;
                tracing::info!(
                    pool_name = name,
                    "Seeded Resource Pool definition snapshot for pool"
                );
            }
        }
    }

    for name in stored.keys() {
        if !declared.contains_key(name) {
            emit(ConfigDefinitionDrifted {
                resource_kind: ConfigResourceKind::ResourcePool,
                drift_kind: ConfigDriftKind::Dropped,
                name: name.clone(),
                stored: None,
                declared: None,
            });
        }
    }

    Ok(())
}

pub async fn define(
    txn: &mut PgConnection,
    name: &str,
    def: &ResourcePoolDef,
) -> Result<(), DefineResourcePoolError> {
    if name == "pkey" {
        return Err(DefineResourcePoolError::InvalidArgument(
            "pkey pool is deprecated. Use ib_fabrics.default.pkeys as replacement".to_string(),
        ));
    }

    match (&def.prefix, &def.ranges) {
        // Neither is given
        (None, ranges) if ranges.is_empty() => {
            return Err(DefineResourcePoolError::InvalidArgument(
                "Please provide one of 'prefix' or 'ranges'".to_string(),
            ));
        }
        // Both are given
        (Some(_), ranges) if !ranges.is_empty() => {
            return Err(DefineResourcePoolError::InvalidArgument(
                "Please provide only one of 'prefix' or 'ranges'".to_string(),
            ));
        }
        // Just prefix
        (Some(prefix), _) => {
            define_by_prefix(txn, name, def.pool_type, prefix, def.delegate_prefix_len).await?;
        }
        // Just ranges
        (None, ranges) => {
            for range in ranges {
                define_by_range(
                    txn,
                    name,
                    def.pool_type,
                    &range.start,
                    &range.end,
                    range.auto_assign,
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn define_by_prefix(
    txn: &mut PgConnection,
    name: &str,
    pool_type: ResourcePoolType,
    prefix: &str,
    delegate_prefix_len: Option<u8>,
) -> Result<(), DefineResourcePoolError> {
    match pool_type {
        ResourcePoolType::Ipv4 => {
            let values = expand_ip_prefix(prefix)
                .map_err(|e| DefineResourcePoolError::InvalidArgument(e.to_string()))?;
            let num_values = values.len();
            if num_values > MAX_POOL_SIZE {
                return Err(DefineResourcePoolError::TooBig(num_values, MAX_POOL_SIZE));
            }
            let pool = model::resource_pool::ResourcePool::new(
                name.to_string(),
                model::resource_pool::ValueType::Ipv4,
            );
            populate(&pool, txn, values, true).await?;
            tracing::debug!(
                pool_name = name,
                prefix,
                value_count = num_values,
                "Populated IPv4 resource pool from prefix"
            );
        }
        ResourcePoolType::Ipv6 => {
            let values = expand_ipv6_prefix(prefix)?;
            let num_values = values.len();
            let pool = model::resource_pool::ResourcePool::new(
                name.to_string(),
                model::resource_pool::ValueType::Ipv6,
            );
            populate(&pool, txn, values, true).await?;
            tracing::debug!(
                pool_name = name,
                prefix,
                value_count = num_values,
                "Populated IPv6 resource pool from prefix"
            );
        }
        ResourcePoolType::Ipv6Prefix => {
            let delegate_len = delegate_prefix_len.ok_or_else(|| {
                DefineResourcePoolError::InvalidArgument(
                    "Pool type 'ipv6prefix' requires 'delegate_prefix_len'".to_string(),
                )
            })?;
            let values = expand_ipv6_prefix_delegation(prefix, delegate_len)?;
            let num_values = values.len();
            let pool = model::resource_pool::ResourcePool::new(
                name.to_string(),
                model::resource_pool::ValueType::Ipv6Prefix,
            );
            populate(&pool, txn, values, true).await?;
            tracing::debug!(
                pool_name = name,
                prefix,
                delegation_prefix_length = delegate_len,
                value_count = num_values,
                "Populated IPv6 prefix delegation pool"
            );
        }
        ResourcePoolType::Integer => {
            return Err(DefineResourcePoolError::InvalidArgument(
                "Pool type 'integer' cannot take a prefix".to_string(),
            ));
        }
    }

    Ok(())
}

async fn define_by_range(
    txn: &mut PgConnection,
    name: &str,
    pool_type: ResourcePoolType,
    range_start: &str,
    range_end: &str,
    auto_assign: bool,
) -> Result<(), DefineResourcePoolError> {
    match pool_type {
        ResourcePoolType::Ipv4 => {
            let values = expand_ip_range(range_start, range_end)
                .map_err(|e| DefineResourcePoolError::InvalidArgument(e.to_string()))?;
            let num_values = values.len();
            if num_values > MAX_POOL_SIZE {
                return Err(DefineResourcePoolError::TooBig(num_values, MAX_POOL_SIZE));
            }
            let pool = model::resource_pool::ResourcePool::new(
                name.to_string(),
                model::resource_pool::ValueType::Ipv4,
            );
            populate(&pool, txn, values, auto_assign).await?;
            tracing::debug!(
                pool_name = name,
                range_start,
                range_end,
                value_count = num_values,
                "Populated IPv4 resource pool from range"
            );
        }
        ResourcePoolType::Ipv6 => {
            let values = expand_ipv6_range(range_start, range_end)?;
            let num_values = values.len();
            let pool = model::resource_pool::ResourcePool::new(
                name.to_string(),
                model::resource_pool::ValueType::Ipv6,
            );
            populate(&pool, txn, values, auto_assign).await?;
            tracing::debug!(
                pool_name = name,
                range_start,
                range_end,
                value_count = num_values,
                "Populated IPv6 resource pool from range"
            );
        }
        ResourcePoolType::Ipv6Prefix => {
            let values = expand_ipv6_prefix_range(range_start, range_end)?;
            let num_values = values.len();
            let pool = model::resource_pool::ResourcePool::new(
                name.to_string(),
                model::resource_pool::ValueType::Ipv6Prefix,
            );
            populate(&pool, txn, values, auto_assign).await?;
            tracing::debug!(
                pool_name = name,
                range_start,
                range_end,
                value_count = num_values,
                "Populated IPv6 prefix pool from range"
            );
        }
        ResourcePoolType::Integer => {
            let values = expand_int_range(range_start, range_end)
                .map_err(|e| DefineResourcePoolError::InvalidArgument(e.to_string()))?;
            let num_values = values.len();
            if num_values > MAX_POOL_SIZE {
                return Err(DefineResourcePoolError::TooBig(num_values, MAX_POOL_SIZE));
            }
            let pool = model::resource_pool::ResourcePool::new(
                name.to_string(),
                model::resource_pool::ValueType::Integer,
            );
            populate(&pool, txn, values, auto_assign).await?;
            tracing::debug!(
                pool_name = name,
                value_count = num_values,
                "Populated int resource pool"
            );
        }
    }
    Ok(())
}

// Expands a string like "10.180.62.1/26" into all the ip addresses it covers
fn expand_ip_prefix(network: &str) -> Result<Vec<Ipv4Addr>, eyre::Report> {
    let n: ipnetwork::IpNetwork = network.parse()?;
    let (start_addr, end_addr) = match (n.network(), n.broadcast()) {
        (IpAddr::V4(start), IpAddr::V4(end)) => (start, end),
        _ => {
            eyre::bail!("invalid IPv4 network: {network}");
        }
    };
    let start: u32 = start_addr.into();
    let end: u32 = end_addr.into();
    Ok((start..end).map(Ipv4Addr::from).collect())
}

// All the IPv4 addresses between start_s and end_s
fn expand_ip_range(start_s: &str, end_s: &str) -> Result<Vec<Ipv4Addr>, eyre::Report> {
    let start_addr: Ipv4Addr = start_s.parse()?;
    let end_addr: Ipv4Addr = end_s.parse()?;
    let start: u32 = start_addr.into();
    let end: u32 = end_addr.into();
    Ok((start..end).map(Ipv4Addr::from).collect())
}

// expand_ipv6_prefix is the IPv6 variant of the expand_ip_prefix
// function, expanding an IPv6 prefix into all addresses it covers.
fn expand_ipv6_prefix(network: &str) -> Result<Vec<Ipv6Addr>, DefineResourcePoolError> {
    let n: ipnetwork::IpNetwork = network.parse().map_err(|e: ipnetwork::IpNetworkError| {
        DefineResourcePoolError::InvalidArgument(e.to_string())
    })?;
    let (start_addr, end_addr, prefix_len) = match n {
        ipnetwork::IpNetwork::V6(v6) => (v6.network(), v6.broadcast(), v6.prefix()),
        _ => {
            return Err(DefineResourcePoolError::InvalidArgument(format!(
                "Invalid IPv6 network: {network}"
            )));
        }
    };

    // Compute the number of addresses this prefix would produce
    // and reject if it exceeds MAX_POOL_SIZE -- there's no point
    // in attempting otherwise. I guess in theory there might also
    // be concerns for OOMing and such if a prefix was too small
    // and we ranged out like 18.4 quintillion addresses, so doing
    // a check against MAX_POOL_SIZE helps to keep that.. in check.
    let host_bits = 128 - prefix_len as u32;
    let num_addresses = 1u128.checked_shl(host_bits).unwrap_or(u128::MAX);
    if num_addresses > MAX_POOL_SIZE as u128 {
        return Err(DefineResourcePoolError::TooBig(
            usize::try_from(num_addresses).unwrap_or(usize::MAX),
            MAX_POOL_SIZE,
        ));
    }

    let start: u128 = start_addr.into();
    let end: u128 = end_addr.into();

    // Unlike IPv4, IPv6 has no broadcast address, so we include
    // all addresses in the prefix (inclusive range) -- this is
    // different from expand_ip_prefix_above, so worth calling
    // it out so someone doesn't think it's a bug.
    Ok((start..=end).map(Ipv6Addr::from).collect())
}

// expand_ipv6_range is the IPv6 variant of expand_ip_range,
// expanding the IPv6 addresses between start_s and end_s.
fn expand_ipv6_range(start_s: &str, end_s: &str) -> Result<Vec<Ipv6Addr>, DefineResourcePoolError> {
    let start_addr: Ipv6Addr = start_s.parse().map_err(|e: std::net::AddrParseError| {
        DefineResourcePoolError::InvalidArgument(e.to_string())
    })?;
    let end_addr: Ipv6Addr = end_s.parse().map_err(|e: std::net::AddrParseError| {
        DefineResourcePoolError::InvalidArgument(e.to_string())
    })?;
    let start: u128 = start_addr.into();
    let end: u128 = end_addr.into();

    let count = end.saturating_sub(start);
    if count > MAX_POOL_SIZE as u128 {
        return Err(DefineResourcePoolError::TooBig(
            usize::try_from(count).unwrap_or(usize::MAX),
            MAX_POOL_SIZE,
        ));
    }

    Ok((start..end).map(Ipv6Addr::from).collect())
}

// expand_ipv6_prefix_delegation expands a parent IPv6 prefix into
// sub-prefixes of the given delegation length. For example, "fd00::/48"
// with delegate_len=64 produces all 65,536 /64 sub-prefixes within that /48.
fn expand_ipv6_prefix_delegation(
    parent: &str,
    delegate_len: u8,
) -> Result<Vec<Ipv6Network>, DefineResourcePoolError> {
    let n: ipnetwork::IpNetwork = parent.parse().map_err(|e: ipnetwork::IpNetworkError| {
        DefineResourcePoolError::InvalidArgument(e.to_string())
    })?;
    let parent_net = match n {
        ipnetwork::IpNetwork::V6(v6) => v6,
        _ => {
            return Err(DefineResourcePoolError::InvalidArgument(format!(
                "Invalid IPv6 network: {parent}"
            )));
        }
    };

    let parent_len = parent_net.prefix();
    if delegate_len <= parent_len {
        return Err(DefineResourcePoolError::InvalidArgument(format!(
            "Delegation prefix length /{delegate_len} must be longer than parent prefix length /{parent_len}"
        )));
    }
    if delegate_len > 128 {
        return Err(DefineResourcePoolError::InvalidArgument(format!(
            "Delegation prefix length /{delegate_len} exceeds maximum /128"
        )));
    }

    // Number of sub-prefixes == 2^(delegate_len - parent_len).
    let sub_bits = (delegate_len - parent_len) as u32;
    let count = 1u128.checked_shl(sub_bits).unwrap_or(u128::MAX);
    if count > MAX_POOL_SIZE as u128 {
        return Err(DefineResourcePoolError::TooBig(
            usize::try_from(count).unwrap_or(usize::MAX),
            MAX_POOL_SIZE,
        ));
    }

    // And now step between consecutive sub-prefix network addresses.
    let step = 1u128 << (128 - delegate_len as u32);
    let base: u128 = parent_net.network().into();

    let mut prefixes = Vec::with_capacity(count as usize);
    for i in 0..count {
        let addr = Ipv6Addr::from(base + i * step);
        let net = Ipv6Network::new(addr, delegate_len)
            .map_err(|e| DefineResourcePoolError::InvalidArgument(e.to_string()))?;
        prefixes.push(net);
    }
    Ok(prefixes)
}

// expand_ipv6_prefix_range expands a range of IPv6 prefixes. Both
// endpoints must be CIDR notation with the same prefix length and
// properly aligned (no stray host bits). Uses exclusive end, matching
// the convention of the other type's range-expansion functions.
fn expand_ipv6_prefix_range(
    start_s: &str,
    end_s: &str,
) -> Result<Vec<Ipv6Network>, DefineResourcePoolError> {
    let start_net: Ipv6Network = start_s.parse().map_err(|e: ipnetwork::IpNetworkError| {
        DefineResourcePoolError::InvalidArgument(e.to_string())
    })?;
    let end_net: Ipv6Network = end_s.parse().map_err(|e: ipnetwork::IpNetworkError| {
        DefineResourcePoolError::InvalidArgument(e.to_string())
    })?;

    let prefix_len = start_net.prefix();
    if end_net.prefix() != prefix_len {
        return Err(DefineResourcePoolError::InvalidArgument(format!(
            "Range endpoints have different prefix lengths: /{} vs /{}",
            prefix_len,
            end_net.prefix()
        )));
    }

    // Verify alignment prefix alignment -- the specified IP must equal
    // the network address (i.e. no host bits set); Ipv6Network::parse()
    // accepts "fd00::1/64" but that's a host address, not a valid network
    // prefix.
    if start_net.ip() != start_net.network() {
        return Err(DefineResourcePoolError::InvalidArgument(format!(
            "Start prefix {start_s} has host bits set"
        )));
    }
    if end_net.ip() != end_net.network() {
        return Err(DefineResourcePoolError::InvalidArgument(format!(
            "End prefix {end_s} has host bits set"
        )));
    }

    let start_addr: u128 = start_net.network().into();
    let end_addr: u128 = end_net.network().into();
    let step = 1u128 << (128 - prefix_len as u32);

    if end_addr <= start_addr {
        return Err(DefineResourcePoolError::InvalidArgument(
            "End prefix must be greater than start prefix".to_string(),
        ));
    }

    let count = (end_addr - start_addr) / step;
    if count > MAX_POOL_SIZE as u128 {
        return Err(DefineResourcePoolError::TooBig(
            usize::try_from(count).unwrap_or(usize::MAX),
            MAX_POOL_SIZE,
        ));
    }

    let mut prefixes = Vec::with_capacity(count as usize);
    for i in 0..count {
        let addr = Ipv6Addr::from(start_addr + i * step);
        let net = Ipv6Network::new(addr, prefix_len)
            .map_err(|e| DefineResourcePoolError::InvalidArgument(e.to_string()))?;
        prefixes.push(net);
    }
    Ok(prefixes)
}

// All the numbers between start_s and end_s
fn expand_int_range(start_s: &str, end_s: &str) -> Result<Vec<i64>, eyre::Report> {
    let start: i64 = parse_int_range(start_s)?;
    let end: i64 = parse_int_range(end_s)?;
    Ok((start..end).collect())
}

const HEX_PRE: &str = "0x";

fn parse_int_range(data: &str) -> Result<i64, eyre::Report> {
    let data = data.to_lowercase();
    let base = if data.starts_with(HEX_PRE) { 16 } else { 10 };
    let p = data.trim_start_matches(HEX_PRE);

    i64::from_str_radix(p, base).map_err(eyre::Report::from)
}

/// How often to update the resource pool metrics
const METRICS_RESOURCEPOOL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

pub async fn create_common_pools(
    db: sqlx::PgPool,
    ib_fabric_ids: HashSet<String>,
) -> eyre::Result<Arc<CommonPools>> {
    let mut pool_names = Vec::new();
    let mut optional_pool_names = Vec::new();

    let pool_loopback_ip: Arc<ResourcePool<IpAddr>> =
        Arc::new(ResourcePool::new(LOOPBACK_IP.to_string(), ValueType::Ipv4));
    pool_names.push(pool_loopback_ip.name().to_string());
    let pool_loopback_ip_v6: Arc<ResourcePool<Ipv6Addr>> = Arc::new(ResourcePool::new(
        LOOPBACK_IP_V6.to_string(),
        ValueType::Ipv6,
    ));
    optional_pool_names.push(pool_loopback_ip_v6.name().to_string());
    let pool_vlan_id: Arc<ResourcePool<i16>> =
        Arc::new(ResourcePool::new(VLANID.to_string(), ValueType::Integer));
    pool_names.push(pool_vlan_id.name().to_string());
    let pool_vni: Arc<ResourcePool<i32>> =
        Arc::new(ResourcePool::new(VNI.to_string(), ValueType::Integer));
    pool_names.push(pool_vni.name().to_string());
    let pool_vpc_vni: Arc<ResourcePool<i32>> =
        Arc::new(ResourcePool::new(VPC_VNI.to_string(), ValueType::Integer));
    pool_names.push(pool_vpc_vni.name().to_string());
    let pool_fnn_asn: Arc<ResourcePool<u32>> =
        Arc::new(ResourcePool::new(FNN_ASN.to_string(), ValueType::Integer));
    optional_pool_names.push(pool_fnn_asn.name().to_string());

    let pool_dpa_vni: Arc<ResourcePool<i32>> =
        Arc::new(ResourcePool::new(DPA_VNI.to_string(), ValueType::Integer));
    optional_pool_names.push(pool_dpa_vni.name().to_string());

    let pool_vpc_dpu_loopback_ip: Arc<ResourcePool<IpAddr>> = Arc::new(ResourcePool::new(
        VPC_DPU_LOOPBACK.to_string(),
        ValueType::Ipv4,
    ));
    //  TODO: This should be removed from optional once FNN become mandatory.
    optional_pool_names.push(pool_vpc_dpu_loopback_ip.name().to_string());

    let pool_external_vpc_vni: Arc<ResourcePool<i32>> = Arc::new(ResourcePool::new(
        EXTERNAL_VPC_VNI.to_string(),
        ValueType::Integer,
    ));
    optional_pool_names.push(pool_external_vpc_vni.name().to_string());

    // We can't run if any of the mandatory pools are missing
    for name in &pool_names {
        if stats(&db, name).await?.free == 0 {
            eyre::bail!("resource pool '{name}' missing or full. edit config file and restart");
        }
    }

    pool_names.extend(optional_pool_names);

    // It's ok for IB partition pools to be missing or full - as long as nobody tries to use partitions
    let pkey_pools: Arc<HashMap<String, ResourcePool<u16>>> = Arc::new(
        ib_fabric_ids
            .into_iter()
            .map(|fabric_id| {
                (
                    fabric_id.clone(),
                    ResourcePool::new(
                        resource_pool::common::ib_pkey_pool_name(&fabric_id),
                        ValueType::Integer,
                    ),
                )
            })
            .collect(),
    );
    pool_names.extend(pkey_pools.values().map(|pool| pool.name().to_string()));

    // Gather resource pool stats. A different thread sends them to Prometheus.
    let (stop_sender, mut stop_receiver) = oneshot::channel();
    let pool_stats: Arc<Mutex<HashMap<String, ResourcePoolStats>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let pool_stats_bg = pool_stats.clone();
    tokio::task::Builder::new()
        .name("resource_pool metrics")
        .spawn(async move {
            loop {
                let mut next_stats = HashMap::with_capacity(pool_names.len());
                for name in &pool_names {
                    if let Ok(st) = stats(&db, name).await {
                        next_stats.insert(name.to_string(), st);
                    }
                }
                *pool_stats_bg.lock().unwrap() = next_stats;

                tokio::select! {
                    _ = tokio::time::sleep(METRICS_RESOURCEPOOL_INTERVAL) => {},
                    _ = &mut stop_receiver => {
                        tracing::info!("CommonPool metrics stop was requested");
                        return;
                    }
                }
            }
        })?;

    Ok(Arc::new(CommonPools {
        ethernet: EthernetPools {
            pool_loopback_ip,
            pool_loopback_ip_v6,
            pool_vlan_id,
            pool_vni,
            pool_vpc_vni,
            pool_external_vpc_vni,
            pool_dpa_vni,
            pool_fnn_asn,
            pool_vpc_dpu_loopback_ip,
        },
        infiniband: IbPools { pkey_pools },
        pool_stats,
        _stop_sender: stop_sender,
    }))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_instrument::MetricKind;
    use carbide_instrument::testing::{MetricsCapture, capture_logs, capture_logs_async};
    use carbide_test_support::Outcome::*;
    use carbide_test_support::query_counter::count_queries;
    use carbide_test_support::{Case, Check, check_cases, check_values};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 50)]
    async fn test_parallel() -> Result<(), eyre::Report> {
        // We can't use #[sqlx::test] here because we need a multi-threaded
        // executor with 50 worker threads. Instead we manage the test database
        // lifecycle manually, using a random name so multiple test runs (or
        // parallel CI jobs) never collide on the same Postgres instance.
        let base_url = std::env::var("DATABASE_URL")?;
        // ResourcePool.name is varchar(32), so keep the DB name short.
        let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let db_name = format!("test_par_{short_id}");
        let base_options = PgConnectOptions::from_str(&base_url)?;

        let admin = PgPoolOptions::new()
            .connect_with(base_options.clone())
            .await?;

        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE \"{db_name}\""
        )))
        .execute(&admin)
        .await?;
        let db_pool = PgPoolOptions::new()
            .connect_with(base_options.database(&db_name))
            .await?;
        crate::migrations::migrate(&db_pool).await?;

        let mut txn = db_pool.begin().await?;
        let pool = Arc::new(ResourcePool::new(db_name.clone(), ValueType::Integer));

        crate::resource_pool::populate(
            &pool,
            &mut txn,
            (1..=5_000).map(|i| i.to_string()).collect(),
            true,
        )
        .await?;
        txn.commit().await?;

        let mut handles = Vec::with_capacity(50);
        let all_values = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
        for i in 0..50 {
            let all_values = all_values.clone();
            let p = pool.clone();
            let db_pool_c = db_pool.clone();
            let handle = tokio::task::spawn(async move {
                let mut got = Vec::with_capacity(100);
                for _ in 0..100 {
                    let mut txn = db_pool_c.begin().await.unwrap();
                    got.push(
                        crate::resource_pool::allocate(
                            &p,
                            &mut txn,
                            OwnerType::Machine,
                            &i.to_string(),
                            None,
                        )
                        .await
                        .unwrap(),
                    );
                    txn.commit().await.unwrap();
                }
                all_values.lock().await.extend(got.clone());
            });
            handles.push(handle);
        }
        futures::future::join_all(handles).await;
        drop(pool);
        db_pool.close().await;

        assert_eq!(all_values.lock().await.len(), 5_000);

        // WITH (FORCE) terminates any lingering backends before dropping,
        // avoiding the flaky "database is being accessed by other users" error.
        let drop_stmt = format!("DROP DATABASE \"{db_name}\" WITH (FORCE)");
        sqlx::query(sqlx::AssertSqlSafe(drop_stmt))
            .execute(&admin)
            .await?;
        admin.close().await;
        Ok(())
    }

    const RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC: &str =
        "carbide_resource_pool_lifecycle_failures_total";
    const RESOURCE_POOL_LIFECYCLE_FAILURES_DESCRIPTION: &str = "Number of resource pool lifecycle failures, by operation, failure, failure policy, allocation mode, and value type.";
    const RESOURCE_POOL_LIFECYCLE_FAILURE_LABELS: [&str; 5] = [
        "operation",
        "failure",
        "failure_policy",
        "allocation_mode",
        "value_type",
    ];

    fn assert_shared_failure_metric_contract<E: Event>(event: E) {
        assert_eq!(
            E::METRIC_NAME,
            Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC)
        );
        assert_eq!(E::METRIC, MetricKind::Counter);
        assert_eq!(E::DESCRIBE, RESOURCE_POOL_LIFECYCLE_FAILURES_DESCRIPTION);
        assert_eq!(E::COMPONENT, "nico-api");

        let labels = event.labels();
        let label_keys = labels
            .as_ref()
            .iter()
            .map(|label| label.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(label_keys, RESOURCE_POOL_LIFECYCLE_FAILURE_LABELS);
    }

    #[test]
    fn resource_pool_failure_events_share_one_metric_contract() {
        assert_shared_failure_metric_contract(ResourcePoolExhausted {
            operation: ResourcePoolOperation::Allocate,
            failure: ResourcePoolFailure::Exhausted,
            failure_policy: ResourcePoolFailurePolicy::Required,
            allocation_mode: ResourcePoolAllocationMode::Automatic,
            value_type: ResourcePoolValueType::Integer,
            owner_id: "owner".to_string(),
            pool: "pool".to_string(),
        });
        assert_shared_failure_metric_contract(ResourcePoolRequestedVniUnavailable {
            operation: ResourcePoolOperation::Allocate,
            failure: ResourcePoolFailure::RequestedValueUnavailable,
            failure_policy: ResourcePoolFailurePolicy::Required,
            allocation_mode: ResourcePoolAllocationMode::Requested,
            value_type: ResourcePoolValueType::Integer,
            owner_id: "owner".to_string(),
            pool: "pool".to_string(),
            requested_vni: 42,
        });
        assert_shared_failure_metric_contract(ResourcePoolAllocationFailed {
            operation: ResourcePoolOperation::Allocate,
            failure: ResourcePoolFailure::Database,
            failure_policy: ResourcePoolFailurePolicy::Required,
            allocation_mode: ResourcePoolAllocationMode::Automatic,
            value_type: ResourcePoolValueType::Integer,
            owner_id: "owner".to_string(),
            error: "database unavailable".to_string(),
            pool: "pool".to_string(),
        });
        assert_shared_failure_metric_contract(ResourcePoolReleaseFailed {
            operation: ResourcePoolOperation::Release,
            failure: ResourcePoolFailure::Database,
            failure_policy: ResourcePoolFailurePolicy::Required,
            allocation_mode: ResourcePoolAllocationMode::NotApplicable,
            value_type: ResourcePoolValueType::Ipv4,
            error: "database unavailable".to_string(),
            pool: "pool".to_string(),
            value: "192.0.2.1".to_string(),
        });
        assert_shared_failure_metric_contract(DpuAsnAllocationFailed {
            operation: ResourcePoolOperation::Allocate,
            failure: ResourcePoolFailure::Exhausted,
            failure_policy: ResourcePoolFailurePolicy::BestEffort,
            allocation_mode: ResourcePoolAllocationMode::Automatic,
            value_type: ResourcePoolValueType::Integer,
            stable_machine_id: "dpu-1".to_string(),
            error: "pool exhausted".to_string(),
        });
    }

    #[test]
    fn requested_vni_unavailable_preserves_diagnostic_and_counter_schema() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit_requested_vni_unavailable(ValueType::Integer, "vpc-1", 42, "vpc-vni");
        });

        assert_eq!(logs.len(), 1);
        let log = &logs[0];
        assert_eq!(log.metadata_name, "resource_pool_requested_vni_unavailable");
        assert_eq!(log.message, "invalid pool value requested, cannot allocate");
        assert_eq!(
            log.field("event_name"),
            Some("resource_pool_requested_vni_unavailable")
        );
        assert_eq!(
            log.field("metric_name"),
            Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC)
        );
        assert_eq!(log.field("owner_id"), Some("vpc-1"));
        assert_eq!(log.field("pool"), Some("vpc-vni"));
        assert_eq!(log.field("requested_vni"), Some("42"));
        assert_eq!(log.field("failure_policy"), Some("required"));
        assert_eq!(
            metrics.counter_delta(
                RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC,
                &[
                    ("operation", "allocate"),
                    ("failure", "requested_value_unavailable"),
                    ("failure_policy", "required"),
                    ("allocation_mode", "requested"),
                    ("value_type", "integer"),
                ],
            ),
            1.0
        );
    }

    #[derive(Debug, Clone, Copy)]
    enum PolicySpecificFailureInput {
        Release,
        BestEffortDpuAsn,
    }

    impl PolicySpecificFailureInput {
        fn metric_labels(self) -> [(&'static str, &'static str); 5] {
            match self {
                Self::Release => [
                    ("operation", "release"),
                    ("failure", "database"),
                    ("failure_policy", "required"),
                    ("allocation_mode", "not_applicable"),
                    ("value_type", "ipv4"),
                ],
                Self::BestEffortDpuAsn => [
                    ("operation", "allocate"),
                    ("failure", "exhausted"),
                    ("failure_policy", "best_effort"),
                    ("allocation_mode", "automatic"),
                    ("value_type", "integer"),
                ],
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct PolicySpecificFailureRecord {
        metadata_name: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        level: tracing::Level,
        message: String,
        operation: Option<String>,
        failure: Option<String>,
        failure_policy: Option<String>,
        allocation_mode: Option<String>,
        value_type: Option<String>,
        counter_delta: f64,
    }

    #[test]
    fn policy_specific_resource_pool_failures_preserve_their_log_contracts() {
        check_values(
            [
                Check {
                    scenario: "release failure",
                    input: PolicySpecificFailureInput::Release,
                    expect: PolicySpecificFailureRecord {
                        metadata_name: "resource_pool_release_failed".to_string(),
                        event_name: Some("resource_pool_release_failed".to_string()),
                        metric_name: Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC.to_string()),
                        level: tracing::Level::ERROR,
                        message: "Error releasing value to resource pool".to_string(),
                        operation: Some("release".to_string()),
                        failure: Some("database".to_string()),
                        failure_policy: Some("required".to_string()),
                        allocation_mode: Some("not_applicable".to_string()),
                        value_type: Some("ipv4".to_string()),
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "best-effort DPU ASN allocation",
                    input: PolicySpecificFailureInput::BestEffortDpuAsn,
                    expect: PolicySpecificFailureRecord {
                        metadata_name: "dpu_asn_allocation_failed".to_string(),
                        event_name: Some("dpu_asn_allocation_failed".to_string()),
                        metric_name: Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC.to_string()),
                        level: tracing::Level::INFO,
                        message: "Failed to allocate asn for dpu".to_string(),
                        operation: Some("allocate".to_string()),
                        failure: Some("exhausted".to_string()),
                        failure_policy: Some("best_effort".to_string()),
                        allocation_mode: Some("automatic".to_string()),
                        value_type: Some("integer".to_string()),
                        counter_delta: 1.0,
                    },
                },
            ],
            |input| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| match input {
                    PolicySpecificFailureInput::Release => {
                        emit(ResourcePoolReleaseFailed {
                            operation: ResourcePoolOperation::Release,
                            failure: ResourcePoolFailure::Database,
                            failure_policy: ResourcePoolFailurePolicy::Required,
                            allocation_mode: ResourcePoolAllocationMode::NotApplicable,
                            value_type: ResourcePoolValueType::Ipv4,
                            error: "database unavailable".to_string(),
                            pool: "lo-ip".to_string(),
                            value: "192.0.2.10".to_string(),
                        });
                    }
                    PolicySpecificFailureInput::BestEffortDpuAsn => {
                        let error: ResourcePoolDatabaseError = ResourcePoolError::Empty.into();
                        emit_best_effort_dpu_asn_allocation_failure(
                            ValueType::Integer,
                            "dpu-1",
                            &error,
                        );
                    }
                });
                assert_eq!(logs.len(), 1);
                let log = &logs[0];
                let operation = log.field("operation").map(str::to_string);
                let failure = log.field("failure").map(str::to_string);
                let failure_policy = log.field("failure_policy").map(str::to_string);
                let allocation_mode = log.field("allocation_mode").map(str::to_string);
                let value_type = log.field("value_type").map(str::to_string);
                PolicySpecificFailureRecord {
                    metadata_name: log.metadata_name.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    level: log.level,
                    message: log.message.clone(),
                    operation,
                    failure,
                    failure_policy,
                    allocation_mode,
                    value_type,
                    counter_delta: metrics.counter_delta(
                        RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC,
                        &input.metric_labels(),
                    ),
                }
            },
        );
    }

    // `capture_logs_async` wraps only `release`, whose await uses this
    // connection. No unrelated work runs while the connection is held.
    #[crate::sqlx_test]
    async fn release_database_failure_emits_at_database_boundary(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut connection = pool.acquire().await?;
        // `sqlx_test` gives this case its own database. Hiding the table forces
        // `release` through its production error boundary without affecting
        // another test.
        sqlx::query("ALTER TABLE resource_pool RENAME TO unavailable_resource_pool")
            .execute(connection.as_mut())
            .await?;

        let resource_pool =
            ResourcePool::<IpAddr>::new("release-test".to_string(), ValueType::Ipv4);
        let value: IpAddr = "192.0.2.10".parse()?;
        let metrics = MetricsCapture::start();
        let (result, logs) =
            capture_logs_async(release(&resource_pool, connection.as_mut(), value)).await;

        let returned_error = result.expect_err("the renamed table must make release fail");
        assert!(
            returned_error
                .to_string()
                .contains("UPDATE resource_pool SET"),
            "the returned error retains its query context"
        );
        let event_logs = logs
            .iter()
            .filter(|log| log.metadata_name == "resource_pool_release_failed")
            .collect::<Vec<_>>();
        assert_eq!(event_logs.len(), 1);
        let log = event_logs[0];
        assert_eq!(log.level, tracing::Level::ERROR);
        assert_eq!(log.message, "Error releasing value to resource pool");
        assert_eq!(
            log.field("event_name"),
            Some("resource_pool_release_failed")
        );
        assert_eq!(
            log.field("metric_name"),
            Some(RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC)
        );
        assert_eq!(log.field("failure_policy"), Some("required"));
        assert_eq!(log.field("pool"), Some("release-test"));
        assert_eq!(log.field("value"), Some("192.0.2.10"));
        let logged_error = log.field("error").expect("release error context");
        assert!(
            logged_error.contains("relation \"resource_pool\" does not exist"),
            "the Event retains the database cause"
        );
        assert!(
            !logged_error.contains("UPDATE"),
            "the Event omits the failed query"
        );
        assert!(
            !logged_error.contains('\n'),
            "the Event error remains single-line"
        );
        assert_eq!(
            metrics.counter_delta(
                RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC,
                &[
                    ("operation", "release"),
                    ("failure", "database"),
                    ("failure_policy", "required"),
                    ("allocation_mode", "not_applicable"),
                    ("value_type", "ipv4"),
                ],
            ),
            1.0
        );

        Ok(())
    }

    /// A single successful auto-assign `allocate` must cost exactly one database
    /// round-trip. It previously cost two: a full-pool `stats()` pre-scan ran
    /// ahead of the allocation statement on every call. The pre-scan is now
    /// consulted only when the allocation statement returns no row, so the
    /// common success path is halved to a single query while the error mapping
    /// on the (rare) empty/unavailable path is unchanged.
    #[crate::sqlx_test]
    async fn allocate_issues_a_single_query(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};

        // Define the pool in its own committed transaction so the counted
        // region below is nothing but the allocation.
        let mut txn = pool.begin().await?;

        // A small auto-assign integer pool: 100..=104 (5 values).
        define(
            &mut txn,
            "test-query-count-pool",
            &ResourcePoolDef {
                prefix: None,
                ranges: vec![Range {
                    start: 100.to_string(),
                    end: 105.to_string(),
                    auto_assign: true,
                }],
                pool_type: ResourcePoolType::Integer,
                delegate_prefix_len: None,
            },
        )
        .await
        .unwrap();
        txn.commit().await?;

        let pool_handle =
            ResourcePool::<i64>::new("test-query-count-pool".to_string(), ValueType::Integer);

        // Measure exactly one successful allocation. The transaction lives
        // inside the counted future: BEGIN is queued without an executed
        // statement, and so is the rollback the drop performs, so neither
        // adds to the count.
        let (allocated, count) = count_queries(async {
            let mut txn = pool.begin().await.expect("begin");
            allocate(
                &pool_handle,
                &mut txn,
                OwnerType::Machine,
                "test-owner",
                None,
            )
            .await
        })
        .await;

        let value = allocated?;
        assert!((100..105).contains(&value), "allocated value in range");

        println!("allocate() issued {count} sqlx query(ies)");
        assert_eq!(
            count, 1,
            "a successful auto-assign allocate must be a single round-trip"
        );
        Ok(())
    }

    /// An exhausted auto-assign pool must report [`ResourcePoolError::Empty`]
    /// (the variant callers such as `allocate_loopback_ip` translate into
    /// `ResourceExhausted`). This behaviour used to come from the `stats()`
    /// pre-scan; it now comes from the allocation statement's `RowNotFound`.
    #[crate::sqlx_test]
    async fn allocate_empty_auto_assign_pool_is_empty_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;

        // Pool with a single auto-assign value, which we drain.
        define(
            &mut txn,
            "test-empty-auto-pool",
            &ResourcePoolDef {
                prefix: None,
                ranges: vec![Range {
                    start: 1.to_string(),
                    end: 2.to_string(),
                    auto_assign: true,
                }],
                pool_type: ResourcePoolType::Integer,
                delegate_prefix_len: None,
            },
        )
        .await
        .unwrap();

        let pool_handle =
            ResourcePool::<i64>::new("test-empty-auto-pool".to_string(), ValueType::Integer);

        // Drain the only value.
        allocate(&pool_handle, &mut txn, OwnerType::Machine, "owner-1", None).await?;

        // The pool is now empty; auto-assign must report `Empty`.
        let err = allocate(&pool_handle, &mut txn, OwnerType::Machine, "owner-2", None)
            .await
            .expect_err("draining then allocating from an empty pool must error");

        assert!(
            matches!(
                err,
                ResourcePoolDatabaseError::ResourcePool(ResourcePoolError::Empty)
            ),
            "empty auto-assign pool must map to ResourcePoolError::Empty, got {err:?}"
        );

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn allocate_exact_preserves_assignment_partitions(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let pool_handle =
            ResourcePool::<i32>::new("test-exact-pool".to_string(), ValueType::Integer);
        for auto_assign in [true, false] {
            let mut txn = pool.begin().await?;
            populate(&pool_handle, &mut txn, vec![41, 42], auto_assign).await?;
            let allocated =
                allocate_exact(&pool_handle, &mut txn, OwnerType::Vpc, "owner", 42).await?;
            assert_eq!(allocated, 42);

            let rows: Vec<(
                i32,
                sqlx::types::Json<ResourcePoolEntryState>,
                Option<bool>,
                bool,
            )> = sqlx::query_as(
                "SELECT value::integer, state, allocated = NOW(), auto_assign
                     FROM resource_pool WHERE name = $1 ORDER BY value::integer",
            )
            .bind(pool_handle.name())
            .fetch_all(&mut *txn)
            .await?;
            assert_eq!(
                rows,
                vec![
                    (
                        41,
                        sqlx::types::Json(ResourcePoolEntryState::Free),
                        None,
                        auto_assign,
                    ),
                    (
                        42,
                        sqlx::types::Json(ResourcePoolEntryState::Allocated {
                            owner: "owner".to_string(),
                            owner_type: OwnerType::Vpc.to_string(),
                        }),
                        Some(true),
                        auto_assign,
                    ),
                ],
                "auto_assign={auto_assign}",
            );
            txn.rollback().await?;
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn find_owned_allocation_returns_typed_value_and_rejects_duplicates(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;
        define(
            &mut txn,
            "test-owned-ipv6-pool",
            &ResourcePoolDef {
                prefix: Some("2001:db8::/126".to_string()),
                ranges: Vec::new(),
                pool_type: ResourcePoolType::Ipv6,
                delegate_prefix_len: None,
            },
        )
        .await?;
        let pool_handle =
            ResourcePool::<Ipv6Addr>::new("test-owned-ipv6-pool".to_string(), ValueType::Ipv6);

        assert_eq!(
            find_owned_allocation(&pool_handle, &mut txn, OwnerType::Machine, "owner-1",).await?,
            None
        );
        let first = allocate(&pool_handle, &mut txn, OwnerType::Machine, "owner-1", None).await?;
        assert_eq!(
            find_owned_allocation(&pool_handle, &mut txn, OwnerType::Machine, "owner-1",).await?,
            Some(first)
        );

        allocate(&pool_handle, &mut txn, OwnerType::Machine, "owner-1", None).await?;
        let error = find_owned_allocation(&pool_handle, &mut txn, OwnerType::Machine, "owner-1")
            .await
            .expect_err("duplicate reservations for one owner must be rejected");
        assert!(matches!(
            error,
            ResourcePoolDatabaseError::Database(ref boxed)
                if matches!(boxed.as_ref(), DatabaseError::FailedPrecondition(_))
        ));

        txn.rollback().await?;
        Ok(())
    }

    /// Requesting a specific value that the pool cannot hand out — while the
    /// pool still has free non-auto-assign entries — must map to
    /// `FailedPrecondition`, distinct from the `Empty` (exhausted) case. The
    /// pool is seeded non-empty on purpose so the asserted status code is what
    /// callers observe regardless of the pre-scan being present or deferred.
    #[crate::sqlx_test]
    async fn allocate_requested_value_unavailable_is_failed_precondition(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;

        // A pool of non-auto-assign values 10, 11, 12 — so it has free
        // non-auto-assign entries, but no auto-assign entries.
        define(
            &mut txn,
            "test-requested-unavail-pool",
            &ResourcePoolDef {
                prefix: None,
                ranges: vec![Range {
                    start: 10.to_string(),
                    end: 13.to_string(),
                    auto_assign: false,
                }],
                pool_type: ResourcePoolType::Integer,
                delegate_prefix_len: None,
            },
        )
        .await
        .unwrap();

        let pool_handle = ResourcePool::<i64>::new(
            "test-requested-unavail-pool".to_string(),
            ValueType::Integer,
        );

        // 99 is not in the pool, so the request cannot be satisfied even though
        // the pool is not exhausted (free non-auto-assign entries remain).
        let err = allocate(
            &pool_handle,
            &mut txn,
            OwnerType::Machine,
            "owner",
            Some(99),
        )
        .await
        .expect_err("requesting an unavailable value must error");

        match err {
            ResourcePoolDatabaseError::Database(boxed) => {
                assert!(
                    matches!(*boxed, DatabaseError::FailedPrecondition(_)),
                    "expected FailedPrecondition, got {boxed:?}"
                );
            }
            other => panic!("expected FailedPrecondition, got {other:?}"),
        }

        txn.rollback().await?;
        Ok(())
    }

    /// Requesting a specific value from an EXHAUSTED non-auto-assign pool must
    /// map to `Empty`, not `FailedPrecondition`: exhaustion wins regardless of
    /// whether the requested value was ever in the pool. The no-row branch
    /// checks `free == 0` before the requested-value case on purpose — this
    /// pins that ordering, which the old pre-scan enforced.
    #[crate::sqlx_test]
    async fn allocate_requested_value_from_exhausted_pool_is_empty_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;

        // A single non-auto-assign value, which we drain by requesting it.
        define(
            &mut txn,
            "test-exhausted-requested-pool",
            &ResourcePoolDef {
                prefix: None,
                ranges: vec![Range {
                    start: 20.to_string(),
                    end: 21.to_string(),
                    auto_assign: false,
                }],
                pool_type: ResourcePoolType::Integer,
                delegate_prefix_len: None,
            },
        )
        .await
        .unwrap();

        let pool_handle = ResourcePool::<i64>::new(
            "test-exhausted-requested-pool".to_string(),
            ValueType::Integer,
        );

        // Drain the only value.
        allocate(
            &pool_handle,
            &mut txn,
            OwnerType::Machine,
            "owner-1",
            Some(20),
        )
        .await?;

        // Requesting any value from the now-exhausted pool reports `Empty`.
        let err = allocate(
            &pool_handle,
            &mut txn,
            OwnerType::Machine,
            "owner-2",
            Some(20),
        )
        .await
        .expect_err("allocating from an exhausted pool must error");

        assert!(
            matches!(
                err,
                ResourcePoolDatabaseError::ResourcePool(ResourcePoolError::Empty)
            ),
            "an exhausted pool must map to ResourcePoolError::Empty even for a requested value, got {err:?}"
        );

        txn.rollback().await?;
        Ok(())
    }

    #[test]
    fn test_expand_ipv6_prefix_120_values() {
        let values = expand_ipv6_prefix("fd00:abcd::/120").expect("valid /120 prefix");
        assert_eq!(values.len(), 256);
        assert_eq!(values[0], "fd00:abcd::".parse::<Ipv6Addr>().unwrap());
        assert_eq!(values[255], "fd00:abcd::ff".parse::<Ipv6Addr>().unwrap());
    }

    #[test]
    fn test_expand_ipv4_prefix_slash24() {
        let values = expand_ip_prefix("192.168.1.0/24").expect("valid /24 prefix");
        // /24 has 256 addresses; expand_ip_prefix excludes the broadcast,
        // yielding .0 .. .254 inclusive (255 entries).
        assert_eq!(values.len(), 255);
        assert_eq!(values[0], "192.168.1.0".parse::<Ipv4Addr>().unwrap());
        assert_eq!(values[254], "192.168.1.254".parse::<Ipv4Addr>().unwrap());
    }

    #[crate::sqlx_test]
    async fn test_ipv4_pool_all_snapshots(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;

        // Define an IPv4 pool from a small range.
        define(
            &mut txn,
            "test-ipv4-pool",
            &ResourcePoolDef {
                prefix: None,
                ranges: vec![Range {
                    start: "10.0.0.1".to_string(),
                    end: "10.0.0.5".to_string(),
                    auto_assign: true,
                }],
                pool_type: ResourcePoolType::Ipv4,
                delegate_prefix_len: None,
            },
        )
        .await
        .unwrap();

        // Call `all()` and make sure types are correct.
        let snapshots = all(txn.as_mut()).await?;
        let ipv4_snap = snapshots
            .iter()
            .find(|s| s.name == "test-ipv4-pool")
            .unwrap();
        assert_eq!(ipv4_snap.min, "10.0.0.1");
        assert_eq!(ipv4_snap.max, "10.0.0.4");
        assert_eq!(ipv4_snap.stats.free, 4);

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_ipv6_pool_define_allocate_release(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;

        // Define an IPv6 pool from a /120 prefix (256 addresses).
        define(
            &mut txn,
            "test-ipv6-pool",
            &ResourcePoolDef {
                prefix: Some("fd00:abcd::/120".to_string()),
                ranges: vec![],
                pool_type: ResourcePoolType::Ipv6,
                delegate_prefix_len: None,
            },
        )
        .await
        .unwrap();

        // Verify pool stats.
        // /120 == 256 usable IPv6 addresses.
        let pool_stats = stats(txn.as_mut(), "test-ipv6-pool").await?;
        assert_eq!(pool_stats.free, 256);

        // `admin-cli resource-pool list` reads `all()`, so the IPv6 pool must
        // appear alongside the older integer and IPv4 pool types.
        let snapshots = all(txn.as_mut()).await?;
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.name == "test-ipv6-pool")
            .expect("IPv6 pool should be listed");
        assert_eq!(snapshot.min, "fd00:abcd::");
        assert_eq!(snapshot.max, "fd00:abcd::ff");
        assert_eq!(snapshot.stats.free, 256);

        // Allocate an address.
        let pool_handle =
            ResourcePool::<Ipv6Addr>::new("test-ipv6-pool".to_string(), ValueType::Ipv6);
        let addr: Ipv6Addr = allocate(
            &pool_handle,
            &mut txn,
            OwnerType::Machine,
            "test-owner",
            None,
        )
        .await?;
        assert!(addr.to_string().starts_with("fd00:abcd::"));

        // Stats should show 1 used.
        let pool_stats = stats(txn.as_mut(), "test-ipv6-pool").await?;
        assert_eq!(pool_stats.used, 1);
        assert_eq!(pool_stats.free, 255);

        // Release the address.
        release(&pool_handle, &mut txn, addr).await?;
        let pool_stats = stats(txn.as_mut(), "test-ipv6-pool").await?;
        assert_eq!(pool_stats.used, 0);
        assert_eq!(pool_stats.free, 256);

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_ipv6_pool_define_by_range(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;

        // Define an IPv6 pool from a range.
        define(
            &mut txn,
            "test-ipv6-range",
            &ResourcePoolDef {
                prefix: None,
                ranges: vec![Range {
                    start: "fd00::1".to_string(),
                    end: "fd00::11".to_string(),
                    auto_assign: true,
                }],
                pool_type: ResourcePoolType::Ipv6,
                delegate_prefix_len: None,
            },
        )
        .await
        .unwrap();

        // Should have 16 addresses (fd00::1 through fd00::10).
        let pool_stats = stats(txn.as_mut(), "test-ipv6-range").await?;
        assert_eq!(pool_stats.free, 16);

        txn.rollback().await?;
        Ok(())
    }

    #[test]
    fn test_expand_ipv6_prefix_rejects_large_prefix() {
        // A /64 would produce 2^64 addresses, which, as of this
        // writing, is beyond MAX_POOL_SIZE (and would cause us
        // to OOM anyway).
        let result = expand_ipv6_prefix("fd00::/64");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DefineResourcePoolError::TooBig(_, _)
        ));
    }

    #[test]
    fn test_expand_ipv6_prefix_boundary() {
        // MAX_POOL_SIZE (currently 250k) is the dividing line: a prefix
        // expands when its address count fits, and is rejected when it
        // doesn't. We assert on that count; the rejection error
        // (DefineResourcePoolError) isn't PartialEq, so the operation
        // discards it and we only assert that the case `Fails`.
        check_cases(
            [
                Case {
                    scenario: "/111 fits: 2^17 = 131,072 addresses",
                    input: "fd00::/111",
                    expect: Yields(131_072),
                },
                Case {
                    scenario: "/110 is too big: 2^18 = 262,144 addresses",
                    input: "fd00::/110",
                    expect: Fails,
                },
            ],
            |network| {
                expand_ipv6_prefix(network)
                    .map(|addrs| addrs.len())
                    .map_err(|_| ())
            },
        );
    }

    #[test]
    fn test_expand_ipv6_prefix_rejects_ipv4() {
        let result = expand_ipv6_prefix("192.168.1.0/24");
        assert!(result.is_err());
    }

    #[test]
    fn test_expand_ipv6_range() {
        let addrs = expand_ipv6_range("fd00::1", "fd00::4").unwrap();
        assert_eq!(addrs.len(), 3);
        assert_eq!(addrs[0], "fd00::1".parse::<Ipv6Addr>().unwrap());
        assert_eq!(addrs[1], "fd00::2".parse::<Ipv6Addr>().unwrap());
        assert_eq!(addrs[2], "fd00::3".parse::<Ipv6Addr>().unwrap());
    }

    #[test]
    fn test_expand_ipv6_range_rejects_ipv4() {
        let result = expand_ipv6_range("192.168.1.1", "192.168.1.5");
        assert!(result.is_err());
    }

    #[test]
    fn test_expand_ipv6_prefix_delegation_48_to_64() {
        // /48 to /64 == 65,536 sub-prefixes.
        let prefixes = expand_ipv6_prefix_delegation("fd00:abcd::/48", 64).unwrap();
        assert_eq!(prefixes.len(), 65_536);
        assert_eq!(
            prefixes[0],
            "fd00:abcd::/64".parse::<Ipv6Network>().unwrap()
        );
        assert_eq!(
            prefixes[65_535],
            "fd00:abcd:0000:ffff::/64".parse::<Ipv6Network>().unwrap()
        );
    }

    #[test]
    fn test_expand_ipv6_prefix_delegation_112_to_120() {
        // /112 to /120 == 256 sub-prefixes.
        let prefixes = expand_ipv6_prefix_delegation("fd00:abcd::/112", 120).unwrap();
        assert_eq!(prefixes.len(), 256);
        assert_eq!(
            prefixes[0],
            "fd00:abcd::/120".parse::<Ipv6Network>().unwrap()
        );
        assert_eq!(
            prefixes[1],
            "fd00:abcd::100/120".parse::<Ipv6Network>().unwrap()
        );
        assert_eq!(
            prefixes[255],
            "fd00:abcd::ff00/120".parse::<Ipv6Network>().unwrap()
        );
    }

    #[test]
    fn test_expand_ipv6_prefix_delegation_rejects_invalid() {
        // Delegation prefix length cant be less than the parent prefix length.
        let result = expand_ipv6_prefix_delegation("fd00::/64", 64);
        assert!(matches!(
            result.unwrap_err(),
            DefineResourcePoolError::InvalidArgument(_)
        ));

        let result = expand_ipv6_prefix_delegation("fd00::/64", 48);
        assert!(matches!(
            result.unwrap_err(),
            DefineResourcePoolError::InvalidArgument(_)
        ));
    }

    #[test]
    fn test_expand_ipv6_prefix_delegation_rejects_too_large() {
        // /32 to /64 == 4 billion, which is over the MAX_POOL_SIZE,
        // at least as of this writing, which is 250k.
        let result = expand_ipv6_prefix_delegation("fd00::/32", 64);
        assert!(matches!(
            result.unwrap_err(),
            DefineResourcePoolError::TooBig(_, _)
        ));
    }

    #[test]
    fn test_expand_ipv6_prefix_delegation_rejects_ipv4() {
        let result = expand_ipv6_prefix_delegation("192.168.0.0/16", 24);
        assert!(matches!(
            result.unwrap_err(),
            DefineResourcePoolError::InvalidArgument(_)
        ));
    }

    #[test]
    fn test_expand_ipv6_prefix_range() {
        // Range of /120s -- fd00::100/120 to fd00::300/120 -- gives us 2,
        // since we exclude the ending prefix.
        let prefixes = expand_ipv6_prefix_range("fd00::100/120", "fd00::300/120").unwrap();
        assert_eq!(prefixes.len(), 2);
        assert_eq!(prefixes[0], "fd00::100/120".parse::<Ipv6Network>().unwrap());
        assert_eq!(prefixes[1], "fd00::200/120".parse::<Ipv6Network>().unwrap());
    }

    #[test]
    fn test_expand_ipv6_prefix_range_rejects_mismatched_len() {
        let result = expand_ipv6_prefix_range("fd00::/64", "fd00:1::/48");
        assert!(matches!(
            result.unwrap_err(),
            DefineResourcePoolError::InvalidArgument(_)
        ));
    }

    #[test]
    fn test_expand_ipv6_prefix_range_rejects_unaligned() {
        // In this case, fd00::1/120 has host bits set, so it's
        // not a valid network prefix.
        let result = expand_ipv6_prefix_range("fd00::1/120", "fd00::200/120");
        assert!(matches!(
            result.unwrap_err(),
            DefineResourcePoolError::InvalidArgument(_)
        ));
    }

    #[crate::sqlx_test]
    async fn test_ipv6_prefix_pool_define_allocate_release(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;

        // Define pool w/ /112 and /120 delegation (giving us
        // 256 sub-prefixes).
        define(
            &mut txn,
            "test-ipv6prefix-pool",
            &ResourcePoolDef {
                prefix: Some("fd00:abcd::/112".to_string()),
                ranges: vec![],
                pool_type: ResourcePoolType::Ipv6Prefix,
                delegate_prefix_len: Some(120),
            },
        )
        .await
        .unwrap();

        // Verify pool stats.
        let pool_stats = stats(txn.as_mut(), "test-ipv6prefix-pool").await?;
        assert_eq!(pool_stats.free, 256);

        // Allocate a sub-prefix.
        let pool_handle = ResourcePool::<Ipv6Network>::new(
            "test-ipv6prefix-pool".to_string(),
            ValueType::Ipv6Prefix,
        );
        let prefix: Ipv6Network = allocate(
            &pool_handle,
            &mut txn,
            OwnerType::Machine,
            "test-owner",
            None,
        )
        .await?;
        assert_eq!(prefix.prefix(), 120);

        // Stats should show 1 used.
        let pool_stats = stats(txn.as_mut(), "test-ipv6prefix-pool").await?;
        assert_eq!(pool_stats.used, 1);
        assert_eq!(pool_stats.free, 255);

        // Release the sub-prefix.
        release(&pool_handle, &mut txn, prefix).await?;
        let pool_stats = stats(txn.as_mut(), "test-ipv6prefix-pool").await?;
        assert_eq!(pool_stats.used, 0);
        assert_eq!(pool_stats.free, 256);

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_ipv6_prefix_pool_define_by_range(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};

        let mut txn = pool.begin().await?;

        // Define pool from a range of /120s.
        define(
            &mut txn,
            "test-ipv6prefix-range",
            &ResourcePoolDef {
                prefix: None,
                ranges: vec![Range {
                    start: "fd00::0/120".to_string(),
                    end: "fd00::1000/120".to_string(),
                    auto_assign: true,
                }],
                pool_type: ResourcePoolType::Ipv6Prefix,
                delegate_prefix_len: None,
            },
        )
        .await
        .unwrap();

        // Here we've got fd00::0/120 to fd00::1000/120 (with our current
        // exclusive end), which gives us 16 prefixes.
        let pool_stats = stats(txn.as_mut(), "test-ipv6prefix-range").await?;
        assert_eq!(pool_stats.free, 16);

        txn.rollback().await?;
        Ok(())
    }

    // the pool has never been seeded. `reconcile_pool_defs`
    // should run `define` *and* record the snapshot.
    #[crate::sqlx_test]
    async fn reconcile_pool_defs_seeds_new_pool(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::Range;

        let def = ResourcePoolDef {
            prefix: None,
            ranges: vec![Range {
                start: 100.to_string(),
                end: 105.to_string(),
                auto_assign: true,
            }],
            pool_type: ResourcePoolType::Integer,
            delegate_prefix_len: None,
        };

        let mut txn = pool.begin().await?;

        let declared: HashMap<String, ResourcePoolDef> = [("brand-new".to_string(), def.clone())]
            .into_iter()
            .collect();

        reconcile_pool_defs(&mut txn, &declared).await?;

        let stored = stored_def(txn.as_mut(), "brand-new").await?;
        assert_eq!(
            stored.as_ref(),
            Some(&def),
            "snapshot must match declared def"
        );

        let snapshots = all(txn.as_mut()).await?;
        assert!(
            snapshots.iter().any(|s| s.name == "brand-new"),
            "pool should have been seeded into resource_pool"
        );

        txn.rollback().await?;
        Ok(())
    }

    // a pool exists in `resource_pool` but has no snapshot —
    // the deployment was created before the snapshot table existed.
    #[crate::sqlx_test]
    async fn reconcile_pool_defs_backfills_existing_pool(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::Range;

        let mut txn = pool.begin().await?;
        let def = ResourcePoolDef {
            prefix: None,
            ranges: vec![Range {
                start: 200.to_string(),
                end: 205.to_string(),
                auto_assign: true,
            }],
            pool_type: ResourcePoolType::Integer,
            delegate_prefix_len: None,
        };
        // Pre-seed: define the pool but skip insert_pool_def so the
        // snapshot table starts empty while resource_pool is non-empty.
        define(&mut txn, "preexisting", &def).await?;
        let row_count_before = all(txn.as_mut()).await?.len();

        let declared: HashMap<String, ResourcePoolDef> = [("preexisting".to_string(), def.clone())]
            .into_iter()
            .collect();

        reconcile_pool_defs(&mut txn, &declared).await?;

        let stored = stored_def(txn.as_mut(), "preexisting").await?;
        assert_eq!(stored.as_ref(), Some(&def), "snapshot must be backfilled");

        // Backfill must not call `define` again — `all()` row count
        // should be unchanged.
        let row_count_after = all(txn.as_mut()).await?.len();
        assert_eq!(
            row_count_before, row_count_after,
            "backfill path must not re-seed an existing pool"
        );

        txn.rollback().await?;
        Ok(())
    }

    // snapshot exists and matches the declared def. Reconcile
    // should be a silent no-op.
    #[crate::sqlx_test]
    async fn reconcile_pool_defs_in_sync_is_noop(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::Range;
        let def = ResourcePoolDef {
            prefix: None,
            ranges: vec![Range {
                start: 300.to_string(),
                end: 305.to_string(),
                auto_assign: true,
            }],
            pool_type: ResourcePoolType::Integer,
            delegate_prefix_len: None,
        };

        let mut txn = pool.begin().await?;

        define(&mut txn, "stable", &def).await?;
        insert_pool_def(&mut txn, "stable", &def).await?;

        let declared: HashMap<String, ResourcePoolDef> =
            [("stable".to_string(), def.clone())].into_iter().collect();

        reconcile_pool_defs(&mut txn, &declared).await?;

        let stored = stored_def(txn.as_mut(), "stable").await?;
        assert_eq!(
            stored.as_ref(),
            Some(&def),
            "snapshot must remain unchanged"
        );

        txn.rollback().await?;
        Ok(())
    }

    // snapshot exists but is different from the declared def. The
    // reconciler must *not* update either the pool or its snapshot — it
    // only logs a warning. Operator must intervene.
    #[crate::sqlx_test]
    async fn reconcile_pool_defs_drift_does_not_apply(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::Range;
        let original = ResourcePoolDef {
            prefix: None,
            ranges: vec![Range {
                start: 400.to_string(),
                end: 405.to_string(),
                auto_assign: true,
            }],
            pool_type: ResourcePoolType::Integer,
            delegate_prefix_len: None,
        };

        let drifted = ResourcePoolDef {
            prefix: None,
            ranges: vec![Range {
                start: 400.to_string(),
                end: 999.to_string(),
                auto_assign: true,
            }],
            pool_type: ResourcePoolType::Integer,
            delegate_prefix_len: None,
        };
        let mut txn = pool.begin().await?;

        define(&mut txn, "drifty", &original).await?;
        insert_pool_def(&mut txn, "drifty", &original).await?;

        let declared: HashMap<String, ResourcePoolDef> = [("drifty".to_string(), drifted.clone())]
            .into_iter()
            .collect();

        reconcile_pool_defs(&mut txn, &declared).await?;

        // The stored snapshot must still reflect the original
        let stored = stored_def(txn.as_mut(), "drifty").await?;
        assert_eq!(
            stored.as_ref(),
            Some(&original),
            "drift path must not overwrite the snapshot"
        );

        txn.rollback().await?;
        Ok(())
    }

    // snapshot exists but the pool has no rows in `resource_pool`.
    // The reconciler must surface the anomaly and leave the snapshot alone
    // so an operator can investigate.
    #[crate::sqlx_test]
    async fn reconcile_pool_defs_snapshot_without_pool_logs_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::resource_pool::Range;
        let def = ResourcePoolDef {
            prefix: None,
            ranges: vec![Range {
                start: 500.to_string(),
                end: 505.to_string(),
                auto_assign: true,
            }],
            pool_type: ResourcePoolType::Integer,
            delegate_prefix_len: None,
        };
        let mut txn = pool.begin().await?;

        // Pre-seed only the snapshot — skip define() so the pool has no
        // rows in resource_pool.
        insert_pool_def(&mut txn, "orphaned-snapshot", &def).await?;

        let declared: HashMap<String, ResourcePoolDef> =
            [("orphaned-snapshot".to_string(), def.clone())]
                .into_iter()
                .collect();

        // Reconcile must succeed (warn-only), not error.
        reconcile_pool_defs(&mut txn, &declared).await?;

        // The snapshot is unchanged.
        let stored = stored_def(txn.as_mut(), "orphaned-snapshot").await?;
        assert_eq!(
            stored.as_ref(),
            Some(&def),
            "anomaly path must not modify the snapshot"
        );

        // The pool was NOT seeded by reconcile.
        let pool_rows = all(txn.as_mut()).await?;
        assert!(
            !pool_rows.iter().any(|s| s.name == "orphaned-snapshot"),
            "anomaly path must not auto-seed the missing pool"
        );

        txn.rollback().await?;
        Ok(())
    }
}
