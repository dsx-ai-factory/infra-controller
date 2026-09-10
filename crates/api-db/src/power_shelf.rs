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

use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackProfileId;
use chrono::prelude::*;
use config_version::{ConfigVersion, Versioned};
use health_report::{HealthReport, HealthReportApplyMode};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::metadata::Metadata;
use model::power_shelf::{
    NewPowerShelf, PowerShelf, PowerShelfControllerState, PowerShelfMaintenanceOperation,
    PowerShelfMaintenanceRequest, PowerShelfReprovisionRequest,
};
use model::rack::{MaintenanceActivity, RackFirmwareUpgradeStatus};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{
    ColumnInfo, DatabaseError, DatabaseResult, FilterableQueryBuilder, ObjectColumnFilter,
};

#[cfg(test)]
mod test_metadata;

#[derive(Debug, Clone, Default)]
pub struct PowerShelfSearchConfig {
    // pub include_history: bool, // unused
    pub controller_state: Option<String>,
    pub rack_id: Option<String>,
}

#[derive(Copy, Clone)]
pub struct IdColumn;
impl ColumnInfo<'_> for IdColumn {
    type TableType = PowerShelf;
    type ColumnType = PowerShelfId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

#[derive(Copy, Clone)]
pub struct NameColumn;
impl ColumnInfo<'_> for NameColumn {
    type TableType = PowerShelf;
    type ColumnType = String;

    fn column_name(&self) -> &'static str {
        "name"
    }
}

#[derive(Copy, Clone)]
pub struct BmcMacAddressColumn;
impl ColumnInfo<'_> for BmcMacAddressColumn {
    type TableType = PowerShelf;
    type ColumnType = mac_address::MacAddress;

    fn column_name(&self) -> &'static str {
        "bmc_mac_address"
    }
}

pub async fn create(
    txn: &mut PgConnection,
    new_power_shelf: &NewPowerShelf,
) -> Result<PowerShelf, DatabaseError> {
    let state = PowerShelfControllerState::Initializing;
    let controller_state_version = ConfigVersion::initial();
    let version = ConfigVersion::initial();

    let default_metadata = Metadata::default();
    let expected_metadata = new_power_shelf
        .metadata
        .as_ref()
        .unwrap_or(&default_metadata);
    let metadata_name = match expected_metadata.name.as_str() {
        "" => new_power_shelf.id.to_string(),
        name => name.to_string(),
    };
    let metadata = Metadata {
        name: metadata_name,
        description: expected_metadata.description.clone(),
        labels: expected_metadata.labels.clone(),
    };

    let query = sqlx::query_as::<_, PowerShelfId>(
        "INSERT INTO power_shelves (id, name, config, controller_state, controller_state_version, bmc_mac_address, description, labels, version, rack_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    );
    let _: PowerShelfId = query
        .bind(new_power_shelf.id)
        .bind(&metadata.name)
        .bind(sqlx::types::Json(&new_power_shelf.config))
        .bind(sqlx::types::Json(&state))
        .bind(controller_state_version)
        .bind(new_power_shelf.bmc_mac_address)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(version)
        .bind(&new_power_shelf.rack_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("create power_shelf", e))?;

    Ok(PowerShelf {
        id: new_power_shelf.id,
        config: new_power_shelf.config.clone(),
        status: None,
        deleted: None,
        bmc_mac_address: new_power_shelf.bmc_mac_address,
        bmc_credential_rotation_requested: false,
        decommission_requested: false,
        bmc_info: None,
        controller_state: Versioned {
            value: state,
            version: controller_state_version,
        },
        controller_state_outcome: None,
        metadata,
        version,
        rack_id: new_power_shelf.rack_id.clone(),
        power_shelf_maintenance_requested: None,
        power_shelf_reprovisioning_requested: None,
        firmware_upgrade_status: None,
        health_reports: Default::default(),
    })
}

pub async fn find_by_name(
    txn: &mut PgConnection,
    name: &str,
) -> DatabaseResult<Option<PowerShelf>> {
    let mut power_shelves =
        find_by(txn, ObjectColumnFilter::One(NameColumn, &name.to_string())).await?;

    if power_shelves.is_empty() {
        Ok(None)
    } else if power_shelves.len() == 1 {
        Ok(Some(power_shelves.swap_remove(0)))
    } else {
        Err(DatabaseError::new(
            "PowerShelf::find_by_name",
            sqlx::Error::Decode(
                eyre::eyre!(
                    "searching for PowerShelf {} returned multiple results",
                    name
                )
                .into(),
            ),
        ))
    }
}

pub async fn find_by_id(
    txn: &mut PgConnection,
    id: &PowerShelfId,
) -> DatabaseResult<Option<PowerShelf>> {
    let mut power_shelves = find_by(txn, ObjectColumnFilter::One(IdColumn, id)).await?;

    if power_shelves.is_empty() {
        Ok(None)
    } else if power_shelves.len() == 1 {
        Ok(Some(power_shelves.swap_remove(0)))
    } else {
        Err(DatabaseError::new(
            "PowerShelf::find_by_id",
            sqlx::Error::Decode(
                eyre::eyre!("searching for PowerShelf {} returned multiple results", id).into(),
            ),
        ))
    }
}

// TODO(chet): Per Issue #925, the goal is to link machines to BMCs via
// the machine_interfaces table, but for now this is going to be like
// this until I take care of the issue.
pub async fn find_by_bmc_mac_address(
    txn: &mut PgConnection,
    bmc_mac_address: mac_address::MacAddress,
) -> DatabaseResult<Option<PowerShelf>> {
    let power_shelves = find_by(
        txn,
        ObjectColumnFilter::One(BmcMacAddressColumn, &bmc_mac_address),
    )
    .await?;
    Ok(power_shelves.into_iter().next())
}

pub async fn find_ids(
    txn: impl DbReader<'_>,
    filter: model::power_shelf::PowerShelfSearchFilter,
) -> Result<Vec<PowerShelfId>, DatabaseError> {
    let mut qb = sqlx::QueryBuilder::new("SELECT DISTINCT ps.id FROM power_shelves ps");

    if filter.bmc_mac.is_some() {
        qb.push(" JOIN machine_interfaces mi ON mi.power_shelf_id = ps.id");
    }

    qb.push(" WHERE TRUE");

    if let Some(rack_id) = filter.rack_id {
        qb.push(" AND ps.rack_id = ");
        qb.push_bind(rack_id);
    }
    match filter.deleted {
        model::DeletedFilter::Exclude => qb.push(" AND ps.deleted IS NULL"),
        model::DeletedFilter::Only => qb.push(" AND ps.deleted IS NOT NULL"),
        model::DeletedFilter::Include => &mut qb,
    };

    if let Some(state) = &filter.controller_state {
        qb.push(" AND ps.controller_state->>'state' = ");
        qb.push_bind(state.clone());
    }

    if let Some(mac) = filter.bmc_mac {
        qb.push(" AND mi.mac_address = ");
        qb.push_bind(mac);
    }

    qb.build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("power_shelf::find_ids", e))
}

