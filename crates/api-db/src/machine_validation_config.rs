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
use model::machine_validation::MachineValidationExternalConfig;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

pub async fn find_config_by_name(
    txn: impl DbReader<'_>,
    name: &str,
) -> DatabaseResult<MachineValidationExternalConfig> {
    let query = "SELECT
            name, description, config, version
        FROM machine_validation_external_config WHERE name=$1";
    match sqlx::query_as(query).bind(name).fetch_one(txn).await {
        Ok(val) => Ok(val),
        Err(_) => Err(DatabaseError::NotFoundError {
            kind: "machine_validation_external_config",
            id: name.to_owned(),
        }),
    }
}

pub async fn save(
    txn: &mut PgConnection,
    name: &str,
    description: &str,
    config: &Vec<u8>,
) -> DatabaseResult<()> {
    let query = "INSERT INTO machine_validation_external_config (name, description, config, version) VALUES ($1, $2, $3, $4) RETURNING name";

    sqlx::query_as::<_, ()>(query)
        .bind(name)
        .bind(description)
        .bind(config.as_slice())
        .bind(ConfigVersion::initial())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

async fn update(
    txn: &mut PgConnection,
    name: &str,
    config: &Vec<u8>,
    next_version: ConfigVersion,
) -> DatabaseResult<()> {
    let query = "UPDATE machine_validation_external_config SET config=$2, version=$3 WHERE name=$1 RETURNING name";

    sqlx::query_as::<_, ()>(query)
        .bind(name)
        .bind(config.as_slice())
        .bind(next_version)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn create_or_update(
    txn: &mut PgConnection,
    name: &str,
    description: &str,
    data: &Vec<u8>,
) -> DatabaseResult<()> {
    match find_config_by_name(&mut *txn, name).await {
        Ok(config) => update(txn, name, data, config.version.increment()).await?,
        Err(_) => save(txn, name, description, data).await?,
    };
    Ok(())
}

pub async fn find_configs(
    txn: impl DbReader<'_>,
) -> DatabaseResult<Vec<MachineValidationExternalConfig>> {
    let query = "SELECT
            name, description, config, version
        FROM machine_validation_external_config";

    let names = sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(names)
}

pub async fn remove_config(
    txn: &mut PgConnection,
    name: &str,
) -> DatabaseResult<MachineValidationExternalConfig> {
    let query = "DELETE FROM machine_validation_external_config WHERE name=$1
        RETURNING
            name, description, config, version";
    match sqlx::query_as(query).bind(name).fetch_one(txn).await {
        Ok(val) => Ok(val),
        Err(_) => Err(DatabaseError::NotFoundError {
            kind: "machine_validation_external_config",
            id: name.to_owned(),
        }),
    }
}
