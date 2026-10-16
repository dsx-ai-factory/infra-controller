// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::num::NonZeroU16;
use std::time::Duration;

use chrono::{DateTime, Utc};
use model::resource_pool::allocation::{AllocationSource, AllocationState, ResourcePoolValue};
use model::resource_pool::binding::INTEGRATED_AUTHORITY_ID;
use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolBackendKind, ResourcePoolSelection,
    ResourcePoolValueDomain, ValueType,
};
use serde_json::{Value, json};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::{IgnoringMissing, MIGRATION_LAYOUT, Migrator};
use crate::ConditionalWrite;
use crate::db_read::DbReader;
use crate::resource_pool::{self, allocation, binding};
use crate::test_support::postgres::wait_for_blocked_query;

const VERSION: i64 = 20261015234212;
const MIGRATION: &str =
    include_str!("../../migrations/20261015234212_resource_pool_allocations.sql");

async fn predecessor(pool: &PgPool) -> eyre::Result<()> {
    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .execute(pool)
        .await?;
    let epoch = MIGRATION_LAYOUT.epochs.last().unwrap();
    epoch.squash.run(pool).await?;
    Migrator::with_migrations(
        epoch
            .post_squash
            .iter()
            .filter(|migration| migration.version < VERSION)
            .cloned()
            .collect(),
    )
    .ignoring_missing()
    .run(pool)
    .await?;
    Ok(())
}

fn allocation_migration() -> Migrator {
    let migration = MIGRATION_LAYOUT
        .epochs
        .last()
        .unwrap()
        .post_squash
        .iter()
        .find(|migration| migration.version == VERSION)
        .expect("shared allocation migration must be registered");
    assert_eq!(migration.sql.as_ref(), MIGRATION);
    Migrator::with_migrations(vec![migration.clone()]).ignoring_missing()
}

async fn stored_integrated_pools(db: impl DbReader<'_>) -> Result<Value, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
            'resources', (SELECT jsonb_agg(to_jsonb(r) ORDER BY name, value) FROM resource_pool r),
            'definitions', (SELECT jsonb_agg(to_jsonb(d) ORDER BY name) FROM resource_pool_def d),
            'bindings', (SELECT jsonb_agg(to_jsonb(b) ORDER BY pool_name) FROM resource_pool_binding b)
        )",
    )
    .fetch_one(db)
    .await
}

// Keep the predecessor's statements here so changing the Rust helpers cannot
// silently replace the old writer exercised by the upgrade test.
async fn old_allocate(db: impl PgExecutor<'_>, name: &str, owner: &str) -> sqlx::Result<String> {
    sqlx::query_scalar(
        "WITH allocate AS (
            SELECT id, value FROM resource_pool
            WHERE name = $1 AND state = $2 AND auto_assign = $4
                AND (auto_assign = 't' OR value = $5)
            ORDER BY random()
            LIMIT 1
            FOR UPDATE SKIP LOCKED
        )
        UPDATE resource_pool SET state = $3, allocated = NOW()
        FROM allocate
        WHERE resource_pool.id = allocate.id
        RETURNING allocate.value",
    )
    .bind(name)
    .bind(json!({"state": "free"}))
    .bind(json!({"state": "allocated", "owner": owner, "owner_type": "vpc"}))
    .bind(true)
    .bind(None::<String>)
    .fetch_one(db)
    .await
}

async fn old_release(
    db: impl PgExecutor<'_>,
    name: &str,
    value: &str,
    owner: &str,
) -> sqlx::Result<u64> {
    sqlx::query(
        "UPDATE resource_pool SET allocated = NULL, state = $1
         WHERE name = $2 AND value = $3 AND state = $4",
    )
    .bind(json!({"state": "free"}))
    .bind(name)
    .bind(value)
    .bind(json!({"state": "allocated", "owner": owner, "owner_type": "vpc"}))
    .execute(db)
    .await
    .map(|result| result.rows_affected())
}

async fn allocation_id(pool: &PgPool, name: &str, value: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations WHERE pool_name = $1 AND value = $2",
    )
    .bind(name)
    .bind(value)
    .fetch_optional(pool)
    .await
}

