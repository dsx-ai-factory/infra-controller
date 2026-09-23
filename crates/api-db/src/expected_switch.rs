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

use std::collections::{BTreeMap, HashMap};

use carbide_uuid::rack::{RackId, RackProfileId};
use itertools::Itertools;
use mac_address::MacAddress;
use model::expected_switch::{ExpectedSwitch, ExpectedSwitchRequest, LinkedExpectedSwitch};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{DatabaseError, DatabaseResult};

const SQL_VIOLATION_DUPLICATE_MAC: &str = "expected_switches_bmc_mac_address_key";
pub async fn find_by_bmc_mac_address(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
) -> Result<Option<ExpectedSwitch>, DatabaseError> {
    let sql = "SELECT * FROM expected_switches WHERE bmc_mac_address=$1";
    sqlx::query_as(sql)
        .bind(bmc_mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// Find by an NVOS MAC. `nvos_mac_addresses` is a `macaddr[]` column, so we match
/// rows where the given MAC is contained in that array. Returns at most one row
/// because the discover hook expects a 1:1 lookup.
pub async fn find_by_nvos_mac_address(
    txn: &mut PgConnection,
    nvos_mac_address: MacAddress,
) -> Result<Option<ExpectedSwitch>, DatabaseError> {
    let sql = "SELECT * FROM expected_switches WHERE $1::macaddr = ANY(nvos_mac_addresses)";
    sqlx::query_as(sql)
        .bind(nvos_mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// Serialize expected-switch writes on a transaction-scoped advisory lock so
/// two concurrent creates or updates can't both pass the NVOS MAC conflict
/// check before either row lands (check-then-write under READ COMMITTED).
/// The lock releases with the transaction; the namespaced key keeps it from
/// colliding with other subsystems' advisory locks.
async fn lock_expected_switch_writes(txn: &mut PgConnection) -> DatabaseResult<()> {
    let sql = "SELECT pg_advisory_xact_lock(hashtextextended('expected_switches:write', 0))";
    sqlx::query(sql)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(sql, err))
}

/// Return an entry of `nvos_mac_addresses` that a different expected switch
/// already claims, if any. "Different" follows the same key `update` targets:
/// `switch.expected_switch_id` when set, otherwise `switch.bmc_mac_address`.
/// `macaddr` comparison canonicalizes case and separator differences.
/// `update_nvos_mac_addresses` stays unguarded on purpose -- it records
/// hardware-observed truth from site-explorer.
async fn find_nvos_mac_claimed_elsewhere(
    txn: &mut PgConnection,
    nvos_mac_addresses: &[MacAddress],
    switch: &ExpectedSwitch,
) -> DatabaseResult<Option<MacAddress>> {
    if nvos_mac_addresses.is_empty() {
        return Ok(None);
    }

    let (sql, exclude_key) = match switch.expected_switch_id {
        Some(id) => (
            "SELECT * FROM expected_switches WHERE expected_switch_id != $1::uuid AND nvos_mac_addresses && $2::macaddr[] LIMIT 1",
            id.to_string(),
        ),
        None => (
            "SELECT * FROM expected_switches WHERE bmc_mac_address != $1::macaddr AND nvos_mac_addresses && $2::macaddr[] LIMIT 1",
            switch.bmc_mac_address.to_string(),
        ),
    };

    let other: Option<ExpectedSwitch> = sqlx::query_as(sql)
        .bind(exclude_key)
        .bind(nvos_mac_addresses)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;

    Ok(other.map(|other| {
        nvos_mac_addresses
            .iter()
            .find(|mac| other.nvos_mac_addresses.contains(mac))
            .copied()
            // The SQL overlap guarantees a shared entry; the first requested
            // MAC is a safe stand-in if equality ever disagrees.
            .unwrap_or(nvos_mac_addresses[0])
    }))
}

pub async fn find_by_serial_number(
    txn: &mut PgConnection,
    serial_number: &str,
) -> Result<Option<ExpectedSwitch>, DatabaseError> {
    let sql = "SELECT * FROM expected_switches WHERE serial_number=$1";
    sqlx::query_as(sql)
        .bind(serial_number)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_by_id(
    txn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<ExpectedSwitch>, DatabaseError> {
    let sql = "SELECT * FROM expected_switches WHERE expected_switch_id=$1";
    sqlx::query_as(sql)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_by_rack_id(
    txn: &mut PgConnection,
    rack_id: String,
) -> Result<Option<ExpectedSwitch>, DatabaseError> {
    let sql = "SELECT * FROM expected_switches WHERE rack_id=$1";
    sqlx::query_as(sql)
        .bind(rack_id)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_many_by_bmc_mac_address(
    txn: &mut PgConnection,
    bmc_mac_addresses: &[MacAddress],
) -> DatabaseResult<HashMap<MacAddress, ExpectedSwitch>> {
    let sql = "SELECT * FROM expected_switches WHERE bmc_mac_address=ANY($1)";
    let v: Vec<ExpectedSwitch> = sqlx::query_as(sql)
        .bind(bmc_mac_addresses)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;

    // expected_switches has a unique constraint on bmc_mac_address,
    // but if the constraint gets dropped and we have multiple mac addresses,
    // we want this code to generate an Err and not silently drop values
    // and/or return nothing.
    v.into_iter()
        .into_group_map_by(|exp| exp.bmc_mac_address)
        .drain()
        .map(|(k, mut v)| {
            if v.len() > 1 {
                Err(DatabaseError::AlreadyFoundError {
                    kind: "ExpectedSwitch",
                    id: k.to_string(),
                })
            } else {
                Ok((k, v.pop().unwrap()))
            }
        })
        .collect()
}

pub async fn find_all(txn: &mut PgConnection) -> DatabaseResult<Vec<ExpectedSwitch>> {
    let sql = "SELECT * FROM expected_switches";
    sqlx::query_as(sql)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// find_all_by_rack_id returns all expected switches for a given rack_id.
pub async fn find_all_by_rack_id(
    txn: &mut PgConnection,
    rack_id: &RackId,
) -> DatabaseResult<Vec<ExpectedSwitch>> {
    let sql = "SELECT * FROM expected_switches WHERE rack_id=$1";
    sqlx::query_as(sql)
        .bind(rack_id)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_all_linked(txn: &mut PgConnection) -> DatabaseResult<Vec<LinkedExpectedSwitch>> {
    let sql = r#"
  SELECT
  es.serial_number,
  es.bmc_mac_address,
  s.id AS switch_id,
  es.expected_switch_id,
  ee.address AS address,
  es.rack_id
 FROM expected_switches es
  LEFT JOIN switches s ON es.bmc_mac_address = s.bmc_mac_address
  LEFT JOIN machine_interfaces mi ON es.bmc_mac_address = mi.mac_address
  LEFT JOIN machine_interface_addresses mia ON mi.id = mia.interface_id
  LEFT JOIN explored_endpoints ee ON mia.address = ee.address
  ORDER BY es.bmc_mac_address
  "#;
    sqlx::query_as(sql)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_one_linked(
    txn: &mut PgConnection,
) -> DatabaseResult<Option<LinkedExpectedSwitch>> {
    let sql = r#"
  SELECT
  es.serial_number,
  es.bmc_mac_address,
  s.id AS switch_id,
  es.expected_switch_id,
  linked_endpoint.address AS address,
  es.rack_id
 FROM expected_switches es
  LEFT JOIN switches s ON es.bmc_mac_address = s.bmc_mac_address
  LEFT JOIN LATERAL (
   SELECT ee.address
   FROM machine_interfaces mi
    JOIN machine_interface_addresses mia ON mi.id = mia.interface_id
    JOIN explored_endpoints ee ON mia.address = ee.address
   WHERE mi.mac_address = es.bmc_mac_address
   -- Keep this single-row lookup deterministic if a MAC has multiple explored addresses.
   ORDER BY ee.address
   LIMIT 1
  ) linked_endpoint ON true
  ORDER BY es.bmc_mac_address
 LIMIT 1
 "#;
    sqlx::query_as(sql)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// create inserts a new expected switch record. If the id field is None,
/// a new UUID is generated.
pub async fn create(
    txn: &mut PgConnection,
    switch: ExpectedSwitch,
) -> DatabaseResult<ExpectedSwitch> {
    // NVOS MACs resolve a DHCPing management port to a single expected switch
    // (`find_by_nvos_mac_address`), so a MAC claimed by another switch is a
    // conflict. The advisory lock makes the check-then-insert deterministic
    // under concurrent writers.
    lock_expected_switch_writes(&mut *txn).await?;
    if let Some(mac) =
        find_nvos_mac_claimed_elsewhere(&mut *txn, &switch.nvos_mac_addresses, &switch).await?
    {
        return Err(DatabaseError::ExpectedSwitchDuplicateNvosMacAddress(mac));
    }

    let id = switch.expected_switch_id.unwrap_or_else(Uuid::new_v4);
    let query = "INSERT INTO expected_switches
             (expected_switch_id, bmc_mac_address, bmc_username, bmc_password, serial_number, bmc_ip_address, metadata_name, metadata_description, rack_id, metadata_labels, nvos_username, nvos_password, nvos_mac_addresses, bmc_retain_credentials, nvos_ip_address)
             VALUES
             ($1::uuid, $2::macaddr, $3::varchar, $4::varchar, $5::varchar, $6::inet, $7::varchar, $8::varchar, $9::varchar, $10::jsonb, $11::varchar, $12::varchar, $13::macaddr[], $14, $15::inet) RETURNING *";

    sqlx::query_as(query)
        .bind(id)
        .bind(switch.bmc_mac_address)
        .bind(&switch.bmc_username)
        .bind(&switch.bmc_password)
        .bind(&switch.serial_number)
        .bind(switch.bmc_ip_address)
        .bind(&switch.metadata.name)
        .bind(&switch.metadata.description)
        .bind(&switch.rack_id)
        .bind(sqlx::types::Json(&switch.metadata.labels))
        .bind(&switch.nvos_username)
        .bind(&switch.nvos_password)
        .bind(&switch.nvos_mac_addresses)
        .bind(switch.bmc_retain_credentials.unwrap_or(false))
        .bind(switch.nvos_ip_address)
        .fetch_one(txn)
        .await
        .map_err(|err: sqlx::Error| match err {
            sqlx::Error::Database(e) if e.constraint() == Some(SQL_VIOLATION_DUPLICATE_MAC) => {
                DatabaseError::ExpectedHostDuplicateMacAddress(switch.bmc_mac_address)
            }
            _ => DatabaseError::query(query, err),
        })
}

/// find returns an expected switch by expected_switch_id if provided,
/// otherwise by bmc_mac_address.
pub async fn find(
    txn: &mut PgConnection,
    req: &ExpectedSwitchRequest,
) -> DatabaseResult<Option<ExpectedSwitch>> {
    if let Some(id) = req.expected_switch_id {
        find_by_id(txn, id).await
    } else if let Some(mac) = req.bmc_mac_address {
        find_by_bmc_mac_address(txn, mac).await
    } else {
        Err(DatabaseError::InvalidArgument(
            "either expected_switch_id or bmc_mac_address must be provided".into(),
        ))
    }
}

/// Locks the expected-switch write domain and returns the selected row for update.
pub async fn find_for_update(
    txn: &mut PgConnection,
    req: &ExpectedSwitchRequest,
) -> DatabaseResult<Option<ExpectedSwitch>> {
    lock_expected_switch_writes(&mut *txn).await?;

    let (query, key) = if let Some(id) = req.expected_switch_id {
        (
            "SELECT * FROM expected_switches WHERE expected_switch_id=$1::uuid FOR UPDATE",
            id.to_string(),
        )
    } else if let Some(mac) = req.bmc_mac_address {
        (
            "SELECT * FROM expected_switches WHERE bmc_mac_address=$1::macaddr FOR UPDATE",
            mac.to_string(),
        )
    } else {
        return Err(DatabaseError::InvalidArgument(
            "either expected_switch_id or bmc_mac_address must be provided".into(),
        ));
    };

    sqlx::query_as(query)
        .bind(key)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))
}

/// delete deletes an expected switch by expected_switch_id if provided,
/// otherwise by bmc_mac_address.
pub async fn delete(txn: &mut PgConnection, req: &ExpectedSwitchRequest) -> DatabaseResult<()> {
    if let Some(id) = req.expected_switch_id {
        delete_by_id(txn, id).await
    } else if let Some(mac) = req.bmc_mac_address {
        delete_by_mac(txn, mac).await
    } else {
        Err(DatabaseError::InvalidArgument(
            "either expected_switch_id or bmc_mac_address must be provided".into(),
        ))
    }
}

/// delete_by_mac deletes an expected switch by bmc_mac_address.
pub async fn delete_by_mac(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_switches WHERE bmc_mac_address=$1";
    let result = sqlx::query(query)
        .bind(bmc_mac_address)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_switch",
            id: bmc_mac_address.to_string(),
        });
    }
    Ok(())
}

/// delete_by_id deletes an expected switch by expected_switch_id.
pub async fn delete_by_id(txn: &mut PgConnection, id: Uuid) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_switches WHERE expected_switch_id=$1";
    let result = sqlx::query(query)
        .bind(id)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_switch",
            id: id.to_string(),
        });
    }
    Ok(())
}

pub async fn update_nvos_mac_addresses(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
    nvos_mac_addresses: &[MacAddress],
) -> DatabaseResult<()> {
    let query = "UPDATE expected_switches SET nvos_mac_addresses = $1 WHERE bmc_mac_address = $2";
    sqlx::query(query)
        .bind(nvos_mac_addresses)
        .bind(bmc_mac_address)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(query, err))
}

pub async fn clear(txn: &mut PgConnection) -> Result<(), DatabaseError> {
    // Take the write lock before the DELETE grabs row locks so clear-then-
    // create flows (`replace_all_expected_switches`) acquire locks in the same
    // order as `create`/`update` -- advisory first, rows second -- instead of
    // forming a deadlock cycle with them.
    lock_expected_switch_writes(&mut *txn).await?;

    let query = "DELETE FROM expected_switches";

    sqlx::query(query)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(query, err))
}

