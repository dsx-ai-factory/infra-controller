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
use carbide_uuid::machine::{HostMachineId, MachineId};
use config_version::ConfigVersion;
use model::power_manager::{PowerOptions, PowerState};
use sqlx::PgConnection;

use crate::DatabaseError;

/// Create a power option entry for a host into db.
pub async fn create(
    host_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<PowerOptions, DatabaseError> {
    let query = "INSERT INTO power_options ( host_id ) VALUES ($1) RETURNING *";

    let options = sqlx::query_as(query)
        .bind(host_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(options)
}

pub async fn update_desired_state(
    host_id: &HostMachineId,
    power_state: PowerState,
    current_version: &ConfigVersion,
    txn: &mut PgConnection,
) -> Result<PowerOptions, DatabaseError> {
    let query = "UPDATE power_options SET desired_power_state=$1, desired_power_state_version=$2 WHERE host_id=$3 RETURNING *";

    let config_version = current_version.increment();

    let updated_value = sqlx::query_as(query)
        .bind(power_state)
        .bind(config_version)
        .bind(host_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(updated_value)
}

pub async fn get_all(txn: &mut PgConnection) -> Result<Vec<PowerOptions>, DatabaseError> {
    let query = "SELECT * FROM power_options";

    let all_options = sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(all_options)
}

pub async fn get_by_ids(
    machine_ids: &[HostMachineId],
    txn: &mut PgConnection,
) -> Result<Vec<PowerOptions>, DatabaseError> {
    let query = "SELECT * FROM power_options WHERE host_id = ANY($1)";

    let all_options = sqlx::query_as(query)
        .bind(machine_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(all_options)
}

pub async fn persist(options: &PowerOptions, txn: &mut PgConnection) -> Result<(), DatabaseError> {
    let query = "UPDATE power_options SET 
                                    last_fetched_updated_at=$1, last_fetched_next_try_at=$2,
                                    last_fetched_power_state=$3, last_fetched_off_counter=$4,
                                    wait_until_time_before_performing_next_power_action=$5,
                                    tried_triggering_on_at=$6, tried_triggering_on_counter=$7
                                WHERE host_id=$8";

    sqlx::query(query)
        .bind(options.last_fetched_updated_at)
        .bind(options.last_fetched_next_try_at)
        .bind(options.last_fetched_power_state)
        .bind(options.last_fetched_off_counter)
        .bind(options.wait_until_time_before_performing_next_power_action)
        .bind(options.tried_triggering_on_at)
        .bind(options.tried_triggering_on_counter)
        .bind(options.host_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}
