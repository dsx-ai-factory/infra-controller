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

//! Persistence for NIC firmware profiles, site defaults, and machine overrides.

use carbide_uuid::machine::HostMachineId;
use config_version::ConfigVersion;
use model::machine::HostMachine;
use model::machine::status::managed_dpu_machine_ids;
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
/// Machine references preserve usable observed pairs supported by the old config
/// without a managed-DPU match. Newly observed unsupported pairs do not block
/// unrelated edits; `GetDesiredNicFirmware` still reports their per-device errors.
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
    let lock = "SELECT id, config, version FROM nic_firmware_profiles WHERE id = $1 FOR UPDATE";
    let old_profile = sqlx::query_as::<_, NicFirmwareProfile>(lock)
        .bind(id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(lock, error))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: KIND,
            id: id.to_string(),
        })?;
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
    for machine in crate::machine::find_by_nic_firmware_profile(&mut *txn, id).await? {
        validate_machine_profile(&machine, config, Some(&old_profile.config))?;
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
/// A profile selected by a site default or machine returns `FailedPrecondition`.
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
            sqlx::Error::Database(error)
                if error.constraint() == Some("machines_nic_firmware_profile_id_fkey") =>
            {
                DatabaseError::FailedPrecondition(
                    "profile is selected by a machine NIC firmware override".into(),
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

/// `set_machine_override` selects a live profile, or clears the selection with
/// `None`, and advances the machine's shared edit version even when unchanged.
/// A selected profile must exist and support every usable observed hardware pair
/// without a managed-DPU match. Missing or incomplete inventory is allowed;
/// clearing does not validate the profile or inventory.
/// Missing profiles or hosts return `NotFoundError`, then stale machine tokens
/// return `ConcurrentModificationError`, then incompatible profiles return
/// `FailedPrecondition`. Callers must use `READ COMMITTED` and hold the transaction
/// through commit so profile edits and assignments serialize.
pub async fn set_machine_override(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    profile_id: Option<&NicFirmwareProfileId>,
    expected_version: ConfigVersion,
) -> DatabaseResult<()> {
    // Profile edits lock this row before inspecting references. Take the shared
    // lock first so neither operation can commit using the other's old config.
    let profile = if let Some(profile_id) = profile_id {
        let query = "SELECT id, config, version FROM nic_firmware_profiles WHERE id = $1 FOR SHARE";
        Some(
            sqlx::query_as::<_, NicFirmwareProfile>(query)
                .bind(profile_id)
                .fetch_optional(&mut *txn)
                .await
                .map_err(|error| DatabaseError::query(query, error))?
                .ok_or_else(|| DatabaseError::NotFoundError {
                    kind: KIND,
                    id: profile_id.to_string(),
                })?,
        )
    } else {
        None
    };
    let query = "SELECT version FROM machines WHERE id = $1 FOR UPDATE";
    let version: ConfigVersion = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;
    if version != expected_version {
        return Err(DatabaseError::ConcurrentModificationError(
            "machine",
            expected_version.to_string(),
        ));
    }
    if let Some(profile) = profile {
        // Read after acquiring the row lock: a Scout report may have committed
        // while we were waiting. Later inventory changes are checked by desired
        // firmware reads and do not make this a hardware qualification record.
        let machine = crate::machine::find_one(&mut *txn, machine_id, Default::default())
            .await?
            .ok_or_else(|| DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            })?;
        validate_machine_profile(&machine, &profile.config, None)?;
    }
    let query = "UPDATE machines SET nic_firmware_profile_id = $2, version = $3 WHERE id = $1";
    sqlx::query(query)
        .bind(machine_id)
        .bind(profile_id)
        .bind(expected_version.increment())
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

// `GetDesiredNicFirmware` reports incomplete or invalid observations as
// per-device errors. Admission checks only usable pairs in this snapshot.
// With `previous_config`, edits preserve existing support without making a new
// or replaced NIC's missing entry block unrelated profile changes.
fn validate_machine_profile(
    machine: &HostMachine,
    config: &NicFirmwareProfileConfig,
    previous_config: Option<&NicFirmwareProfileConfig>,
) -> DatabaseResult<()> {
    let Some(observation) = &machine.status.mlx_device_observation else {
        return Ok(());
    };
    for device in &observation.devices {
        if !managed_dpu_machine_ids(device.base_mac, &machine.status.interfaces).is_empty() {
            continue;
        }
        let (Some(part_number), Some(psid)) = (&device.part_number, &device.psid) else {
            continue;
        };
        let Ok(hardware) = (NicFirmwareHardware {
            part_number: part_number.clone(),
            psid: psid.clone(),
        })
        .validate() else {
            continue;
        };
        if previous_config.is_some_and(|previous| previous.entry_for(&hardware).is_none()) {
            continue;
        }
        if config.entry_for(&hardware).is_none() {
            return Err(DatabaseError::FailedPrecondition(format!(
                "profile does not support machine {} part number {} / PSID {}",
                machine.id, hardware.part_number, hardware.psid
            )));
        }
    }
    Ok(())
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

/// `find_site_defaults` loads all assignments for desired firmware selection,
/// ordered by part number and PSID.
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
    use carbide_libmlx_model::device::info::MlxDeviceInfo;
    use carbide_libmlx_model::firmware::FirmwareSpec;
    use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
    use model::machine::status::MlxDeviceObservation;
    use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
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

    // Use the production insert and snapshot read so the fixture also works
    // before the nullable override column exists in the migration test.
    async fn create_host(txn: &mut PgConnection, index: u8) -> HostMachine {
        let id = MachineId::new(MachineIdSource::Tpm, [index; 32], MachineType::Host);
        crate::machine::create(
            txn,
            None,
            &id,
            ManagedHostState::Ready,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await
        .unwrap()
        .try_into_subtype()
        .unwrap()
    }

    // Only compatibility pairs matter here; optional device facts stay absent.
    fn observation(config: &NicFirmwareProfileConfig) -> MlxDeviceObservation {
        MlxDeviceObservation {
            observed_at: chrono::Utc::now(),
            devices: config
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    serde_json::from_value::<MlxDeviceInfo>(serde_json::json!({
                        "pci_name": format!("0000:{index:02x}:00.0"),
                        "device_type": "ConnectX7",
                        "part_number": entry.firmware.part_number,
                        "psid": entry.firmware.psid,
                    }))
                    .unwrap()
                })
                .collect(),
        }
    }

    // Whole-row comparisons protect unrelated settings, including fields added
    // later, while every accepted set or clear consumes the shared token.
    #[crate::sqlx_test]
    async fn machine_override_changes_only_selection_and_shared_edit_version(pool: sqlx::PgPool) {
        let id: NicFirmwareProfileId = "profile".parse().unwrap();
        let mut txn = pool.begin().await.unwrap();
        create(&mut txn, &id, &profile_config()).await.unwrap();
        let host = create_host(&mut txn, 1).await;
        sqlx::query(
            "UPDATE machines SET firmware_autoupdate = false, description = 'preserve me', labels = '{\"team\":\"network\"}' WHERE id = $1",
        )
        .bind(host.id)
        .execute(&mut *txn)
        .await
        .unwrap();
        txn.commit().await.unwrap();

        // The existing machine trigger updates `updated` on every write.
        let unchanged = "SELECT to_jsonb(m) - 'version' - 'nic_firmware_profile_id' - 'updated' FROM machines m WHERE id = $1";
        let before: serde_json::Value = sqlx::query_scalar(unchanged)
            .bind(host.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut version = host.version;
        for (scenario, selection) in [
            ("set without inventory", Some(&id)),
            ("set unchanged profile", Some(&id)),
            ("clear", None),
            ("clear absent override", None),
        ] {
            let mut txn = pool.begin().await.unwrap();
            set_machine_override(&mut txn, &host.id, selection, version)
                .await
                .unwrap();
            txn.commit().await.unwrap();
            let stored = crate::machine::find_one(&pool, &host.id, Default::default())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                stored.config.nic_firmware_profile_id.as_ref(),
                selection,
                "{scenario}"
            );
            assert_eq!(
                stored.version.version_nr(),
                version.version_nr() + 1,
                "{scenario}"
            );
            assert_ne!(stored.version, version, "{scenario}");
            let after: serde_json::Value = sqlx::query_scalar(unchanged)
                .bind(host.id)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(after, before, "{scenario}");
            version = stored.version;
        }

        // Metadata and the override use one token. Exercise both directions so
        // neither writer can reuse a snapshot consumed by the other.
        let metadata = model::metadata::Metadata {
            name: "operator edit".into(),
            description: "new metadata".into(),
            labels: Default::default(),
        };
        let mut txn = pool.begin().await.unwrap();
        let error =
            crate::machine::update_metadata(&mut txn, &host.id, host.version, metadata.clone())
                .await
                .unwrap_err();
        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "{error:?}"
        );
        crate::machine::update_metadata(&mut txn, &host.id, version, metadata.clone())
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let mut txn = pool.begin().await.unwrap();
        let error = set_machine_override(&mut txn, &host.id, Some(&id), version)
            .await
            .unwrap_err();
        assert!(
            matches!(error, DatabaseError::ConcurrentModificationError(..)),
            "{error:?}"
        );
        txn.rollback().await.unwrap();
        let stored = crate::machine::find_one(&pool, &host.id, Default::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.metadata, metadata);
        assert_eq!(stored.config.nic_firmware_profile_id, None);
        assert_eq!(stored.version.version_nr(), version.version_nr() + 1);

        let missing = MachineId::new(MachineIdSource::Tpm, [2; 32], MachineType::Host)
            .try_into()
            .unwrap();
        let mut txn = pool.begin().await.unwrap();
        let error = set_machine_override(&mut txn, &missing, None, version)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                DatabaseError::NotFoundError {
                    kind: "machine",
                    ..
                }
            ),
            "{error:?}"
        );
        txn.rollback().await.unwrap();
    }

    // Wait for a real row-lock conflict, then prove each stale operation leaves
    // the committed winner's selection and complete version intact.
    #[crate::sqlx_test]
    async fn concurrent_machine_set_and_clear_preserve_the_winner(pool: sqlx::PgPool) {
        let id: NicFirmwareProfileId = "profile".parse().unwrap();
        let mut txn = pool.begin().await.unwrap();
        create(&mut txn, &id, &profile_config()).await.unwrap();
        txn.commit().await.unwrap();

        for (index, scenario, winning_selection, stale_selection) in [
            (1, "set beats stale clear", Some(&id), None),
            (2, "clear beats stale set", None, Some(&id)),
        ] {
            let mut txn = pool.begin().await.unwrap();
            let host = create_host(&mut txn, index).await;
            set_machine_override(&mut txn, &host.id, Some(&id), host.version)
                .await
                .unwrap();
            let host = crate::machine::find_one(&mut *txn, &host.id, Default::default())
                .await
                .unwrap()
                .unwrap();
            txn.commit().await.unwrap();

            let mut winner = pool.begin().await.unwrap();
            set_machine_override(&mut winner, &host.id, winning_selection, host.version)
                .await
                .unwrap();
            let expected = crate::machine::find_one(&mut *winner, &host.id, Default::default())
                .await
                .unwrap()
                .unwrap();
            let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *winner)
                .await
                .unwrap();
            let loser = async {
                let mut txn = pool.begin().await.unwrap();
                let error = set_machine_override(&mut txn, &host.id, stale_selection, host.version)
                    .await
                    .unwrap_err();
                txn.rollback().await.unwrap();
                error
            };
            let release = async {
                wait_for_blocked_query(&pool, blocker, "SELECT version FROM machines").await;
                winner.commit().await.unwrap();
            };
            let (error, ()) = tokio::join!(loser, release);
            assert!(
                matches!(error, DatabaseError::ConcurrentModificationError(..)),
                "{scenario}: {error:?}"
            );
            let stored = crate::machine::find_one(&pool, &host.id, Default::default())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                stored.config.nic_firmware_profile_id.as_ref(),
                winning_selection,
                "{scenario}"
            );
            assert_eq!(stored.version, expected.version, "{scenario}");
        }
    }

    #[crate::sqlx_test]
    async fn machine_assignment_checks_observation_committed_while_waiting(pool: sqlx::PgPool) {
        let selected_id: NicFirmwareProfileId = "selected".parse().unwrap();
        let replacement_id: NicFirmwareProfileId = "replacement".parse().unwrap();
        let config = profile_config();
        let initial_observation = observation(&config);
        let mut txn = pool.begin().await.unwrap();
        for id in [&selected_id, &replacement_id] {
            create(&mut txn, id, &config).await.unwrap();
        }
        let host = create_host(&mut txn, 1).await;
        assert_eq!(
            crate::machine::update_mlx_device_observation(
                &mut txn,
                &host.id,
                &initial_observation,
            )
            .await
            .unwrap(),
            crate::ConditionalWrite::Applied(())
        );
        set_machine_override(&mut txn, &host.id, Some(&selected_id), host.version)
            .await
            .unwrap();
        let host = crate::machine::find_one(&mut *txn, &host.id, Default::default())
            .await
            .unwrap()
            .unwrap();
        txn.commit().await.unwrap();

        // Scout does not consume the shared edit token. The setter has to read
        // inventory after acquiring the lock to see this incompatible report.
        let mut changed_config = config;
        changed_config.entries[0].firmware.psid = "replacement_nic".into();
        let mut changed_observation = observation(&changed_config);
        changed_observation.observed_at =
            initial_observation.observed_at + chrono::Duration::seconds(1);
        let mut scout = pool.begin().await.unwrap();
        assert_eq!(
            crate::machine::update_mlx_device_observation(
                &mut scout,
                &host.id,
                &changed_observation,
            )
            .await
            .unwrap(),
            crate::ConditionalWrite::Applied(())
        );
        let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *scout)
            .await
            .unwrap();
        let setter = async {
            let mut txn = pool.begin().await.unwrap();
            let error =
                set_machine_override(&mut txn, &host.id, Some(&replacement_id), host.version)
                    .await
                    .unwrap_err();
            txn.commit().await.unwrap();
            error
        };
        let release = async {
            wait_for_blocked_query(&pool, blocker, "SELECT version FROM machines").await;
            scout.commit().await.unwrap();
        };
        let (error, ()) = tokio::join!(setter, release);
        assert!(
            matches!(error, DatabaseError::FailedPrecondition(_)),
            "{error:?}"
        );
        // With both failures present, the machine token is checked before
        // inventory compatibility. Neither rejected request may change intent.
        let mut txn = pool.begin().await.unwrap();
        let error = set_machine_override(
            &mut txn,
            &host.id,
            Some(&replacement_id),
            host.version.increment(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            DatabaseError::ConcurrentModificationError(..)
        ));
        txn.commit().await.unwrap();
        let stored = crate::machine::find_one(&pool, &host.id, Default::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.config.nic_firmware_profile_id, Some(selected_id));
        assert_eq!(stored.version, host.version);
        assert_eq!(
            stored.status.mlx_device_observation,
            Some(changed_observation)
        );
    }

    #[crate::sqlx_test]
    async fn profile_edits_preserve_supported_pairs_after_inventory_drift(pool: sqlx::PgPool) {
        let id: NicFirmwareProfileId = "profile".parse().unwrap();
        let first_config = profile_config();
        let mut second_config = profile_config();
        second_config.entries[0].firmware.psid = "second_pair".into();
        let config = NicFirmwareProfileConfig {
            entries: vec![
                first_config.entries[0].clone(),
                second_config.entries[0].clone(),
            ],
        };
        let first_observation = observation(&first_config);
        let second_observation = observation(&second_config);
        let mut txn = pool.begin().await.unwrap();
        let profile = create(&mut txn, &id, &config).await.unwrap();
        let mut host_ids = Vec::new();
        // The hosts need different pairs. Reject removing either pair so a
        // lookup that returns only one host cannot pass based on its order.
        for (index, observation) in [(1, &first_observation), (2, &second_observation)] {
            let host = create_host(&mut txn, index).await;
            assert_eq!(
                crate::machine::update_mlx_device_observation(&mut txn, &host.id, observation)
                    .await
                    .unwrap(),
                crate::ConditionalWrite::Applied(())
            );
            set_machine_override(&mut txn, &host.id, Some(&id), host.version)
                .await
                .unwrap();
            host_ids.push(host.id);
        }
        txn.commit().await.unwrap();

        // A newly reported NIC does not invalidate the profile's support for
        // the other NICs. Unrelated firmware edits must remain available.
        let mut changed_config = first_config;
        let mut added = changed_config.entries[0].clone();
        added.firmware.psid = "new_nic".into();
        changed_config.entries.push(added);
        let mut changed_observation = observation(&changed_config);
        changed_observation.observed_at =
            first_observation.observed_at + chrono::Duration::seconds(1);
        let mut txn = pool.begin().await.unwrap();
        assert_eq!(
            crate::machine::update_mlx_device_observation(
                &mut txn,
                &host_ids[0],
                &changed_observation,
            )
            .await
            .unwrap(),
            crate::ConditionalWrite::Applied(())
        );
        txn.commit().await.unwrap();

        let mut expected_hosts = Vec::new();
        for host_id in &host_ids {
            let host = crate::machine::find_one(&pool, host_id, Default::default())
                .await
                .unwrap()
                .unwrap();
            expected_hosts.push((host.id, host.config.nic_firmware_profile_id, host.version));
        }

        let mut replacement = config;
        replacement.entries[0].firmware.version = "28.44.1000".into();
        let mut txn = pool.begin().await.unwrap();
        let updated = update(&mut txn, &id, &replacement, profile.version)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let stored = find_by_ids(&pool, std::slice::from_ref(&id))
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(stored.config, replacement);
        assert_eq!(stored.version, updated.version);
        assert_eq!(
            stored.version.version_nr(),
            profile.version.version_nr() + 1
        );
        for (host_id, selection, version) in &expected_hosts {
            let stored = crate::machine::find_one(&pool, host_id, Default::default())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&stored.config.nic_firmware_profile_id, selection);
            assert_eq!(&stored.version, version);
        }

        // Compatibility takes precedence even when the caller also has a stale
        // profile token. Both failures must leave the profile and hosts intact.
        for (scenario, removed_entry, expected_version) in [
            ("first pair with current token", 0, updated.version),
            ("second pair with stale token", 1, profile.version),
        ] {
            let mut incompatible = replacement.clone();
            incompatible.entries.remove(removed_entry);
            let mut txn = pool.begin().await.unwrap();
            let error = update(&mut txn, &id, &incompatible, expected_version)
                .await
                .unwrap_err();
            assert!(
                matches!(error, DatabaseError::FailedPrecondition(_)),
                "{scenario}: {error:?}"
            );
            txn.commit().await.unwrap();
            let stored = find_by_ids(&pool, std::slice::from_ref(&id))
                .await
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(stored.config, updated.config, "{scenario}");
            assert_eq!(stored.version, updated.version, "{scenario}");
            for (host_id, selection, version) in &expected_hosts {
                let stored = crate::machine::find_one(&pool, host_id, Default::default())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    &stored.config.nic_firmware_profile_id, selection,
                    "{scenario}: {host_id}"
                );
                assert_eq!(&stored.version, version, "{scenario}: {host_id}");
            }
        }
    }

    // Both lock orders must reject the incompatible second operation. The
    // removable second pair catches validation that inspects only one device.
    #[crate::sqlx_test]
    async fn machine_assignment_serializes_profile_edits(pool: sqlx::PgPool) {
        for (index, scenario, assign_first) in [
            (1, "assignment before edit", true),
            (2, "edit before assignment", false),
        ] {
            let id: NicFirmwareProfileId = format!("profile_{index}").parse().unwrap();
            let replacement = profile_config();
            let mut original = replacement.clone();
            let mut second = original.entries[0].clone();
            second.firmware.psid = "second_pair".into();
            original.entries.push(second);
            let mut txn = pool.begin().await.unwrap();
            let profile = create(&mut txn, &id, &original).await.unwrap();
            let host = create_host(&mut txn, index).await;
            assert_eq!(
                crate::machine::update_mlx_device_observation(
                    &mut txn,
                    &host.id,
                    &observation(&original),
                )
                .await
                .unwrap(),
                crate::ConditionalWrite::Applied(())
            );
            txn.commit().await.unwrap();

            let mut winner = pool.begin().await.unwrap();
            let expected_profile = if assign_first {
                set_machine_override(&mut winner, &host.id, Some(&id), host.version)
                    .await
                    .unwrap();
                profile.clone()
            } else {
                update(&mut winner, &id, &replacement, profile.version)
                    .await
                    .unwrap()
            };
            let expected_host =
                crate::machine::find_one(&mut *winner, &host.id, Default::default())
                    .await
                    .unwrap()
                    .unwrap();
            let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *winner)
                .await
                .unwrap();
            let loser = async {
                let mut txn = pool.begin().await.unwrap();
                let error = if assign_first {
                    update(&mut txn, &id, &replacement, profile.version)
                        .await
                        .unwrap_err()
                } else {
                    set_machine_override(&mut txn, &host.id, Some(&id), host.version)
                        .await
                        .unwrap_err()
                };
                txn.rollback().await.unwrap();
                error
            };
            let release = async {
                let query = if assign_first {
                    "FOR UPDATE"
                } else {
                    "FOR SHARE"
                };
                wait_for_blocked_query(&pool, blocker, query).await;
                winner.commit().await.unwrap();
            };
            let (error, ()) = tokio::join!(loser, release);
            assert!(
                matches!(error, DatabaseError::FailedPrecondition(_)),
                "{scenario}: {error:?}"
            );
            let stored = find_by_ids(&pool, std::slice::from_ref(&id))
                .await
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(stored.config, expected_profile.config, "{scenario}");
            assert_eq!(stored.version, expected_profile.version, "{scenario}");
            let stored_host = crate::machine::find_one(&pool, &host.id, Default::default())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                stored_host.config.nic_firmware_profile_id,
                expected_host.config.nic_firmware_profile_id,
                "{scenario}"
            );
            assert_eq!(stored_host.version, expected_host.version, "{scenario}");

            if assign_first {
                let mut txn = pool.begin().await.unwrap();
                let error = delete(&mut txn, &id, stored.version).await.unwrap_err();
                assert!(
                    matches!(error, DatabaseError::FailedPrecondition(_)),
                    "{error:?}"
                );
                txn.rollback().await.unwrap();

                // Inventory can change after admission. Clearing must remain
                // available even when the profile no longer covers that report.
                let mut changed = original;
                changed.entries[0].firmware.psid = "new_inventory".into();
                let mut txn = pool.begin().await.unwrap();
                assert_eq!(
                    crate::machine::update_mlx_device_observation(
                        &mut txn,
                        &host.id,
                        &observation(&changed),
                    )
                    .await
                    .unwrap(),
                    crate::ConditionalWrite::Applied(())
                );
                set_machine_override(&mut txn, &host.id, None, stored_host.version)
                    .await
                    .unwrap();
                delete(&mut txn, &id, stored.version).await.unwrap();
                txn.commit().await.unwrap();
                assert!(
                    find_by_ids(&pool, std::slice::from_ref(&id))
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }

    // A populated upgrade must preserve existing hosts, then enforce the same
    // profile reference used by assignment and deletion in production.
    #[crate::sqlx_test]
    async fn machine_override_migration_preserves_hosts_and_enforces_references(
        pool: sqlx::PgPool,
    ) {
        let mut txn = pool.begin().await.unwrap();
        // The harness applies all migrations. Remove only this migration's
        // column to restore its predecessor before populating the host.
        sqlx::query("ALTER TABLE machines DROP COLUMN nic_firmware_profile_id")
            .execute(&mut *txn)
            .await
            .unwrap();
        let id: NicFirmwareProfileId = "existing".parse().unwrap();
        let profile = create(&mut txn, &id, &profile_config()).await.unwrap();
        let host = create_host(&mut txn, 1).await;
        let before: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(m) FROM machines m WHERE id = $1")
                .bind(host.id)
                .fetch_one(&mut *txn)
                .await
                .unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/20261013011104_machine_nic_firmware_profile.sql"
        ))
        .execute(&mut *txn)
        .await
        .unwrap();
        let after: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(m) FROM machines m WHERE id = $1")
                .bind(host.id)
                .fetch_one(&mut *txn)
                .await
                .unwrap();
        let mut expected = before;
        expected["nic_firmware_profile_id"] = serde_json::Value::Null;
        assert_eq!(after, expected);
        set_machine_override(&mut txn, &host.id, Some(&id), host.version)
            .await
            .unwrap();
        let stored = crate::machine::find_one(&mut *txn, &host.id, Default::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.config.nic_firmware_profile_id.as_ref(), Some(&id));
        let stored_profile = find_by_ids(&mut *txn, std::slice::from_ref(&id))
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(stored_profile.config, profile.config);
        assert_eq!(stored_profile.version, profile.version);
        let error = sqlx::query("DELETE FROM nic_firmware_profiles WHERE id = $1")
            .bind(&id)
            .execute(&mut *txn)
            .await
            .unwrap_err();
        let error = error.as_database_error().unwrap();
        assert!(error.is_foreign_key_violation());
        assert_eq!(
            error.constraint(),
            Some("machines_nic_firmware_profile_id_fkey")
        );
        txn.rollback().await.unwrap();
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
    async fn concurrent_site_default_writes_preserve_the_winner(pool: sqlx::PgPool) {
        for (scenario, clear, check_error) in [
            (
                "replace",
                false,
                (|error: &DatabaseError| {
                    assert!(
                        matches!(error, DatabaseError::ConcurrentModificationError(..)),
                        "{error:?}"
                    );
                }) as fn(&DatabaseError),
            ),
            ("clear", true, |error: &DatabaseError| {
                assert!(
                    matches!(error, DatabaseError::NotFoundError { .. }),
                    "{error:?}"
                );
            }),
        ] {
            let profile_a: NicFirmwareProfileId = format!("{scenario}_a").parse().unwrap();
            let profile_b: NicFirmwareProfileId = format!("{scenario}_b").parse().unwrap();
            let mut config = profile_config();
            config.entries[0].firmware.psid = scenario.into();
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
            let expected = if clear {
                clear_site_default(&mut winner, &hardware, created.version)
                    .await
                    .unwrap();
                None
            } else {
                let updated =
                    set_site_default(&mut winner, &hardware, &profile_b, Some(created.version))
                        .await
                        .unwrap();
                assert_eq!(
                    updated.version.version_nr(),
                    created.version.version_nr() + 1
                );
                Some(updated)
            };
            let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *winner)
                .await
                .unwrap();
            let competing_update = async {
                let mut txn = pool.begin().await.unwrap();
                let error =
                    set_site_default(&mut txn, &hardware, &profile_a, Some(created.version))
                        .await
                        .unwrap_err();
                txn.rollback().await.unwrap();
                error
            };
            let release_winner = async {
                wait_for_blocked_query(&pool, blocker_pid, "UPDATE nic_firmware_site_defaults")
                    .await;
                winner.commit().await.unwrap();
            };
            let (error, ()) = tokio::join!(competing_update, release_winner);
            check_error(&error);

            let bindings = find_site_defaults_by_ids(&pool, std::slice::from_ref(&hardware))
                .await
                .unwrap();
            assert_eq!(
                bindings
                    .iter()
                    .map(|binding| (&binding.hardware, &binding.profile_id, binding.version))
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|binding| (&hardware, &profile_b, binding.version))
                    .collect::<Vec<_>>(),
                "{scenario}"
            );
        }
    }

    #[crate::sqlx_test]
    async fn clear_site_default_survives_an_incompatible_profile_write(pool: sqlx::PgPool) {
        let id: NicFirmwareProfileId = "profile".parse().unwrap();
        let config = profile_config();
        let hardware = NicFirmwareHardware {
            part_number: config.entries[0].firmware.part_number.clone(),
            psid: config.entries[0].firmware.psid.clone(),
        };
        let mut txn = pool.begin().await.unwrap();
        let profile = create(&mut txn, &id, &config).await.unwrap();
        let binding = set_site_default(&mut txn, &hardware, &id, None)
            .await
            .unwrap();
        let mut incompatible = config;
        incompatible.entries[0].firmware.psid = "unsupported".into();
        let incompatible = incompatible.validate_and_normalize().unwrap();
        assert!(incompatible.entry_for(&hardware).is_none());
        let profile_version = profile.version.increment();
        // Bypass reference validation to exercise clearing an existing assignment
        // after a direct write leaves its profile valid but incompatible.
        sqlx::query("UPDATE nic_firmware_profiles SET config = $1, version = $2 WHERE id = $3")
            .bind(sqlx::types::Json(&incompatible))
            .bind(profile_version)
            .bind(&id)
            .execute(&mut *txn)
            .await
            .unwrap();
        txn.commit().await.unwrap();

        let mut txn = pool.begin().await.unwrap();
        clear_site_default(&mut txn, &hardware, binding.version)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        assert!(find_site_defaults(&pool).await.unwrap().is_empty());
        let stored = find_by_ids(&pool, &[id]).await.unwrap().pop().unwrap();
        assert_eq!(stored.config, incompatible);
        assert_eq!(stored.version, profile_version);
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
                // Retain the first referenced pair so checking only one binding
                // would miss the second pair removed by this replacement.
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
