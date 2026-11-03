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
use carbide_uuid::DbTable;
use config_version::ConfigVersion;
use model::machine_validation::MachineValidationExternalConfig;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// `find_config_by_name` loads a named external configuration. Only a missing
/// row returns `NotFoundError`; other failures retain their SQLx source and query.
pub async fn find_config_by_name(
    txn: impl DbReader<'_>,
    name: &str,
) -> DatabaseResult<MachineValidationExternalConfig> {
    let query = format!(
        "SELECT {} FROM machine_validation_external_config WHERE name=$1",
        MachineValidationExternalConfig::db_table_columns(),
    );
    match sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(name)
        .fetch_one(txn)
        .await
    {
        Ok(val) => Ok(val),
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::NotFoundError {
            kind: "machine_validation_external_config",
            id: name.to_owned(),
        }),
        Err(error) => Err(DatabaseError::query(&query, error)),
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

/// `find_configs` loads configurations whose name appears in `names`. Missing
/// names contribute no entries; empty `names` lists all configurations. The result
/// order is unspecified. Query failures retain their SQLx source and query.
pub async fn find_configs(
    txn: impl DbReader<'_>,
    names: &[String],
) -> DatabaseResult<Vec<MachineValidationExternalConfig>> {
    let query = format!(
        "SELECT {} FROM machine_validation_external_config
         WHERE cardinality($1::text[]) = 0 OR name = ANY($1)",
        MachineValidationExternalConfig::db_table_columns(),
    );

    let configs = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(names)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(&query, e))?;
    Ok(configs)
}

/// `remove_config` deletes and returns a named external configuration. Only a
/// missing row returns `NotFoundError`; other failures retain their SQLx source
/// and query. Run this in a transaction and roll it back on error: PostgreSQL
/// deletes the row before SQLx decodes the returned configuration.
pub async fn remove_config(
    txn: &mut PgConnection,
    name: &str,
) -> DatabaseResult<MachineValidationExternalConfig> {
    let query = format!(
        "DELETE FROM machine_validation_external_config WHERE name=$1 RETURNING {}",
        MachineValidationExternalConfig::db_table_columns(),
    );
    match sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(name)
        .fetch_one(txn)
        .await
    {
        Ok(val) => Ok(val),
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::NotFoundError {
            kind: "machine_validation_external_config",
            id: name.to_owned(),
        }),
        Err(error) => Err(DatabaseError::query(&query, error)),
    }
}
