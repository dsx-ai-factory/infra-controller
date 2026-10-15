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
use std::sync::LazyLock;

use sqlx::PgPool;
use sqlx::migrate::{MigrateError, Migrator};

#[cfg(test)]
mod prefix_overlap_scope;

static MIGRATION_LAYOUT: LazyLock<MigrationLayout> = LazyLock::new(|| {
    MigrationLayout::new(
        sqlx::migrate!("./migrations.pre-squash.20260708172302"),
        vec![MigrationEpoch::from_source(
            20260708172302,
            sqlx::migrate!("./migrations"),
        )],
    )
});

struct MigrationLayout {
    legacy: Migrator,
    epochs: Vec<MigrationEpoch>,
}

struct MigrationEpoch {
    squash: Migrator,
    post_squash: Migrator,
    squash_version: i64,
}

trait IgnoringMissing {
    fn ignoring_missing(self) -> Self;
}

impl IgnoringMissing for Migrator {
    fn ignoring_missing(mut self) -> Self {
        self.set_ignore_missing(true);
        self
    }
}

impl MigrationLayout {
    fn new(legacy: Migrator, epochs: Vec<MigrationEpoch>) -> Self {
        // Crash if these things aren't valid... proceeding anyway can cause us to skip migrations
        // we actually wanted, which is difficult to recover from.
        assert!(
            !epochs.is_empty(),
            "at least one migration epoch is required"
        );
        assert!(
            epochs
                .windows(2)
                .all(|epochs| epochs[0].squash_version < epochs[1].squash_version),
            "migration epochs must be ordered by squash version"
        );

        Self {
            legacy: legacy.ignoring_missing(),
            epochs,
        }
    }

    async fn run(
        &self,
        pool: &PgPool,
        mut applied_versions: HashSet<i64>,
    ) -> Result<(), MigrateError> {
        if applied_versions.is_empty() {
            // A fresh database starts at the newest snapshot and does not traverse older epochs.
            let current_epoch = self.epochs.last().expect("migration layout has no epochs");
            current_epoch.squash.run(pool).await?;
            return current_epoch.post_squash.run(pool).await;
        }

        let latest_applied_epoch = self
            .epochs
            .iter()
            .rposition(|epoch| applied_versions.contains(&epoch.squash_version));

        // A database with no squash marker predates the first snapshot. If a marker exists, that
        // snapshot already incorporates every earlier epoch, so resume from the newest marker.
        let first_epoch = if let Some(latest_applied_epoch) = latest_applied_epoch {
            latest_applied_epoch
        } else {
            self.legacy.run(pool).await?;
            0
        };

        for epoch in &self.epochs[first_epoch..] {
            if !applied_versions.contains(&epoch.squash_version) {
                // `squash` contains exactly one migration, so this cannot skip a raced migration
                // merely because its timestamp sorts before the squash timestamp.
                epoch.squash.skip(pool, None).await?;
                applied_versions.insert(epoch.squash_version);
            }

            epoch.post_squash.run(pool).await?;
        }

        Ok(())
    }

    fn expected_checksums(&self) -> HashMap<i64, Vec<u8>> {
        std::iter::once(&self.legacy)
            .chain(
                self.epochs
                    .iter()
                    .flat_map(|epoch| [&epoch.squash, &epoch.post_squash]),
            )
            .flat_map(|migrator| migrator.iter())
            .map(|migration| (migration.version, migration.checksum.to_vec()))
            .collect::<HashMap<i64, Vec<u8>>>()
    }
}

impl MigrationEpoch {
    fn from_source(squash_version: i64, source: Migrator) -> Self {
        // Select the squash by its exact identity, then treat every other file as post-squash. In
        // particular, do not infer membership from migration timestamps: a concurrently-developed
        // migration may have an earlier timestamp and must still run after the snapshot.
        let (squash, post_squash): (Vec<_>, Vec<_>) = source
            .iter()
            .cloned()
            .partition(|migration| migration.version == squash_version);
        assert_eq!(
            squash.len(),
            1,
            "expected exactly one squash migration with version {squash_version}"
        );

        Self {
            squash: Migrator::with_migrations(squash).ignoring_missing(),
            post_squash: Migrator::with_migrations(post_squash).ignoring_missing(),
            squash_version,
        }
    }
}

