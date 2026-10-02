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

use carbide_uuid::machine::MachineInterfaceId;
use model::machine_boot_override::MachineBootOverride;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{
    ColumnInfo, DatabaseError, DatabaseResult, FilterableQueryBuilder, ObjectColumnFilter,
};

#[derive(Clone, Copy)]
struct MachineInterfaceIdColumn;
impl ColumnInfo<'_> for MachineInterfaceIdColumn {
    type TableType = MachineBootOverride;
    type ColumnType = MachineInterfaceId;
    fn column_name(&self) -> &'static str {
        "machine_interface_id"
    }
}

pub async fn create(
    txn: &mut PgConnection,
    machine_interface_id: MachineInterfaceId,
    custom_pxe: Option<String>,
    custom_user_data: Option<String>,
) -> DatabaseResult<Option<MachineBootOverride>> {
    let query = "INSERT INTO machine_boot_override VALUES ($1, $2, $3) RETURNING machine_interface_id, custom_pxe, custom_user_data";
    let res = sqlx::query_as(query)
        .bind(machine_interface_id)
        .bind(custom_pxe)
        .bind(custom_user_data)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(Some(res))
}

pub async fn update_or_insert(
    value: &MachineBootOverride,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    match find_optional(&mut *txn, value.machine_interface_id).await? {
        Some(existing_mbo) => {
            let custom_pxe = if value.custom_pxe.is_some() {
                value.custom_pxe.clone()
            } else {
                existing_mbo.custom_pxe
            };

            let custom_user_data = if value.custom_user_data.is_some() {
                value.custom_user_data.clone()
            } else {
                existing_mbo.custom_user_data
            };

            let query = r#"UPDATE machine_boot_override SET custom_pxe=$1, custom_user_data=$2 WHERE machine_interface_id=$3;"#;

            sqlx::query(query)
                .bind(custom_pxe)
                .bind(custom_user_data)
                .bind(value.machine_interface_id)
                .execute(txn)
                .await
                .map_err(|e| DatabaseError::query(query, e))?;
        }
        None => {
            create(
                txn,
                value.machine_interface_id,
                value.custom_pxe.clone(),
                value.custom_user_data.clone(),
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn clear(
    txn: &mut PgConnection,
    machine_interface_id: MachineInterfaceId,
) -> DatabaseResult<()> {
    let query = "DELETE FROM machine_boot_override WHERE machine_interface_id = $1";

    sqlx::query(query)
        .bind(machine_interface_id)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_optional(
    txn: impl DbReader<'_>,
    machine_interface_id: MachineInterfaceId,
) -> DatabaseResult<Option<MachineBootOverride>> {
    let mut interfaces = find_by(
        txn,
        ObjectColumnFilter::One(MachineInterfaceIdColumn, &machine_interface_id),
    )
    .await?;
    match interfaces.len() {
        0 => Ok(None),
        1 => Ok(Some(interfaces.remove(0))),
        _ => Err(DatabaseError::FindOneReturnedManyResultsError(
            machine_interface_id.into(),
        )),
    }
}

async fn find_by<'a, C: ColumnInfo<'a, TableType = MachineBootOverride>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<MachineBootOverride>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new(
        "SELECT machine_interface_id, custom_pxe, custom_user_data FROM machine_boot_override",
    )
    .filter(&filter);

    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}