#[crate::sqlx_test]
async fn allocation_migration_backfills_without_changing_inventory_or_old_writers(
    pool: PgPool,
) -> eyre::Result<()> {
    predecessor(&pool).await?;
    let vni_pool = ResourcePool::<i64>::new("vpc-vni".to_string(), ValueType::Integer);
    let definition = ResourcePoolDef {
        // Integer ranges exclude the end, so these are exactly the values 73 and 74.
        ranges: vec![Range {
            start: "73".to_string(),
            end: "75".to_string(),
            auto_assign: true,
        }],
        prefix: None,
        pool_type: ResourcePoolType::Integer,
        delegate_prefix_len: None,
    };
    let owner_a = "00000000-0000-0000-0000-000000007135";
    let owner_b = "00000000-0000-0000-0000-000000007136";
    let mut txn = pool.begin().await?;
    resource_pool::define(&mut txn, vni_pool.name(), &definition).await?;
    resource_pool::insert_pool_def(&mut txn, vni_pool.name(), &definition).await?;
    binding::bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: vni_pool.name().to_string(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: None,
            backend_kind: ResourcePoolBackendKind::Integrated,
            authority_id: INTEGRATED_AUTHORITY_ID.to_string(),
            remote_pool: vni_pool.name().to_string(),
        },
    )
    .await?;
    let vni = old_allocate(&mut *txn, vni_pool.name(), owner_a).await?;
    sqlx::raw_sql(
        r#"INSERT INTO resource_pool (name, value, value_type, state, auto_assign)
           VALUES ('historical', '00075', 'integer',
                   '{"state":"allocated","owner":"segment-a","owner_type":"network_segment"}',
                   NULL),
                  ('free', '192.0.2.1', 'ipv4', '{"state":"free"}', true),
                  ('uninitialized', '192.0.2.2', 'ipv4', '{}', true)"#,
    )
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;

    // The migration must wait for an old writer, then include its committed
    // allocation in the backfill. Later old writes must use the trigger.
    let mut writer = pool.begin().await?;
    let during_migration = old_allocate(&mut *writer, vni_pool.name(), owner_b).await?;
    let before = stored_integrated_pools(&mut *writer).await?;
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    let migration = allocation_migration();
    let migrate = async {
        migration.run(&pool).await?;
        Ok::<_, eyre::Report>(())
    };
    let commit_writer = async {
        wait_for_blocked_query(&pool, writer_pid, "resource_pool_operations").await;
        // This is after the migration transaction began, but before its backfill.
        let backfill_not_before: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *writer)
            .await?;
        writer.commit().await?;
        Ok::<_, eyre::Report>(backfill_not_before)
    };
    let ((), backfill_not_before) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(migrate, commit_writer)
    })
    .await??;
    assert_eq!(stored_integrated_pools(&pool).await?, before);

    let (allocation_count, projections_match): (i64, bool) = sqlx::query_as(
        "SELECT count(*), bool_and(
            a.pool_name = r.name AND a.value = r.value AND a.value_type = r.value_type
            AND a.owner_type = r.state->>'owner_type' AND a.owner_id = r.state->>'owner'
            AND a.auto_assign IS NOT DISTINCT FROM r.auto_assign
            AND a.allocated_at IS NOT DISTINCT FROM r.allocated
            AND a.recorded_at BETWEEN $1 AND clock_timestamp()
            AND a.remote_request_id IS NULL AND r.state->>'state' = 'allocated')
         FROM resource_pool_allocations a
         JOIN resource_pool r ON r.id = a.integrated_resource_id",
    )
    .bind(backfill_not_before)
    .fetch_one(&pool)
    .await?;
    assert_eq!(allocation_count, 3);
    assert!(projections_match);
    let historical_id = allocation_id(&pool, "historical", "00075").await?.unwrap();
    let historical = allocation::load(&pool, historical_id).await?.unwrap();
    assert_eq!(historical.value, ResourcePoolValue::Integer(75));
    assert_eq!(historical.assignment_mode, None);
    assert_eq!(historical.allocated_at, None);
    assert!(
        allocation_id(&pool, vni_pool.name(), &during_migration)
            .await?
            .is_some()
    );

    let original_id = allocation_id(&pool, vni_pool.name(), &vni).await?.unwrap();
    assert_eq!(old_release(&pool, vni_pool.name(), &vni, owner_a).await?, 1);
    assert_eq!(allocation_id(&pool, vni_pool.name(), &vni).await?, None);
    let mut connection = pool.acquire().await?;
    resource_pool::allocate_exact(
        &vni_pool,
        &mut connection,
        OwnerType::Vpc,
        owner_b,
        vni.parse::<i64>()?,
    )
    .await?;
    drop(connection);
    let current_id = allocation_id(&pool, vni_pool.name(), &vni).await?.unwrap();
    assert_ne!(current_id, original_id);

    // Both old statements run in autocommit after the new helper has written.
    assert_eq!(old_release(&pool, vni_pool.name(), &vni, owner_b).await?, 1);
    assert_eq!(old_allocate(&pool, vni_pool.name(), owner_a).await?, vni);
    let old_writer_id = allocation_id(&pool, vni_pool.name(), &vni).await?.unwrap();
    assert_ne!(old_writer_id, current_id);
    let mut connection = pool.acquire().await?;
    assert_eq!(
        resource_pool::release(
            &vni_pool,
            &mut connection,
            vni.parse::<i64>()?,
            OwnerType::Vpc,
            owner_a,
        )
        .await?,
        ConditionalWrite::Applied(())
    );
    assert_eq!(allocation_id(&pool, vni_pool.name(), &vni).await?, None);
    Ok(())
}

