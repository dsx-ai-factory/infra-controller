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
use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::MachineValidationId;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine_validation::MachineValidationResult;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{
    ColumnInfo, DatabaseError, DatabaseResult, FilterableQueryBuilder, ObjectColumnFilter,
    machine_validation_suites,
};

#[derive(Copy, Clone)]
pub struct MachineValidationIdColumn;
impl ColumnInfo<'_> for MachineValidationIdColumn {
    type TableType = MachineValidationResult;
    type ColumnType = MachineValidationId;

    fn column_name(&self) -> &'static str {
        "machine_validation_id"
    }
}

pub async fn find_by_machine_id<DB>(
    txn: &mut DB,
    machine_id: &MachineId,
    include_history: bool,
) -> DatabaseResult<Vec<MachineValidationResult>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    if include_history {
        // Fetch all validation_id from machine_validation table
        let machine_validation = crate::machine_validation::find_by(
            &mut *txn,
            ObjectColumnFilter::List(
                super::machine_validation::MachineIdColumn,
                std::slice::from_ref(machine_id),
            ),
        )
        .await?;

        let machine_validation_ids = machine_validation
            .into_iter()
            .map(|v| v.id)
            .collect::<Vec<_>>();

        return find_by(
            &mut *txn,
            ObjectColumnFilter::List(MachineValidationIdColumn, &machine_validation_ids),
        )
        .await;
    };
    let machine =
        match crate::machine::find_one(&mut *txn, machine_id, MachineSearchConfig::default()).await
        {
            Err(err) => {
                tracing::warn!(%machine_id, error = %err, "failed loading machine");
                return Err(DatabaseError::InvalidArgument(
                    "err loading machine".to_string(),
                ));
            }
            Ok(None) => {
                tracing::info!(%machine_id, "machine not found");
                return Err(DatabaseError::NotFoundError {
                    kind: "machine",
                    id: machine_id.to_string(),
                });
            }
            Ok(Some(m)) => m,
        };
    let discovery_machine_validation_id =
        machine.discovery_machine_validation_id.unwrap_or_default();
    let cleanup_machine_validation_id = machine.cleanup_machine_validation_id.unwrap_or_default();

    let on_demand_machine_validation_id =
        machine.on_demand_machine_validation_id.unwrap_or_default();
    find_by(
        &mut *txn,
        ObjectColumnFilter::List(
            MachineValidationIdColumn,
            &[
                cleanup_machine_validation_id,
                discovery_machine_validation_id,
                on_demand_machine_validation_id,
            ],
        ),
    )
    .await
}

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = MachineValidationResult>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<MachineValidationResult>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new(
        "SELECT
            machine_validation_id, name, description, command, args, context, stdout, stderr,
            exit_code, start_time, end_time, test_id
        FROM machine_validation_results",
    )
    .filter(&filter);
    query.push(" ORDER BY start_time");
    let custom_results = query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("machine_validation_results find_by", e))?;

    Ok(custom_results)
}

pub async fn create(value: MachineValidationResult, txn: &mut PgConnection) -> DatabaseResult<()> {
    let query = "
        INSERT INTO machine_validation_results (
            name,
            description,
            command,
            args,
            stdout,
            stderr,
            context,
            exit_code,
            machine_validation_id,
            start_time,
            end_time,
            test_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
        ON CONFLICT DO NOTHING";
    let _result = sqlx::query(query)
        .bind(&value.name)
        .bind(&value.description)
        .bind(&value.command)
        .bind(&value.args)
        .bind(&value.stdout)
        .bind(&value.stderr)
        .bind(&value.context)
        .bind(value.exit_code)
        .bind(value.validation_id)
        .bind(value.start_time)
        .bind(value.end_time)
        .bind(
            value
                .test_id
                .clone()
                .unwrap_or(machine_validation_suites::generate_test_id(&value.name)),
        )
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn validate_current_context(
    txn: &mut PgConnection,
    id: &MachineValidationId,
) -> DatabaseResult<Option<String>> {
    let db_results = find_by(
        txn,
        ObjectColumnFilter::List(MachineValidationIdColumn, std::slice::from_ref(id)),
    )
    .await?;

    for result in db_results {
        if result.exit_code != 0 {
            return Ok(Some(format!("{} is failed", result.name)));
        }
    }
    Ok(None)
}

pub async fn find_by_validation_id(
    txn: impl DbReader<'_>,
    validation_id: &MachineValidationId,
) -> DatabaseResult<Vec<MachineValidationResult>> {
    find_by(
        txn,
        ObjectColumnFilter::List(
            MachineValidationIdColumn,
            std::slice::from_ref(validation_id),
        ),
    )
    .await
}
