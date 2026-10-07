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

//! Persistence for named NIC firmware profile definitions.

use config_version::ConfigVersion;
use model::nic_firmware::{NicFirmwareProfile, NicFirmwareProfileConfig, NicFirmwareProfileId};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

const KIND: &str = "NIC firmware profile";

/// `create` stores a profile at its initial version; duplicate IDs return
/// `AlreadyFoundError` without replacing the existing definition.
pub async fn create(
    txn: &mut PgConnection,
    id: &NicFirmwareProfileId,
    config: &NicFirmwareProfileConfig,
) -> DatabaseResult<NicFirmwareProfile> {
    let query = r#"
        INSERT INTO nic_firmware_profiles (id, config, version)
        VALUES ($1, $2, $3)
        RETURNING id, config, version
    "#;
    sqlx::query_as(query)
        .bind(id)
        .bind(sqlx::types::Json(config))
        .bind(ConfigVersion::initial())
        .fetch_one(txn)
        .await
        .map_err(|error| match error {
            sqlx::Error::Database(error) if error.is_unique_violation() => {
                DatabaseError::AlreadyFoundError {
                    kind: KIND,
                    id: id.to_string(),
                }
            }
            error => DatabaseError::query(query, error),
        })
}

/// `find_ids` lists profile IDs in ascending database order.
pub async fn find_ids(db: impl DbReader<'_>) -> DatabaseResult<Vec<NicFirmwareProfileId>> {
    let query = "SELECT id FROM nic_firmware_profiles ORDER BY id";
    sqlx::query_scalar(query)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `find_by_ids` returns existing profiles once each, in ascending ID order.
/// Missing IDs are omitted, and an empty list returns no profiles.
pub async fn find_by_ids(
    db: impl DbReader<'_>,
    ids: &[NicFirmwareProfileId],
) -> DatabaseResult<Vec<NicFirmwareProfile>> {
    let query =
        "SELECT id, config, version FROM nic_firmware_profiles WHERE id = ANY($1) ORDER BY id";
    sqlx::query_as(query)
        .bind(ids)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `update` replaces the entire config and increments its version.
/// Missing profiles return `NotFoundError`; stale tokens return
/// `ConcurrentModificationError` without changing the definition.
pub async fn update(
    txn: &mut PgConnection,
    id: &NicFirmwareProfileId,
    config: &NicFirmwareProfileConfig,
    expected_version: ConfigVersion,
) -> DatabaseResult<NicFirmwareProfile> {
    let query = r#"
        UPDATE nic_firmware_profiles SET config = $1, version = $2
        WHERE id = $3 AND version = $4
        RETURNING id, config, version
    "#;
    let updated = sqlx::query_as(query)
        .bind(sqlx::types::Json(config))
        .bind(expected_version.increment())
        .bind(id)
        .bind(expected_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    match updated {
        Some(profile) => Ok(profile),
        None => Err(unmatched_version(txn, id, expected_version).await?),
    }
}

/// `delete` removes a profile whose complete version matches the caller's token.
/// Missing profiles return `NotFoundError`; stale tokens return
/// `ConcurrentModificationError` and leave the profile in place.
pub async fn delete(
    txn: &mut PgConnection,
    id: &NicFirmwareProfileId,
    expected_version: ConfigVersion,
) -> DatabaseResult<()> {
    let query = "DELETE FROM nic_firmware_profiles WHERE id = $1 AND version = $2";
    let deleted = sqlx::query(query)
        .bind(id)
        .bind(expected_version)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if deleted.rows_affected() == 0 {
        return Err(unmatched_version(txn, id, expected_version).await?);
    }
    Ok(())
}

async fn unmatched_version(
    txn: &mut PgConnection,
    id: &NicFirmwareProfileId,
    expected_version: ConfigVersion,
) -> DatabaseResult<DatabaseError> {
    let query = "SELECT EXISTS(SELECT 1 FROM nic_firmware_profiles WHERE id = $1)";
    let exists: bool = sqlx::query_scalar(query)
        .bind(id)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(if exists {
        DatabaseError::ConcurrentModificationError(KIND, expected_version.to_string())
    } else {
        DatabaseError::NotFoundError {
            kind: KIND,
            id: id.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use carbide_libmlx_model::firmware::FirmwareSpec;
    use model::nic_firmware::{NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareProfileEntry};

    use super::*;
    use crate::test_support::postgres::wait_for_blocked_query;

    #[crate::sqlx_test]
    async fn concurrent_profile_update_and_stale_delete_preserve_the_winner(pool: sqlx::PgPool) {
        let id: NicFirmwareProfileId = "profile".parse().unwrap();
        let original = NicFirmwareProfileConfig {
            entries: vec![NicFirmwareProfileEntry {
                firmware: FirmwareSpec {
                    part_number: "MCX75310AAS-NEAT".into(),
                    psid: "MT_0000000838".into(),
                    version: "28.43.1014".into(),
                },
                image: NicFirmwareArtifact {
                    url: "https://firmware.invalid/image.bin".parse().unwrap(),
                    sha256: "ab".repeat(32),
                },
                device_config: None,
                approach: NicFirmwareApproach::Scout,
            }],
        };
        let mut txn = pool.begin().await.unwrap();
        let created = create(&mut txn, &id, &original).await.unwrap();
        txn.commit().await.unwrap();

        let mut replacement = original.clone();
        replacement.entries[0].firmware.version = "28.42.1000".into();
        let mut winner = pool.begin().await.unwrap();
        let updated = update(&mut winner, &id, &replacement, created.version)
            .await
            .unwrap();
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *winner)
            .await
            .unwrap();

        // Hold the winning write until PostgreSQL confirms the competing writer
        // is waiting. It must compare against the committed version after waking.
        let competing_update = async {
            let mut txn = pool.begin().await.unwrap();
            let error = update(&mut txn, &id, &original, created.version)
                .await
                .unwrap_err();
            txn.rollback().await.unwrap();
            error
        };
        let release_winner = async {
            wait_for_blocked_query(&pool, blocker_pid, "UPDATE nic_firmware_profiles").await;
            winner.commit().await.unwrap();
        };
        let (error, ()) = tokio::join!(competing_update, release_winner);
        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "{error:?}"
        );

        let mut txn = pool.begin().await.unwrap();
        let error = delete(&mut txn, &id, created.version).await.unwrap_err();
        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "{error:?}"
        );
        txn.rollback().await.unwrap();
        let mut txn = pool.begin().await.unwrap();
        let error = create(&mut txn, &id, &original).await.unwrap_err();
        assert!(
            matches!(error, DatabaseError::AlreadyFoundError { .. }),
            "{error:?}"
        );
        txn.rollback().await.unwrap();

        let stored = find_by_ids(&pool, &[id]).await.unwrap().pop().unwrap();
        assert_eq!(stored.config, replacement);
        assert_eq!(stored.version, updated.version);

        let mut txn = pool.begin().await.unwrap();
        delete(&mut txn, &stored.id, stored.version).await.unwrap();
        txn.commit().await.unwrap();
        assert!(find_ids(&pool).await.unwrap().is_empty());

        let mut txn = pool.begin().await.unwrap();
        let error = delete(&mut txn, &stored.id, stored.version)
            .await
            .unwrap_err();
        assert!(
            matches!(error, DatabaseError::NotFoundError { .. }),
            "{error:?}"
        );
        txn.rollback().await.unwrap();

        let mut txn = pool.begin().await.unwrap();
        let recreated = create(&mut txn, &stored.id, &original).await.unwrap();
        txn.commit().await.unwrap();
        assert_eq!(created.version.version_nr(), recreated.version.version_nr());
        assert_ne!(created.version, recreated.version);

        let mut txn = pool.begin().await.unwrap();
        let error = update(&mut txn, &stored.id, &replacement, created.version)
            .await
            .unwrap_err();
        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "{error:?}"
        );
        let error = delete(&mut txn, &stored.id, created.version)
            .await
            .unwrap_err();
        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "{error:?}"
        );
        txn.commit().await.unwrap();

        let stored = find_by_ids(&pool, &[recreated.id])
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(stored.config, original);
        assert_eq!(stored.version, recreated.version);
    }
}
