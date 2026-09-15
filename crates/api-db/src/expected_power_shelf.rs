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

use carbide_uuid::rack::RackId;
use itertools::Itertools;
use mac_address::MacAddress;
use model::expected_power_shelf::{
    ExpectedPowerShelf, ExpectedPowerShelfRequest, LinkedExpectedPowerShelf,
};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{DatabaseError, DatabaseResult};

const SQL_VIOLATION_DUPLICATE_MAC: &str = "expected_power_shelves_bmc_mac_address_key";

pub async fn find_by_bmc_mac_address(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
) -> DatabaseResult<Option<ExpectedPowerShelf>> {
    let sql = "SELECT * FROM expected_power_shelves WHERE bmc_mac_address=$1";
    sqlx::query_as(sql)
        .bind(bmc_mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_by_id(
    txn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<ExpectedPowerShelf>, DatabaseError> {
    let sql = "SELECT * FROM expected_power_shelves WHERE expected_power_shelf_id=$1";
    sqlx::query_as(sql)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// `find_by_id_for_update` holds the selected shelf until its patch commits.
pub async fn find_by_id_for_update(
    txn: &mut PgConnection,
    expected_power_shelf_id: Uuid,
) -> DatabaseResult<Option<ExpectedPowerShelf>> {
    let query = "SELECT * FROM expected_power_shelves WHERE expected_power_shelf_id=$1 FOR UPDATE";
    sqlx::query_as(query)
        .bind(expected_power_shelf_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

pub async fn find_many_by_bmc_mac_address(
    txn: &mut PgConnection,
    bmc_mac_addresses: &[MacAddress],
) -> DatabaseResult<HashMap<MacAddress, ExpectedPowerShelf>> {
    let sql = "SELECT * FROM expected_power_shelves WHERE bmc_mac_address=ANY($1)";
    let v: Vec<ExpectedPowerShelf> = sqlx::query_as(sql)
        .bind(bmc_mac_addresses)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;

    // expected_power_shelves has a unique constraint on bmc_mac_address,
    // but if the constraint gets dropped and we have multiple mac addresses,
    // we want this code to generate an Err and not silently drop values
    // and/or return nothing.
    v.into_iter()
        .into_group_map_by(|exp| exp.bmc_mac_address)
        .drain()
        .map(|(k, mut v)| {
            if v.len() > 1 {
                Err(DatabaseError::AlreadyFoundError {
                    kind: "ExpectedPowerShelf",
                    id: k.to_string(),
                })
            } else {
                Ok((k, v.pop().unwrap()))
            }
        })
        .collect()
}

pub async fn find_all(txn: &mut PgConnection) -> DatabaseResult<Vec<ExpectedPowerShelf>> {
    let sql = "SELECT * FROM expected_power_shelves";
    sqlx::query_as(sql)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// find_all_by_rack_id returns all expected power shelves for a given rack_id.
pub async fn find_all_by_rack_id(
    txn: &mut PgConnection,
    rack_id: &RackId,
) -> DatabaseResult<Vec<ExpectedPowerShelf>> {
    let sql = "SELECT * FROM expected_power_shelves WHERE rack_id=$1";
    sqlx::query_as(sql)
        .bind(rack_id)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

pub async fn find_all_linked(
    txn: &mut PgConnection,
) -> DatabaseResult<Vec<LinkedExpectedPowerShelf>> {
    let sql = r#"
 SELECT
 eps.serial_number,
 eps.bmc_mac_address,
 ps.id AS power_shelf_id,
 eps.expected_power_shelf_id,
 ee.address AS address,
 eps.rack_id
FROM expected_power_shelves eps
 LEFT JOIN power_shelves ps ON eps.serial_number = ps.config->>'name'
 LEFT JOIN machine_interfaces mi ON eps.bmc_mac_address = mi.mac_address
 LEFT JOIN machine_interface_addresses mia ON mi.id = mia.interface_id
 LEFT JOIN explored_endpoints ee ON mia.address = ee.address
 ORDER BY eps.bmc_mac_address
 "#;
    sqlx::query_as(sql)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// create inserts a new expected power shelf record. If the id field is None,
/// a new UUID is generated.
pub async fn create(
    txn: &mut PgConnection,
    power_shelf: ExpectedPowerShelf,
) -> DatabaseResult<ExpectedPowerShelf> {
    let id = power_shelf
        .expected_power_shelf_id
        .unwrap_or_else(Uuid::new_v4);
    let query = "INSERT INTO expected_power_shelves
            (expected_power_shelf_id, bmc_mac_address, bmc_username, bmc_password, serial_number, bmc_ip_address, metadata_name, metadata_description, metadata_labels, rack_id, bmc_retain_credentials)
            VALUES
            ($1::uuid, $2::macaddr, $3::varchar, $4::varchar, $5::varchar, $6::inet, $7, $8, $9::jsonb, $10, $11) RETURNING *";

    sqlx::query_as(query)
        .bind(id)
        .bind(power_shelf.bmc_mac_address)
        .bind(&power_shelf.bmc_username)
        .bind(&power_shelf.bmc_password)
        .bind(&power_shelf.serial_number)
        .bind(power_shelf.bmc_ip_address)
        .bind(&power_shelf.metadata.name)
        .bind(&power_shelf.metadata.description)
        .bind(sqlx::types::Json(&power_shelf.metadata.labels))
        .bind(&power_shelf.rack_id)
        .bind(power_shelf.bmc_retain_credentials.unwrap_or(false))
        .fetch_one(txn)
        .await
        .map_err(|err: sqlx::Error| match err {
            sqlx::Error::Database(e) if e.constraint() == Some(SQL_VIOLATION_DUPLICATE_MAC) => {
                DatabaseError::ExpectedHostDuplicateMacAddress(power_shelf.bmc_mac_address)
            }
            _ => DatabaseError::query(query, err),
        })
}

/// find returns an expected power shelf by expected_power_shelf_id if
/// provided, otherwise by bmc_mac_address.
pub async fn find(
    txn: &mut PgConnection,
    req: &ExpectedPowerShelfRequest,
) -> DatabaseResult<Option<ExpectedPowerShelf>> {
    if let Some(id) = req.expected_power_shelf_id {
        find_by_id(txn, id).await
    } else if let Some(mac) = req.bmc_mac_address {
        find_by_bmc_mac_address(txn, mac).await
    } else {
        Err(DatabaseError::InvalidArgument(
            "either expected_power_shelf_id or bmc_mac_address must be provided".into(),
        ))
    }
}

/// delete deletes an expected power shelf by expected_power_shelf_id if
/// provided, otherwise by bmc_mac_address.
pub async fn delete(txn: &mut PgConnection, req: &ExpectedPowerShelfRequest) -> DatabaseResult<()> {
    if let Some(id) = req.expected_power_shelf_id {
        delete_by_id(txn, id).await
    } else if let Some(mac) = req.bmc_mac_address {
        delete_by_mac(txn, mac).await
    } else {
        Err(DatabaseError::InvalidArgument(
            "either expected_power_shelf_id or bmc_mac_address must be provided".into(),
        ))
    }
}

/// delete_by_mac deletes an expected power shelf by bmc_mac_address.
pub async fn delete_by_mac(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_power_shelves WHERE bmc_mac_address=$1";
    let result = sqlx::query(query)
        .bind(bmc_mac_address)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_power_shelf",
            id: bmc_mac_address.to_string(),
        });
    }
    Ok(())
}

/// delete_by_id deletes an expected power shelf by expected_power_shelf_id.
pub async fn delete_by_id(txn: &mut PgConnection, id: Uuid) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_power_shelves WHERE expected_power_shelf_id=$1";
    let result = sqlx::query(query)
        .bind(id)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_power_shelf",
            id: id.to_string(),
        });
    }
    Ok(())
}

pub async fn clear(txn: &mut PgConnection) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_power_shelves";

    sqlx::query(query)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(query, err))
}

