// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::num::NonZeroU16;
use std::time::Duration;

use chrono::{DateTime, Utc};
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolValue,
};
use model::resource_pool::{OwnerType, ResourcePool, ValueType};
use serde_json::Value;
use sqlx::PgPool;

use super::{IgnoringMissing, MIGRATION_LAYOUT, Migrator};
use crate::ConditionalWrite::Applied;
use crate::db_read::DbReader;
use crate::resource_pool::{self, allocation};
use crate::test_support::postgres::wait_for_blocked_query;

const VERSION: i64 = 20261019201851;
const MIGRATION: &str =
    include_str!("../../migrations/20261019201851_resource_pool_allocations.sql");

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

#[crate::sqlx_test]
async fn allocation_migration_backfills_existing_allocations(pool: PgPool) -> eyre::Result<()> {
    predecessor(&pool).await?;
    sqlx::raw_sql(
        r#"INSERT INTO resource_pool (name, value, value_type, state, auto_assign, allocated)
           VALUES ('vpc-vni', '73', 'integer',
                   '{"state":"allocated","owner":"vpc-a","owner_type":"vpc"}',
                   true, '2026-10-01T12:00:00Z'),
                  ('integer', '-9223372036854775808', 'integer',
                   '{"state":"allocated","owner":"machine-a","owner_type":"machine"}',
                   NULL, NULL),
                  ('ipv4', '192.0.2.7', 'ipv4',
                   '{"state":"allocated","owner":"machine-a","owner_type":"machine"}',
                   NULL, NULL),
                  ('ipv6', '2001:db8::7', 'ipv6',
                   '{"state":"allocated","owner":"machine-a","owner_type":"machine"}',
                   NULL, NULL),
                  ('prefix', '2001:db8::/64', 'ipv6prefix',
                   '{"state":"allocated","owner":"machine-a","owner_type":"machine"}',
                   NULL, NULL),
                  ('free', '192.0.2.1', 'ipv4', '{"state":"free"}', true, NULL),
                  ('uninitialized', '192.0.2.2', 'ipv4', '{}', true, NULL);
           INSERT INTO resource_pool_def (name, definition)
           VALUES ('vpc-vni', '{"type":"integer","ranges":[{"start":"73","end":"74","auto_assign":true}]}');
           INSERT INTO resource_pool_binding (
               binding_id, pool_name, value_domain, backend_kind, authority_id, remote_pool
           ) VALUES (gen_random_uuid(), 'vpc-vni', 'integer', 'integrated', 'local-db', 'vpc-vni')"#,
    )
    .execute(&pool)
    .await?;
    let before = stored_integrated_pools(&pool).await?;
    let backfill_started: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&pool)
        .await?;

    // Pool setup writes its binding before inventory. The migration must wait
    // for that writer before taking the inventory lock it needs for backfill.
    let mut writer = pool.begin().await?;
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    sqlx::query("UPDATE resource_pool_binding SET backend_name = backend_name")
        .execute(&mut *writer)
        .await?;
    let migration = allocation_migration();
    let (migration_result, writer_result) = tokio::time::timeout(Duration::from_secs(35), async {
        tokio::join!(migration.run(&pool), async {
            let migration_pid =
                wait_for_blocked_query(&pool, writer_pid, "CREATE TABLE resource_pool_allocations")
                    .await;
            let inventory_locked: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks
                 WHERE pid = $1 AND relation = 'resource_pool'::regclass
                   AND mode = 'ShareRowExclusiveLock' AND granted)",
            )
            .bind(migration_pid)
            .fetch_one(&pool)
            .await?;
            if inventory_locked {
                writer.rollback().await?;
                eyre::bail!("migration locked inventory before waiting for the binding writer");
            }
            sqlx::query("UPDATE resource_pool SET value = value WHERE name = 'vpc-vni'")
                .execute(&mut *writer)
                .await?;
            writer.commit().await?;
            Ok::<_, eyre::Report>(())
        })
    })
    .await?;
    writer_result?;
    migration_result?;
    assert_eq!(stored_integrated_pools(&pool).await?, before);
    let (count, records_match): (i64, bool) = sqlx::query_as(
        "SELECT count(*), bool_and(
            a.pool_name = r.name AND a.value = r.value AND a.value_type = r.value_type
            AND a.owner_type = r.state->>'owner_type' AND a.owner_id = r.state->>'owner'
            AND a.auto_assign IS NOT DISTINCT FROM r.auto_assign
            AND a.allocated_at IS NOT DISTINCT FROM r.allocated
            AND a.created_at BETWEEN $1 AND clock_timestamp()
            AND a.updated_at BETWEEN a.created_at AND clock_timestamp() AND a.state = 'attached'
            AND a.binding_id IS NULL AND a.remote_request IS NULL AND a.result IS NULL
            AND r.state->>'state' = 'allocated')
         FROM resource_pool_allocations a
         JOIN resource_pool r ON r.id = a.integrated_resource_id",
    )
    .bind(backfill_started)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 5, "free and uninitialized values have no allocation");
    assert!(records_match);

    let limit = NonZeroU16::new(1).unwrap();
    for (pool_name, expected) in [
        ("integer", ResourcePoolValue::Integer(i64::MIN)),
        ("ipv4", ResourcePoolValue::IpAddress("192.0.2.7".parse()?)),
        ("ipv6", ResourcePoolValue::IpAddress("2001:db8::7".parse()?)),
        (
            "prefix",
            ResourcePoolValue::Ipv6Prefix("2001:db8::/64".parse()?),
        ),
    ] {
        let records = allocation::list(&pool, pool_name, None, limit).await?;
        assert_eq!(records.len(), 1, "{pool_name}");
        let AllocationSource::Integrated {
            value,
            assignment_mode,
            allocated_at,
            ..
        } = &records[0].source
        else {
            panic!("{pool_name}: expected Integrated allocation");
        };
        assert_eq!(*value, expected, "{pool_name}");
        assert_eq!(
            *assignment_mode, None,
            "unknown assignment mode stays absent"
        );
        assert_eq!(*allocated_at, None, "unknown allocation time stays absent");
        assert_eq!(records[0].state, AllocationState::Attached);
    }

    // The normal helpers can release a backfilled allocation and reuse its value.
    let inventory = ResourcePool::<i64>::new("vpc-vni".to_owned(), ValueType::Integer);
    let original = allocation::list(&pool, inventory.name(), None, limit)
        .await?
        .remove(0);
    let AllocationSource::Integrated {
        assignment_mode,
        allocated_at,
        ..
    } = &original.source
    else {
        panic!("expected Integrated allocation");
    };
    assert_eq!(*assignment_mode, Some(AssignmentMode::Automatic));
    assert_eq!(*allocated_at, Some("2026-10-01T12:00:00Z".parse()?));
    let mut connection = pool.acquire().await?;
    assert_eq!(
        resource_pool::release(&inventory, &mut connection, 73, OwnerType::Vpc, "vpc-a").await?,
        Applied(())
    );
    assert_eq!(allocation::load(&pool, original.allocation_id).await?, None);
    resource_pool::allocate_exact(&inventory, &mut connection, OwnerType::Vpc, "vpc-b", 73).await?;
    let replacement = allocation::list(&pool, inventory.name(), None, limit)
        .await?
        .remove(0);
    assert_ne!(replacement.allocation_id, original.allocation_id);
    assert_eq!(replacement.owner_id, "vpc-b");
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
            AND NOT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = $1)",
    )
    .bind(VERSION)
    .fetch_one(&pool)
    .await?;
    assert!(rolled_back);
    Ok(())
}