/// update updates an existing expected switch. If expected_switch_id is set,
/// matches by ID; otherwise matches by bmc_mac_address.
pub async fn update(txn: &mut PgConnection, switch: &ExpectedSwitch) -> DatabaseResult<()> {
    // The lock serializes the existence read, conflict check, and UPDATE as
    // one unit against concurrent expected-switch writers.
    lock_expected_switch_writes(&mut *txn).await?;

    // Resolve the target first: a missing switch reports NotFound rather than
    // a MAC conflict, and the current row bounds the conflict check to newly
    // claimed MACs -- re-asserting a list the row already holds stays valid
    // even when pre-existing data or site-explorer's hardware-truth writes
    // recorded the same MAC on two switches.
    let current = find(
        &mut *txn,
        &ExpectedSwitchRequest {
            expected_switch_id: switch.expected_switch_id,
            bmc_mac_address: Some(switch.bmc_mac_address),
        },
    )
    .await?
    .ok_or_else(|| DatabaseError::NotFoundError {
        kind: "expected_switch",
        id: switch
            .expected_switch_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| switch.bmc_mac_address.to_string()),
    })?;

    let newly_claimed: Vec<MacAddress> = switch
        .nvos_mac_addresses
        .iter()
        .filter(|mac| !current.nvos_mac_addresses.contains(mac))
        .copied()
        .collect();
    if let Some(mac) = find_nvos_mac_claimed_elsewhere(&mut *txn, &newly_claimed, switch).await? {
        return Err(DatabaseError::ExpectedSwitchDuplicateNvosMacAddress(mac));
    }

    macro_rules! update_expected_switch_query {
        ($where_clause:literal) => {
            concat!(
                "UPDATE expected_switches \
                 SET bmc_username=$1, bmc_password=$2, serial_number=$3, bmc_ip_address=$4, \
                     metadata_name=$5, metadata_description=$6, metadata_labels=$7, \
                     rack_id=$8, nvos_username=$9, nvos_password=$10, nvos_mac_addresses=$11::macaddr[], \
                     bmc_retain_credentials=COALESCE($12, bmc_retain_credentials), \
                     nvos_ip_address=$13::inet \
                 WHERE ",
                $where_clause,
            )
        };
    }

    let (query, target_id) = match switch.expected_switch_id {
        Some(id) => (
            update_expected_switch_query!("expected_switch_id=$14::uuid"),
            id.to_string(),
        ),
        None => (
            update_expected_switch_query!("bmc_mac_address=$14::macaddr"),
            switch.bmc_mac_address.to_string(),
        ),
    };

    let result = sqlx::query(query)
        .bind(&switch.bmc_username)
        .bind(&switch.bmc_password)
        .bind(&switch.serial_number)
        .bind(switch.bmc_ip_address)
        .bind(&switch.metadata.name)
        .bind(&switch.metadata.description)
        .bind(sqlx::types::Json(&switch.metadata.labels))
        .bind(&switch.rack_id)
        .bind(&switch.nvos_username)
        .bind(&switch.nvos_password)
        .bind(&switch.nvos_mac_addresses)
        .bind(switch.bmc_retain_credentials)
        .bind(switch.nvos_ip_address)
        .bind(&target_id)
        .execute(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_switch",
            id: target_id,
        });
    }
    Ok(())
}