/// Base relation for loading power shelves. Wraps the `power_shelves` table in a
/// derived table (aliased `power_shelves`) that adds a `bmc_info` JSON column
/// resolved from the `Bmc` machine_interface linked back to the shelf --
/// mirroring how the machine snapshot query materializes `bmc_info` (see
/// `sql/machine_snapshots.sql.template`). Keeping the alias `power_shelves` lets
/// the generic `FilterableQueryBuilder` filters reference unqualified columns.
const POWER_SHELVES_WITH_BMC_INFO: &str = r#"SELECT * FROM (
    SELECT ps.*, bmc.json AS bmc_info
    FROM power_shelves ps
    LEFT JOIN LATERAL (
        SELECT jsonb_strip_nulls(jsonb_build_object(
            'machine_interface_id', bmc_i.id,
            'ip', host(bmc_addr.address),
            'mac', bmc_i.mac_address::text
        )) AS json
        FROM machine_interfaces bmc_i
        LEFT JOIN LATERAL (
            SELECT a.address
            FROM machine_interface_addresses a
            WHERE a.interface_id = bmc_i.id
            ORDER BY family(a.address), a.address
            LIMIT 1
        ) AS bmc_addr ON true
        WHERE bmc_i.power_shelf_id = ps.id
          AND bmc_i.interface_type = 'Bmc'
        ORDER BY bmc_i.created ASC
        LIMIT 1
    ) AS bmc ON true
) AS power_shelves"#;

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = PowerShelf>>(
    txn: &mut PgConnection,
    filter: ObjectColumnFilter<'a, C>,
) -> DatabaseResult<Vec<PowerShelf>> {
    let mut query = FilterableQueryBuilder::new(POWER_SHELVES_WITH_BMC_INFO).filter(&filter);

    query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new(query.sql(), e))
}

pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
    expected_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &PowerShelfControllerState,
) -> DatabaseResult<bool> {
    let query_result = sqlx::query_as::<_, PowerShelfId>(
            "UPDATE power_shelves SET controller_state = $1, controller_state_version = $2 WHERE id = $3 AND controller_state_version = $4 RETURNING id",
        )
            .bind(sqlx::types::Json(new_state))
            .bind(new_version)
            .bind(power_shelf_id)
            .bind(expected_version)
            .fetch_optional(txn)
            .await
            .map_err(|e| DatabaseError::new("try_update_controller_state", e))?;

    Ok(query_result.is_some())
}

pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
    outcome: PersistentStateHandlerOutcome,
) -> DatabaseResult<()> {
    sqlx::query("UPDATE power_shelves SET controller_state_outcome = $1 WHERE id = $2")
        .bind(sqlx::types::Json(outcome))
        .bind(power_shelf_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("update_controller_state_outcome", e))?;

    Ok(())
}

