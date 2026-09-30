// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use sqlx::{Acquire, PgPool};

use super::{IgnoringMissing, MIGRATION_LAYOUT, Migrator};

const VERSION: i64 = 20260922070926;
const MIGRATION: &str =
    include_str!("../../migrations/20260922070926_add_prefix_overlap_scope.sql");
const CUTOVER_VERSION: i64 = 20260922073124;
const CUTOVER_MIGRATION: &str =
    include_str!("../../migrations/20260922073124_drop_global_prefix_exclusions.sql");
const VPC: &str = "00000000-0000-0000-0000-000000003891";
const OTHER_VPC: &str = "00000000-0000-0000-0000-000000003892";
const ROOT: &str = "00000000-0000-0000-0000-000000003893";
const PARENT: &str = "00000000-0000-0000-0000-000000003894";
const IPV6_PARENT: &str = "00000000-0000-0000-0000-000000003895";
const SEGMENT: &str = "00000000-0000-0000-0000-000000003896";

async fn seed_prefix_rows(pool: &PgPool) {
    sqlx::raw_sql(
        "INSERT INTO tenants (organization_id, organization_name, version)
         VALUES ('scope-test', 'scope test', 'V1-T0')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO vpcs (id, name, organization_id, network_virtualization_type, version)
         VALUES ($1::uuid, 'first', 'scope-test', 'fnn', 'V1-T0'),
                ($2::uuid, 'second', 'scope-test', 'fnn', 'V1-T0')",
    )
    .bind(VPC)
    .bind(OTHER_VPC)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO site_prefixes
         (id, prefix, authority, tenant_organization_id, routing_scope, lifecycle_state, name, version)
         VALUES ($1::uuid, '10.123.0.0/16', 'tenant_managed', 'scope-test',
                 'datacenter_only', 'ready', 'tenant root', 'V1-T0')",
    ).bind(ROOT).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id, site_prefix_id, deleted)
         VALUES ($1::uuid, '10.123.1.0/24', 'tenant child', $3::uuid, $4::uuid, NULL),
                ($2::uuid, 'fd00:3891::/64', 'deleting IPv6', $3::uuid, NULL, NOW())",
    )
    .bind(PARENT)
    .bind(IPV6_PARENT)
    .bind(VPC)
    .bind(ROOT)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO network_segments (id, name, version, vpc_id, network_segment_type, can_stretch)
         VALUES ($1::uuid, 'generated segment', 'V1-T0', $2::uuid, 'tenant', false)",
    ).bind(SEGMENT).bind(VPC).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO network_prefixes (segment_id, prefix, vpc_prefix_id, vpc_prefix)
         VALUES ($1::uuid, '10.123.1.0/31', $2::uuid, '10.123.1.0/24'),
                ($1::uuid, 'fd00:3891::/127', NULL, NULL)",
    )
    .bind(SEGMENT)
    .bind(PARENT)
    .execute(pool)
    .await
    .unwrap();
}

