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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use model::hardware_info::DmiData;
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use sqlx::Connection;

use super::*;

/// Creates one discovered host topology so BIOS-refresh tests exercise the production write path.
async fn create_discovered_host(
    txn: &mut PgConnection,
    id_byte: u8,
) -> Result<(MachineId, HardwareInfo, MachineTopology), Box<dyn std::error::Error>> {
    // Create the owning machine before its foreign-keyed topology.
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [id_byte; 32],
        MachineType::Host,
    );
    crate::machine::create(
        txn,
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;

    // Seed DMI data whose non-BIOS fields must remain protected by the update gate.
    let hardware = HardwareInfo {
        dmi_data: Some(DmiData {
            product_name: "original-model".to_string(),
            product_serial: "synthetic-serial".to_string(),
            bios_version: "old-version".to_string(),
            bios_date: "old-date".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let topology = create_or_update(txn, &machine_id, &hardware).await?;

    Ok((machine_id, hardware, topology))
}

/// Verifies rediscovery with a new BIOS version stores only the BIOS version and date, and measured
/// boot reads the new version because its profile is chosen from the stored BIOS version.
#[crate::sqlx_test]
async fn topology_bios_refresh_changes_only_bios_version_and_date(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let (machine_id, hardware, original) = create_discovered_host(txn.as_mut(), 0x71).await?;

    // Rediscover with a new BIOS version and date plus an unrelated DMI mutation.
    let mut rediscovered = hardware.clone();
    let incoming_dmi = rediscovered.dmi_data.as_mut().unwrap();
    incoming_dmi.bios_version = "new-version".to_string();
    incoming_dmi.bios_date = "new-date".to_string();
    incoming_dmi.product_name = "changed-model".to_string();
    let refreshed = create_or_update(txn.as_mut(), &machine_id, &rediscovered).await?;

    // Only the BIOS version and date change; the gate and inventory timestamp remain untouched.
    let stored_dmi = refreshed
        .topology
        .discovery_data
        .info
        .dmi_data
        .as_ref()
        .unwrap();
    assert_eq!(stored_dmi.bios_version, "new-version");
    assert_eq!(stored_dmi.bios_date, "new-date");
    assert_eq!(stored_dmi.product_name, "original-model");
    assert!(!refreshed.topology_update_needed);
    assert_eq!(refreshed.updated, original.updated);

    // Measured boot must select the newly observed BIOS profile attribute.
    let candidate = crate::measured_boot::machine::from_id(txn.as_mut(), machine_id).await?;
    assert_eq!(
        candidate.attrs.get("bios_version").map(String::as_str),
        Some("new-version")
    );

    txn.rollback().await?;
    Ok(())
}

/// Verifies an unchanged BIOS observation does not write the topology row.
#[crate::sqlx_test]
async fn topology_bios_refresh_skips_unchanged_values(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let (machine_id, hardware, original) = create_discovered_host(txn.as_mut(), 0x73).await?;

    // Capture the row location (ctid) so an otherwise invisible UPDATE would be detected.
    let tuple_before: (String,) =
        sqlx::query_as("SELECT ctid::text FROM machine_topologies WHERE machine_id = $1")
            .bind(machine_id)
            .fetch_one(txn.as_mut())
            .await?;

    // Report exactly the BIOS identity that is already stored.
    let repeated = create_or_update(txn.as_mut(), &machine_id, &hardware).await?;
    let tuple_after: (String,) =
        sqlx::query_as("SELECT ctid::text FROM machine_topologies WHERE machine_id = $1")
            .bind(machine_id)
            .fetch_one(txn.as_mut())
            .await?;

    assert_eq!(
        serde_json::to_value(&repeated)?,
        serde_json::to_value(&original)?
    );
    assert_eq!(
        tuple_after, tuple_before,
        "the unchanged row must not be updated"
    );

    txn.rollback().await?;
    Ok(())
}

/// Verifies missing or blank incoming BIOS data cannot erase the stored BIOS version and date.
#[crate::sqlx_test]
async fn topology_bios_refresh_ignores_missing_or_blank_versions(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let (machine_id, hardware, original) = create_discovered_host(txn.as_mut(), 0x72).await?;

    // Missing DMI data must preserve the complete stored topology.
    let without_dmi = create_or_update(txn.as_mut(), &machine_id, &HardwareInfo::default()).await?;
    assert_eq!(
        serde_json::to_value(&without_dmi)?,
        serde_json::to_value(&original)?
    );

    // A blank version cannot authorize a date-only or unrelated inventory write.
    let mut blank_version = hardware;
    let incoming_dmi = blank_version.dmi_data.as_mut().unwrap();
    incoming_dmi.bios_version = " \t ".to_string();
    incoming_dmi.bios_date = "new-date".to_string();
    incoming_dmi.product_name = "changed-model".to_string();
    let after_blank = create_or_update(txn.as_mut(), &machine_id, &blank_version).await?;
    assert_eq!(
        serde_json::to_value(&after_blank)?,
        serde_json::to_value(&original)?
    );

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn topology_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep prepared statements on the API connection while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE machine_topologies ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_queries(connection: &mut PgConnection) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x76; 32],
        MachineType::Host,
    );
    crate::machine::create(
        txn.as_mut(),
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    let mut hardware = HardwareInfo {
        dmi_data: Some(DmiData {
            product_serial: "projection-serial".to_string(),
            bios_version: "1.2.3".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let created = create_or_update(txn.as_mut(), &machine_id, &hardware).await?;
    assert_eq!(created.machine_id, machine_id);
    assert_eq!(created.topology.discovery_data.info, hardware);
    assert!(!created.topology_update_needed);
    set_topology_update_needed(txn.as_mut(), &machine_id, true).await?;
    let pending = find_by_machine_ids(txn.as_mut(), &[machine_id]).await?;
    assert!(pending[&machine_id][0].topology_update_needed);

    hardware.dmi_data.as_mut().unwrap().bios_version = "4.5.6".to_string();
    let updated = create_or_update(txn.as_mut(), &machine_id, &hardware).await?;
    assert_eq!(updated.machine_id, machine_id);
    assert_eq!(updated.created, created.created);
    assert!(updated.updated >= created.updated);
    assert_eq!(updated.topology.discovery_data.info, hardware);
    assert!(!updated.topology_update_needed);
    let found = find_by_machine_ids(txn.as_mut(), &[machine_id]).await?;
    assert_eq!(found[&machine_id].len(), 1);
    assert_eq!(
        serde_json::to_value(&found[&machine_id][0])?,
        serde_json::to_value(&updated)?
    );
    txn.rollback().await?;
    Ok(())
}