pub async fn set_power_shelf_maintenance_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
    initiator: &str,
    operation: PowerShelfMaintenanceOperation,
) -> DatabaseResult<()> {
    let req = PowerShelfMaintenanceRequest {
        requested_at: Utc::now(),
        initiator: initiator.to_string(),
        operation,
    };
    let query = "UPDATE power_shelves SET power_shelf_maintenance_requested = $1 WHERE id = $2 RETURNING id";
    sqlx::query_as::<_, PowerShelfId>(query)
        .bind(sqlx::types::Json(req))
        .bind(power_shelf_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("set_power_shelf_maintenance_requested", e))?;
    Ok(())
}

/// Records a request to start decommissioning when the power shelf is Ready.
pub async fn set_decommission_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
) -> DatabaseResult<()> {
    const QUERY: &str =
        "UPDATE power_shelves SET decommission_requested = TRUE WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, PowerShelfId>(QUERY)
        .bind(power_shelf_id)
        .fetch_one(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::new("set_decommission_requested", error))
}

pub async fn clear_decommission_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
) -> DatabaseResult<()> {
    const QUERY: &str = "UPDATE power_shelves
        SET decommission_requested = FALSE
        WHERE id = $1
        RETURNING id";
    sqlx::query_as::<_, PowerShelfId>(QUERY)
        .bind(power_shelf_id)
        .fetch_one(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::new("clear_decommission_requested", error))
}

pub async fn clear_power_shelf_maintenance_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
) -> DatabaseResult<()> {
    let query = "UPDATE power_shelves SET power_shelf_maintenance_requested = NULL WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, PowerShelfId>(query)
        .bind(power_shelf_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("clear_power_shelf_maintenance_requested", e))?;
    Ok(())
}

/// Record an operator force-converge request against a power shelf's BMC (PMC)
///. The power-shelf state controller consumes it on its next sweep.
/// Mirrors [`crate::switch::set_bmc_credential_rotation_requested`].
pub async fn set_bmc_credential_rotation_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
) -> DatabaseResult<()> {
    let query = "UPDATE power_shelves SET bmc_credential_rotation_requested = true WHERE id = $1 RETURNING id";
    sqlx::query_scalar::<_, PowerShelfId>(query)
        .bind(power_shelf_id)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            // `RETURNING id` yields no row for an unknown power shelf; surface a
            // clean not-found rather than a generic wrapped error.
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "power_shelf",
                id: power_shelf_id.to_string(),
            },
            e => DatabaseError::new("power_shelf::set_bmc_credential_rotation_requested", e),
        })?;
    Ok(())
}

/// Clear a power shelf's force-converge request, committed with the
/// return to `Ready` once a forced tick settles. Mirrors
/// [`crate::switch::clear_bmc_credential_rotation_requested`].
pub async fn clear_bmc_credential_rotation_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
) -> DatabaseResult<()> {
    let query = "UPDATE power_shelves SET bmc_credential_rotation_requested = false WHERE id = $1 RETURNING id";
    sqlx::query_scalar::<_, PowerShelfId>(query)
        .bind(power_shelf_id)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "power_shelf",
                id: power_shelf_id.to_string(),
            },
            e => DatabaseError::new("power_shelf::clear_bmc_credential_rotation_requested", e),
        })?;
    Ok(())
}

/// Sets `power_shelf_reprovisioning_requested` so the Ready handler can enter
/// `ReProvisioning` when rack-firmware reprovisioning is enabled.
///
/// `activities` selects which rack maintenance phases the power shelf should wait
/// for. Empty means all activities.
pub async fn set_power_shelf_reprovisioning_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
    initiator: &str,
    activities: Vec<MaintenanceActivity>,
) -> DatabaseResult<()> {
    let req = PowerShelfReprovisionRequest {
        requested_at: Utc::now(),
        initiator: initiator.to_string(),
        activities,
    };
    let query = "UPDATE power_shelves SET power_shelf_reprovisioning_requested = $1 WHERE id = $2 RETURNING id";
    sqlx::query_as::<_, PowerShelfId>(query)
        .bind(sqlx::types::Json(req))
        .bind(power_shelf_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("set_power_shelf_reprovisioning_requested", e))?;
    Ok(())
}

/// Clears `power_shelf_reprovisioning_requested`. Typically called when
/// reprovisioning completes or is cancelled.
pub async fn clear_power_shelf_reprovisioning_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
) -> DatabaseResult<()> {
    let query = "UPDATE power_shelves SET power_shelf_reprovisioning_requested = NULL WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, PowerShelfId>(query)
        .bind(power_shelf_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("clear_power_shelf_reprovisioning_requested", e))?;
    Ok(())
}

