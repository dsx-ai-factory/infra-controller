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
use std::collections::HashMap;

use carbide_uuid::DbTable;
use carbide_uuid::machine::{HostMachineId, MachineId};
use mac_address::MacAddress;
use model::machine_boot_interface::MachineBootInterface;
use model::network_segment::NetworkSegmentType;
use model::predicted_machine_interface::{NewPredictedMachineInterface, PredictedMachineInterface};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{
    ColumnInfo, DatabaseError, DatabaseResult, FilterableQueryBuilder, ObjectColumnFilter,
};

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
    let mut query = FilterableQueryBuilder::new(format!(
        "SELECT {} FROM predicted_machine_interfaces",
        PredictedMachineInterface::db_table_columns(),
    ))
    .filter(&filter);
    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

/// Matches a host's primary HostInband prediction to a supplied boot pair so callers can verify boot intent before DHCP promotion.
///
/// Callers must obtain the target from current host Redfish evidence before accepting it for boot work.
pub(crate) fn matches_primary_host_boot_interface(
    prediction: &PredictedMachineInterface,
    host_machine_id: &HostMachineId,
    target: &MachineBootInterface,
) -> bool {
    prediction.machine_id == MachineId::from(*host_machine_id)
        && prediction.mac_address == target.mac_address
        && prediction.primary_interface
        && prediction.expected_network_segment_type == NetworkSegmentType::HostInband
        && prediction.boot_interface_id.as_deref() == Some(target.interface_id.as_str())
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

/// Returns the machine IDs owning predictions for the requested MACs.
/// Missing MACs are omitted; real interface ownership is not consulted.
pub async fn find_machine_ids_by_mac_addresses(
    db: impl DbReader<'_>,
    mac_addresses: &[MacAddress],
) -> DatabaseResult<HashMap<MacAddress, MachineId>> {
    if mac_addresses.is_empty() {
        return Ok(HashMap::new());
    }
    let query = "SELECT mac_address, machine_id FROM predicted_machine_interfaces \
                 WHERE mac_address = ANY($1)";
    let rows: Vec<(MacAddress, MachineId)> = sqlx::query_as(query)
        .bind(mac_addresses)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(rows.into_iter().collect())
}

pub async fn create(
    value: NewPredictedMachineInterface<'_>,
    txn: &mut PgConnection,
) -> Result<PredictedMachineInterface, DatabaseError> {
    let query = format!(
        "INSERT INTO predicted_machine_interfaces (machine_id, mac_address, expected_network_segment_type, boot_interface_id, primary_interface) VALUES ($1, $2, $3, $4, $5)
        RETURNING {}",
        PredictedMachineInterface::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(value.machine_id)
        .bind(value.mac_address)
        .bind(value.expected_network_segment_type)
        .bind(value.boot_interface_id)
        .bind(value.primary_interface)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

/// Promotes the requested pending interfaces after competing predictions are demoted.
pub async fn promote_interfaces(macs: &[MacAddress], txn: &mut PgConnection) -> DatabaseResult<()> {
    if macs.is_empty() {
        return Ok(());
    }
    let query =
        "UPDATE predicted_machine_interfaces SET primary_interface=true WHERE mac_address=ANY($1)";
    sqlx::query(query)
        .bind(macs)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// Refreshes report IDs in one statement, preserving IDs omitted from the report.
pub async fn set_boot_interface_ids(
    ids: &[(MacAddress, &str)],
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let macs: Vec<_> = ids.iter().map(|(mac, _)| *mac).collect();
    let boot_ids: Vec<_> = ids.iter().map(|(_, id)| *id).collect();
    let query = "UPDATE predicted_machine_interfaces AS p SET boot_interface_id=v.boot_id \
                 FROM UNNEST($1::macaddr[], $2::text[]) AS v(mac, boot_id) WHERE p.mac_address=v.mac";
    sqlx::query(query)
        .bind(macs)
        .bind(boot_ids)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// Inserts pending interfaces in batches bounded by PostgreSQL's bind limit.
pub async fn create_many(
    values: &[NewPredictedMachineInterface<'_>],
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    for chunk in values.chunks(crate::BIND_LIMIT / 5) {
        let mut query = sqlx::QueryBuilder::new(
            "INSERT INTO predicted_machine_interfaces (machine_id, mac_address, expected_network_segment_type, boot_interface_id, primary_interface) ",
        );
        query.push_values(chunk, |mut row, value| {
            row.push_bind(value.machine_id)
                .push_bind(value.mac_address)
                .push_bind(value.expected_network_segment_type)
                .push_bind(value.boot_interface_id)
                .push_bind(value.primary_interface);
        });
        query
            .build()
            .execute(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query.sql(), error))?;
    }
    Ok(())
}

#[cfg(test)]
mod test_bulk_host_interfaces {
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;

    #[crate::sqlx_test]
    async fn bulk_host_interface_predictions_preserve_omitted_ids(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x91; 32],
            MachineType::Host,
        );
        let mut txn = pool.begin().await?;
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(txn.as_mut())
            .await?;
        let macs = [
            MacAddress::new([2, 0, 0, 0, 0x91, 1]),
            MacAddress::new([2, 0, 0, 0, 0x91, 2]),
        ];
        let values: Vec<_> = macs
            .iter()
            .map(|mac| NewPredictedMachineInterface {
                machine_id: &machine_id,
                mac_address: *mac,
                expected_network_segment_type:
                    model::network_segment::NetworkSegmentType::HostInband,
                boot_interface_id: Some("NIC.Stored"),
                primary_interface: false,
            })
            .collect();
        create_many(&values, txn.as_mut()).await?;
        promote_interfaces(&macs[1..], txn.as_mut()).await?;
        set_boot_interface_ids(&[(macs[1], "NIC.Refreshed")], txn.as_mut()).await?;
        for (mac, primary, boot_id) in [
            (macs[0], false, "NIC.Stored"),
            (macs[1], true, "NIC.Refreshed"),
        ] {
            let row = find_by_mac_address(txn.as_mut(), mac).await?.unwrap();
            assert_eq!(row.machine_id, machine_id);
            assert_eq!(row.primary_interface, primary);
            assert_eq!(row.boot_interface_id.as_deref(), Some(boot_id));
        }
        txn.rollback().await?;
        let mut connection = pool.acquire().await?;
        assert!(
            find_by_mac_address(&mut connection, macs[0])
                .await?
                .is_none()
        );
        Ok(())
    }
}
