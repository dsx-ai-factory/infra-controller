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

//! Persistence for NIC firmware profiles and compatible site defaults.

use config_version::ConfigVersion;
use model::nic_firmware::{
    NicFirmwareHardware, NicFirmwareProfile, NicFirmwareProfileConfig, NicFirmwareProfileId,
    NicFirmwareSiteDefault,
};
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

/// `find_compatible_ids` filters the catalog using the resolver's literal entry
/// match, preserving the same ordering as `find_ids`.
pub async fn find_compatible_ids(
    db: impl DbReader<'_>,
    hardware: &NicFirmwareHardware,
) -> DatabaseResult<Vec<NicFirmwareProfileId>> {
    let query = "SELECT id, config, version FROM nic_firmware_profiles ORDER BY id";
    let profiles: Vec<NicFirmwareProfile> = sqlx::query_as(query)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(profiles
        .into_iter()
        .filter(|profile| profile.config.entry_for(hardware).is_some())
        .map(|profile| profile.id)
        .collect())
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
/// Missing profiles return `NotFoundError`. Removing a referenced hardware pair
/// returns `FailedPrecondition` before checking for a stale token; otherwise,
/// stale tokens return `ConcurrentModificationError`. Neither changes the profile.
/// Callers must use a `READ COMMITTED` transaction and hold it through commit so
/// reference reads see assignments committed while the profile lock was waiting.
pub async fn update(
    txn: &mut PgConnection,
    id: &NicFirmwareProfileId,
    config: &NicFirmwareProfileConfig,
    expected_version: ConfigVersion,
) -> DatabaseResult<NicFirmwareProfile> {
    // Hold the profile lock before reading references. A concurrent assignment
    // must either see this replacement or commit before we validate its pair.
    let lock = "SELECT id FROM nic_firmware_profiles WHERE id = $1 FOR UPDATE";
    sqlx::query_scalar::<_, NicFirmwareProfileId>(lock)
        .bind(id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(lock, error))?;
    let references =
        "SELECT part_number, psid FROM nic_firmware_site_defaults WHERE profile_id = $1";
    let hardware: Vec<NicFirmwareHardware> = sqlx::query_as(references)
        .bind(id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(references, error))?;
    if hardware
        .iter()
        .any(|hardware| config.entry_for(hardware).is_none())
    {
        return Err(DatabaseError::FailedPrecondition(
            "profile replacement would remove hardware required by a NIC firmware site default"
                .into(),
        ));
    }
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
/// A profile selected by a site default returns `FailedPrecondition`.
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
        .map_err(|error| match error {
            sqlx::Error::Database(error)
                if error.constraint() == Some("nic_firmware_site_defaults_profile_id_fkey") =>
            {
                DatabaseError::FailedPrecondition(
                    "profile is selected by a NIC firmware site default".into(),
                )
            }
            error => DatabaseError::query(query, error),
        })?;
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

/// `find_site_default_ids` lists hardware pairs in ascending part-number/PSID order.
/// `profile_id` limits the list to that exact profile; `None` returns all pairs.
pub async fn find_site_default_ids(
    db: impl DbReader<'_>,
    profile_id: Option<&NicFirmwareProfileId>,
) -> DatabaseResult<Vec<NicFirmwareHardware>> {
    let query = r#"
        SELECT part_number, psid FROM nic_firmware_site_defaults
        WHERE $1::text IS NULL OR profile_id = $1
        ORDER BY part_number, psid
    "#;
    sqlx::query_as(query)
        .bind(profile_id)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `find_site_defaults_by_ids` returns assignments for the exact hardware pairs,
/// once each, in ascending part-number/PSID order. Missing pairs are omitted,
/// and an empty list returns no assignments.
pub async fn find_site_defaults_by_ids(
    db: impl DbReader<'_>,
    hardware: &[NicFirmwareHardware],
) -> DatabaseResult<Vec<NicFirmwareSiteDefault>> {
    let (part_numbers, psids): (Vec<_>, Vec<_>) = hardware
        .iter()
        .map(|hardware| (hardware.part_number.as_str(), hardware.psid.as_str()))
        .unzip();
    let query = r#"
        SELECT part_number, psid, profile_id, version FROM nic_firmware_site_defaults
        WHERE (part_number, psid) IN (
            SELECT * FROM UNNEST($1::text[], $2::text[])
        )
        ORDER BY part_number, psid
    "#;
    sqlx::query_as(query)
        .bind(part_numbers)
        .bind(psids)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `find_site_defaults` loads all assignments for the plan, ordered by part number
/// and PSID.
/// Assignment versions do not pin profile versions.
pub async fn find_site_defaults(
    db: impl DbReader<'_>,
) -> DatabaseResult<Vec<NicFirmwareSiteDefault>> {
    let query = "SELECT part_number, psid, profile_id, version FROM nic_firmware_site_defaults ORDER BY part_number, psid";
    sqlx::query_as(query)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `set_site_default` validates under a shared profile lock, then inserts without
/// a token or replaces only the assignment matching `expected_version`.
/// Profile existence and compatibility are checked before the assignment token.
/// Callers must use a `READ COMMITTED` transaction and hold it through commit to
/// serialize profile edits with the validated assignment.
/// `hardware` must have passed `NicFirmwareHardware::validate`. A missing profile
/// or conditional-update target returns `NotFoundError`; an incompatible profile
/// returns `FailedPrecondition`. Duplicate creates return `AlreadyFoundError`,
/// and stale assignment tokens return `ConcurrentModificationError`.
pub async fn set_site_default(
    txn: &mut PgConnection,
    hardware: &NicFirmwareHardware,
    profile_id: &NicFirmwareProfileId,
    expected_version: Option<ConfigVersion>,
) -> DatabaseResult<NicFirmwareSiteDefault> {
    let query = "SELECT id, config, version FROM nic_firmware_profiles WHERE id = $1 FOR SHARE";
    let profile: NicFirmwareProfile = sqlx::query_as(query)
        .bind(profile_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: KIND,
            id: profile_id.to_string(),
        })?;
    if profile.config.entry_for(hardware).is_none() {
        return Err(DatabaseError::FailedPrecondition(
            "profile does not support the selected part number/PSID".into(),
        ));
    }
    let Some(expected_version) = expected_version else {
        let query = "INSERT INTO nic_firmware_site_defaults (part_number, psid, profile_id, version) VALUES ($1, $2, $3, $4) RETURNING part_number, psid, profile_id, version";
        return sqlx::query_as(query)
            .bind(&hardware.part_number)
            .bind(&hardware.psid)
            .bind(profile_id)
            .bind(ConfigVersion::initial())
            .fetch_one(txn)
            .await
            .map_err(|error| match error {
                sqlx::Error::Database(error) if error.is_unique_violation() => {
                    DatabaseError::AlreadyFoundError {
                        kind: "NIC firmware site default",
                        id: format!("{}/{}", hardware.part_number, hardware.psid),
                    }
                }
                error => DatabaseError::query(query, error),
            });
    };
    let query = "UPDATE nic_firmware_site_defaults SET profile_id = $3, version = $4 WHERE part_number = $1 AND psid = $2 AND version = $5 RETURNING part_number, psid, profile_id, version";
    let result = sqlx::query_as(query)
        .bind(&hardware.part_number)
        .bind(&hardware.psid)
        .bind(profile_id)
        .bind(expected_version.increment())
        .bind(expected_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    match result {
        Some(binding) => Ok(binding),
        None => Err(unmatched_site_default_version(txn, hardware, expected_version).await?),
    }
}

/// `clear_site_default` removes only the assignment matching the full version.
/// It remains usable when a selected profile no longer supports the hardware.
/// `hardware` must have passed `NicFirmwareHardware::validate`. Missing assignments
/// return `NotFoundError`; stale tokens return `ConcurrentModificationError`.
pub async fn clear_site_default(
    txn: &mut PgConnection,
    hardware: &NicFirmwareHardware,
    expected_version: ConfigVersion,
) -> DatabaseResult<()> {
    let query = "DELETE FROM nic_firmware_site_defaults WHERE part_number = $1 AND psid = $2 AND version = $3";
    let deleted = sqlx::query(query)
        .bind(&hardware.part_number)
        .bind(&hardware.psid)
        .bind(expected_version)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if deleted.rows_affected() == 0 {
        return Err(unmatched_site_default_version(txn, hardware, expected_version).await?);
    }
    Ok(())
}

async fn unmatched_site_default_version(
    txn: &mut PgConnection,
    hardware: &NicFirmwareHardware,
    expected_version: ConfigVersion,
) -> DatabaseResult<DatabaseError> {
    let query = "SELECT EXISTS(SELECT 1 FROM nic_firmware_site_defaults WHERE part_number = $1 AND psid = $2)";
    let exists: bool = sqlx::query_scalar(query)
        .bind(&hardware.part_number)
        .bind(&hardware.psid)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let id = format!("{}/{}", hardware.part_number, hardware.psid);
    Ok(if exists {
        DatabaseError::ConcurrentModificationError(
            "NIC firmware site default",
            expected_version.to_string(),
        )
    } else {
        DatabaseError::NotFoundError {
            kind: "NIC firmware site default",
            id,
        }
    })
}

#[cfg(test)]
mod tests {
    use carbide_libmlx_model::firmware::FirmwareSpec;
    use model::nic_firmware::{NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareProfileEntry};

    use super::*;
    use crate::test_support::postgres::wait_for_blocked_query;

    fn profile_config() -> NicFirmwareProfileConfig {
        NicFirmwareProfileConfig {
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
        }
    }

    #[crate::sqlx_test]
    async fn concurrent_profile_update_and_stale_delete_preserve_the_winner(pool: sqlx::PgPool) {
        let id: NicFirmwareProfileId = "profile".parse().unwrap();
        let original = profile_config();
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
            wait_for_blocked_query(&pool, blocker_pid, "FOR UPDATE").await;
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

    #[crate::sqlx_test]
    async fn concurrent_site_default_replacements_preserve_the_winner(pool: sqlx::PgPool) {
        let profile_a: NicFirmwareProfileId = "profile_a".parse().unwrap();
        let profile_b: NicFirmwareProfileId = "profile_b".parse().unwrap();
        let config = profile_config();
        let hardware = NicFirmwareHardware {
            part_number: config.entries[0].firmware.part_number.clone(),
            psid: config.entries[0].firmware.psid.clone(),
        };
        let mut txn = pool.begin().await.unwrap();
        for id in [&profile_a, &profile_b] {
            create(&mut txn, id, &config).await.unwrap();
        }
        let created = set_site_default(&mut txn, &hardware, &profile_a, None)
            .await
            .unwrap();
        txn.commit().await.unwrap();

        let mut winner = pool.begin().await.unwrap();
        let updated = set_site_default(&mut winner, &hardware, &profile_b, Some(created.version))
            .await
            .unwrap();
        assert_eq!(
            updated.version.version_nr(),
            created.version.version_nr() + 1
        );
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *winner)
            .await
            .unwrap();
        let competing_update = async {
            let mut txn = pool.begin().await.unwrap();
            let error = set_site_default(&mut txn, &hardware, &profile_a, Some(created.version))
                .await
                .unwrap_err();
            txn.rollback().await.unwrap();
            error
        };
        let release_winner = async {
            wait_for_blocked_query(&pool, blocker_pid, "UPDATE nic_firmware_site_defaults").await;
            winner.commit().await.unwrap();
        };
        let (error, ()) = tokio::join!(competing_update, release_winner);
        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "{error:?}"
        );

        let bindings = find_site_defaults(&pool).await.unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].hardware, hardware);
        assert_eq!(bindings[0].profile_id, profile_b);
        assert_eq!(bindings[0].version, updated.version);
    }

    #[crate::sqlx_test]
    async fn site_default_serializes_profile_edits_and_deletion(pool: sqlx::PgPool) {
        for (scenario, delete_profile, bind_first) in [
            ("bind_before_edit", false, true),
            ("edit_before_bind", false, false),
            ("bind_before_delete", true, true),
            ("delete_before_bind", true, false),
        ] {
            let id: NicFirmwareProfileId = scenario.parse().unwrap();
            let original = profile_config();
            let hardware = NicFirmwareHardware {
                part_number: original.entries[0].firmware.part_number.clone(),
                psid: scenario.into(),
            };
            let mut original = original;
            original.entries[0].firmware.psid = hardware.psid.clone();
            let mut replacement = original.clone();
            if bind_first && !delete_profile {
                let mut removed_entry = original.entries[0].clone();
                removed_entry.firmware.psid = format!("{scenario}_removed");
                original.entries.push(removed_entry);
            } else {
                replacement.entries[0].firmware.psid = "unsupported".into();
            }
            let mut txn = pool.begin().await.unwrap();
            let profile = create(&mut txn, &id, &original).await.unwrap();
            txn.commit().await.unwrap();
            let mut winner = pool.begin().await.unwrap();
            let mut expected_bindings = Vec::new();
            let expected_profile = if bind_first {
                for entry in &original.entries {
                    let hardware = NicFirmwareHardware {
                        part_number: entry.firmware.part_number.clone(),
                        psid: entry.firmware.psid.clone(),
                    };
                    expected_bindings.push(
                        set_site_default(&mut winner, &hardware, &id, None)
                            .await
                            .unwrap(),
                    );
                }
                Some(profile.clone())
            } else if delete_profile {
                delete(&mut winner, &id, profile.version).await.unwrap();
                None
            } else {
                Some(
                    update(&mut winner, &id, &replacement, profile.version)
                        .await
                        .unwrap(),
                )
            };
            let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *winner)
                .await
                .unwrap();
            let loser = async {
                let mut txn = pool.begin().await.unwrap();
                let result = if !bind_first {
                    set_site_default(&mut txn, &hardware, &id, None)
                        .await
                        .map(|_| ())
                } else if delete_profile {
                    delete(&mut txn, &id, profile.version).await
                } else {
                    update(&mut txn, &id, &replacement, profile.version)
                        .await
                        .map(|_| ())
                };
                let error = result.unwrap_err();
                txn.rollback().await.unwrap();
                error
            };
            let release = async {
                let query = if !bind_first {
                    "FOR SHARE"
                } else if delete_profile {
                    "DELETE FROM nic_firmware_profiles"
                } else {
                    "FOR UPDATE"
                };
                wait_for_blocked_query(&pool, blocker, query).await;
                winner.commit().await.unwrap();
            };
            let (error, ()) = tokio::join!(loser, release);
            if delete_profile && !bind_first {
                assert!(
                    matches!(error, DatabaseError::NotFoundError { .. }),
                    "{scenario}: {error}"
                );
            } else {
                assert!(
                    matches!(error, DatabaseError::FailedPrecondition(_)),
                    "{scenario}: {error}"
                );
            }
            let stored = find_by_ids(&pool, std::slice::from_ref(&id))
                .await
                .unwrap()
                .pop();
            assert_eq!(
                stored
                    .as_ref()
                    .map(|profile| (&profile.config, profile.version)),
                expected_profile
                    .as_ref()
                    .map(|profile| (&profile.config, profile.version)),
                "{scenario}"
            );
            let bindings: Vec<_> = find_site_defaults(&pool)
                .await
                .unwrap()
                .into_iter()
                .filter(|binding| binding.profile_id == id)
                .collect();
            assert_eq!(bindings.len(), expected_bindings.len(), "{scenario}");
            for (binding, expected) in bindings.iter().zip(&expected_bindings) {
                assert_eq!(
                    (&binding.hardware, &binding.profile_id, binding.version),
                    (&expected.hardware, &expected.profile_id, expected.version),
                    "{scenario}"
                );
            }
            if bind_first && !delete_profile {
                // Removing the second assignment makes that pair removable from
                // the profile, while the first assignment stays valid.
                let removed = &expected_bindings[1];
                let mut txn = pool.begin().await.unwrap();
                clear_site_default(&mut txn, &removed.hardware, removed.version)
                    .await
                    .unwrap();
                let updated = update(&mut txn, &id, &replacement, profile.version)
                    .await
                    .unwrap();
                txn.commit().await.unwrap();

                // Reference validation takes precedence when the replacement
                // also uses the version from before the accepted edit.
                let mut incompatible = replacement.clone();
                incompatible.entries[0].firmware.psid = "unsupported".into();
                let mut txn = pool.begin().await.unwrap();
                let error = update(&mut txn, &id, &incompatible, profile.version)
                    .await
                    .unwrap_err();
                assert!(
                    matches!(error, DatabaseError::FailedPrecondition(_)),
                    "{error}"
                );
                txn.rollback().await.unwrap();

                let stored = find_by_ids(&pool, std::slice::from_ref(&id))
                    .await
                    .unwrap()
                    .pop()
                    .unwrap();
                assert_eq!(stored.config, replacement);
                assert_eq!(stored.version, updated.version);
                let bindings = find_site_defaults_by_ids(&pool, std::slice::from_ref(&hardware))
                    .await
                    .unwrap();
                assert_eq!(bindings.len(), 1);
                assert_eq!(bindings[0].profile_id, id);
                assert_eq!(bindings[0].version, expected_bindings[0].version);
            }
        }
    }

    #[crate::sqlx_test]
    async fn site_default_reads_filter_and_order_exact_hardware_pairs(pool: sqlx::PgPool) {
        let hardware = [
            NicFirmwareHardware {
                part_number: "PN_A".into(),
                psid: "PSID_A".into(),
            },
            NicFirmwareHardware {
                part_number: "PN_A".into(),
                psid: "PSID_B".into(),
            },
            NicFirmwareHardware {
                part_number: "PN_B".into(),
                psid: "PSID_A".into(),
            },
        ];
        let mut config = profile_config();
        let entry = config.entries[0].clone();
        config.entries = hardware
            .iter()
            .map(|hardware| {
                let mut entry = entry.clone();
                entry.firmware.part_number = hardware.part_number.clone();
                entry.firmware.psid = hardware.psid.clone();
                entry
            })
            .collect();
        let profile_a: NicFirmwareProfileId = "profile_a".parse().unwrap();
        let profile_b: NicFirmwareProfileId = "profile_a_extra".parse().unwrap();
        let missing_profile: NicFirmwareProfileId = "missing".parse().unwrap();
        let mut txn = pool.begin().await.unwrap();
        for id in [&profile_a, &profile_b] {
            create(&mut txn, id, &config).await.unwrap();
        }
        let mut created = Vec::new();
        for (hardware, id) in [
            (&hardware[2], &profile_b),
            (&hardware[1], &profile_a),
            (&hardware[0], &profile_a),
        ] {
            created.push(
                set_site_default(&mut txn, hardware, id, None)
                    .await
                    .unwrap(),
            );
        }
        txn.commit().await.unwrap();

        for (scenario, profile_id, expected) in [
            ("all profiles", None, hardware.as_slice()),
            ("exact profile", Some(&profile_a), &hardware[..2]),
            ("unknown profile", Some(&missing_profile), &[]),
        ] {
            assert_eq!(
                find_site_default_ids(&pool, profile_id).await.unwrap(),
                expected,
                "{scenario}"
            );
        }
        let all = find_site_defaults(&pool).await.unwrap();
        assert_eq!(
            all.iter()
                .map(|binding| &binding.hardware)
                .collect::<Vec<_>>(),
            hardware.iter().collect::<Vec<_>>()
        );

        for (scenario, requested, expected) in [
            (
                "exact pairs with duplicates and an absent pair",
                vec![
                    hardware[2].clone(),
                    hardware[1].clone(),
                    hardware[2].clone(),
                    NicFirmwareHardware {
                        part_number: "PN_B".into(),
                        psid: "PSID_B".into(),
                    },
                ],
                &hardware[1..],
            ),
            ("empty selection", vec![], &[]),
        ] {
            let bindings = find_site_defaults_by_ids(&pool, &requested).await.unwrap();
            assert_eq!(
                bindings
                    .iter()
                    .map(|binding| &binding.hardware)
                    .collect::<Vec<_>>(),
                expected.iter().collect::<Vec<_>>(),
                "{scenario}"
            );
            for binding in bindings {
                let original = created
                    .iter()
                    .find(|original| original.hardware == binding.hardware)
                    .unwrap();
                assert_eq!(binding.profile_id, original.profile_id, "{scenario}");
                assert_eq!(binding.version, original.version, "{scenario}");
            }
        }
    }

    #[crate::sqlx_test]
    async fn site_default_migration_preserves_profiles_and_enforces_references(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        // Recreate only the predecessor tables in a private schema, with a real
        // catalog row present before applying the migration being tested.
        sqlx::raw_sql("CREATE SCHEMA nic_predecessor; SET LOCAL search_path TO nic_predecessor")
            .execute(&mut *txn)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/20261001211240_nic_firmware_profiles.sql"
        ))
        .execute(&mut *txn)
        .await
        .unwrap();
        let id = "existing".parse().unwrap();
        let profile = create(&mut txn, &id, &profile_config()).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/20261009195417_nic_firmware_site_defaults.sql"
        ))
        .execute(&mut *txn)
        .await
        .unwrap();
        let hardware = NicFirmwareHardware {
            part_number: profile.config.entries[0].firmware.part_number.clone(),
            psid: profile.config.entries[0].firmware.psid.clone(),
        };
        let binding = set_site_default(&mut txn, &hardware, &id, None)
            .await
            .unwrap();
        let rows = find_site_defaults(&mut *txn).await.unwrap();
        assert_eq!(rows[0].version, binding.version);
        let stored = find_by_ids(&mut *txn, std::slice::from_ref(&id))
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(stored.config, profile.config);
        assert_eq!(stored.version, profile.version);
        let error = sqlx::query("DELETE FROM nic_firmware_profiles WHERE id = $1")
            .bind(&id)
            .execute(&mut *txn)
            .await
            .unwrap_err();
        assert!(
            error
                .as_database_error()
                .unwrap()
                .is_foreign_key_violation()
        );
        txn.rollback().await.unwrap();
    }
}
