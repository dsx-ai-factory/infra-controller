// SPDX-FileCopyrightText: Copyright (c) 2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;

use carbide_uuid::machine::MachineId;
use model::lldp::LldpNeighbor;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// The stored LLDP neighbors of each of `machine_ids`, ordered by link. Machines without
/// neighbors are absent from the map.
pub async fn find_by_machine_ids(
    txn: impl DbReader<'_>,
    machine_ids: &[MachineId],
) -> DatabaseResult<HashMap<MachineId, Vec<LldpNeighbor>>> {
    if machine_ids.is_empty() {
        return Ok(HashMap::new());
    }

    #[derive(sqlx::FromRow)]
    struct Row {
        machine_id: MachineId,
        #[sqlx(flatten)]
        neighbor: LldpNeighbor,
    }

    let query = "SELECT machine_id, local_mac_address, local_port, chassis_id_type, chassis_id_value, \
         remote_port_type, remote_port_value, system_name, system_description, \
         management_addresses, med_serial, med_manufacturer, med_model \
         FROM machine_lldp_neighbors WHERE machine_id = ANY($1) \
         ORDER BY machine_id, local_mac_address, local_port, chassis_id_type, chassis_id_value, \
         remote_port_type, remote_port_value";
    let rows: Vec<Row> = sqlx::query_as(query)
        .bind(machine_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    let mut by_machine = HashMap::<MachineId, Vec<LldpNeighbor>>::new();
    for row in rows {
        by_machine
            .entry(row.machine_id)
            .or_default()
            .push(row.neighbor);
    }
    Ok(by_machine)
}

/// An empty slice clears the LLDP neighbors.
///
/// Fails with [`DatabaseError::NotFoundError`] when `machine_id` is unknown.
pub async fn replace_all(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    neighbors: &[LldpNeighbor],
) -> DatabaseResult<()> {
    // Serialize replacements per machine. Under READ COMMITTED the DELETE below only locks
    // rows that exist now, so two overlapping reports for the same machine (a scout retry
    // racing the original) could both delete and then the later INSERT would fail with a
    // primary-key violation once the first commits. Holding the parent row makes the second
    // report wait and then replace the first one's committed rows.
    let lock = "SELECT 1 FROM machines WHERE id = $1 FOR UPDATE";
    sqlx::query(lock)
        .bind(machine_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(lock, e))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;

    let delete = "DELETE FROM machine_lldp_neighbors WHERE machine_id = $1";
    sqlx::query(delete)
        .bind(machine_id)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(delete, e))?;

    if neighbors.is_empty() {
        return Ok(());
    }

    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "INSERT INTO machine_lldp_neighbors (machine_id, local_mac_address, local_port, chassis_id_type, chassis_id_value, \
         remote_port_type, remote_port_value, system_name, system_description, \
         management_addresses, med_serial, med_manufacturer, med_model) ",
    );
    builder.push_values(neighbors, |mut row, neighbor| {
        row.push_bind(machine_id)
            .push_bind(neighbor.local_mac_address)
            .push_bind(&neighbor.local_port)
            .push_bind(&neighbor.chassis_id_type)
            .push_bind(&neighbor.chassis_id_value)
            .push_bind(&neighbor.remote_port_type)
            .push_bind(&neighbor.remote_port_value)
            .push_bind(&neighbor.system_name)
            .push_bind(&neighbor.system_description)
            .push_bind(&neighbor.management_addresses)
            .push_bind(&neighbor.med_serial)
            .push_bind(&neighbor.med_manufacturer)
            .push_bind(&neighbor.med_model);
    });
    builder
        .build()
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))?;

    Ok(())
}
