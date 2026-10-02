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
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn power_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep prepared statements on the API connection while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE power_options ADD COLUMN test_added_column text;",
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
        [0x75; 32],
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
    // Machine creation uses the production power-options INSERT.
    let host_id: HostMachineId = machine_id.try_into()?;
    let mut options = get_by_ids(&[host_id], txn.as_mut()).await?;
    assert_eq!(options.len(), 1);
    let mut expected = options.remove(0);
    assert_eq!(expected.host_id, host_id);
    assert_eq!(expected.tried_triggering_on_at, None);

    let timestamp = "2026-09-01T12:00:00Z".parse()?;
    expected.last_fetched_updated_at = timestamp;
    expected.last_fetched_next_try_at = timestamp + chrono::Duration::minutes(2);
    expected.last_fetched_power_state = PowerState::Off;
    expected.last_fetched_off_counter = 2;
    expected.wait_until_time_before_performing_next_power_action =
        timestamp + chrono::Duration::minutes(15);
    // A populated timestamp catches a missing column that the decoder would turn into None.
    expected.tried_triggering_on_at = Some(timestamp);
    expected.tried_triggering_on_counter = 3;
    persist(&expected, txn.as_mut()).await?;
    let current_version = expected.desired_power_state_version;
    expected.desired_power_state = PowerState::Off;
    let updated = update_desired_state(
        &expected.host_id,
        PowerState::Off,
        &current_version,
        txn.as_mut(),
    )
    .await?;
    assert_eq!(
        updated.desired_power_state_version.version_nr(),
        current_version.version_nr() + 1
    );
    expected.desired_power_state_version = updated.desired_power_state_version;
    assert_eq!(
        serde_json::to_value(&updated)?,
        serde_json::to_value(&expected)?
    );

    for found in [
        get_all(txn.as_mut()).await?,
        get_by_ids(&[expected.host_id], txn.as_mut()).await?,
    ] {
        assert_eq!(found.len(), 1);
        assert_eq!(
            serde_json::to_value(&found[0])?,
            serde_json::to_value(&expected)?
        );
    }
    txn.rollback().await?;
    Ok(())
}