#[tracing::instrument(skip(pool))]
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    let applied = load_and_validate_history(pool, &MIGRATION_LAYOUT).await?;
    let applied_versions: HashSet<_> = applied.into_iter().map(|(version, _)| version).collect();
    MIGRATION_LAYOUT.run(pool, applied_versions).await
}

async fn load_and_validate_history(
    pool: &PgPool,
    layout: &MigrationLayout,
) -> Result<Vec<(i64, Vec<u8>)>, MigrateError> {
    let migrations_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await?;

    if !migrations_table_exists {
        return Ok(Vec::new());
    }

    let applied: Vec<(i64, Vec<u8>, bool)> =
        sqlx::query_as("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;
    let expected = layout.expected_checksums();

    applied
        .into_iter()
        .map(|(version, checksum, success)| {
            if !success {
                return Err(MigrateError::Dirty(version));
            }

            let Some(expected_checksum) = expected.get(&version) else {
                return Err(MigrateError::VersionMissing(version));
            };

            if checksum != *expected_checksum {
                return Err(MigrateError::VersionMismatch(version));
            }

            Ok((version, checksum))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RENAME_SITE_PREFIX_AUTHORITY_VERSION: i64 = 20260805083545;
    const ASSOCIATE_VPC_PREFIX_SITE_PREFIX: &str =
        include_str!("../../migrations/20260805131227_associate_vpc_prefix_site_prefix.sql");
    const REMOVE_SECONDARY_VTEP_DATA: &str =
        include_str!("../../migrations/20260804153414_remove_secondary_vtep_data.sql");
    const VALIDATE_SECONDARY_VTEP_CONSTRAINT: &str =
        include_str!("../../migrations/20260804171948_validate_secondary_vtep_constraint.sql");

    #[test]
    fn migration_versions_are_unique() {
        let mut versions = HashSet::new();

        for migration in std::iter::once(&MIGRATION_LAYOUT.legacy)
            .chain(
                MIGRATION_LAYOUT
                    .epochs
                    .iter()
                    .flat_map(|epoch| [&epoch.squash, &epoch.post_squash]),
            )
            .flat_map(|migrator| migrator.iter())
        {
            assert!(
                versions.insert(migration.version),
                "duplicate migration version {}",
                migration.version
            );
        }
    }

    #[test]
    fn epochs_are_ordered_and_point_to_their_squash_migration() {
        let mut previous = None;

        for epoch in &MIGRATION_LAYOUT.epochs {
            assert!(
                previous.is_none_or(|version| version < epoch.squash_version),
                "migration epochs must be ordered by squash version"
            );
            assert_eq!(epoch.squash.iter().count(), 1);
            assert!(epoch.squash.version_exists(epoch.squash_version));
            assert!(
                epoch
                    .post_squash
                    .iter()
                    .all(|migration| migration.version != epoch.squash_version)
            );
            previous = Some(epoch.squash_version);
        }
    }

    // Ensure that if we squash migrations in one PR while a new migration is added in another PR,
    // we still apply the latter migration when merged. Basically, ensure that we don't just sort
    // by version number and skip all the way up to the squashed migration.
    #[test]
    fn post_squash_migrations_are_selected_by_identity_not_timestamp() {
        let configured_epoch = MIGRATION_LAYOUT.epochs.first().unwrap();
        let squash_version = configured_epoch.squash_version;
        let squash = configured_epoch.squash.iter().next().unwrap().clone();
        let mut raced_migration = squash.clone();
        raced_migration.version = squash_version - 1;
        let source = Migrator::with_migrations(vec![raced_migration, squash]);

        let epoch = MigrationEpoch::from_source(squash_version, source);

        assert_eq!(epoch.squash.iter().count(), 1);
        assert_eq!(epoch.post_squash.iter().count(), 1);
        assert!(epoch.post_squash.version_exists(squash_version - 1));
    }

    #[crate::sqlx_test]
    async fn site_prefix_authority_migration_preserves_rows_and_schema_objects(pool: PgPool) {
        sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
            .execute(&pool)
            .await
            .unwrap();

        let current_epoch = MIGRATION_LAYOUT.epochs.last().unwrap();
        assert!(
            current_epoch
                .post_squash
                .version_exists(RENAME_SITE_PREFIX_AUTHORITY_VERSION),
            "site prefix authority migration must belong to the current epoch"
        );
        current_epoch.squash.run(&pool).await.unwrap();

        let pre_rename_migrations = current_epoch
            .post_squash
            .iter()
            .filter(|migration| migration.version < RENAME_SITE_PREFIX_AUTHORITY_VERSION)
            .cloned()
            .collect();
        Migrator::with_migrations(pre_rename_migrations)
            .ignoring_missing()
            .run(&pool)
            .await
            .unwrap();

        let site_prefix_id = "626a8858-9896-4c14-9564-cb1f15e88b23";
        sqlx::query(
            r#"
                INSERT INTO site_prefixes (
                    id,
                    prefix,
                    authority,
                    tenant_organization_id,
                    routing_scope,
                    lifecycle_state,
                    name,
                    description,
                    labels,
                    version
                )
                VALUES (
                    $1::uuid,
                    '203.0.113.0/24',
                    'configured',
                    NULL,
                    'datacenter_only',
                    'ready',
                    'operator root',
                    'preserve every field',
                    '{"source":"migration-test"}',
                    '1'
                )
            "#,
        )
        .bind(site_prefix_id)
        .execute(&pool)
        .await
        .unwrap();
        let row_before: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(site_prefixes) - 'authority' \
             FROM site_prefixes WHERE id = $1::uuid",
        )
        .bind(site_prefix_id)
        .fetch_one(&pool)
        .await
        .unwrap();

        // Compare only the authority rename; later migrations can add columns.
        let rename_migration = current_epoch
            .post_squash
            .iter()
            .find(|migration| migration.version == RENAME_SITE_PREFIX_AUTHORITY_VERSION)
            .unwrap()
            .clone();
        Migrator::with_migrations(vec![rename_migration])
            .ignoring_missing()
            .run(&pool)
            .await
            .unwrap();

        let row_after: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(site_prefixes) - 'authority' \
             FROM site_prefixes WHERE id = $1::uuid",
        )
        .bind(site_prefix_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row_after, row_before);

        let authority: String =
            sqlx::query_scalar("SELECT authority::text FROM site_prefixes WHERE id = $1::uuid")
                .bind(site_prefix_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(authority, "operator_managed");

        let authority_labels: Vec<String> = sqlx::query_scalar(
            r#"
                SELECT enum_value.enumlabel::text
                FROM pg_enum enum_value
                JOIN pg_type enum_type ON enum_type.oid = enum_value.enumtypid
                JOIN pg_namespace namespace ON namespace.oid = enum_type.typnamespace
                WHERE namespace.nspname = 'public'
                  AND enum_type.typname = 'site_prefix_authority'
                ORDER BY enum_value.enumsortorder
            "#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            authority_labels,
            vec!["operator_managed".to_string(), "tenant_managed".to_string()]
        );

        let (
            operator_constraint_exists,
            configured_constraint_exists,
            operator_index_exists,
            configured_index_exists,
        ): (bool, bool, bool, bool) = sqlx::query_as(
            r#"
                SELECT
                    EXISTS (
                        SELECT 1
                        FROM pg_constraint
                        WHERE conrelid = 'site_prefixes'::regclass
                          AND conname = 'site_prefixes_operator_managed_lifecycle_check'
                    ),
                    EXISTS (
                        SELECT 1
                        FROM pg_constraint
                        WHERE conrelid = 'site_prefixes'::regclass
                          AND conname = 'site_prefixes_configured_lifecycle_check'
                    ),
                    to_regclass('public.site_prefixes_operator_managed_prefix_key') IS NOT NULL,
                    to_regclass('public.site_prefixes_configured_prefix_key') IS NOT NULL
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(operator_constraint_exists);
        assert!(!configured_constraint_exists);
        assert!(operator_index_exists);
        assert!(!configured_index_exists);
    }

    #[crate::sqlx_test]
    async fn vpc_prefix_site_prefix_migration_backfills_unique_operator_parents(pool: PgPool) {
        // An ordinary sqlx_test begins with every migration applied. This regression needs the
        // schema from immediately before this migration because the migration itself adds the
        // column under test. Remove only its FK, index, and column before applying its exact SQL.
        sqlx::raw_sql(
            r#"
                ALTER TABLE network_vpc_prefixes
                    DROP CONSTRAINT network_vpc_prefixes_site_prefix_id_fkey;
                DROP INDEX network_vpc_prefixes_site_prefix_id_idx;
                DROP INDEX network_prefixes_vpc_prefix_id_idx;
                ALTER TABLE network_vpc_prefixes
                    DROP COLUMN site_prefix_id;
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let vpc_id = "00000000-0000-0000-0000-000000003886";
        sqlx::query(
            r#"
                INSERT INTO vpcs (id, name, version)
                VALUES ($1::uuid, 'site-prefix migration VPC', 'V1-T0')
            "#,
        )
        .bind(vpc_id)
        .execute(&pool)
        .await
        .unwrap();

        let ready_parent_id = "00000000-0000-0000-0000-000000003901";
        let deleting_parent_id = "00000000-0000-0000-0000-000000003902";
        let broad_ambiguous_parent_id = "00000000-0000-0000-0000-000000003903";
        let narrow_ambiguous_parent_id = "00000000-0000-0000-0000-000000003904";
        sqlx::query(
            r#"
                INSERT INTO site_prefixes (
                    id,
                    prefix,
                    authority,
                    tenant_organization_id,
                    routing_scope,
                    lifecycle_state,
                    name,
                    version
                )
                VALUES
                    ($1::uuid, 'fd00:3886:1::/48', 'operator_managed', NULL,
                     'datacenter_only', 'ready', 'ready parent', 'V1-T0'),
                    ($2::uuid, 'fd00:3886:2::/48', 'operator_managed', NULL,
                     'datacenter_only', 'deleting', 'deleting parent', 'V1-T0'),
                    ($3::uuid, 'fd00:3886:3::/48', 'operator_managed', NULL,
                     'datacenter_only', 'ready', 'broad ambiguous parent', 'V1-T0'),
                    ($4::uuid, 'fd00:3886:3:1000::/52', 'operator_managed', NULL,
                     'datacenter_only', 'ready', 'narrow ambiguous parent', 'V1-T0')
            "#,
        )
        .bind(ready_parent_id)
        .bind(deleting_parent_id)
        .bind(broad_ambiguous_parent_id)
        .bind(narrow_ambiguous_parent_id)
        .execute(&pool)
        .await
        .unwrap();

        let ready_child_id = "00000000-0000-0000-0000-000000003911";
        let deleting_child_id = "00000000-0000-0000-0000-000000003912";
        let ambiguous_child_id = "00000000-0000-0000-0000-000000003913";
        let missing_child_id = "00000000-0000-0000-0000-000000003914";
        sqlx::query(
            r#"
                INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id)
                VALUES
                    ($1::uuid, 'fd00:3886:1:1::/64', 'ready child', $5::uuid),
                    ($2::uuid, 'fd00:3886:2:1::/64', 'deleting child', $5::uuid),
                    ($3::uuid, 'fd00:3886:3:1234::/64', 'ambiguous child', $5::uuid),
                    ($4::uuid, 'fd00:3886:4:1::/64', 'missing child', $5::uuid)
            "#,
        )
        .bind(ready_child_id)
        .bind(deleting_child_id)
        .bind(ambiguous_child_id)
        .bind(missing_child_id)
        .bind(vpc_id)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(ASSOCIATE_VPC_PREFIX_SITE_PREFIX)
            .execute(&pool)
            .await
            .unwrap();

        let parent_by_child: HashMap<String, Option<String>> = sqlx::query_as(
            r#"
                SELECT id::text, site_prefix_id::text
                FROM network_vpc_prefixes
                WHERE vpc_id = $1::uuid
            "#,
        )
        .bind(vpc_id)
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
        assert_eq!(parent_by_child.len(), 4);
        for (child_id, expected_parent_id) in [
            (ready_child_id, Some(ready_parent_id)),
            (deleting_child_id, Some(deleting_parent_id)),
            (ambiguous_child_id, None),
            (missing_child_id, None),
        ] {
            assert_eq!(
                parent_by_child
                    .get(child_id)
                    .and_then(|parent_id| parent_id.as_deref()),
                expected_parent_id,
                "child: {child_id}",
            );
        }

        let (constraint_is_valid, delete_action): (bool, String) = sqlx::query_as(
            r#"
                SELECT convalidated, confdeltype::text
                FROM pg_constraint
                WHERE conrelid = 'network_vpc_prefixes'::regclass
                  AND conname = 'network_vpc_prefixes_site_prefix_id_fkey'
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(constraint_is_valid);
        assert_eq!(delete_action, "r");

        sqlx::query("DELETE FROM site_prefixes WHERE id = $1::uuid")
            .bind(ready_parent_id)
            .execute(&pool)
            .await
            .expect_err("the associated VPC prefix must restrict parent deletion");

        let (site_prefix_index_exists, allocation_index_exists): (bool, bool) = sqlx::query_as(
            r#"
                SELECT
                    to_regclass('public.network_vpc_prefixes_site_prefix_id_idx') IS NOT NULL,
                    to_regclass('public.network_prefixes_vpc_prefix_id_idx') IS NOT NULL
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(site_prefix_index_exists);
        assert!(allocation_index_exists);
    }

    #[crate::sqlx_test]
    async fn secondary_vtep_migration_removes_persisted_data(pool: PgPool) {
        sqlx::query(
            "ALTER TABLE machines \
             DROP CONSTRAINT machines_network_config_excludes_secondary_vtep_ip",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO machines (id, network_config, dpf)
               VALUES (
                   'fm100dsecondaryvtep',
                   '{"loopback_ip":"192.0.2.1","secondary_overlay_vtep_ip":"198.51.100.1"}',
                   '{"enabled":true,"used_for_ingestion":false}'
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO resource_pool_def (name, definition)
               VALUES
                   ('secondary-vtep-ip', '{}'),
                   ('retained-pool', '{}')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO resource_pool
                   (name, value, allocated, state, value_type)
               VALUES
                   ('secondary-vtep-ip', '198.51.100.1', NOW(), '{"owner_id":"fm100dsecondaryvtep"}', 'ipv4'),
                   ('secondary-vtep-ip', '198.51.100.2', NULL, '{}', 'ipv4'),
                   ('retained-pool', '203.0.113.1', NULL, '{}', 'ipv4')"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(REMOVE_SECONDARY_VTEP_DATA)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(VALIDATE_SECONDARY_VTEP_CONSTRAINT)
            .execute(&pool)
            .await
            .unwrap();

        let network_config: serde_json::Value = sqlx::query_scalar(
            "SELECT network_config FROM machines WHERE id = 'fm100dsecondaryvtep'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(network_config["loopback_ip"], "192.0.2.1");
        assert!(network_config.get("secondary_overlay_vtep_ip").is_none());

        let obsolete_values: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resource_pool WHERE name = 'secondary-vtep-ip'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let obsolete_definitions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resource_pool_def WHERE name = 'secondary-vtep-ip'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(obsolete_values, 0);
        assert_eq!(obsolete_definitions, 0);

        let retained_values: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM resource_pool WHERE name = 'retained-pool'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let retained_definitions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resource_pool_def WHERE name = 'retained-pool'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retained_values, 1);
        assert_eq!(retained_definitions, 1);

        let constraint_is_valid: bool = sqlx::query_scalar(
            "SELECT convalidated FROM pg_constraint \
             WHERE conname = 'machines_network_config_excludes_secondary_vtep_ip' \
               AND conrelid = 'machines'::regclass",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(constraint_is_valid);
        sqlx::query(
            r#"UPDATE machines
               SET network_config = jsonb_set(
                   network_config,
                   '{secondary_overlay_vtep_ip}',
                   '"198.51.100.3"'
               )
               WHERE id = 'fm100dsecondaryvtep'"#,
        )
        .execute(&pool)
        .await
        .expect_err("the constraint must reject the removed field");
    }

    #[crate::sqlx_test]
    async fn fully_migrated_legacy_database_skips_squash(pool: PgPool) {
        // The test template is built by the fresh-install path, so its schema already
        // contains every post-squash migration. Rewinding only the _sqlx_migrations
        // markers would leave those columns in place, and migrate() would fail
        // re-applying the post-squash migrations against them. Instead rebuild a
        // faithful pre-squash database: drop the schema and apply only the legacy
        // migrations, so both the schema and the recorded history match a database
        // that predates the snapshot.
        sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
            .execute(&pool)
            .await
            .unwrap();

        MIGRATION_LAYOUT.legacy.run(&pool).await.unwrap();

        migrate(&pool).await.unwrap();

        let execution_time: i64 =
            sqlx::query_scalar("SELECT execution_time FROM _sqlx_migrations WHERE version = $1")
                .bind(MIGRATION_LAYOUT.epochs.first().unwrap().squash_version)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            execution_time, -1,
            "squash SQL must not run on legacy databases"
        );
    }
}