/// fn will insert rows that are not currently present in DB for each expected_switch arg in list,
/// but will NOT overwrite existing rows matching by MAC addr.
pub async fn create_missing_from(
    txn: &mut PgConnection,
    expected_switches: &[ExpectedSwitch],
) -> DatabaseResult<()> {
    let existing_switches = find_all(txn).await?;
    let existing_map: BTreeMap<String, ExpectedSwitch> = existing_switches
        .into_iter()
        .map(|switch| (switch.bmc_mac_address.to_string(), switch))
        .collect();

    for expected_switch in expected_switches {
        if existing_map.contains_key(&expected_switch.bmc_mac_address.to_string()) {
            tracing::debug!(
                bmc_mac_address = %expected_switch.bmc_mac_address,
                "Expected switch already exists; not overwriting",
            );
            continue;
        }

        create(txn, expected_switch.clone()).await?;
    }

    Ok(())
}

/// RMS rack identity for a switch that does not yet have a `switches` row,
/// resolved from the expected inventory. Every switch is rack-scale
/// (RMS-managed), so this serves the pre-ingestion firmware and power paths for
/// any switch.
#[derive(Debug, sqlx::FromRow)]
pub struct PreIngestionSwitchRmsIdentity {
    pub bmc_mac_address: MacAddress,
    pub rack_id: RackId,
    pub rack_profile_id: Option<RackProfileId>,
}