/// update updates an existing expected power shelf. If expected_power_shelf_id
/// is set, matches by ID; otherwise matches by bmc_mac_address.
pub async fn update(
    txn: &mut PgConnection,
    power_shelf: &ExpectedPowerShelf,
) -> DatabaseResult<()> {
    macro_rules! update_expected_power_shelf_query {
        ($where_clause:literal) => {
            concat!(
                "UPDATE expected_power_shelves \
                 SET bmc_username=$1, bmc_password=$2, serial_number=$3, bmc_ip_address=$4, \
                     metadata_name=$5, metadata_description=$6, metadata_labels=$7, rack_id=$8, \
                     bmc_retain_credentials=COALESCE($9, bmc_retain_credentials) \
                 WHERE ",
                $where_clause,
            )
        };
    }

    let (query, target_id) = match power_shelf.expected_power_shelf_id {
        Some(id) => (
            update_expected_power_shelf_query!("expected_power_shelf_id=$10::uuid"),
            id.to_string(),
        ),
        None => (
            update_expected_power_shelf_query!("bmc_mac_address=$10::macaddr"),
            power_shelf.bmc_mac_address.to_string(),
        ),
    };

    let result = sqlx::query(query)
        .bind(&power_shelf.bmc_username)
        .bind(&power_shelf.bmc_password)
        .bind(&power_shelf.serial_number)
        .bind(power_shelf.bmc_ip_address)
        .bind(&power_shelf.metadata.name)
        .bind(&power_shelf.metadata.description)
        .bind(sqlx::types::Json(&power_shelf.metadata.labels))
        .bind(&power_shelf.rack_id)
        .bind(power_shelf.bmc_retain_credentials)
        .bind(&target_id)
        .execute(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_power_shelf",
            id: target_id,
        });
    }
    Ok(())
}

/// fn will insert rows that are not currently present in DB for each expected_power_shelf arg in list,
/// but will NOT overwrite existing rows matching by MAC addr.
pub async fn create_missing_from(
    txn: &mut PgConnection,
    expected_power_shelves: &[ExpectedPowerShelf],
) -> DatabaseResult<()> {
    let existing_power_shelves = find_all(txn).await?;
    let existing_map: BTreeMap<String, ExpectedPowerShelf> = existing_power_shelves
        .into_iter()
        .map(|power_shelf| (power_shelf.bmc_mac_address.to_string(), power_shelf))
        .collect();

    for expected_power_shelf in expected_power_shelves {
        if existing_map.contains_key(&expected_power_shelf.bmc_mac_address.to_string()) {
            tracing::debug!(
                bmc_mac_address = %expected_power_shelf.bmc_mac_address,
                "Expected power shelf already exists; not overwriting",
            );
            continue;
        }

        create(txn, expected_power_shelf.clone()).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests;
