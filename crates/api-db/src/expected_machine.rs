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
use std::net::IpAddr;

use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::{RackId, RackProfileId};
use itertools::Itertools;
use mac_address::MacAddress;
use model::expected_machine::{
    ExpectedMachine, ExpectedMachineRequest, LinkedExpectedMachine, UnexpectedMachine,
};
use model::site_explorer::EndpointExplorationReport;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

const SQL_VIOLATION_DUPLICATE_MAC: &str = "expected_machines_bmc_mac_address_key";

pub async fn find_by_bmc_mac_address(
    txn: impl DbReader<'_>,
    bmc_mac_address: MacAddress,
) -> Result<Option<ExpectedMachine>, DatabaseError> {
    let sql = "SELECT * FROM expected_machines WHERE bmc_mac_address=$1";
    sqlx::query_as(sql)
        .bind(bmc_mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_by_id(
    txn: impl DbReader<'_>,
    id: Uuid,
) -> Result<Option<ExpectedMachine>, DatabaseError> {
    let sql = "SELECT * FROM expected_machines WHERE id=$1";
    sqlx::query_as(sql)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_many_by_bmc_mac_address(
    txn: &mut PgConnection,
    bmc_mac_addresses: &[MacAddress],
) -> DatabaseResult<HashMap<MacAddress, ExpectedMachine>> {
    let sql = "SELECT * FROM expected_machines WHERE bmc_mac_address=ANY($1)";
    let v: Vec<ExpectedMachine> = sqlx::query_as(sql)
        .bind(bmc_mac_addresses)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;

    // expected_machines has a unique constraint on bmc_mac_address,
    // but if the constraint gets dropped and we have multiple mac addresses,
    // we want this code to generate an Err and not silently drop values
    // and/or return nothing.
    v.into_iter()
        .into_group_map_by(|exp| exp.bmc_mac_address)
        .drain()
        .map(|(k, mut v)| {
            if v.len() > 1 {
                Err(DatabaseError::AlreadyFoundError {
                    kind: "ExpectedMachine",
                    id: k.to_string(),
                })
            } else {
                Ok((k, v.pop().unwrap()))
            }
        })
        .collect()
}

/// RMS rack identity for a compute tray that does not yet have a machine row,
/// resolved from the expected inventory. Used for pre-ingestion firmware
/// updates against rack-scale (RMS-managed) compute trays.
#[derive(Debug, sqlx::FromRow)]
pub struct PreIngestionComputeRmsIdentity {
    pub bmc_mac_address: MacAddress,
    pub rack_id: RackId,
    pub rack_profile_id: Option<RackProfileId>,
}

/// Resolve RMS rack identities for pre-ingestion compute trays by BMC MAC.
///
/// A compute tray is rack-scale (RMS-managed) exactly when its expected record
/// declares a `rack_id`; this mirrors the signal site-explorer uses to build an
/// RMS node identity at machine creation. The rack profile is taken from the
/// live `racks` row when it exists and otherwise from the `expected_racks`
/// declaration, so the descriptor resolves before the rack row is created.
/// Rows without a `rack_id` are omitted (standalone servers are not RMS-managed).
pub async fn find_rms_identities_by_bmc_macs(
    db: impl DbReader<'_>,
    bmc_mac_addresses: &[MacAddress],
) -> DatabaseResult<Vec<PreIngestionComputeRmsIdentity>> {
    let sql = r#"
        SELECT
            em.bmc_mac_address AS bmc_mac_address,
            em.rack_id AS rack_id,
            COALESCE(r.rack_profile_id, er.rack_profile_id) AS rack_profile_id
        FROM expected_machines em
        LEFT JOIN racks r ON r.id = em.rack_id
        LEFT JOIN expected_racks er ON er.rack_id = em.rack_id
        WHERE em.bmc_mac_address = ANY($1)
          AND em.rack_id IS NOT NULL
    "#;

    sqlx::query_as(sql)
        .bind(bmc_mac_addresses)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// Find the ExpectedMachine that declares an interface MAC.
///
/// DHCP uses this lookup when NICo is not authoritative for every address on
/// the surrounding network. The SQL keeps the physical `host_nics` column so
/// existing databases need no migration for the model rename.
pub async fn find_by_interface_mac_address(
    txn: &mut PgConnection,
    interface_mac_address: MacAddress,
) -> DatabaseResult<Option<ExpectedMachine>> {
    let query = "SELECT * FROM expected_machines WHERE host_nics @> $1::jsonb";
    let mac_address = serde_json::json!([{ "mac_address": interface_mac_address.to_string() }]);
    sqlx::query_as(query)
        .bind(sqlx::types::Json(mac_address))
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))
}

pub async fn find_one_linked(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
) -> DatabaseResult<Option<LinkedExpectedMachine>> {
    let sql = r#"
 SELECT
 em.serial_number,
 em.bmc_mac_address,
 mi.id AS interface_id,
 ee.address AS address,
 mi.machine_id,
 em.id AS expected_machine_id
FROM expected_machines em
 LEFT JOIN machine_interfaces mi ON em.bmc_mac_address = mi.mac_address
 LEFT JOIN machine_interface_addresses mia ON mi.id = mia.interface_id
 LEFT JOIN explored_endpoints ee ON mia.address = ee.address
 WHERE em.bmc_mac_address = $1
 ORDER BY em.bmc_mac_address
 "#;
    sqlx::query_as(sql)
        .bind(bmc_mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_all(txn: impl DbReader<'_>) -> DatabaseResult<Vec<ExpectedMachine>> {
    let sql = "SELECT * FROM expected_machines";
    sqlx::query_as(sql)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// Lock expected-machine writes and return the inventory being replaced.
///
/// Older clients omit interface fields they do not know about. Replace-all
/// reads those values before clearing the table, so the read must exclude
/// concurrent inserts, updates, and deletes until the replacement transaction
/// commits. Replace-all is rare, so a table-level writer lock keeps the complete
/// inventory operation easy to reason about.
pub async fn find_all_for_replace(txn: &mut PgConnection) -> DatabaseResult<Vec<ExpectedMachine>> {
    let lock = "LOCK TABLE expected_machines IN SHARE ROW EXCLUSIVE MODE";
    sqlx::query(lock)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(lock, error))?;

    let query = "SELECT * FROM expected_machines ORDER BY id";
    sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// find_all_by_rack_id returns all expected machines for a given rack_id.
pub async fn find_all_by_rack_id(
    txn: &mut PgConnection,
    rack_id: &RackId,
) -> DatabaseResult<Vec<ExpectedMachine>> {
    let sql = "SELECT * FROM expected_machines WHERE rack_id=$1";
    sqlx::query_as(sql)
        .bind(rack_id)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_all_linked(txn: impl DbReader<'_>) -> DatabaseResult<Vec<LinkedExpectedMachine>> {
    let sql = r#"
 SELECT
 em.serial_number,
 em.bmc_mac_address,
 mi.id AS interface_id,
 ee.address AS address,
 mi.machine_id,
 em.id AS expected_machine_id
FROM expected_machines em
 LEFT JOIN machine_interfaces mi ON em.bmc_mac_address = mi.mac_address
 LEFT JOIN machine_interface_addresses mia ON mi.id = mia.interface_id
 LEFT JOIN explored_endpoints ee ON mia.address = ee.address
 ORDER BY em.bmc_mac_address
 "#;
    sqlx::query_as(sql)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// Returns host BMC endpoints that Site Explorer has explored but whose MAC is
/// not listed in any of `expected_machines`, `expected_power_shelves`, or
/// `expected_switches`. DPUs, power shelves, and switches are filtered out so
/// the result only contains actual host BMCs. Rows with `machine_id = Some`
/// are orphans (already ingested before the `expected_machines` entry was
/// removed).
pub async fn find_all_unexpected(txn: impl DbReader<'_>) -> DatabaseResult<Vec<UnexpectedMachine>> {
    #[derive(FromRow)]
    struct UnexpectedRow {
        address: IpAddr,
        bmc_mac_address: MacAddress,
        exploration_report: sqlx::types::Json<EndpointExplorationReport>,
        machine_id: Option<MachineId>,
    }

    let sql = r#"
SELECT
    ee.address,
    mi.mac_address AS bmc_mac_address,
    ee.exploration_report,
    mi.machine_id
FROM explored_endpoints ee
    LEFT JOIN machine_interface_addresses mia ON ee.address = mia.address
    LEFT JOIN machine_interfaces mi ON mia.interface_id = mi.id
WHERE mi.mac_address IS NOT NULL
  AND ee.exploration_report->>'EndpointType' = 'Bmc'
  AND mi.mac_address NOT IN (SELECT bmc_mac_address FROM expected_machines)
  AND mi.mac_address NOT IN (SELECT bmc_mac_address FROM expected_power_shelves)
  AND mi.mac_address NOT IN (SELECT bmc_mac_address FROM expected_switches)
ORDER BY ee.address
    "#;

    let rows: Vec<UnexpectedRow> = sqlx::query_as(sql)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;

    Ok(rows
        .into_iter()
        .filter(|row| {
            !row.exploration_report.0.is_dpu()
                && !row.exploration_report.0.is_power_shelf()
                && !row.exploration_report.0.is_switch()
        })
        .map(|row| UnexpectedMachine {
            address: row.address,
            bmc_mac_address: row.bmc_mac_address,
            machine_id: row.machine_id,
        })
        .collect())
}

pub async fn update_bmc_credentials<'a>(
    value: &'a mut ExpectedMachine,
    txn: &mut PgConnection,
    bmc_username: String,
    bmc_password: String,
) -> DatabaseResult<&'a mut ExpectedMachine> {
    let query = "UPDATE expected_machines SET bmc_username=$1, bmc_password=$2 WHERE bmc_mac_address=$3 RETURNING bmc_mac_address";

    sqlx::query_as::<_, ()>(query)
        .bind(&bmc_username)
        .bind(&bmc_password)
        .bind(value.bmc_mac_address)
        .fetch_one(txn)
        .await
        .map_err(|err: sqlx::Error| match err {
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "expected_machine",
                id: value.bmc_mac_address.to_string(),
            },
            _ => DatabaseError::query(query, err),
        })?;

    value.data.bmc_username = bmc_username;
    value.data.bmc_password = bmc_password;

    Ok(value)
}

/// Inserts a new expected machine row. If the id field is None, a new UUID is generated.
///
/// Persisted `bmc_ip_address` is the configured static BMC IP; the API layer is responsible for
/// creating/updating the matching `machine_interface` rows before calling this (see
/// `preallocate_machine_interface` / `update_preallocated_machine_interface`).
pub async fn create(
    txn: &mut PgConnection,
    machine: ExpectedMachine,
) -> DatabaseResult<ExpectedMachine> {
    let id = machine.id.unwrap_or_else(Uuid::new_v4);
    let query = "INSERT INTO expected_machines
            (id, bmc_mac_address, bmc_username, bmc_password, serial_number, fallback_dpu_serial_numbers, metadata_name, metadata_description, metadata_labels, sku_id, host_nics, rack_id, default_pause_ingestion_and_poweron, dpf_enabled, bmc_ip_address, bmc_retain_credentials, dpu_mode, bmc_ip_allocation, host_lifecycle_profile)
            VALUES
            ($1::uuid, $2::macaddr, $3::varchar, $4::varchar, $5::varchar, $6::text[], $7, $8, $9::jsonb, $10::varchar, $11::jsonb, $12, $13, $14, $15::inet, $16, $17, $18, $19::jsonb) RETURNING *";

    sqlx::query_as(query)
        .bind(id)
        .bind(machine.bmc_mac_address)
        .bind(&machine.data.bmc_username)
        .bind(&machine.data.bmc_password)
        .bind(&machine.data.serial_number)
        .bind(&machine.data.fallback_dpu_serial_numbers)
        .bind(&machine.data.metadata.name)
        .bind(&machine.data.metadata.description)
        .bind(sqlx::types::Json(&machine.data.metadata.labels))
        .bind(&machine.data.sku_id)
        .bind(sqlx::types::Json(&machine.data.interfaces))
        .bind(&machine.data.rack_id)
        .bind(
            machine
                .data
                .default_pause_ingestion_and_poweron
                .unwrap_or(false),
        )
        .bind(machine.data.dpf_enabled.unwrap_or(true))
        .bind(machine.data.bmc_ip_address)
        .bind(machine.data.bmc_retain_credentials.unwrap_or(false))
        .bind(machine.data.dpu_policy)
        .bind(machine.data.bmc_ip_allocation)
        .bind(sqlx::types::Json(&machine.data.host_lifecycle_profile))
        .fetch_one(txn)
        .await
        .map_err(|err: sqlx::Error| match err {
            sqlx::Error::Database(e) if e.constraint() == Some(SQL_VIOLATION_DUPLICATE_MAC) => {
                DatabaseError::ExpectedHostDuplicateMacAddress(machine.bmc_mac_address)
            }
            _ => DatabaseError::query(query, err),
        })
}

/// find returns an expected machine by id if provided, otherwise by bmc_mac_address.
pub async fn find(
    txn: impl DbReader<'_>,
    req: &ExpectedMachineRequest,
) -> DatabaseResult<Option<ExpectedMachine>> {
    if let Some(id) = req.id {
        find_by_id(txn, id).await
    } else if let Some(mac) = req.bmc_mac_address {
        find_by_bmc_mac_address(txn, mac).await
    } else {
        Err(DatabaseError::InvalidArgument(
            "either id or bmc_mac_address must be provided".into(),
        ))
    }
}

/// Returns the selected expected machine while locking it for the current
/// transaction.
///
/// Acquire the table lock an eventual update needs before taking the row lock.
/// Replace-all takes a stronger writer lock, so this order prevents each path
/// from holding one lock while waiting to upgrade to the other.
pub async fn find_for_update(
    txn: &mut PgConnection,
    req: &ExpectedMachineRequest,
) -> DatabaseResult<Option<ExpectedMachine>> {
    // Resolve the selector before taking a table lock so an invalid request
    // cannot delay a real writer.
    enum Selector {
        Id(Uuid),
        BmcMacAddress(MacAddress),
    }
    let selector = if let Some(id) = req.id {
        Selector::Id(id)
    } else if let Some(mac_address) = req.bmc_mac_address {
        Selector::BmcMacAddress(mac_address)
    } else {
        return Err(DatabaseError::InvalidArgument(
            "either id or bmc_mac_address must be provided".into(),
        ));
    };

    let lock = "LOCK TABLE expected_machines IN ROW EXCLUSIVE MODE";
    sqlx::query(lock)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(lock, error))?;

    match selector {
        Selector::Id(id) => {
            let query = "SELECT * FROM expected_machines WHERE id=$1 FOR UPDATE";
            sqlx::query_as(query)
                .bind(id)
                .fetch_optional(txn)
                .await
                .map_err(|err| DatabaseError::query(query, err))
        }
        Selector::BmcMacAddress(mac_address) => {
            let query = "SELECT * FROM expected_machines WHERE bmc_mac_address=$1 FOR UPDATE";
            sqlx::query_as(query)
                .bind(mac_address)
                .fetch_optional(txn)
                .await
                .map_err(|err| DatabaseError::query(query, err))
        }
    }
}

/// delete deletes an expected machine by id if provided, otherwise by bmc_mac_address.
pub async fn delete(txn: &mut PgConnection, req: &ExpectedMachineRequest) -> DatabaseResult<()> {
    if let Some(id) = req.id {
        delete_by_id(txn, id).await
    } else if let Some(mac) = req.bmc_mac_address {
        delete_by_mac(txn, mac).await
    } else {
        Err(DatabaseError::InvalidArgument(
            "either id or bmc_mac_address must be provided".into(),
        ))
    }
}

/// delete_by_mac deletes an expected machine by bmc_mac_address.
pub async fn delete_by_mac(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_machines WHERE bmc_mac_address=$1";

    let result = sqlx::query(query)
        .bind(bmc_mac_address)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_machine",
            id: bmc_mac_address.to_string(),
        });
    }

    Ok(())
}