/// Clears a rack-owned reprovisioning request before the power shelf leaves
/// `Ready`.
///
/// Returns whether the request was cleared. Once the power-shelf controller
/// has started reprovisioning, it owns the request and unwinds it after
/// observing the parent rack in `Error`.
pub async fn clear_ready_power_shelf_reprovisioning_requested(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
    initiator: &str,
) -> DatabaseResult<bool> {
    let query = r#"UPDATE power_shelves
        SET power_shelf_reprovisioning_requested = NULL
        WHERE id = $1
          AND controller_state->>'state' = 'ready'
          AND power_shelf_reprovisioning_requested->>'initiator' = $2
        RETURNING id"#;
    let cleared = sqlx::query_as::<_, PowerShelfId>(query)
        .bind(power_shelf_id)
        .bind(initiator)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("clear_ready_power_shelf_reprovisioning_requested", e))?;
    Ok(cleared.is_some())
}

/// Sets `firmware_upgrade_status` on the power shelf. Call from rack maintenance
/// to report upgrade progress. `WaitingForRackFirmwareUpgrade` reads this:
/// Completed → Ready, Failed → Error.
pub async fn update_firmware_upgrade_status(
    txn: &mut PgConnection,
    power_shelf_id: PowerShelfId,
    status: Option<&RackFirmwareUpgradeStatus>,
) -> DatabaseResult<()> {
    let query = "UPDATE power_shelves SET firmware_upgrade_status = $1 WHERE id = $2 RETURNING id";
    sqlx::query_as::<_, PowerShelfId>(query)
        .bind(status.map(|s| sqlx::types::Json(s.clone())))
        .bind(power_shelf_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("update_firmware_upgrade_status", e))?;
    Ok(())
}

pub async fn mark_as_deleted<'a>(
    power_shelf: &'a mut PowerShelf,
    txn: &mut PgConnection,
) -> DatabaseResult<&'a mut PowerShelf> {
    let now = Utc::now();
    power_shelf.deleted = Some(now);

    sqlx::query("UPDATE power_shelves SET deleted = $1 WHERE id = $2")
        .bind(now)
        .bind(power_shelf.id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("mark_as_deleted", e))?;

    Ok(power_shelf)
}

pub async fn final_delete(
    power_shelf_id: PowerShelfId,
    txn: &mut PgConnection,
) -> DatabaseResult<PowerShelfId> {
    let query =
        sqlx::query_as::<_, PowerShelfId>("DELETE FROM power_shelves WHERE id = $1 RETURNING id");

    let power_shelf: PowerShelfId = query
        .bind(power_shelf_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("final_delete", e))?;

    Ok(power_shelf)
}

pub async fn update(
    power_shelf: &PowerShelf,
    txn: &mut PgConnection,
) -> DatabaseResult<PowerShelf> {
    sqlx::query("UPDATE power_shelves SET status = $1 WHERE id = $2")
        .bind(sqlx::types::Json(&power_shelf.status))
        .bind(power_shelf.id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("update", e))?;

    Ok(power_shelf.clone())
}

use std::net::IpAddr;

use carbide_uuid::rack::RackId;
use mac_address::MacAddress;

/// Resolve PowerShelfIds to BMC/PMC IPs via the machine_interfaces path.
pub async fn find_bmc_ips_by_power_shelf_ids(
    db: impl crate::db_read::DbReader<'_>,
    power_shelf_ids: &[PowerShelfId],
) -> DatabaseResult<Vec<(PowerShelfId, IpAddr)>> {
    let sql = r#"
        SELECT DISTINCT ON (ps.id)
            ps.id,
            mia.address
        FROM power_shelves ps
        JOIN expected_power_shelves eps ON eps.serial_number = ps.config->>'name'
        JOIN machine_interfaces mi ON mi.mac_address = eps.bmc_mac_address
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        WHERE ps.id = ANY($1)
        ORDER BY ps.id
    "#;

    sqlx::query_as(sql)
        .bind(power_shelf_ids)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::new("power_shelf::find_bmc_ips_by_power_shelf_ids", err))
}

/// Full endpoint info for a power shelf: PMC MAC and PMC IP.
#[derive(Debug, sqlx::FromRow)]
pub struct PowerShelfEndpointRow {
    pub power_shelf_id: PowerShelfId,
    pub pmc_mac: MacAddress,
    pub pmc_ip: IpAddr,
}

/// Resolve PowerShelfIds to PMC MAC + IP.
pub async fn find_power_shelf_endpoints_by_ids(
    db: impl crate::db_read::DbReader<'_>,
    power_shelf_ids: &[PowerShelfId],
) -> DatabaseResult<Vec<PowerShelfEndpointRow>> {
    // DISTINCT ON guards against a machine_interface having multiple addresses
    let sql = r#"
        SELECT DISTINCT ON (ps.id)
            ps.id                AS power_shelf_id,
            eps.bmc_mac_address  AS pmc_mac,
            mia.address          AS pmc_ip
        FROM power_shelves ps
        JOIN expected_power_shelves eps ON eps.serial_number = ps.config->>'name'
        JOIN machine_interfaces mi ON mi.mac_address = eps.bmc_mac_address
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        WHERE ps.id = ANY($1)
        ORDER BY ps.id
    "#;

    sqlx::query_as(sql)
        .bind(power_shelf_ids)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::new("power_shelf::find_power_shelf_endpoints_by_ids", err))
}