async fn stored_prefixes(pool: &PgPool) -> (serde_json::Value, serde_json::Value) {
    sqlx::query_as(
        "SELECT
            (SELECT jsonb_agg(to_jsonb(vp) - 'overlap_vpc_id' ORDER BY id)
             FROM network_vpc_prefixes vp),
            (SELECT jsonb_agg(to_jsonb(np) - 'overlap_vpc_id' ORDER BY id)
             FROM network_prefixes np)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn migrate_before(pool: &PgPool, version: i64) {
    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .execute(pool)
        .await
        .unwrap();
    let epoch = MIGRATION_LAYOUT.epochs.last().unwrap();
    epoch.squash.run(pool).await.unwrap();
    Migrator::with_migrations(
        epoch
            .post_squash
            .iter()
            .filter(|migration| migration.version < version)
            .cloned()
            .collect(),
    )
    .ignoring_missing()
    .run(pool)
    .await
    .unwrap();
}

#[crate::sqlx_test]
async fn scope_migration_preserves_rows_and_old_writers(pool: PgPool) {
    // Keep this expand-phase proof on its actual predecessor, even after
    // subsequent migrations remove the original exclusions.
    migrate_before(&pool, VERSION).await;
    let epoch = MIGRATION_LAYOUT.epochs.last().unwrap();
    seed_prefix_rows(&pool).await;
    let before = stored_prefixes(&pool).await;
    let migration = epoch
        .post_squash
        .iter()
        .find(|migration| migration.version == VERSION)
        .unwrap();
    assert_eq!(migration.sql.as_ref(), MIGRATION);
    Migrator::with_migrations(vec![migration.clone()])
        .ignoring_missing()
        .run(&pool)
        .await
        .unwrap();
    assert_eq!(stored_prefixes(&pool).await, before);
    let scoped_count: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM network_vpc_prefixes WHERE overlap_vpc_id IS NOT NULL)
              + (SELECT count(*) FROM network_prefixes WHERE overlap_vpc_id IS NOT NULL)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scoped_count, 0);

    // A new parent can be scoped while an older binary still creates and
    // attaches a global child. Neither old statement supplies the new column.
    sqlx::query("UPDATE network_vpc_prefixes SET overlap_vpc_id = vpc_id WHERE id = $1::uuid")
        .bind(PARENT)
        .execute(&pool)
        .await
        .unwrap();
    let old_parent_scope: Option<VpcId> = sqlx::query_scalar(
        "INSERT INTO network_vpc_prefixes (prefix, name, vpc_id, site_prefix_id)
         VALUES ('10.123.2.0/24', 'old writer', $1::uuid, $2::uuid)
         RETURNING overlap_vpc_id",
    )
    .bind(VPC)
    .bind(ROOT)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(old_parent_scope, None);
    let segment: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version) VALUES ('old segment', 'V1-T0') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let child: NetworkPrefixId = sqlx::query_scalar(
        "INSERT INTO network_prefixes (segment_id, prefix) VALUES ($1, '10.123.1.2/31') RETURNING id",
    ).bind(segment).fetch_one(&pool).await.unwrap();
    let child_scope: Option<VpcId> = sqlx::query_scalar(
        "UPDATE network_prefixes SET vpc_prefix_id = $1::uuid, vpc_prefix = '10.123.1.0/24'
         WHERE id = $2 RETURNING overlap_vpc_id",
    )
    .bind(PARENT)
    .bind(child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(child_scope, None);

    let constraints: Vec<(String, bool)> = sqlx::query_as(
        "SELECT conname::text, convalidated FROM pg_constraint
         WHERE conrelid IN ('network_vpc_prefixes'::regclass, 'network_prefixes'::regclass)
           AND (conname LIKE '%overlap%' OR contype = 'x') ORDER BY conname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(constraints.len(), 10);
    assert!(constraints.iter().all(|(_, valid)| *valid));
    assert!(
        constraints
            .iter()
            .any(|(name, _)| name == "network_vpc_prefixes_globally_unique")
    );
    assert!(
        constraints
            .iter()
            .any(|(name, _)| name == "network_prefixes_prefix_excl")
    );
}

#[crate::sqlx_test]
async fn scoped_prefix_constraints_reject_invalid_keys_and_conflicts(pool: PgPool) {
    migrate_before(&pool, CUTOVER_VERSION).await;
    seed_prefix_rows(&pool).await;
    let mut txn = pool.begin().await.unwrap();
    sqlx::query("UPDATE network_vpc_prefixes SET overlap_vpc_id = vpc_id WHERE id = $1::uuid")
        .bind(PARENT)
        .execute(&mut *txn)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE network_prefixes SET overlap_vpc_id = $1::uuid WHERE vpc_prefix_id = $2::uuid",
    )
    .bind(VPC)
    .bind(PARENT)
    .execute(&mut *txn)
    .await
    .unwrap();

    for (scenario, query, expected) in [
        (
            "scope belongs to another VPC",
            sqlx::query(
                "UPDATE network_vpc_prefixes SET overlap_vpc_id = $1::uuid WHERE id = $2::uuid",
            )
            .bind(OTHER_VPC)
            .bind(PARENT),
            "network_vpc_prefixes_overlap_scope_check",
        ),
        (
            "scoped parent has no SitePrefix",
            sqlx::query(
                "UPDATE network_vpc_prefixes SET site_prefix_id = NULL WHERE id = $1::uuid",
            )
            .bind(PARENT),
            "network_vpc_prefixes_overlap_scope_check",
        ),
        (
            "IPv6 parent cannot be scoped",
            sqlx::query(
                "UPDATE network_vpc_prefixes SET overlap_vpc_id = vpc_id, site_prefix_id = $1::uuid
                 WHERE id = $2::uuid",
            )
            .bind(ROOT)
            .bind(IPV6_PARENT),
            "network_vpc_prefixes_overlap_scope_check",
        ),
    ] {
        let mut attempt = txn.begin().await.unwrap();
        let error = query.execute(&mut *attempt).await.expect_err(scenario);
        assert_eq!(
            error.as_database_error().and_then(|e| e.constraint()),
            Some(expected),
            "{scenario}"
        );
        attempt.rollback().await.unwrap();
    }
    for (scenario, query, expected) in [
        (
            "scoped child has no parent",
            sqlx::query(
                "UPDATE network_prefixes SET vpc_prefix_id = NULL, vpc_prefix = NULL
                 WHERE vpc_prefix_id = $1::uuid",
            )
            .bind(PARENT),
            "network_prefixes_overlap_scope_check",
        ),
        (
            "child scope differs from its parent",
            sqlx::query(
                "UPDATE network_prefixes SET overlap_vpc_id = $1::uuid
                 WHERE vpc_prefix_id = $2::uuid",
            )
            .bind(OTHER_VPC)
            .bind(PARENT),
            "network_prefixes_overlap_scope_fkey",
        ),
    ] {
        let mut attempt = txn.begin().await.unwrap();
        let error = query.execute(&mut *attempt).await.expect_err(scenario);
        assert_eq!(
            error.as_database_error().and_then(|e| e.constraint()),
            Some(expected),
            "{scenario}"
        );
        attempt.rollback().await.unwrap();
    }

    let other_parent = VpcPrefixId::new();
    let insert_parent = "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id, site_prefix_id, overlap_vpc_id)
                         VALUES ($1, '10.123.1.0/24', 'overlapping parent', $2::uuid, $3::uuid, $2::uuid)";
    let mut attempt = txn.begin().await.unwrap();
    let error = sqlx::query(insert_parent)
        .bind(other_parent)
        .bind(OTHER_VPC)
        .bind(ROOT)
        .execute(&mut *attempt)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.constraint()),
        Some("network_vpc_prefixes_globally_unique")
    );
    attempt.rollback().await.unwrap();

    // Remove only the old constraints in this private, rolled-back transaction
    // so their rejection cannot mask a broken replacement constraint.
    sqlx::query(
        "ALTER TABLE network_vpc_prefixes DROP CONSTRAINT network_vpc_prefixes_globally_unique",
    )
    .execute(&mut *txn)
    .await
    .unwrap();
    sqlx::query(insert_parent)
        .bind(other_parent)
        .bind(OTHER_VPC)
        .bind(ROOT)
        .execute(&mut *txn)
        .await
        .unwrap();
    let segment: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version) VALUES ('other scope', 'V1-T0') RETURNING id",
    )
    .fetch_one(&mut *txn)
    .await
    .unwrap();
    let insert_child = "INSERT INTO network_prefixes (segment_id, prefix, vpc_prefix_id, vpc_prefix, overlap_vpc_id)
                        VALUES ($1, '10.123.1.0/31', $2, '10.123.1.0/24', $3::uuid)";
    let mut attempt = txn.begin().await.unwrap();
    let error = sqlx::query(insert_child)
        .bind(segment)
        .bind(other_parent)
        .bind(OTHER_VPC)
        .execute(&mut *attempt)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.constraint()),
        Some("network_prefixes_prefix_excl")
    );
    attempt.rollback().await.unwrap();
    sqlx::query("ALTER TABLE network_prefixes DROP CONSTRAINT network_prefixes_prefix_excl")
        .execute(&mut *txn)
        .await
        .unwrap();
    sqlx::query(insert_child)
        .bind(segment)
        .bind(other_parent)
        .bind(OTHER_VPC)
        .execute(&mut *txn)
        .await
        .unwrap();
    let empty_segment: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version) VALUES ('conflict attempt', 'V1-T0') RETURNING id",
    ).fetch_one(&mut *txn).await.unwrap();

    for (scenario, query, expected) in [
        (
            "global parents remain disjoint",
            sqlx::query(
                "INSERT INTO network_vpc_prefixes (prefix, name, vpc_id)
                 VALUES ('fd00:3891::/64', 'duplicate', $1::uuid)",
            )
            .bind(VPC),
            "network_vpc_prefixes_global_prefix_excl",
        ),
        (
            "scoped parents remain disjoint within one VPC",
            sqlx::query(
                "INSERT INTO network_vpc_prefixes (prefix, name, vpc_id, site_prefix_id, overlap_vpc_id)
                 VALUES ('10.123.1.128/25', 'duplicate', $1::uuid, $2::uuid, $1::uuid)",
            )
            .bind(VPC)
            .bind(ROOT),
            "network_vpc_prefixes_scoped_prefix_excl",
        ),
        (
            "global children remain disjoint",
            sqlx::query(
                "INSERT INTO network_prefixes (segment_id, prefix)
                 VALUES ($1, 'fd00:3891::/127')",
            )
            .bind(segment),
            "network_prefixes_global_prefix_excl",
        ),
        (
            "scoped children remain disjoint within one VPC",
            sqlx::query(
                "INSERT INTO network_prefixes (segment_id, prefix, vpc_prefix_id, vpc_prefix, overlap_vpc_id)
                 VALUES ($1, '10.123.1.0/31', $2::uuid, '10.123.1.0/24', $3::uuid)",
            )
            .bind(empty_segment)
            .bind(PARENT)
            .bind(VPC),
            "network_prefixes_scoped_prefix_excl",
        ),
    ] {
        let mut attempt = txn.begin().await.unwrap();
        let error = query.execute(&mut *attempt).await.expect_err(scenario);
        assert_eq!(error.as_database_error().and_then(|e| e.constraint()), Some(expected), "{scenario}");
        attempt.rollback().await.unwrap();
    }

    for (prefix, overlap_vpc_id) in [
        ("fd00:3891::/64", None),
        ("10.123.1.0/24", Some(VPC.parse::<VpcId>().unwrap())),
    ] {
        let mut attempt = txn.begin().await.unwrap();
        let error = crate::vpc_prefix::persist(
            model::vpc_prefix::NewVpcPrefix {
                id: VpcPrefixId::new(),
                site_prefix_id: Some(ROOT.parse().unwrap()),
                vpc_id: VPC.parse().unwrap(),
                overlap_vpc_id,
                config: model::vpc_prefix::VpcPrefixConfig {
                    prefix: prefix.parse().unwrap(),
                },
                metadata: model::metadata::Metadata::new_with_default_name(),
            },
            config_version::ConfigVersion::initial(),
            &mut attempt,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, crate::DatabaseError::InvalidArgument(message)
            if message == format!("The requested VPC prefix ({prefix}) overlaps an existing or deleting VPC prefix"))
        );
        attempt.rollback().await.unwrap();
    }
    txn.rollback().await.unwrap();
}