/// Resolve RMS rack identities for pre-ingestion switches by BMC MAC.
///
/// Every switch is rack-scale (RMS-managed), so its expected record is expected
/// to declare a `rack_id`; that rack is required to build the RMS node
/// descriptor. The rack profile is taken from the live `racks` row when it
/// exists and otherwise from the `expected_racks` declaration, so the descriptor
/// resolves before the rack row is created. Rows missing a `rack_id` are a
/// misconfiguration and are omitted (they cannot resolve an RMS identity).
/// Mirrors `expected_machine::find_rms_identities_by_bmc_macs`.
pub async fn find_rms_identities_by_bmc_macs(
    db: impl crate::db_read::DbReader<'_>,
    bmc_macs: &[MacAddress],
) -> DatabaseResult<Vec<PreIngestionSwitchRmsIdentity>> {
    let sql = r#"
        SELECT
            es.bmc_mac_address AS bmc_mac_address,
            es.rack_id AS rack_id,
            COALESCE(r.rack_profile_id, er.rack_profile_id) AS rack_profile_id
        FROM expected_switches es
        LEFT JOIN racks r ON r.id = es.rack_id
        LEFT JOIN expected_racks er ON er.rack_id = es.rack_id
        WHERE es.bmc_mac_address = ANY($1)
          AND es.rack_id IS NOT NULL
    "#;

    sqlx::query_as(sql)
        .bind(bmc_macs)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::new("expected_switch::find_rms_identities_by_bmc_macs", err))
}

#[cfg(test)]
mod tests;