#[crate::sqlx_test]
async fn old_writer_waiting_on_migration_records_its_allocation(pool: PgPool) -> eyre::Result<()> {
    predecessor(&pool).await?;
    let name = "waiting-writer";
    let owner = "00000000-0000-0000-0000-000000007135";
    let resource_id: i64 = sqlx::query_scalar(
        r#"INSERT INTO resource_pool (name, value, value_type, state)
           VALUES ($1, '73', 'integer', '{"state":"free"}') RETURNING id"#,
    )
    .bind(name)
    .fetch_one(&pool)
    .await?;
    let mut writer = pool.acquire().await?;
    // Reuse the old statement on the same connection after its schema changes.
    assert_eq!(old_allocate(&mut *writer, name, owner).await?, "73");
    assert_eq!(old_release(&mut *writer, name, "73", owner).await?, 1);

    let mut migration = pool.begin().await?;
    let migration_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *migration)
        .await?;
    sqlx::raw_sql(MIGRATION).execute(&mut *migration).await?;
    let allocate = async {
        let value = old_allocate(&mut *writer, name, owner).await?;
        Ok::<_, eyre::Report>(value)
    };
    let commit_migration = async {
        wait_for_blocked_query(&pool, migration_pid, "UPDATE resource_pool").await;
        migration.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    let (value, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(allocate, commit_migration)
    })
    .await??;
    assert_eq!(value, "73");

    let records = allocation::list(&pool, name, None, NonZeroU16::new(2).unwrap()).await?;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.value, ResourcePoolValue::Integer(73));
    assert_eq!(record.owner_type, OwnerType::Vpc);
    assert_eq!(record.owner_id, owner);
    assert_eq!(record.source, AllocationSource::Integrated { resource_id });
    assert_eq!(record.state, AllocationState::Attached);
    assert!(record.allocated_at.is_some());
    Ok(())
}

#[crate::sqlx_test]
async fn allocation_migration_rolls_back_when_allocated_ownership_is_missing(
    pool: PgPool,
) -> eyre::Result<()> {
    predecessor(&pool).await?;
    sqlx::query(
        r#"INSERT INTO resource_pool (name, value, value_type, state)
           VALUES ('incomplete', '73', 'integer',
                   '{"state":"allocated","owner_type":"network_segment"}')"#,
    )
    .execute(&pool)
    .await?;
    let before = stored_integrated_pools(&pool).await?;
    let error = allocation_migration()
        .run(&pool)
        .await
        .expect_err("allocated inventory without an owner must fail visibly");
    assert!(error.to_string().contains("owner_id"), "{error}");
    assert_eq!(stored_integrated_pools(&pool).await?, before);
    let rolled_back: bool = sqlx::query_scalar(
        "SELECT to_regclass('resource_pool_allocations') IS NULL
            AND to_regclass('resource_pool_operations') IS NULL
            AND to_regprocedure('project_integrated_resource_pool_allocation()') IS NULL
            AND NOT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = $1)",
    )
    .bind(VERSION)
    .fetch_one(&pool)
    .await?;
    assert!(rolled_back);
    Ok(())
}
