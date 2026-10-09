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
use mac_address::MacAddress;
use model::predicted_machine_interface::{NewPredictedMachineInterface, PredictedMachineInterface};
use sqlx::PgConnection;

use crate::{ColumnInfo, DatabaseError, FilterableQueryBuilder, ObjectColumnFilter};

#[derive(Clone, Copy)]
pub struct MachineIdColumn;

impl ColumnInfo<'_> for crate::predicted_machine_interface::MachineIdColumn {
    type TableType = PredictedMachineInterface;
    type ColumnType = carbide_uuid::machine::MachineId;
    fn column_name(&self) -> &'static str {
        "machine_id"
    }
}

#[derive(Clone, Copy)]
pub struct MacAddressColumn;
impl ColumnInfo<'_> for MacAddressColumn {
    type TableType = PredictedMachineInterface;
    type ColumnType = MacAddress;
    fn column_name(&self) -> &'static str {
        "mac_address"
    }
}

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = PredictedMachineInterface>>(
    txn: &mut PgConnection,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<PredictedMachineInterface>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new(
        "SELECT
            id, machine_id, mac_address, expected_network_segment_type,
            boot_interface_id, primary_interface
        FROM predicted_machine_interfaces",
    )
    .filter(&filter);
    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

/// Records the vendor-named Redfish `EthernetInterface.Id` on the predicted
/// row(s) with the given MAC, keeping pending predictions as current as the
/// live report -- the same per-exploration refresh `machine_interfaces`
/// rows get.
pub async fn set_boot_interface_id(
    txn: &mut PgConnection,
    mac_address: MacAddress,
    boot_interface_id: &str,
) -> Result<(), DatabaseError> {
    let query =
        "UPDATE predicted_machine_interfaces SET boot_interface_id = $1 WHERE mac_address = $2";
    sqlx::query(query)
        .bind(boot_interface_id)
        .bind(mac_address)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// Reconciles the selected primary state on a pending prediction.
pub async fn set_primary_interface(
    id: uuid::Uuid,
    primary_interface: bool,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE predicted_machine_interfaces SET primary_interface = $1 WHERE id = $2";
    sqlx::query(query)
        .bind(primary_interface)
        .bind(id)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// Clears the primary flag from every pending prediction except the selected
/// MAC, keeping boot selection unambiguous when a refreshed report adds or
/// changes the primary candidate.
pub async fn demote_other_primary_interfaces_for_machine(
    machine_id: &carbide_uuid::machine::MachineId,
    primary_mac_address: MacAddress,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE predicted_machine_interfaces SET primary_interface=false WHERE machine_id=$1 AND mac_address!=$2 AND primary_interface=true";
    sqlx::query(query)
        .bind(machine_id)
        .bind(primary_mac_address)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn delete(
    value: &PredictedMachineInterface,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "DELETE FROM predicted_machine_interfaces WHERE id = $1";
    sqlx::query(query)
        .bind(value.id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// All predicted interfaces for a machine -- the boot-interface candidates a
/// host offers while it awaits its first DHCP lease.
pub async fn find_by_machine_id(
    txn: &mut PgConnection,
    machine_id: &carbide_uuid::machine::MachineId,
) -> Result<Vec<PredictedMachineInterface>, DatabaseError> {
    find_by(txn, ObjectColumnFilter::One(MachineIdColumn, machine_id)).await
}

pub async fn find_by_mac_address(
    txn: &mut PgConnection,
    mac_address: MacAddress,
) -> Result<Option<PredictedMachineInterface>, DatabaseError> {
    Ok(
        find_by(txn, ObjectColumnFilter::One(MacAddressColumn, &mac_address))
            .await?
            .into_iter()
            .next(),
    )
}

pub async fn create(
    value: NewPredictedMachineInterface<'_>,
    txn: &mut PgConnection,
) -> Result<PredictedMachineInterface, DatabaseError> {
    let query = "INSERT INTO predicted_machine_interfaces (machine_id, mac_address, expected_network_segment_type, boot_interface_id, primary_interface) VALUES ($1, $2, $3, $4, $5)
        RETURNING
            id, machine_id, mac_address, expected_network_segment_type,
            boot_interface_id, primary_interface";
    sqlx::query_as(query)
        .bind(value.machine_id)
        .bind(value.mac_address)
        .bind(value.expected_network_segment_type)
        .bind(&value.boot_interface_id)
        .bind(value.primary_interface)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}