/// Endpoint info (PMC MAC + PMC IP) for a power shelf that may not be ingested
/// yet.
#[derive(Debug, sqlx::FromRow)]
pub struct PreIngestionPowerShelfEndpointRow {
    pub pmc_mac: MacAddress,
    pub pmc_ip: IpAddr,
}

/// Resolve PMC MACs to endpoint info (PMC MAC + IP) for power shelves that may
/// not be ingested yet.
///
/// Identical joins to [`find_power_shelf_endpoints_by_ids`] but anchored on
/// `expected_power_shelves.bmc_mac_address` instead of a `power_shelves` row, so
/// it works before ingestion creates the power shelf. `DISTINCT ON
/// (eps.bmc_mac_address)` collapses duplicate address rows.
pub async fn find_power_shelf_endpoints_by_bmc_macs(
    db: impl crate::db_read::DbReader<'_>,
    pmc_macs: &[MacAddress],
) -> DatabaseResult<Vec<PreIngestionPowerShelfEndpointRow>> {
    let sql = r#"
        SELECT DISTINCT ON (eps.bmc_mac_address)
            eps.bmc_mac_address  AS pmc_mac,
            mia.address          AS pmc_ip
        FROM expected_power_shelves eps
        JOIN machine_interfaces mi ON mi.mac_address = eps.bmc_mac_address
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        WHERE eps.bmc_mac_address = ANY($1)
        ORDER BY eps.bmc_mac_address
    "#;

    sqlx::query_as(sql)
        .bind(pmc_macs)
        .fetch_all(db)
        .await
        .map_err(|err| {
            DatabaseError::new("power_shelf::find_power_shelf_endpoints_by_bmc_macs", err)
        })
}

pub async fn update_metadata(
    txn: &mut PgConnection,
    power_shelf_id: &PowerShelfId,
    expected_version: ConfigVersion,
    metadata: Metadata,
) -> Result<(), DatabaseError> {
    let next_version = expected_version.increment();

    let query = "UPDATE power_shelves SET
            version=$1,
            name=$2, description=$3, labels=$4::jsonb
            WHERE id=$5 AND version=$6
            RETURNING id";

    let query_result: Result<(PowerShelfId,), _> = sqlx::query_as(query)
        .bind(next_version)
        .bind(&metadata.name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(power_shelf_id)
        .bind(expected_version)
        .fetch_one(txn)
        .await;

    match query_result {
        Ok((_id,)) => Ok(()),
        Err(e) => Err(match e {
            sqlx::Error::RowNotFound => DatabaseError::ConcurrentModificationError(
                "power_shelf",
                expected_version.to_string(),
            ),
            e => DatabaseError::query(query, e),
        }),
    }
}

/// A power shelf resolved by its BMC MAC address, along with the rack it
/// belongs to. Used by the Component Manager state controller wrapper to
/// build a rack-level `MaintenanceScope` for the power shelves it's been
/// asked to act on.
#[derive(Debug, sqlx::FromRow)]
pub struct PowerShelfIdByBmcMac {
    pub bmc_mac_address: MacAddress,
    pub id: PowerShelfId,
    pub rack_id: Option<RackId>,
}

/// Resolve BMC MAC addresses to `PowerShelfId`s + `rack_id`s.
pub async fn find_ids_by_bmc_macs(
    db: impl crate::db_read::DbReader<'_>,
    macs: &[MacAddress],
) -> DatabaseResult<Vec<PowerShelfIdByBmcMac>> {
    let sql = r#"
        SELECT ps.bmc_mac_address, ps.id, ps.rack_id
        FROM power_shelves ps
        WHERE ps.bmc_mac_address = ANY($1)
    "#;

    sqlx::query_as(sql)
        .bind(macs)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::new("power_shelf::find_ids_by_bmc_macs", err))
}

/// RMS identity for a power shelf, including rack profile context for node
/// descriptor construction.
#[derive(Debug, sqlx::FromRow)]
pub struct PowerShelfRmsIdentity {
    pub id: String,
    pub bmc_mac_address: MacAddress,
    pub rack_id: Option<RackId>,
    pub rack_profile_id: Option<RackProfileId>,
}

/// Look up RMS identities and rack profile context for power shelves by their
/// BMC MAC addresses.
pub async fn find_rms_identities_by_macs(
    db: impl crate::db_read::DbReader<'_>,
    macs: &[MacAddress],
) -> DatabaseResult<Vec<PowerShelfRmsIdentity>> {
    let sql = r#"
        SELECT
            ps.id::text,
            ps.bmc_mac_address,
            ps.rack_id,
            r.rack_profile_id
        FROM power_shelves ps
        LEFT JOIN racks r ON r.id = ps.rack_id
        WHERE ps.bmc_mac_address = ANY($1)
    "#;

    sqlx::query_as(sql)
        .bind(macs)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::new("power_shelf::find_rms_identities_by_macs", err))
}