pub async fn delete_by_id(txn: &mut PgConnection, id: Uuid) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_machines WHERE id=$1";

    let result = sqlx::query(query)
        .bind(id)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_machine",
            id: id.to_string(),
        });
    }

    Ok(())
}

pub async fn clear(txn: &mut PgConnection) -> Result<(), DatabaseError> {
    let query = "DELETE FROM expected_machines";

    sqlx::query(query)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// Updates an existing expected machine. If id is set, matches by ID; otherwise matches by
/// `bmc_mac_address`. Includes `bmc_ip_address` when the operator configures a static BMC IP.
pub async fn update(txn: &mut PgConnection, machine: &ExpectedMachine) -> DatabaseResult<()> {
    macro_rules! update_expected_machine_query {
        ($where_clause:literal) => {
            concat!(
                "UPDATE expected_machines \
                 SET bmc_username=$1, bmc_password=$2, serial_number=$3, \
                     fallback_dpu_serial_numbers=$4, metadata_name=$5, metadata_description=$6, \
                     metadata_labels=$7, sku_id=$8, host_nics=$9::jsonb, rack_id=$10, \
                     default_pause_ingestion_and_poweron=COALESCE($11, default_pause_ingestion_and_poweron), \
                     dpf_enabled=COALESCE($12, dpf_enabled), \
                     bmc_ip_address=$13, \
                     bmc_retain_credentials=COALESCE($14, bmc_retain_credentials), \
                     dpu_mode=$15, \
                     bmc_ip_allocation=$16, \
                     host_lifecycle_profile=COALESCE($17, host_lifecycle_profile) \
                 WHERE ",
                $where_clause,
            )
        };
    }

    let (query, target_id) = match machine.id {
        Some(id) => (
            update_expected_machine_query!("id=$18::uuid"),
            id.to_string(),
        ),
        None => (
            update_expected_machine_query!("bmc_mac_address=$18::macaddr"),
            machine.bmc_mac_address.to_string(),
        ),
    };

    let result = sqlx::query(query)
        .bind(&machine.data.bmc_username)
        .bind(&machine.data.bmc_password)
        .bind(&machine.data.serial_number)
        .bind(&machine.data.fallback_dpu_serial_numbers)
        .bind(&machine.data.metadata.name)
        .bind(&machine.data.metadata.description)
        .bind(sqlx::types::Json(&machine.data.metadata.labels))
        .bind(&machine.data.sku_id)
        .bind(sqlx::types::Json(&machine.data.interfaces))
        .bind(&machine.data.rack_id)
        .bind(machine.data.default_pause_ingestion_and_poweron)
        .bind(machine.data.dpf_enabled)
        .bind(machine.data.bmc_ip_address)
        .bind(machine.data.bmc_retain_credentials)
        .bind(machine.data.dpu_policy)
        .bind(machine.data.bmc_ip_allocation)
        .bind(
            (!machine.data.host_lifecycle_profile.is_empty())
                .then_some(sqlx::types::Json(&machine.data.host_lifecycle_profile)),
        )
        .bind(&target_id)
        .execute(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_machine",
            id: target_id,
        });
    }
    Ok(())
}
