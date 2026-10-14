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

//! CRUD for `expected_switch_nvos_interfaces`, the authoritative source of a
//! switch's NVOS ethernet ports and which one NICo treats as primary.

use mac_address::MacAddress;
use model::expected_switch::ExpectedNvosInterface;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

/// Replace the NVOS port rows for one switch with `interfaces`.
///
/// A non-empty set from an operator must already be validated by
/// [`model::expected_switch::normalize_nvos_interfaces`] (at least one port,
/// exactly one primary). An empty set is permitted for legacy zero-port records
/// and replace-all compatibility.
pub(super) async fn replace_for_switch(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
    interfaces: &[ExpectedNvosInterface],
) -> DatabaseResult<()> {
    delete_by_switch_bmc(&mut *txn, bmc_mac_address).await?;
    for interface in interfaces {
        insert(&mut *txn, bmc_mac_address, interface).await?;
    }
    Ok(())
}

/// Return every NVOS port of a switch, primary first.
pub(super) async fn find_by_switch_bmc(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
) -> DatabaseResult<Vec<ExpectedNvosInterface>> {
    let query = "SELECT mac_address, is_primary
         FROM expected_switch_nvos_interfaces
         WHERE bmc_mac_address = $1
         ORDER BY is_primary DESC, mac_address";
    sqlx::query_as(query)
        .bind(bmc_mac_address)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))
}

/// Return the switch BMC MAC that owns `nvos_mac_address`, if any.
pub(super) async fn find_switch_bmc_by_nvos_mac(
    txn: &mut PgConnection,
    nvos_mac_address: MacAddress,
) -> DatabaseResult<Option<MacAddress>> {
    let query = "SELECT bmc_mac_address
         FROM expected_switch_nvos_interfaces
         WHERE mac_address = $1";
    sqlx::query_scalar(query)
        .bind(nvos_mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))
}

/// Return the first MAC in `candidates` that a switch other than
/// `exclude_bmc_mac_address` already claims, if any.
pub(super) async fn find_mac_claimed_elsewhere(
    txn: &mut PgConnection,
    candidates: &[MacAddress],
    exclude_bmc_mac_address: MacAddress,
) -> DatabaseResult<Option<MacAddress>> {
    if candidates.is_empty() {
        return Ok(None);
    }

    let query = "SELECT mac_address
         FROM expected_switch_nvos_interfaces
         WHERE mac_address = ANY($1) AND bmc_mac_address != $2
         LIMIT 1";
    sqlx::query_scalar(query)
        .bind(candidates)
        .bind(exclude_bmc_mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))
}

async fn insert(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
    interface: &ExpectedNvosInterface,
) -> DatabaseResult<()> {
    let query =
        "INSERT INTO expected_switch_nvos_interfaces (mac_address, bmc_mac_address, is_primary)
         VALUES ($1, $2, $3)";
    sqlx::query(query)
        .bind(interface.mac_address)
        .bind(bmc_mac_address)
        .bind(interface.is_primary)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(query, err))
}

async fn delete_by_switch_bmc(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_switch_nvos_interfaces WHERE bmc_mac_address = $1";
    sqlx::query(query)
        .bind(bmc_mac_address)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(query, err))
}
