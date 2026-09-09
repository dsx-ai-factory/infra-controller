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
use config_version::ConfigVersion;
use model::attestation::profile::{AttestationPolicyDocument, AttestationProfile};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

const KIND: &str = "attestation profile";

pub async fn create(
    txn: &mut PgConnection,
    hardware_class: &str,
    policy_document: &AttestationPolicyDocument,
    updated_by: &str,
) -> DatabaseResult<AttestationProfile> {
    policy_document.validate()?;

    let query = r#"
        INSERT INTO attestation_profiles (hardware_class, version, policy_document, updated_by)
        VALUES ($1, $2, $3::jsonb, $4)
        RETURNING *
    "#;
    sqlx::query_as(query)
        .bind(hardware_class)
        .bind(ConfigVersion::initial())
        .bind(sqlx::types::Json(policy_document))
        .bind(updated_by)
        .fetch_one(txn)
        .await
        .map_err(|error| match error {
            sqlx::Error::Database(db_error) if db_error.is_unique_violation() => {
                DatabaseError::AlreadyFoundError {
                    kind: KIND,
                    id: hardware_class.to_string(),
                }
            }
            error => DatabaseError::query(query, error),
        })
}

pub async fn find(
    db: impl DbReader<'_>,
    hardware_class: &str,
) -> DatabaseResult<Option<AttestationProfile>> {
    let query = "SELECT * FROM attestation_profiles WHERE hardware_class = $1";
    sqlx::query_as(query)
        .bind(hardware_class)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Reads one profile and holds it against concurrent writers, so a caller can
/// compare versions and write inside the same transaction.
pub async fn find_for_update(
    txn: &mut PgConnection,
    hardware_class: &str,
) -> DatabaseResult<Option<AttestationProfile>> {
    let query = "SELECT * FROM attestation_profiles WHERE hardware_class = $1 FOR UPDATE";
    sqlx::query_as(query)
        .bind(hardware_class)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Every profile, ordered by class so pages and diffs stay stable.
pub async fn list(db: impl DbReader<'_>) -> DatabaseResult<Vec<AttestationProfile>> {
    let query = "SELECT * FROM attestation_profiles ORDER BY hardware_class";
    sqlx::query_as(query)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Replaces the policy, failing if another writer has moved the row on since
/// `expected_version` was read.
pub async fn update(
    txn: &mut PgConnection,
    hardware_class: &str,
    policy_document: &AttestationPolicyDocument,
    updated_by: &str,
    expected_version: ConfigVersion,
) -> DatabaseResult<AttestationProfile> {
    policy_document.validate()?;

    let query = r#"
        UPDATE attestation_profiles
        SET policy_document = $1::jsonb,
            version = $2,
            updated_by = $3,
            updated_at = now()
        WHERE hardware_class = $4
          AND version = $5
        RETURNING *
    "#;
    sqlx::query_as(query)
        .bind(sqlx::types::Json(policy_document))
        .bind(expected_version.increment())
        .bind(updated_by)
        .bind(hardware_class)
        .bind(expected_version)
        .fetch_one(txn)
        .await
        .map_err(|error| match error {
            sqlx::Error::RowNotFound => {
                DatabaseError::ConcurrentModificationError(KIND, expected_version.to_string())
            }
            error => DatabaseError::query(query, error),
        })
}

/// Removes the profile, failing if another writer has moved the row on since
/// `expected_version` was read.
pub async fn delete(
    txn: &mut PgConnection,
    hardware_class: &str,
    expected_version: ConfigVersion,
) -> DatabaseResult<()> {
    let query = "DELETE FROM attestation_profiles WHERE hardware_class = $1 AND version = $2";
    let deleted = sqlx::query(query)
        .bind(hardware_class)
        .bind(expected_version)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    if deleted.rows_affected() == 0 {
        return Err(DatabaseError::ConcurrentModificationError(
            KIND,
            expected_version.to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use model::attestation::profile::{AttesterSelection, AttesterSelectionMode, ComponentIdMatch};

    use super::*;

    const OPERATOR: &str = "external-role/nico-admin";

    fn policy(
        mode: AttesterSelectionMode,
        ids: Vec<ComponentIdMatch>,
    ) -> AttestationPolicyDocument {
        AttestationPolicyDocument::new(AttesterSelection {
            mode,
            component_ids: ids,
        })
    }

    fn gpu_allowlist() -> AttestationPolicyDocument {
        policy(
            AttesterSelectionMode::Allowlist,
            vec![ComponentIdMatch::Prefix("HGX_IRoT_GPU_".to_string())],
        )
    }

    #[crate::sqlx_test]
    async fn create_stores_a_policy_that_reads_back_unchanged(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        let created = create(&mut txn, "Gb200", &gpu_allowlist(), OPERATOR)
            .await
            .unwrap();

        // A ConfigVersion carries a timestamp taken when it was minted, so
        // only the counter is comparable against a freshly built value.
        assert_eq!(
            created.version.version_nr(),
            ConfigVersion::initial().version_nr()
        );
        assert_eq!(created.updated_by, OPERATOR);

        let found = find(&mut *txn, "Gb200").await.unwrap().unwrap();
        assert_eq!(found.policy_document, gpu_allowlist());
        assert_eq!(found.version, created.version);
        assert!(
            find(&mut *txn, "DgxGb300").await.unwrap().is_none(),
            "a class with no profile reads as absent, not as a default"
        );
    }

    #[crate::sqlx_test]
    async fn a_class_can_hold_only_one_profile(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        create(&mut txn, "Gb200", &gpu_allowlist(), OPERATOR)
            .await
            .unwrap();

        let error = create(&mut txn, "Gb200", &gpu_allowlist(), OPERATOR)
            .await
            .expect_err("a second create for one class must be refused");

        assert!(
            matches!(error, DatabaseError::AlreadyFoundError { .. }),
            "expected an already-exists error, got {error:?}"
        );
    }

    #[crate::sqlx_test]
    async fn update_moves_the_version_on_and_refuses_a_stale_one(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let created = create(&mut txn, "Gb200", &gpu_allowlist(), OPERATOR)
            .await
            .unwrap();

        let replacement = policy(AttesterSelectionMode::All, vec![]);
        let updated = update(&mut txn, "Gb200", &replacement, OPERATOR, created.version)
            .await
            .unwrap();

        assert_eq!(
            updated.version.version_nr(),
            created.version.increment().version_nr()
        );
        assert_ne!(updated.version, created.version);
        assert_eq!(updated.policy_document, replacement);

        // The version the caller read is now stale, so a second write against
        // it must not silently overwrite the first.
        let error = update(
            &mut txn,
            "Gb200",
            &gpu_allowlist(),
            OPERATOR,
            created.version,
        )
        .await
        .expect_err("a stale version must be refused");

        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "expected a concurrent-modification error, got {error:?}"
        );
        let stored = find(&mut *txn, "Gb200").await.unwrap().unwrap();
        assert_eq!(
            stored.policy_document, replacement,
            "the refused write must leave the stored policy alone"
        );
    }

    #[crate::sqlx_test]
    async fn delete_refuses_a_stale_version_and_a_later_create_cannot_be_mistaken_for_the_old_row(
        pool: sqlx::PgPool,
    ) {
        let mut txn = pool.begin().await.unwrap();
        let created = create(&mut txn, "Gb200", &gpu_allowlist(), OPERATOR)
            .await
            .unwrap();
        let updated = update(
            &mut txn,
            "Gb200",
            &policy(AttesterSelectionMode::All, vec![]),
            OPERATOR,
            created.version,
        )
        .await
        .unwrap();

        let error = delete(&mut txn, "Gb200", created.version)
            .await
            .expect_err("a stale version must be refused");
        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "expected a concurrent-modification error, got {error:?}"
        );
        assert!(
            find(&mut *txn, "Gb200").await.unwrap().is_some(),
            "the refused delete must leave the row in place"
        );

        delete(&mut txn, "Gb200", updated.version).await.unwrap();
        assert!(find(&mut *txn, "Gb200").await.unwrap().is_none());

        // The counter restarts, but the token carries a timestamp, so the
        // version a caller read before the delete cannot match the new row.
        let recreated = create(&mut txn, "Gb200", &gpu_allowlist(), OPERATOR)
            .await
            .unwrap();
        assert_eq!(recreated.version.version_nr(), created.version.version_nr());
        assert_ne!(recreated.version, created.version);
    }

    #[crate::sqlx_test]
    async fn list_orders_by_class(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let all = policy(AttesterSelectionMode::All, vec![]);
        for class in ["Gb200", "any", "DgxGb300"] {
            create(&mut txn, class, &all, OPERATOR).await.unwrap();
        }

        let classes: Vec<_> = list(&mut *txn)
            .await
            .unwrap()
            .into_iter()
            .map(|profile| profile.hardware_class)
            .collect();

        assert_eq!(classes, ["DgxGb300", "Gb200", "any"]);
    }
}