pub async fn insert_health_report(
    txn: &mut PgConnection,
    power_shelf_id: &PowerShelfId,
    mode: HealthReportApplyMode,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    crate::health_report::insert_health_report(
        txn,
        "power_shelves",
        power_shelf_id,
        mode,
        health_report,
    )
    .await
}

pub async fn remove_health_report(
    txn: &mut PgConnection,
    power_shelf_id: &PowerShelfId,
    mode: HealthReportApplyMode,
    source: &str,
) -> Result<(), DatabaseError> {
    crate::health_report::remove_health_report(txn, "power_shelves", power_shelf_id, mode, source)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::power_shelf::{create_seeded, create_seeded_with_config, seeded_id};

    /// The power-shelf load query must surface `bmc_info` (PMC MAC + IP +
    /// machine-interface id) resolved from the BMC machine_interface linked
    /// back to the shelf (`power_shelf_id` + `interface_type = 'Bmc'`),
    /// regardless of which network segment the interface lives on. A shelf with
    /// only a non-BMC interface must load with `bmc_info == None`.
    #[crate::sqlx_test]
    async fn test_find_by_populates_bmc_info(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::machine::MachineInterfaceId;
        use carbide_uuid::network::NetworkSegmentId;
        use model::allocation_type::AllocationType;

        let mut txn = pool.begin().await?;

        let shelf = create_seeded(&mut txn, 10, "BMC info shelf").await?;
        let other = create_seeded(&mut txn, 11, "Data-only shelf").await?;

        // A non-underlay segment on purpose: resolution must not depend on the
        // segment type, only on the BMC link back to the shelf.
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version, network_segment_type)
             VALUES ($1, 'V1-T0', 'tenant') RETURNING id",
        )
        .bind("power-shelf-bmc-info")
        .fetch_one(txn.as_mut())
        .await?;

        let pmc_mac = "02:00:00:00:0a:01";
        let pmc_ip: IpAddr = "10.20.30.40".parse()?;
        let bmc_interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                 (power_shelf_id, association_type, segment_id, mac_address,
                  primary_interface, hostname, interface_type)
             VALUES ($1, 'PowerShelf', $2, $3::macaddr, false, 'pmc', 'Bmc')
             RETURNING id",
        )
        .bind(shelf.id)
        .bind(segment_id)
        .bind(pmc_mac)
        .fetch_one(txn.as_mut())
        .await?;
        crate::machine_interface_address::insert(
            txn.as_mut(),
            bmc_interface_id,
            pmc_ip,
            AllocationType::Dhcp,
        )
        .await?;

        // A 'Data' interface linked to a different shelf must be ignored.
        let data_interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                 (power_shelf_id, association_type, segment_id, mac_address,
                  primary_interface, hostname, interface_type)
             VALUES ($1, 'PowerShelf', $2, $3::macaddr, false, 'data', 'Data')
             RETURNING id",
        )
        .bind(other.id)
        .bind(segment_id)
        .bind("02:00:00:00:0a:02")
        .fetch_one(txn.as_mut())
        .await?;
        crate::machine_interface_address::insert(
            txn.as_mut(),
            data_interface_id,
            "10.20.30.41".parse::<IpAddr>()?,
            AllocationType::Dhcp,
        )
        .await?;

        let loaded = find_by_id(&mut txn, &shelf.id)
            .await?
            .expect("power shelf should exist");
        let bmc_info = loaded
            .bmc_info
            .expect("shelf load should populate bmc_info from the BMC interface");
        assert_eq!(bmc_info.machine_interface_id, Some(bmc_interface_id));
        assert_eq!(bmc_info.mac, Some(pmc_mac.parse()?));
        assert_eq!(bmc_info.ip, Some(pmc_ip));

        // The shelf whose only interface is `Data` must load without bmc_info.
        let other_loaded = find_by_id(&mut txn, &other.id)
            .await?
            .expect("power shelf should exist");
        assert!(
            other_loaded.bmc_info.is_none(),
            "a shelf with only a non-BMC interface must not surface bmc_info"
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_power_shelf_database_operations(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let power_shelf = create_seeded_with_config(
            &mut txn,
            20,
            "Database Test Power Shelf",
            Some(6000),
            Some(480),
        )
        .await?;
        let power_shelf_id = power_shelf.id;

        assert_eq!(power_shelf_id, seeded_id(20));
        assert_eq!(power_shelf.config.name, "Database Test Power Shelf");
        assert_eq!(power_shelf.config.capacity, Some(6000));
        assert_eq!(power_shelf.config.voltage, Some(480));

        let found_power_shelves = find_by(
            &mut txn,
            crate::ObjectColumnFilter::One(IdColumn, &power_shelf_id),
        )
        .await?;

        assert_eq!(found_power_shelves.len(), 1);
        let mut found_power_shelf = found_power_shelves[0].clone();
        assert_eq!(found_power_shelf.id, power_shelf_id);
        assert_eq!(found_power_shelf.config.name, "Database Test Power Shelf");

        let deleted_power_shelf = mark_as_deleted(&mut found_power_shelf, &mut txn).await?;
        assert!(deleted_power_shelf.deleted.is_some());
        assert!(deleted_power_shelf.is_marked_as_deleted());

        txn.rollback().await?;

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_power_shelf_status_update(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let mut power_shelf = create_seeded_with_config(
            &mut txn,
            21,
            "Status Test Power Shelf",
            Some(5000),
            Some(240),
        )
        .await?;

        let status = model::power_shelf::PowerShelfStatus {
            shelf_name: "Status Test Power Shelf".to_string(),
            power_state: "on".to_string(),
            health_status: "ok".to_string(),
        };

        power_shelf.status = Some(status.clone());
        let updated_power_shelf = update(&power_shelf, &mut txn).await?;

        assert!(updated_power_shelf.status.is_some());
        let updated_status = updated_power_shelf.status.as_ref().unwrap();
        assert_eq!(updated_status.shelf_name, "Status Test Power Shelf");
        assert_eq!(updated_status.power_state, "on");
        assert_eq!(updated_status.health_status, "ok");

        txn.rollback().await?;

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_power_shelf_controller_state_transitions(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let power_shelf = create_seeded_with_config(
            &mut txn,
            22,
            "Controller State Test Power Shelf",
            Some(5000),
            Some(240),
        )
        .await?;
        let power_shelf_id = power_shelf.id;

        let initial_state = &power_shelf.controller_state.value;
        assert!(matches!(
            initial_state,
            PowerShelfControllerState::Initializing
        ));

        let new_state = PowerShelfControllerState::Ready;
        let current_version = power_shelf.controller_state.version;

        let next_version = current_version.increment();
        let updated = try_update_controller_state(
            &mut txn,
            power_shelf_id,
            current_version,
            next_version,
            &new_state,
        )
        .await?;
        assert!(updated, "update with correct version should succeed");

        let updated_power_shelves = find_by(
            &mut txn,
            crate::ObjectColumnFilter::One(IdColumn, &power_shelf_id),
        )
        .await?;

        assert_eq!(updated_power_shelves.len(), 1);
        let updated_power_shelf = &updated_power_shelves[0];
        assert!(matches!(
            updated_power_shelf.controller_state.value,
            PowerShelfControllerState::Ready
        ));

        assert_eq!(
            updated_power_shelf.controller_state.version.version_nr(),
            current_version.version_nr() + 1,
            "version should be incremented after update"
        );

        let stale_update = try_update_controller_state(
            &mut txn,
            power_shelf_id,
            current_version,
            current_version.increment(),
            &PowerShelfControllerState::Initializing,
        )
        .await?;
        assert!(
            !stale_update,
            "update with stale version should be rejected"
        );

        let new_version = updated_power_shelf.controller_state.version;
        let updated_again = try_update_controller_state(
            &mut txn,
            power_shelf_id,
            new_version,
            new_version.increment(),
            &PowerShelfControllerState::Initializing,
        )
        .await?;
        assert!(updated_again, "update with current version should succeed");

        txn.rollback().await?;

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_power_shelf_list_segment_ids(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let configs = [
            ("List Test Power Shelf 1", 5000, 240),
            ("List Test Power Shelf 2", 3000, 120),
            ("List Test Power Shelf 3", 4000, 208),
        ];

        let mut created_ids = Vec::new();

        for (index, (name, capacity, voltage)) in configs.into_iter().enumerate() {
            let power_shelf = create_seeded_with_config(
                &mut txn,
                30 + index as u8,
                name,
                Some(capacity),
                Some(voltage),
            )
            .await?;
            created_ids.push(power_shelf.id);
        }

        let listed_ids = find_ids(
            txn.as_mut(),
            model::power_shelf::PowerShelfSearchFilter {
                rack_id: None,
                deleted: model::DeletedFilter::Include,
                controller_state: None,
                bmc_mac: None,
            },
        )
        .await?;

        for created_id in &created_ids {
            assert!(listed_ids.contains(created_id));
        }

        assert!(listed_ids.len() >= created_ids.len());

        txn.rollback().await?;

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_power_shelf_controller_state_outcome(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let power_shelf = create_seeded_with_config(
            &mut txn,
            40,
            "Outcome Test Power Shelf",
            Some(5000),
            Some(240),
        )
        .await?;
        let power_shelf_id = power_shelf.id;

        let outcome = model::controller_outcome::PersistentStateHandlerOutcome::Transition {
            source_ref: None,
        };

        update_controller_state_outcome(&mut txn, power_shelf_id, outcome).await?;

        let updated_power_shelves = find_by(
            &mut txn,
            crate::ObjectColumnFilter::One(IdColumn, &power_shelf_id),
        )
        .await?;

        assert_eq!(updated_power_shelves.len(), 1);
        let updated_power_shelf = &updated_power_shelves[0];
        assert!(updated_power_shelf.controller_state_outcome.is_some());

        let updated_outcome = updated_power_shelf
            .controller_state_outcome
            .as_ref()
            .unwrap();
        assert!(matches!(
            updated_outcome,
            model::controller_outcome::PersistentStateHandlerOutcome::Transition { .. }
        ));

        txn.rollback().await?;

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_set_power_shelf_maintenance_requested_power_on(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let shelf = create_seeded(&mut txn, 1, "PowerOn shelf").await?;
        assert!(
            shelf.power_shelf_maintenance_requested.is_none(),
            "freshly created power shelf should have no maintenance request"
        );

        set_power_shelf_maintenance_requested(
            &mut txn,
            shelf.id,
            "operator (TICKET-123)",
            PowerShelfMaintenanceOperation::PowerOn,
        )
        .await?;

        let reloaded = find_by_id(&mut txn, &shelf.id).await?.unwrap();
        let request = reloaded
            .power_shelf_maintenance_requested
            .expect("expected a maintenance request to be persisted");
        assert_eq!(
            request.operation,
            PowerShelfMaintenanceOperation::PowerOn,
            "operation should round-trip as PowerOn"
        );
        assert_eq!(request.initiator, "operator (TICKET-123)");

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_set_power_shelf_maintenance_requested_power_off(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let shelf = create_seeded(&mut txn, 2, "PowerOff shelf").await?;

        set_power_shelf_maintenance_requested(
            &mut txn,
            shelf.id,
            "admin-cli",
            PowerShelfMaintenanceOperation::PowerOff,
        )
        .await?;

        let reloaded = find_by_id(&mut txn, &shelf.id).await?.unwrap();
        let request = reloaded
            .power_shelf_maintenance_requested
            .expect("expected a maintenance request to be persisted");
        assert_eq!(
            request.operation,
            PowerShelfMaintenanceOperation::PowerOff,
            "operation should round-trip as PowerOff"
        );
        assert_eq!(request.initiator, "admin-cli");

        Ok(())
    }

    /// Calling `set_power_shelf_maintenance_requested` a second time should
    /// overwrite the previous request (e.g., switching from PowerOn to
    /// PowerOff before the controller has acted on it).
    #[crate::sqlx_test]
    async fn test_set_power_shelf_maintenance_requested_overwrites(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let shelf = create_seeded(&mut txn, 3, "Overwrite shelf").await?;

        set_power_shelf_maintenance_requested(
            &mut txn,
            shelf.id,
            "first",
            PowerShelfMaintenanceOperation::PowerOn,
        )
        .await?;
        set_power_shelf_maintenance_requested(
            &mut txn,
            shelf.id,
            "second",
            PowerShelfMaintenanceOperation::PowerOff,
        )
        .await?;

        let reloaded = find_by_id(&mut txn, &shelf.id).await?.unwrap();
        let request = reloaded
            .power_shelf_maintenance_requested
            .expect("expected the second maintenance request to be persisted");
        assert_eq!(request.operation, PowerShelfMaintenanceOperation::PowerOff);
        assert_eq!(request.initiator, "second");

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_clear_power_shelf_maintenance_requested(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let shelf = create_seeded(&mut txn, 4, "Clear shelf").await?;

        // Test clearing both flavors of operation.
        for operation in [
            PowerShelfMaintenanceOperation::PowerOn,
            PowerShelfMaintenanceOperation::PowerOff,
        ] {
            set_power_shelf_maintenance_requested(&mut txn, shelf.id, "operator", operation)
                .await?;
            assert!(
                find_by_id(&mut txn, &shelf.id)
                    .await?
                    .unwrap()
                    .power_shelf_maintenance_requested
                    .is_some(),
                "request should be set before clear (op={:?})",
                operation
            );

            clear_power_shelf_maintenance_requested(&mut txn, shelf.id).await?;
            assert!(
                find_by_id(&mut txn, &shelf.id)
                    .await?
                    .unwrap()
                    .power_shelf_maintenance_requested
                    .is_none(),
                "request should be cleared after clear (op={:?})",
                operation
            );
        }

        Ok(())
    }

    /// Clearing a maintenance request when none is set must be a no-op
    /// (idempotent), since the state controller may call this after the
    /// request has already been cleared by another path.
    #[crate::sqlx_test]
    async fn test_clear_power_shelf_maintenance_requested_when_none(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let shelf = create_seeded(&mut txn, 5, "Idempotent clear shelf").await?;
        assert!(shelf.power_shelf_maintenance_requested.is_none());

        clear_power_shelf_maintenance_requested(&mut txn, shelf.id).await?;
        let reloaded = find_by_id(&mut txn, &shelf.id).await?.unwrap();
        assert!(reloaded.power_shelf_maintenance_requested.is_none());

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_set_decommission_requested(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let shelf = create_seeded(&mut txn, 6, "Decommission request shelf").await?;

        set_decommission_requested(&mut txn, shelf.id).await?;
        let reloaded = find_by_id(&mut txn, &shelf.id)
            .await?
            .expect("power shelf should still exist");
        assert!(reloaded.decommission_requested);

        Ok(())
    }
}
