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
use carbide_uuid::machine::MachineInterfaceId;
use sqlx::{FromRow, PgConnection};

use super::DatabaseError;

/// A machine dhcp response is a representation of some booting interface by Mac Address or DUID
/// (not implemented) that returns the network information for that interface on that node, and
/// contains everything necessary to return a DHCP response
#[derive(Debug, FromRow, carbide_macros::DbTable)]
#[db_table(name = "dhcp_entries")]
pub struct DhcpEntry {
    pub machine_interface_id: MachineInterfaceId,
    pub vendor_string: String,
}

#[derive(Clone, Copy)]
pub struct MachineInterfaceIdColumn;

impl super::ColumnInfo<'_> for MachineInterfaceIdColumn {
    type TableType = DhcpEntry;
    type ColumnType = MachineInterfaceId;

    fn column_name(&self) -> &'static str {
        "machine_interface_id"
    }
}

pub async fn find_by<'a, C: super::ColumnInfo<'a, TableType = DhcpEntry>>(
    txn: &mut PgConnection,
    filter: super::ObjectColumnFilter<'a, C>,
) -> Result<Vec<DhcpEntry>, DatabaseError> {
    let mut query = super::FilterableQueryBuilder::new(format!(
        "SELECT {} FROM dhcp_entries",
        DhcpEntry::db_table_columns()
    ))
    .filter(&filter);

    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

pub async fn persist(value: DhcpEntry, txn: &mut PgConnection) -> Result<(), DatabaseError> {
    let query = "
INSERT INTO dhcp_entries (machine_interface_id, vendor_string)
VALUES ($1::uuid, $2::varchar)
ON CONFLICT DO NOTHING";
    let _result = sqlx::query(query)
        .bind(value.machine_interface_id)
        .bind(&value.vendor_string)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn delete(
    txn: &mut PgConnection,
    machine_interface_id: &MachineInterfaceId,
) -> Result<(), DatabaseError> {
    let query = "
DELETE FROM dhcp_entries WHERE machine_interface_id=$1::uuid";
    sqlx::query(query)
        .bind(machine_interface_id)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}
