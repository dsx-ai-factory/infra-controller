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