#[crate::sqlx_test]
async fn overlap_cutover_preserves_rows_and_global_predecessor_writes(pool: PgPool) {
    migrate_before(&pool, CUTOVER_VERSION).await;
    seed_prefix_rows(&pool).await;
    let before = stored_prefixes(&pool).await;
    let migration = MIGRATION_LAYOUT
        .epochs
        .last()
        .unwrap()
        .post_squash
        .iter()
        .find(|migration| migration.version == CUTOVER_VERSION)
        .unwrap();
    assert_eq!(migration.sql.as_ref(), CUTOVER_MIGRATION);
    Migrator::with_migrations(vec![migration.clone()])
        .ignoring_missing()
        .run(&pool)
        .await
        .unwrap();
    assert_eq!(stored_prefixes(&pool).await, before);
    // Broad overlap probes cannot use either scope-specific partial index.
    for (table, index) in [
        ("network_prefixes", "network_prefixes_prefix_idx"),
        ("network_vpc_prefixes", "network_vpc_prefixes_prefix_idx"),
    ] {
        let full_overlap_index: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM pg_index i
                JOIN pg_class c ON c.oid = i.indexrelid
                JOIN pg_am am ON am.oid = c.relam
                JOIN pg_opclass op ON op.oid = i.indclass[0]
                JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0]
                WHERE i.indrelid = $1::regclass AND i.indexrelid = $2::regclass
                  AND i.indisvalid AND NOT i.indisexclusion
                  AND i.indpred IS NULL AND i.indexprs IS NULL
                  AND i.indnatts = 1 AND i.indnkeyatts = 1
                  AND a.attname = 'prefix'
                  AND am.amname = 'gist' AND op.opcname = 'inet_ops'
            )",
        )
        .bind(table)
        .bind(index)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(full_overlap_index, "missing full overlap index for {table}");
    }
    let scoped_rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM network_vpc_prefixes WHERE overlap_vpc_id IS NOT NULL)
              + (SELECT count(*) FROM network_prefixes WHERE overlap_vpc_id IS NOT NULL)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scoped_rows, 0);

    // These are the predecessor's SQL statements, not an old-binary test.
    // Omitting scope must still create globally exclusive rows after cutover.
    let old_parent: (VpcPrefixId, Option<VpcId>) = sqlx::query_as(
        "INSERT INTO network_vpc_prefixes (prefix, name, vpc_id)
         VALUES ('10.123.2.0/24', 'old writer', $1::uuid) RETURNING id, overlap_vpc_id",
    )
    .bind(VPC)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(old_parent.1, None);
    let old_segment = NetworkSegmentId::new();
    let conflicting_segment = NetworkSegmentId::new();
    for (segment_id, name) in [
        (old_segment, "old writer"),
        (conflicting_segment, "overlapping old writer"),
    ] {
        sqlx::query(
            "INSERT INTO network_segments (id, name, version, vpc_id, network_segment_type, can_stretch)
             VALUES ($1, $2, 'V1-T0', $3::uuid, 'tenant', false)",
        )
        .bind(segment_id)
        .bind(name)
        .bind(VPC)
        .execute(&pool)
        .await
        .unwrap();
    }
    let old_child_scope: Option<VpcId> = sqlx::query_scalar(
        "INSERT INTO network_prefixes (segment_id, prefix, vpc_prefix_id, vpc_prefix)
         VALUES ($1::uuid, '10.123.2.0/31', $2, '10.123.2.0/24') RETURNING overlap_vpc_id",
    )
    .bind(old_segment)
    .bind(old_parent.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(old_child_scope, None);

    for (scenario, query, expected) in [
        (
            "deleting global parent remains reserved",
            sqlx::query(
                "INSERT INTO network_vpc_prefixes (prefix, name, vpc_id)
                         VALUES ('fd00:3891::/64', 'overlapping old writer', $1::uuid)",
            )
            .bind(OTHER_VPC),
            "network_vpc_prefixes_global_prefix_excl",
        ),
        (
            "global child remains reserved",
            sqlx::query(
                "INSERT INTO network_prefixes (segment_id, prefix)
                         VALUES ($1::uuid, '10.123.2.0/31')",
            )
            .bind(conflicting_segment),
            "network_prefixes_global_prefix_excl",
        ),
    ] {
        let error = query.execute(&pool).await.expect_err(scenario);
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.constraint()),
            Some(expected),
            "{scenario}"
        );
    }
}
