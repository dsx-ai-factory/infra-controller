/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Persistence and membership admission for expected NVLink domains.

use carbide_uuid::rack::{RackGroupId, RackId};
use mac_address::MacAddress;
use model::expected_nvlink_domain::ExpectedNvLinkDomain;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// Validate expected inventory and atomically claim members in the caller's transaction.
/// The caller must roll back on failure, including a concurrent membership conflict.
pub async fn create(txn: &mut PgConnection, domain: &ExpectedNvLinkDomain) -> DatabaseResult<()> {
    // Hold the group stable while checking its racks and inserting the declaration.
    let query = "SELECT racks FROM expected_rack_groups WHERE rack_group_id=$1 FOR SHARE";
    let racks: Option<sqlx::types::Json<Vec<model::expected_rack_group::ExpectedRackGroupRack>>> =
        sqlx::query_scalar(query)
            .bind(&domain.rack_group_id)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
    let racks = racks.ok_or_else(|| DatabaseError::NotFoundError {
        kind: "expected_rack_group",
        id: domain.rack_group_id.to_string(),
    })?;

    let query = "SELECT bmc_mac_address, rack_id FROM expected_machines WHERE bmc_mac_address = ANY($1) FOR SHARE";
    let compute: Vec<(MacAddress, Option<RackId>)> = sqlx::query_as(query)
        .bind(&domain.member_bmc_mac_addresses)
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    let query = "SELECT bmc_mac_address, rack_id FROM expected_switches WHERE bmc_mac_address = ANY($1) FOR SHARE";
    let switches: Vec<(MacAddress, Option<RackId>)> = sqlx::query_as(query)
        .bind(&domain.member_bmc_mac_addresses)
        .fetch_all(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    for mac in &domain.member_bmc_mac_addresses {
        let matches: Vec<_> = compute
            .iter()
            .chain(&switches)
            .filter(|(id, _)| id == mac)
            .collect();
        let [(_, Some(rack_id))] = matches.as_slice() else {
            return Err(DatabaseError::InvalidArgument(format!(
                "member {mac} must identify exactly one expected compute or switch tray with a rack"
            )));
        };
        if !racks.0.iter().any(|rack| &rack.rack_id == rack_id) {
            return Err(DatabaseError::InvalidArgument(format!(
                "member {mac} is not in rack group {}",
                domain.rack_group_id
            )));
        }
    }
    if compute.is_empty() || switches.is_empty() {
        return Err(DatabaseError::InvalidArgument(
            "a domain requires both compute and switch trays".into(),
        ));
    }

    let query = "INSERT INTO expected_nvlink_domains (id, rack_group_id, name) VALUES ($1, $2, $3)";
    sqlx::query(query)
        .bind(domain.id)
        .bind(&domain.rack_group_id)
        .bind(&domain.name)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    // Sorted insertion order prevents deadlocks between overlapping requests.
    let query = "INSERT INTO expected_nvlink_domain_members (domain_id, bmc_mac_address) SELECT $1, mac FROM unnest($2::macaddr[]) AS mac ORDER BY mac";
    sqlx::query(query)
        .bind(domain.id)
        .bind(&domain.member_bmc_mac_addresses)
        .execute(txn)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|e| e.constraint() == Some("expected_nvlink_domain_members_pkey"))
            {
                DatabaseError::AlreadyFoundError {
                    kind: "expected NVLink domain member",
                    id: "one or more BMC MAC addresses are already claimed".into(),
                }
            } else {
                DatabaseError::query(query, e)
            }
        })?;
    Ok(())
}

/// List declaration IDs in UUID order, optionally restricted to one rack group.
pub async fn find_ids(
    txn: impl DbReader<'_>,
    rack_group_id: Option<&RackGroupId>,
) -> DatabaseResult<Vec<Uuid>> {
    let query = "SELECT id FROM expected_nvlink_domains WHERE ($1::varchar IS NULL OR rack_group_id=$1) ORDER BY id";
    sqlx::query_scalar(query)
        .bind(rack_group_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Load declarations in UUID order with membership from the same database snapshot.
pub async fn find_by_ids(
    txn: impl DbReader<'_>,
    ids: &[Uuid],
) -> DatabaseResult<Vec<ExpectedNvLinkDomain>> {
    let query = "SELECT d.id, d.rack_group_id, d.name, ARRAY(SELECT m.bmc_mac_address FROM expected_nvlink_domain_members m WHERE m.domain_id=d.id ORDER BY m.bmc_mac_address) AS member_bmc_mac_addresses FROM expected_nvlink_domains d WHERE d.id = ANY($1) ORDER BY d.id";
    sqlx::query_as(query)
        .bind(ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Delete a declaration and its membership claims, without touching hardware or observed domains.
pub async fn delete(txn: &mut PgConnection, id: Uuid) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_nvlink_domains WHERE id=$1";
    let result = sqlx::query(query)
        .bind(id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_nvlink_domain",
            id: id.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
