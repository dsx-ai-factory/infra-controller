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
//!
//! Machine - represents a database-backed Machine object

use std::collections::HashMap;
use std::net::IpAddr;
use std::ops::Deref;
use std::str::FromStr;

use carbide_uuid::dpa_interface::DpaInterfaceId;
use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::{MachineId, MachineType};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::rack::{RackId, RackProfileId};
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use health_report::{HealthReport, HealthReportApplyMode};
use itertools::Itertools;
use lazy_static::lazy_static;
use mac_address::MacAddress;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::expected_machine::ExpectedMachineData;
use model::hardware_info::{
    MachineInventory, MachineNvLinkInfo, mnnvl_gpu_name_sql_like_conditions,
};
use model::machine::infiniband::MachineInfinibandStatusObservation;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::network::{
    DpuFabricInterfaceStatusObservation, MachineNetworkStatusObservation, ManagedHostNetworkConfig,
    ManagedHostQuarantineState,
};
use model::machine::nvlink::MachineNvLinkStatusObservation;
use model::machine::spx::MachineSpxStatusObservation;
use model::machine::upgrade_policy::AgentUpgradePolicy;
use model::machine::{
    Dpf, DpuInfo, DpuInfoStatusObservation, DpuOsOperationalState, DpuRepresentorStatus,
    FailureDetails, HostProfile, Machine, MachineInterfaceSnapshot, MachineLastRebootRequested,
    MachineLastRebootRequestedMode, MachineValidationContext, ManagedHostState, ReprovisionRequest,
    UpgradeDecision,
};
use model::machine_interface_address::MachineInterfaceAssociation;
use model::metadata::Metadata;
use model::resource_pool;
use model::resource_pool::ResourcePoolError;
use model::resource_pool::common::CommonPools;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgConnection, Pool, Postgres, Row};

use super::{DatabaseError, ObjectFilter, Transaction, queries};
use crate::DatabaseResult;
use crate::db_read::DbReader;

#[derive(Serialize)]
struct ReprovisionRequestRestart {
    pub update_firmware: bool,
    pub restart_reprovision_requested_at: DateTime<Utc>,
}

lazy_static! {
    pub static ref JSON_MACHINE_SNAPSHOT_WITH_HISTORY_QUERY: String = format!(
        "SELECT row_to_json(m.*) FROM ({}) m INNER JOIN machines ON machines.id = m.id",
        queries::MACHINE_SNAPSHOTS_WITH_HISTORY.as_str(),
    );
    pub static ref JSON_MACHINE_SNAPSHOT_QUERY: String = format!(
        "SELECT row_to_json(m.*) FROM ({}) m INNER JOIN machines ON machines.id = m.id",
        queries::MACHINE_SNAPSHOTS_NO_HISTORY.as_str(),
    );
}

/// Load a Machine object matching an interface, creating it if not already present.
/// Returns a tuple of (Machine, bool did_we_just_create_it)
///
/// Arguments:
///
/// * `txn` - A reference to a currently open database transaction
/// * `interface` - Network interface of the machine
pub async fn get_or_create(
    txn: &mut PgConnection,
    common_pools: Option<&CommonPools>,
    stable_machine_id: &MachineId,
    interface: &MachineInterfaceSnapshot,
) -> DatabaseResult<Machine> {
    let existing_machine =
        find_one(&mut *txn, stable_machine_id, MachineSearchConfig::default()).await?;
    if let Some(machine_id) = interface.machine_id.as_ref() {
        if machine_id != stable_machine_id {
            return Err(DatabaseError::internal(format!(
                "Database inconsistency: MachineId {} on interface {} does not match stable machine ID {} which now uses this interface",
                machine_id, interface.id, stable_machine_id
            )));
        }

        if existing_machine.is_none() {
            tracing::warn!(
                %machine_id,
                interface_id = %interface.id,
                "Interface ID refers to missing machine",
            );
            return Err(DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            });
        }
    }

    // Get or create
    if let Some(machine) = existing_machine {
        // New site-explorer redfish discovery path.
        crate::machine_interface::associate_interface_with_machine(
            &interface.id,
            MachineInterfaceAssociation::Machine(machine.id),
            txn,
        )
        .await?;
        Ok(machine)
    } else {
        // Old manual discovery path.
        // Host and DPU machines are created in same `discover_machine` call. Update same
        // state in both machines.
        let state = ManagedHostState::Created;
        let machine = create(txn, common_pools, stable_machine_id, state, None, 2).await?;
        crate::machine_interface::associate_interface_with_machine(
            &interface.id,
            MachineInterfaceAssociation::Machine(machine.id),
            txn,
        )
        .await?;
        Ok(machine)
    }
}

pub async fn find_one(
    txn: impl DbReader<'_>,
    id: &MachineId,
    search_config: MachineSearchConfig,
) -> Result<Option<Machine>, DatabaseError> {
    Ok(find(txn, ObjectFilter::One(*id), search_config)
        .await?
        .pop())
}

pub async fn find_existing_machine(
    txn: &mut PgConnection,
    macaddr: MacAddress,
    relay: IpAddr,
) -> Result<Option<MachineId>, DatabaseError> {
    let query = "
    SELECT m.id FROM
    machines m
    INNER JOIN machine_interfaces mi
        ON m.id = mi.machine_id
    INNER JOIN network_segments ns
        ON mi.segment_id = ns.id
    INNER JOIN network_prefixes np
        ON np.segment_id = ns.id
    WHERE
        mi.mac_address = $1::macaddr
        AND
        mi.interface_type != 'Bmc'
        AND
        $2::inet <<= np.prefix";

    let id: Option<MachineId> = sqlx::query_as(query)
        .bind(macaddr)
        .bind(relay)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(id)
}

/// Perform an arbitrary action to a Machine and advance it to the next state given the last
/// state.
///
/// Arguments:
///
/// * `txn` - A reference to a currently open database transaction
/// * `state` - A reference to a MachineState enum
// TODO: abhi, Make it private.
pub async fn advance(
    machine: &Machine,
    txn: &mut PgConnection,
    state: &ManagedHostState,
    version: Option<ConfigVersion>,
) -> Result<bool, DatabaseError> {
    // Get current version
    let version = version.unwrap_or_else(|| machine.state.version.increment());

    // Store history of machine state changes.
    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::Machine,
        &machine.id,
        state,
        version,
    )
    .await?;

    let _id: (String,) = sqlx::query_as(
            "UPDATE machines SET controller_state_version=$1, controller_state=$2 WHERE id=$3 RETURNING id",
        )
        .bind(version)
        .bind(sqlx::types::Json(state))
        .bind(machine.id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update machines state", e))?;

    Ok(true)
}

/// Find machines given a set of criteria
///
/// Arguments:
///
/// * `txn`           - A reference to a currently open database transaction
/// * `filter`        - An ObjectFilter to control the size of the response set
/// * `search_config` - A MachineSearchConfig with search options to control the
///   records selected
pub async fn find(
    txn: impl DbReader<'_>,
    filter: ObjectFilter<'_, MachineId>,
    search_config: MachineSearchConfig,
) -> Result<Vec<Machine>, DatabaseError> {
    // The TRUE will be optimized away by the query planner,
    // but it simplifies the rest of the building for us.
    lazy_static! {
        static ref query_no_history: String =
            format!("{} WHERE TRUE", JSON_MACHINE_SNAPSHOT_QUERY.deref());
        static ref query_with_history: String = format!(
            "{} WHERE TRUE",
            JSON_MACHINE_SNAPSHOT_WITH_HISTORY_QUERY.deref()
        );
    }

    let mut builder = sqlx::QueryBuilder::new(if search_config.include_history {
        query_with_history.deref()
    } else {
        query_no_history.deref()
    });

    match filter {
        ObjectFilter::All => {} // Nothing to add.
        ObjectFilter::One(id) => {
            builder.push(" AND m.id= ");
            builder.push_bind(id.to_string());
        }
        ObjectFilter::List(list) => {
            builder.push(" AND m.id=ANY( ");
            builder.push_bind(
                list.iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<String>>(),
            );
            builder.push(" ) ");
        }
    }

    if search_config.only_maintenance {
        builder.push(
            " AND m.health_reports->'merges'->'maintenance'->'alerts'->0->>'id' = 'Maintenance' ",
        );
    }

    if search_config.only_quarantine {
        builder.push(" AND m.network_config->>'quarantine_state' IS NOT NULL ");
    }

    if let Some(id) = search_config.instance_type_id {
        builder.push(" AND m.instance_type_id = ");
        builder.push_bind(id);
    }

    if let Some(rack_id) = search_config.rack_id {
        builder.push(" AND m.rack_id = ");
        builder.push_bind(rack_id);
    }

    if search_config.for_update {
        builder.push(" ORDER BY m.id ");
        builder.push(" FOR UPDATE OF machines ");
    };

    let all_machines: Vec<Machine> = builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))?;

    Ok(all_machines)
}

pub async fn find_by_ip(
    txn: &mut PgConnection,
    ip: &IpAddr,
) -> Result<Option<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            r#"{}
                INNER JOIN machine_interfaces mi ON mi.machine_id = m.id
                INNER JOIN machine_interface_addresses mia on mia.interface_id=mi.id
                WHERE mia.address = $1::inet
                AND mi.interface_type != 'Bmc'"#,
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(ip.to_string())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

pub async fn find_id_by_bmc_ip(
    txn: &mut PgConnection,
    bmc_ip: &IpAddr,
) -> Result<Option<MachineId>, DatabaseError> {
    crate::machine_topology::find_machine_id_by_bmc_ip(txn, &bmc_ip.to_string()).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteExplorerMachineAuditState {
    pub bmc_ip: IpAddr,
    pub machine_id: MachineId,
    pub site_explorer_health_report: Option<HealthReport>,
}

#[derive(Debug, sqlx::FromRow)]
struct SiteExplorerMachineAuditStateRow {
    bmc_ip: IpAddr,
    machine_id: MachineId,
    site_explorer_health_report: Option<sqlx::types::Json<HealthReport>>,
}

pub async fn find_site_explorer_machine_audit_states_by_bmc_ips(
    txn: impl DbReader<'_>,
    bmc_ips: &[IpAddr],
) -> Result<Vec<SiteExplorerMachineAuditState>, DatabaseError> {
    if bmc_ips.is_empty() {
        return Ok(Vec::new());
    }

    let query = r#"
        SELECT
            mia.address AS bmc_ip,
            mi.machine_id,
            m.health_reports->'merges'->'site-explorer' AS site_explorer_health_report
        FROM machine_interfaces mi
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        JOIN machines m ON m.id = mi.machine_id
        WHERE mi.interface_type = 'Bmc'
            AND mi.machine_id IS NOT NULL
            AND mia.address = ANY($1::inet[])
    "#;
    let bmc_ip_networks: Vec<ipnetwork::IpNetwork> =
        bmc_ips.iter().map(|bmc_ip| (*bmc_ip).into()).collect();
    let rows: Vec<SiteExplorerMachineAuditStateRow> = sqlx::query_as(query)
        .bind(bmc_ip_networks)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(rows
        .into_iter()
        .map(|row| SiteExplorerMachineAuditState {
            bmc_ip: row.bmc_ip,
            machine_id: row.machine_id,
            site_explorer_health_report: row.site_explorer_health_report.map(|report| report.0),
        })
        .collect())
}

/// Finds machines associated with a specified instance type
///
/// * `txn`              - A reference to an active DB transaction
/// * `instance_type_id` - An reference to an InstanceTypeId to query for
/// * `for_update`       - A boolean flag to acquire DB locks for synchronization
pub async fn find_ids_by_instance_type_id(
    txn: &mut PgConnection,
    instance_type_id: &InstanceTypeId,
    for_update: bool,
) -> Result<Vec<(MachineId, ConfigVersion)>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT id, version FROM machines WHERE");

    builder.push(" instance_type_id = ");
    builder.push_bind(instance_type_id);

    if for_update {
        builder.push(" ORDER BY id ");
        builder.push(" FOR UPDATE ");
    }

    builder
        .build_query_as()
        .bind(instance_type_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Finds NVLink info for a list of machine IDs
///
/// * `txn` - A reference to an active DB transaction
/// * `machine_ids` - A slice of machine IDs to query for
pub async fn find_nvlink_info_by_machine_ids(
    txn: &mut PgConnection,
    machine_ids: &[MachineId],
) -> Result<HashMap<MachineId, Option<MachineNvLinkInfo>>, DatabaseError> {
    if machine_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let query = "SELECT id, nvlink_info FROM machines WHERE id = ANY($1)";
    let machine_id_strings: Vec<String> = machine_ids.iter().map(|id| id.to_string()).collect();

    let rows = sqlx::query(query)
        .bind(machine_id_strings)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    let mut result = HashMap::new();
    for row in rows {
        let machine_id: MachineId = row.try_get(0).map_err(|e| DatabaseError::query(query, e))?;
        let nvlink_info: Option<sqlx::types::Json<MachineNvLinkInfo>> =
            row.try_get(1).map_err(|e| DatabaseError::query(query, e))?;
        let nvlink_info = nvlink_info.map(|json| json.0);
        result.insert(machine_id, nvlink_info);
    }

    Ok(result)
}

async fn update_machine_instance_type(
    txn: &mut PgConnection,
    instance_type_id: Option<&InstanceTypeId>,
    machine_versions: &[(&MachineId, &ConfigVersion)],
) -> Result<Vec<MachineId>, DatabaseError> {
    if machine_versions.is_empty() {
        return Ok(vec![]);
    }
    let mut builder = sqlx::QueryBuilder::default();
    builder
        .push("UPDATE machines AS t SET instance_type_id = ")
        .push_bind(instance_type_id)
        .push("::varchar")
        .push(
            ", version = v.new_version \
               FROM ( ",
        )
        .push_values(machine_versions.iter(), |mut b, (id, old_version)| {
            let new_version = old_version.increment();
            b.push_bind(id)
                .push_bind(old_version)
                .push_bind(new_version);
        })
        .push(
            ") AS v(id, old_version, new_version) \
         WHERE t.id = v.id \
           AND t.version = v.old_version \
         RETURNING t.id",
        );
    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Associates machines with an InstanceType.
///
/// * `txn`              - A reference to an active DB transaction
/// * `instance_type_id` - An reference to an InstanceTypeId to associate with a set of machines
/// * `machine_ids`      - A list of machine IDs to associate to the desired instance type
pub async fn associate_machines_with_instance_type(
    txn: &mut PgConnection,
    instance_type_id: &InstanceTypeId,
    machine_versions: &[(&MachineId, &ConfigVersion)],
) -> Result<Vec<MachineId>, DatabaseError> {
    update_machine_instance_type(txn, Some(instance_type_id), machine_versions).await
}

/// Removes multiple machine associations with an InstanceType.
/// This does *NOT* check if the machines are in use.
///
/// * `txn`         - A reference to an active DB transaction
/// * `machine_ids` - A slice of machine IDs to update
pub async fn remove_instance_type_associations(
    txn: &mut PgConnection,
    machine_versions: &[(&MachineId, &ConfigVersion)],
) -> Result<Vec<MachineId>, DatabaseError> {
    update_machine_instance_type(txn, None, machine_versions).await
}

pub async fn find_by_hostname(
    txn: &mut PgConnection,
    hostname: &str,
) -> Result<Option<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} JOIN machine_interfaces mi ON m.id = mi.machine_id WHERE mi.hostname = $1 AND mi.interface_type != 'Bmc'",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }

    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(hostname)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

pub async fn find_by_mac_address(
    txn: &mut PgConnection,
    mac_address: &MacAddress,
) -> Result<Option<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} JOIN machine_interfaces mi ON m.id = mi.machine_id WHERE mi.mac_address = $1::macaddr AND mi.interface_type != 'Bmc'",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

pub async fn find_by_loopback_ip(
    txn: impl DbReader<'_>,
    loopback_ip: &str,
) -> Result<Option<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} WHERE m.network_config->>'loopback_ip' = $1",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(loopback_ip)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;
    Ok(machine)
}

/// Finds a machine by a query
///
/// - If the query looks like a MachineId, it will try to load the information based on the MachineId
/// - If the query looks like an IP address, it will try to look up the machine based on its admin IP address
/// - If the query looks like a MAC address, it will look up the machine by MAC address
/// - Otherwise, it will try to look up the Machine by hostname
pub async fn find_by_query(
    txn: &mut PgConnection,
    query: &str,
) -> Result<Option<Machine>, DatabaseError> {
    if let Ok(id) = MachineId::from_str(query) {
        return find_one(txn, &id, MachineSearchConfig::default()).await;
    }

    if let Ok(ip) = IpAddr::from_str(query) {
        return find_by_ip(txn, &ip).await;
    }

    if let Ok(mac) = MacAddress::from_str(query) {
        return find_by_mac_address(txn, &mac).await;
    }

    find_by_hostname(txn, query).await
}

pub async fn update_reboot_time(
    machine: &Machine,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_reboot_time=NOW() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine.id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_reboot_requested_explicit_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
    mode: MachineLastRebootRequestedMode,
    time: DateTime<Utc>,
) -> Result<(), DatabaseError> {
    let mut restart_verified = None;
    if matches!(mode, MachineLastRebootRequestedMode::Reboot) {
        restart_verified = Some(false);
    }
    let data = MachineLastRebootRequested {
        time,
        mode,
        restart_verified,
        verification_attempts: Some(0),
    };

    let query = "UPDATE machines SET last_reboot_requested=$1 WHERE id=$2 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(sqlx::types::Json(&data))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_reboot_requested_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
    mode: MachineLastRebootRequestedMode,
) -> Result<(), DatabaseError> {
    update_reboot_requested_explicit_time(machine_id, txn, mode, Utc::now()).await
}

pub async fn update_restart_verification_status(
    machine_id: &MachineId,
    mut current_reboot: MachineLastRebootRequested,
    verified: Option<bool>,
    attempts: i32,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    current_reboot.restart_verified = verified;
    current_reboot.verification_attempts = Some(attempts);

    let query = "UPDATE machines SET last_reboot_requested=$1 WHERE id=$2 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(sqlx::types::Json(&current_reboot))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_cleanup_time(
    machine: &Machine,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_cleanup_time=NOW() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine.id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn set_cleanup_time(
    machine_id: &MachineId,
    cleanup_time: DateTime<Utc>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_cleanup_time=$1 WHERE id=$2 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(cleanup_time)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn clear_cleanup_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_cleanup_time=NULL WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_bios_password_set_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET bios_password_set_time=NOW() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_discovery_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_discovery_time=NOW() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_scout_contact_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_scout_contact_time=NOW() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_last_scout_observed_version(
    machine_id: &MachineId,
    scout_version: &str,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_scout_observed_version=$2 WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(scout_version)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn find_host_by_dpu_machine_id(
    txn: &mut PgConnection,
    dpu_machine_id: &MachineId,
) -> Result<Option<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            r#"{} INNER JOIN machine_interfaces mi ON m.id = mi.machine_id
                    WHERE mi.attached_dpu_machine_id=$1
                    AND mi.interface_type != 'Bmc'
                    AND mi.attached_dpu_machine_id != mi.machine_id"#,
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(dpu_machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

pub async fn lookup_host_machine_ids_by_dpu_ids(
    conn: impl DbReader<'_>,
    dpu_machine_ids: &[MachineId],
) -> Result<Vec<MachineId>, DatabaseError> {
    let query = r#"SELECT mi.machine_id
        FROM machine_interfaces mi
        WHERE mi.attached_dpu_machine_id != mi.machine_id
        AND mi.interface_type != 'Bmc'
        AND mi.attached_dpu_machine_id = ANY($1)"#;

    sqlx::query_as(query)
        .bind(
            dpu_machine_ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>(),
        )
        .fetch_all(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Return the [`ManagedHostState`] for a machine given its id without returning the whole snapshot.
pub async fn lookup_managed_host_state(
    conn: impl DbReader<'_>,
    machine_id: MachineId,
) -> DatabaseResult<Option<ManagedHostState>> {
    let query = "SELECT controller_state FROM machines WHERE id = $1";

    let Some(json): Option<sqlx::types::Json<ManagedHostState>> = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
    else {
        return Ok(None);
    };

    Ok(Some(json.0))
}

/// Returns the `use_admin_network` flag from the host that owns the
/// given DPA interface.
pub async fn get_host_use_admin_network_for_dpa_interface(
    conn: impl DbReader<'_>,
    dpa_interface_id: &DpaInterfaceId,
) -> Result<bool, DatabaseError> {
    let query = r#"
        SELECT COALESCE((h.network_config->>'use_admin_network')::bool, true)
        FROM dpa_interfaces da
        JOIN machines h ON h.id = da.machine_id
        WHERE da.id = $1
        LIMIT 1"#;
    let flag: Option<bool> = sqlx::query_scalar(query)
        .bind(dpa_interface_id)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(flag.unwrap_or(true))
}

pub async fn find_dpus_by_host_machine_id(
    txn: &mut PgConnection,
    host_machine_id: &MachineId,
) -> Result<Vec<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            r#"{}
                    INNER JOIN machine_interfaces mi
                      ON m.id = mi.attached_dpu_machine_id
                    WHERE mi.machine_id=$1
                    AND mi.interface_type != 'Bmc'"#,
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machines = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(host_machine_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machines)
}

pub async fn update_metadata(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    expected_version: ConfigVersion,
    metadata: Metadata,
) -> Result<(), DatabaseError> {
    let next_version = expected_version.increment();

    let query = "UPDATE machines SET
            version=$1,
            name=$2, description=$3, labels=$4::jsonb
            WHERE id=$5 AND version=$6
            RETURNING id";
    let query_result: Result<(MachineId,), _> = sqlx::query_as(query)
        .bind(next_version)
        .bind(&metadata.name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(machine_id)
        .bind(expected_version)
        .fetch_one(txn)
        .await;

    match query_result {
        Ok((_machine_id,)) => Ok(()),
        Err(e) => Err(match e {
            sqlx::Error::RowNotFound => {
                DatabaseError::ConcurrentModificationError("machine", expected_version.to_string())
            }
            e => DatabaseError::query(query, e),
        }),
    }
}

/// Only does the update if the passed observation is newer than any existing one
pub async fn update_network_status_observation(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    observation: &MachineNetworkStatusObservation,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET network_status_observation = $1::json WHERE id = $2 AND
                (
                    (network_status_observation->>'observed_at' IS NULL)
                    OR ((network_status_observation->>'observed_at')::timestamp <= $3::timestamp)
                ) RETURNING id";
    let _id: (MachineId,) = match sqlx::query_as(query)
        .bind(sqlx::types::Json(&observation))
        .bind(machine_id)
        .bind(observation.observed_at)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
    {
        Ok(result) => result,
        Err(e) if e.is_not_found() => {
            // This function is intended to be able to capture why the update sometimes fails in unit-test
            // even though all prerequisite data is present.
            // It compiles to a no-op in production environments.
            debug_failed_machine_status_update(
                txn,
                machine_id,
                "network_status_observation",
                observation,
            )
            .await;
            return Err(e);
        }
        Err(e) => return Err(e),
    };

    Ok(())
}

/// Only does the update if the passed observation is newer than any existing one
pub async fn update_infiniband_status_observation(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    observation: &MachineInfinibandStatusObservation,
) -> Result<(), DatabaseError> {
    let query =
            "UPDATE machines SET infiniband_status_observation = $1::json WHERE id = $2 AND
             (infiniband_status_observation IS NULL
                OR (infiniband_status_observation ? 'observed_at' AND infiniband_status_observation->>'observed_at' <= $3)
            ) RETURNING id";
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(&observation))
        .bind(machine_id)
        .bind(observation.observed_at.to_rfc3339())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_nvlink_status_observation(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    observation: &MachineNvLinkStatusObservation,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET nvlink_status_observation = $1::json WHERE id = $2 AND
                (nvlink_status_observation->>'observed_at' IS NULL OR nvlink_status_observation->>'observed_at' <= $3) RETURNING id";
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(&observation))
        .bind(machine_id)
        .bind(observation.observed_at.to_rfc3339())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Clears `machines.nvlink_status_observation` for the given machine IDs.
///
/// Used when NMX-C is unreachable so instance state does not retain stale partition observations.
pub async fn clear_nvlink_status_observations(
    txn: &mut PgConnection,
    machine_ids: &[MachineId],
) -> Result<(), DatabaseError> {
    if machine_ids.is_empty() {
        return Ok(());
    }

    let query = "UPDATE machines SET nvlink_status_observation = NULL WHERE id = ANY($1)";
    let machine_id_strings: Vec<String> = machine_ids.iter().map(|id| id.to_string()).collect();
    sqlx::query(query)
        .bind(machine_id_strings)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_spx_status_observation(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    observation: &MachineSpxStatusObservation,
) -> Result<(), DatabaseError> {
    tracing::debug!(
        "update_spx_status_observation: observation {:#?}",
        observation
    );
    let query = "UPDATE machines SET spx_status_observation = $1::json WHERE id = $2 AND
                (spx_status_observation->>'observed_at' IS NULL OR spx_status_observation->>'observed_at' <= $3) RETURNING id";
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(&observation))
        .bind(machine_id)
        .bind(observation.observed_at.to_rfc3339())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

#[cfg(test)]
async fn debug_failed_machine_status_update(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    column_name: &str,
    column_data: &impl serde::Serialize,
) {
    let serialized_data =
        serde_json::to_string_pretty(column_data).unwrap_or_else(|_| "Invalid".to_string());
    tracing::error!(machine_id=%machine_id, column_name, "Failed to update column. New column data: {serialized_data}");
    // Dump the raw Machine state for debugging purposes
    let query = "SELECT * from machines WHERE id = $1";
    match sqlx::query(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
    {
        Ok(Some(row)) => {
            tracing::error!("Machine Data: {:?}", row);
        }
        Ok(None) => {
            tracing::error!("No Machine Data");
        }
        Err(e) => {
            tracing::error!("Failed to load Machine Data. Error: {e}");
        }
    }
}

#[cfg(not(test))]
#[allow(clippy::unused_async)]
async fn debug_failed_machine_status_update(
    _txn: &mut PgConnection,
    _machine_id: &MachineId,
    _column_name: &str,
    _column_data: &impl serde::Serialize,
) {
}

pub async fn update_dpu_agent_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    let mut health_report = health_report.clone();
    health_report.source = HealthReport::DPU_AGENT_SOURCE.to_string();
    crate::health_report::insert_health_report(
        txn,
        "machines",
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
    )
    .await
}

pub async fn update_machine_validation_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    if health_report.alerts.is_empty() {
        return crate::health_report::remove_health_report(
            txn,
            "machines",
            machine_id,
            HealthReportApplyMode::Merge,
            HealthReport::MACHINE_VALIDATION_SOURCE,
        )
        .await;
    }

    let mut health_report = health_report.clone();
    health_report.source = HealthReport::MACHINE_VALIDATION_SOURCE.to_string();
    crate::health_report::insert_health_report(
        txn,
        "machines",
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
    )
    .await
}

pub async fn update_site_explorer_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    if health_report.alerts.is_empty() {
        return crate::health_report::remove_health_report(
            txn,
            "machines",
            machine_id,
            HealthReportApplyMode::Merge,
            HealthReport::SITE_EXPLORER_SOURCE,
        )
        .await;
    }

    let mut health_report = health_report.clone();
    health_report.source = HealthReport::SITE_EXPLORER_SOURCE.to_string();
    crate::health_report::insert_health_report(
        txn,
        "machines",
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
    )
    .await
}

pub async fn update_sku_validation_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    if health_report.alerts.is_empty() {
        return crate::health_report::remove_health_report(
            txn,
            "machines",
            machine_id,
            HealthReportApplyMode::Merge,
            HealthReport::SKU_VALIDATION_SOURCE,
        )
        .await;
    }

    let mut health_report = health_report.clone();
    health_report.source = HealthReport::SKU_VALIDATION_SOURCE.to_string();
    crate::health_report::insert_health_report(
        txn,
        "machines",
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
    )
    .await
}

pub async fn insert_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    mode: HealthReportApplyMode,
    health_report: &HealthReport,
    no_overwrite: bool,
) -> Result<(), DatabaseError> {
    if no_overwrite {
        // TODO(chet): This appears to be a machine-specific thing -- skip insert
        // if a merge with the same source already exists -- but I'm not sure what
        // it's used for, since others seem to do a remove + insert. Do we need
        // to support this still? Might be nice to explain it somewhere.
        let path = match mode {
            HealthReportApplyMode::Merge => {
                vec!["merges".to_string(), health_report.source.clone()]
            }
            HealthReportApplyMode::Replace => vec!["replace".to_string()],
        };

        let mut query = sqlx::QueryBuilder::new(
            "UPDATE machines SET health_reports = jsonb_set(
                coalesce(health_reports, '{\"merges\": {}}'::jsonb),
                ",
        );
        query.push_bind(path);
        query.push(", ");
        query.push_bind(sqlx::types::Json(health_report));
        query.push(") WHERE id = ");
        query.push_bind(machine_id);
        query.push(" AND coalesce(health_reports, '{\"merges\": {}}'::jsonb)->'merges' ? ");
        query.push_bind(&health_report.source);
        query.push(" = FALSE RETURNING id");

        let _id: (MachineId,) = query
            .build_query_as()
            .fetch_one(txn)
            .await
            .map_err(|e| DatabaseError::new("insert health report override", e))?;

        Ok(())
    } else {
        crate::health_report::insert_health_report(txn, "machines", machine_id, mode, health_report)
            .await
    }
}

pub async fn remove_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    mode: HealthReportApplyMode,
    source: &str,
) -> Result<(), DatabaseError> {
    crate::health_report::remove_health_report(txn, "machines", machine_id, mode, source).await
}

pub async fn update_agent_reported_inventory(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    inventory: &MachineInventory,
) -> Result<(), DatabaseError> {
    let query =
        "UPDATE machines SET agent_reported_inventory = $1::json WHERE id = $2 RETURNING id";
    tracing::debug!(machine_id = %machine_id, "Updating machine inventory");
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(&inventory))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn get_all_network_status_observation(
    txn: impl DbReader<'_>,
    limit: i64, // return at most this many rows
) -> Result<Vec<MachineNetworkStatusObservation>, DatabaseError> {
    let query = "SELECT network_status_observation FROM machines
            WHERE network_status_observation IS NOT NULL
            ORDER BY network_status_observation->'machine_id'
            LIMIT $1::integer";
    let rows = sqlx::query(query)
        .bind(limit)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    let mut all = Vec::with_capacity(rows.len());
    for row in rows {
        let s: sqlx::types::Json<MachineNetworkStatusObservation> = row
            .try_get("network_status_observation")
            .map_err(|e| DatabaseError::query(query, e))?;
        all.push(s.0);
    }
    Ok(all)
}

/// Force cleans all tables related to a Machine - except MachineInterfaces
///
/// DO NOT USE OUTSIDE OF ADMIN CLI
pub async fn force_cleanup(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    // Note: It might be nicer to actually write the full query here so we can
    // report more results.
    // But this way we at least unit-test the stored procedure and make sure
    // it stays up to date.
    let query = r#"call cleanup_machine_by_id($1)"#;
    let _query_result = sqlx::query(query)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Updates the `network_config` on the target machine row (host or
/// DPU), and bumps the `network_config_version` on *every* machine
/// row in the same "machine group" (host + every DPU attached to it)
/// to the same new version, so all rows in the group stay version-equal.
///
/// The caller can pass any machine ID in the group; group membership is
/// computed by the `machine_group_member_ids` Postgres function, which
/// walks `machine_interfaces`. For zero-DPU hosts, the group is just the
/// target itself.
pub async fn try_update_network_config(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    expected_version: ConfigVersion,
    new_state: &ManagedHostNetworkConfig,
) -> Result<bool, DatabaseError> {
    let next_version = expected_version.increment();

    // First, do our usual "optimistic" lock update on the target row, which
    // writes content and bumps to the next_version (which is conditional on
    // the row currently being at `expected_version`).
    let target_query = "UPDATE machines SET network_config_version=$1, network_config=$2::json
            WHERE id=$3 AND network_config_version=$4
            RETURNING id";
    let target_result: Result<MachineId, _> = sqlx::query_as(target_query)
        .bind(next_version)
        .bind(sqlx::types::Json(new_state))
        .bind(machine_id)
        .bind(expected_version)
        .fetch_one(&mut *txn)
        .await;

    match target_result {
        Ok(_) => {
            // ..and if the version bumped, THEN bump every other machine row
            // in the "machine group" to the same new version. This is where
            // we pull in the `machine_group_member_ids` Postgres function.
            let group_query = r#"
                UPDATE machines
                SET network_config_version = $1
                WHERE id != $2
                  AND id IN (SELECT id FROM machine_group_member_ids($2))
            "#;

            sqlx::query(group_query)
                .bind(next_version)
                .bind(machine_id)
                .execute(&mut *txn)
                .await
                .map_err(|e| DatabaseError::query(group_query, e))?;
            Ok(true)
        }
        Err(sqlx::Error::RowNotFound) => Ok(false),
        Err(e) => Err(DatabaseError::query(target_query, e)),
    }
}

/// Replaces predicted host id with stable host id.
/// Once forge receives DiscoveryData from Host, forge can create StableMachineId.
/// This StableMachineId must replace existing PredictedHostId in db.
/// State machine does not act on receiving discoverydata, but discoverycompleted message,
/// so updating host id must not interfere state machine handling.
pub async fn try_sync_stable_id_with_current_machine_id_for_host(
    txn: &mut PgConnection,
    current_machine_id: &Option<MachineId>,
    stable_machine_id: &MachineId,
) -> Result<MachineId, DatabaseError> {
    let Some(current_machine_id) = current_machine_id else {
        return Err(DatabaseError::NotFoundError {
            kind: "machine_id",
            id: stable_machine_id.to_string(),
        });
    };

    // This is repeated call. Machine is already updated with stable ID.
    if !current_machine_id.machine_type().is_predicted_host() {
        return match find_one(txn, current_machine_id, MachineSearchConfig::default()).await? {
            Some(machine) => Ok(machine.id),
            None => Err(DatabaseError::NotFoundError {
                kind: "machine",
                id: current_machine_id.to_string(),
            }),
        };
    }

    // Update the machine state and health history to account for the rename
    crate::state_history::update_object_ids(
        txn,
        crate::state_history::StateHistoryTableId::Machine,
        current_machine_id,
        stable_machine_id,
    )
    .await?;
    crate::health_history::update_object_ids(
        txn,
        crate::health_history::HealthHistoryTableId::Machine,
        &current_machine_id,
        &stable_machine_id,
    )
    .await?;

    // Table machine_interfaces has a FK ON UPDATE CASCADE so machine_interfaces.machine_id will
    // also change.
    let query = "UPDATE machines SET id=$1 WHERE id=$2 RETURNING id";
    let machine_id = sqlx::query_as(query)
        .bind(stable_machine_id)
        .bind(current_machine_id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // If the Machines name in Metadata matched the predicted machine id,
    // then update it to the new ID.
    // If someone changed the name manually then don't bother
    let query = "UPDATE machines SET name=$1 WHERE id=$2 AND name=$3";
    sqlx::query(query)
        .bind(stable_machine_id)
        .bind(stable_machine_id)
        .bind(current_machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(machine_id)
}

pub async fn update_failure_details(
    machine: &Machine,
    txn: &mut PgConnection,
    failure: FailureDetails,
) -> Result<(), DatabaseError> {
    update_failure_details_by_machine_id(&machine.id, txn, failure).await
}

pub async fn clear_failure_details(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let failure_details = FailureDetails {
        cause: model::machine::FailureCause::NoError,
        failed_at: chrono::Utc::now(),
        source: model::machine::FailureSource::NoError,
    };

    let query = "UPDATE machines SET failure_details = $1::json WHERE id = $2 RETURNING id";
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(failure_details))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Create new machine in DB.
///
/// If metadata is not specified then default metadata is used
/// with name set to stable_machine_id.
///
/// If metadata.name is empty then it is initialized in
/// stable_machine_id.to_string().
pub async fn create(
    txn: &mut PgConnection,
    common_pools: Option<&CommonPools>,
    stable_machine_id: &MachineId,
    state: ManagedHostState,
    expected_machine_data: Option<&ExpectedMachineData>,
    state_model_version: i16,
) -> DatabaseResult<Machine> {
    let stable_machine_id_string = stable_machine_id.to_string();

    let default_metadata = &Metadata::default();
    let metadata = expected_machine_data
        .map(|data| &data.metadata)
        .unwrap_or(default_metadata);
    let metadata_name = if !metadata.name.is_empty() {
        &metadata.name
    } else {
        &stable_machine_id_string
    };
    let rack_id = expected_machine_data.and_then(|data| data.rack_id.as_ref());
    let sku_id = expected_machine_data.and_then(|data| data.sku_id.as_ref());
    let dpf_enabled = match expected_machine_data {
        Some(data) => data.dpf_enabled.unwrap_or(true), // EM entry exists
        None => true,                                   // EM entry does not exist
    };
    let host_profile = HostProfile::from_expected_machine(expected_machine_data);

    // Host and DPU machines are created in same `discover_machine` call. Update same
    // state in both machines.
    let state_version = ConfigVersion::initial();
    let version = ConfigVersion::initial();

    let network_config_version = ConfigVersion::initial();
    let network_config = ManagedHostNetworkConfig::default();
    let asn: Option<i64> = if stable_machine_id.machine_type() == MachineType::Dpu {
        if let Some(common_pools) = common_pools {
            match crate::resource_pool::allocate(
                &common_pools.ethernet.pool_fnn_asn,
                txn,
                resource_pool::OwnerType::Machine,
                &stable_machine_id_string,
                None,
            )
            .await
            {
                Ok(asn) => Some(asn as i64),
                Err(e) => {
                    tracing::info!("Failed to allocate asn for dpu {stable_machine_id}: {e}");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let query = r#"INSERT INTO machines(
                            id, controller_state_version, controller_state, network_config_version, network_config, machine_state_model_version, asn, version, name, description, labels, rack_id, hw_sku, dpf, host_profile)
                            VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::json, $12, $13, $14, $15) RETURNING id"#;
    let machine_id: MachineId = sqlx::query_as(query)
        .bind(&stable_machine_id_string)
        .bind(state_version)
        .bind(sqlx::types::Json(&state))
        .bind(network_config_version)
        .bind(sqlx::types::Json(&network_config))
        .bind(state_model_version)
        .bind(asn)
        .bind(version)
        .bind(metadata_name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(rack_id)
        .bind(sku_id)
        .bind(sqlx::types::Json(Dpf {
            enabled: dpf_enabled,
            used_for_ingestion: false,
        }))
        .bind(sqlx::types::Json(&host_profile))
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if machine_id != *stable_machine_id {
        return Err(DatabaseError::internal(format!(
            "Machine {stable_machine_id} was just created, but database failed to return any rows"
        )));
    }

    let machine = find_one(&mut *txn, stable_machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: stable_machine_id.to_string(),
        })?;
    advance(&machine, txn, &state, None).await?;

    // Create a entry in power_options table as well.
    if !machine.is_dpu() {
        crate::power_options::create(&machine.id, txn).await?;
    }
    Ok(machine)
}

pub async fn update_slot_and_tray(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    slot_number: Option<i32>,
    tray_index: Option<i32>,
) -> DatabaseResult<()> {
    sqlx::query("UPDATE machines SET slot_number = $1, tray_index = $2 WHERE id = $3")
        .bind(slot_number)
        .bind(tray_index)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("update_slot_and_tray", e))?;
    Ok(())
}

// Trigger DPU reprovisioning. For machine assigned to user, needs user approval to start
// reprovisioning.
pub async fn trigger_dpu_reprovisioning_request(
    machine_id: &MachineId,
    txn: &mut PgConnection,
    initiator: &str,
    update_firmware: bool,
) -> Result<(), DatabaseError> {
    let reprovision_time = chrono::Utc::now();
    let req = ReprovisionRequest {
        requested_at: reprovision_time,
        initiator: initiator.to_string(),
        update_firmware,
        started_at: None,
        user_approval_received: false,
        restart_reprovision_requested_at: reprovision_time,
    };

    let query = "UPDATE machines SET reprovisioning_requested=$2 WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(req))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

// Update reprovision start time to the current time
pub async fn update_dpu_reprovision_start_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    update_dpu_reprovision_explicit_start_time(machine_id, chrono::Utc::now(), txn).await
}

// Update reprovision start time to a specific start time
pub async fn update_dpu_reprovision_explicit_start_time(
    machine_id: &MachineId,
    time: DateTime<Utc>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET reprovisioning_requested=
                                    jsonb_set(reprovisioning_requested,
                                                '{started_at}', $2, true)
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(time))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

// Update reprovision start time to the current time
pub async fn update_host_reprovision_start_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    update_host_reprovision_explicit_start_time(machine_id, chrono::Utc::now(), txn).await
}

pub async fn update_host_reprovision_explicit_start_time(
    machine_id: &MachineId,
    time: DateTime<Utc>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET host_reprovisioning_requested=
                                    jsonb_set(host_reprovisioning_requested,
                                                '{started_at}', $2, true)
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(time))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn get_host_reprovisioning_machines(
    txn: &mut PgConnection,
) -> Result<Vec<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{}
                WHERE m.host_reprovisioning_requested IS NOT NULL",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))
}

pub async fn update_firmware_update_time_window_start_end(
    machine_ids: &[MachineId],
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET firmware_update_time_window_start = $2, firmware_update_time_window_end = $3, update_complete = false
                       WHERE id = ANY($1) RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_ids.iter().map(|x| x.to_string()).collect_vec())
        .bind(start)
        .bind(end)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(())
}

pub async fn update_update_complete(
    machine_id: &MachineId,
    complete: bool,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET update_complete = $2
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(complete)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(())
}

pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    outcome: PersistentStateHandlerOutcome,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET controller_state_outcome=$1 WHERE id=$2";
    sqlx::query(query)
        .bind(sqlx::types::Json(outcome))
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

// Update user's approval status in db.
pub async fn approve_dpu_reprovision_request(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET reprovisioning_requested=
                                    jsonb_set(reprovisioning_requested,
                                                '{user_approval_received}', $2, true)
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(true))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn approve_host_reprovision_request(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET host_reprovisioning_requested=
                                    jsonb_set(host_reprovisioning_requested,
                                                '{user_approval_received}', $2, true)
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(true))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// This will reset the dpu_reprov request.
pub async fn restart_dpu_reprovisioning(
    txn: &mut PgConnection,
    machine_ids: &[&MachineId],
    update_firmware: bool,
) -> Result<(), DatabaseError> {
    let restart_request = ReprovisionRequestRestart {
        update_firmware,
        restart_reprovision_requested_at: chrono::Utc::now(),
    };
    let query = r#"UPDATE machines
                                SET reprovisioning_requested=reprovisioning_requested || $1
                        WHERE id=ANY($2) RETURNING id"#;

    let str_list: Vec<String> = machine_ids.iter().map(|id| id.to_string()).collect();
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(sqlx::types::Json(restart_request))
        .bind(str_list)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("restart reprovisioning_requested", e))?;

    Ok(())
}

/// This will fail if reprovisioning is already started.
pub async fn clear_dpu_reprovisioning_request(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    validate_started_time: bool,
) -> Result<(), DatabaseError> {
    let query = if validate_started_time {
        "UPDATE machines SET reprovisioning_requested=NULL
            WHERE id=$1 AND reprovisioning_requested->'started_at' = 'null'::jsonb RETURNING id"
    } else {
        "UPDATE machines SET reprovisioning_requested=NULL
            WHERE id=$1 RETURNING id"
    };

    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("clear reprovisioning_requested", e))?;

    Ok(())
}

pub async fn list_machines_requested_for_reprovisioning(
    txn: impl DbReader<'_>,
) -> Result<Vec<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} WHERE m.reprovisioning_requested IS NOT NULL",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))
}

pub async fn list_machines_requested_for_host_reprovisioning(
    txn: impl DbReader<'_>,
) -> Result<Vec<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} WHERE m.host_reprovisioning_requested IS NOT NULL",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))
}

/// Apply dpu agent upgrade policy to a single DPU.
/// Returns Ok(true) if it needs upgrading, Ok(false) otherwise.
pub async fn apply_agent_upgrade_policy(
    txn: &mut PgConnection,
    policy: AgentUpgradePolicy,
    machine: &Machine,
) -> Result<bool, DatabaseError> {
    if policy == AgentUpgradePolicy::Off {
        return Ok(false);
    }

    match machine.network_status_observation.as_ref() {
        None => Ok(false),
        Some(obs) => {
            let carbide_api_version = carbide_version::v!(build_version);
            let Some(agent_version) = obs.agent_version.clone() else {
                return Ok(false);
            };
            let should_upgrade = policy.should_upgrade(&agent_version, carbide_api_version);
            if should_upgrade != machine.needs_agent_upgrade() {
                set_dpu_agent_upgrade_requested(
                    txn,
                    &machine.id,
                    should_upgrade,
                    carbide_api_version,
                )
                .await?;
            }

            Ok(true)
        }
    }
}

pub async fn set_dpu_agent_upgrade_requested(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    should_upgrade: bool,
    to_version: &str,
) -> Result<(), DatabaseError> {
    let decision = UpgradeDecision {
        should_upgrade,
        to_version: to_version.to_string(),
        last_updated: chrono::Utc::now(),
    };
    let query = "UPDATE machines SET dpu_agent_upgrade_requested = $1::json WHERE id = $2";
    sqlx::query(query)
        .bind(sqlx::types::Json(decision))
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn find_machine_ids(
    txn: impl DbReader<'_>,
    search_config: MachineSearchConfig,
) -> Result<Vec<MachineId>, DatabaseError> {
    let mut qb = sqlx::QueryBuilder::new("SELECT id FROM machines");

    if search_config.mnnvl_only {
        qb.push(" INNER JOIN machine_topologies mt ON machines.id = mt.machine_id");
    }

    // Return only machines that are powered on and have a health override with leak classification
    if let Some(pstate) = &search_config.only_with_power_state {
        let pstate_normalized = pstate.to_lowercase();
        qb.push(" INNER JOIN power_options po ON po.host_id = machines.id AND po.last_fetched_power_state = ");
        qb.push_bind(pstate_normalized);
        qb.push("::host_power_state_t");
    }

    qb.push(" WHERE TRUE");

    if search_config.only_maintenance {
        qb.push(" AND health_reports->'merges'->'maintenance'->'alerts'->0->>'id' = 'Maintenance'");
    }

    if search_config.only_quarantine {
        qb.push(" AND ");

        // If we're including DPU's, don't filter them out (DPU's don't get the quarantine state,
        // only the managed host does.)
        if search_config.include_dpus {
            qb.push(format!(
                "(starts_with(id, '{}') OR network_config->>'quarantine_state' IS NOT NULL) ",
                MachineType::Dpu.id_prefix(),
            ));
        } else {
            qb.push("network_config->>'quarantine_state' IS NOT NULL ");
        }
    }

    if !search_config.include_dpus {
        qb.push(format!(
            " AND NOT starts_with(id, '{}')",
            MachineType::Dpu.id_prefix(),
        ));
    }

    if search_config.exclude_hosts {
        qb.push(format!(
            " AND NOT starts_with(id, '{}')",
            MachineType::Host.id_prefix(),
        ));
    }

    if !search_config.include_predicted_host {
        qb.push(format!(
            " AND NOT starts_with(id, '{}')",
            MachineType::PredictedHost.id_prefix(),
        ));
    }

    if let Some(ovrrd_str) = &search_config.only_with_health_alert {
        qb.push(" AND health_reports->'merges' ? ");
        qb.push_bind(ovrrd_str.clone());
        qb.push(" AND jsonb_array_length(health_reports->'merges'->");
        qb.push_bind(ovrrd_str);
        qb.push("->'alerts') > 0");
    }
    if search_config.mnnvl_only {
        qb.push(format!(
            " AND EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    COALESCE(mt.topology->'discovery_data'->'Info'->'gpus', '[]'::jsonb)
                ) AS gpu
                WHERE gpu->'platform_info' IS NOT NULL
                  AND gpu->'platform_info' <> 'null'::jsonb
                  AND (
                      {}
                  )
            )",
            mnnvl_gpu_name_sql_like_conditions()
        ));
    }

    if let Some(id) = search_config.instance_type_id {
        qb.push(" AND instance_type_id = ");
        qb.push_bind(id);
    }

    if let Some(rack_id) = search_config.rack_id {
        qb.push(" AND rack_id = ");
        qb.push_bind(rack_id);
    }

    if let Some(state) = search_config.controller_state {
        qb.push(" AND controller_state->>'state' = ");
        qb.push_bind(state);
    }

    if search_config.for_update {
        qb.push(" ORDER BY id ");
        qb.push(" FOR UPDATE");
    }

    let q = qb.build_query_as();

    let machine_ids: Vec<MachineId> = q
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("find_machine_ids", e))?;

    Ok(machine_ids)
}

pub async fn update_state(
    txn: &mut PgConnection,
    host_id: &MachineId,
    new_state: &ManagedHostState,
) -> Result<(), DatabaseError> {
    let host = find_one(
        &mut *txn,
        host_id,
        // TODO(?): Should we be using for_update/row-level locks here?
        // This is a select that's later used for an update on both version
        // and state below with the calls to `advance`.
        crate::machine::MachineSearchConfig::default(),
    )
    .await?
    .ok_or_else(|| DatabaseError::new("crate::machine::find_one", sqlx::Error::RowNotFound))?;

    let version = host.current_version().increment();
    tracing::info!(machine_id = %host.id, ?new_state, "Updating host state");
    advance(&host, txn, new_state, Some(version)).await?;

    // Keep both host and dpu's states in sync.
    let dpus = find_dpus_by_host_machine_id(txn, host_id).await?;

    for dpu in dpus {
        advance(&dpu, txn, new_state, Some(version)).await?;
    }
    Ok(())
}

pub async fn update_machine_validation_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_machine_validation_time=NOW() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_machine_validation_id(
    machine_id: &MachineId,
    validation_id: MachineValidationId,
    context: MachineValidationContext,
    txn: &mut PgConnection,
) -> Result<MachineId, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("UPDATE machines SET ");
    let query = builder
        .push({
            match context {
                MachineValidationContext::Discovery => "discovery_machine_validation_id",
                MachineValidationContext::Cleanup => "cleanup_machine_validation_id",
                MachineValidationContext::OnDemand => "on_demand_machine_validation_id",
            }
        })
        .push("=")
        .push_bind(validation_id)
        .push(" WHERE id=")
        .push_bind(machine_id)
        .push(" RETURNING id");

    query
        .build_query_as()
        .bind(validation_id)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

pub async fn update_failure_details_by_machine_id(
    machine_id: &MachineId,
    txn: &mut PgConnection,
    failure: FailureDetails,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET failure_details = $1::json WHERE id = $2 RETURNING id";
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(failure))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Find a list of DPU operational information.
///
/// Returns: `Vec<DpuInfo>` - A list of DPU information with loopback and operational state.
///
/// Arguments
///
/// * `txn` - A reference to currently open database transaction
pub async fn find_dpu_infos(txn: &mut PgConnection) -> Result<Vec<DpuInfo>, DatabaseError> {
    type DpuInfoRow = (
        String,
        String,
        sqlx::types::Json<ManagedHostState>,
        sqlx::types::Json<Option<MachineNetworkStatusObservation>>,
        Option<String>,
    );

    // Pull the DPU rows with the JSON fields needed to shape the response.
    let query = r#"
        SELECT
            m.id,
            m.network_config->>'loopback_ip' AS loopback_ip,
            m.controller_state,
            COALESCE(m.network_status_observation, 'null'::jsonb) AS network_status_observation,
            mt.topology->'discovery_data'->'Info'->'dpu_info'->>'firmware_version' AS firmware_version
        FROM machines m
        LEFT JOIN machine_topologies mt ON mt.machine_id = m.id
        WHERE m.network_config->>'loopback_ip' IS NOT NULL
        ORDER BY m.id"#;

    let dpu_rows: Vec<DpuInfoRow> = sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // TODO: Replace this heuristic once the agent reports interface roles explicitly.
    let is_representor = |interface_name: &str| interface_name.starts_with("pf");

    let representor_statuses = |interfaces: &[DpuFabricInterfaceStatusObservation]| {
        let mut representors: Vec<_> = interfaces
            .iter()
            .filter(|interface| is_representor(&interface.interface_name))
            .map(|interface| DpuRepresentorStatus {
                name: interface.interface_name.clone(),
                carrier_up: interface
                    .link_data
                    .as_ref()
                    .and_then(|link| link.carrier_up),
                state: interface
                    .link_data
                    .as_ref()
                    .and_then(|link| link.state.clone()),
            })
            .collect();

        representors.sort_by(|left, right| left.name.cmp(&right.name));
        representors
    };

    // Shape each DB row into the public DPU info model.
    dpu_rows
        .into_iter()
        .map(
            |(id, loopback_ip, controller_state, network_status_observation, firmware_version)| {
                let dpu_id = MachineId::from_str(&id).map_err(|e| {
                    DatabaseError::internal(format!("Invalid DPU machine ID {id}: {e}"))
                })?;
                let network_status_observation = network_status_observation.0;
                let representors = network_status_observation
                    .as_ref()
                    .map(|observation| representor_statuses(&observation.fabric_interfaces))
                    .unwrap_or_default();

                Ok(DpuInfo {
                    id,
                    loopback_ip,
                    observed_status: Some(DpuInfoStatusObservation {
                        os_operational_state: Some(DpuOsOperationalState {
                            state_detail: controller_state.0.dpu_state_string(&dpu_id),
                        }),
                        firmware_version,
                        representors,
                        last_heartbeat: network_status_observation
                            .as_ref()
                            .map(|observation| observation.observed_at),
                    }),
                })
            },
        )
        .collect()
}

/// Allocate a value from the loopback IP resource pool.
///
/// If the pool exists but is empty or has en error, return that.
pub async fn allocate_loopback_ip(
    common_pools: &CommonPools,
    txn: &mut PgConnection,
    owner_id: &str,
) -> Result<IpAddr, DatabaseError> {
    match crate::resource_pool::allocate(
        &common_pools.ethernet.pool_loopback_ip,
        txn,
        resource_pool::OwnerType::Machine,
        owner_id,
        None,
    )
    .await
    {
        Ok(val) => Ok(val),
        Err(crate::resource_pool::ResourcePoolDatabaseError::ResourcePool(
            ResourcePoolError::Empty,
        )) => {
            tracing::error!(owner_id, pool = "lo-ip", "Pool exhausted, cannot allocate");
            Err(DatabaseError::ResourceExhausted("pool lo-ip".to_string()))
        }
        Err(err) => {
            tracing::error!(owner_id, error = %err, pool = "lo-ip", "Error allocating from resource pool");
            Err(err.into())
        }
    }
}

/// Allocate a value from the loopback IP resource pool.
///
/// If the pool exists but is empty or has en error, return that.
pub async fn allocate_vpc_dpu_loopback(
    common_pools: &CommonPools,
    txn: &mut PgConnection,
    owner_id: &str,
) -> Result<IpAddr, DatabaseError> {
    match crate::resource_pool::allocate(
        &common_pools.ethernet.pool_vpc_dpu_loopback_ip,
        txn,
        resource_pool::OwnerType::Machine,
        owner_id,
        None,
    )
    .await
    {
        Ok(val) => Ok(val),
        Err(crate::resource_pool::ResourcePoolDatabaseError::ResourcePool(
            resource_pool::ResourcePoolError::Empty,
        )) => {
            tracing::error!(
                owner_id,
                pool = "vpc-dpu-lo-ip",
                "Pool exhausted, cannot allocate"
            );
            Err(DatabaseError::ResourceExhausted(
                "pool vpc-dpu-lo-ip".to_string(),
            ))
        }
        Err(err) => {
            tracing::error!(owner_id, error = %err, pool = "lo-ip", "Error allocating from resource pool");
            Err(err.into())
        }
    }
}

/// Allocate a value from the secondary VTEP IP resource pool.
pub async fn allocate_secondary_vtep_ip(
    common_pools: &CommonPools,
    txn: &mut PgConnection,
    owner_id: &str,
) -> Result<IpAddr, DatabaseError> {
    match crate::resource_pool::allocate(
        &common_pools.ethernet.pool_secondary_vtep_ip,
        txn,
        resource_pool::OwnerType::Machine,
        owner_id,
        None,
    )
    .await
    {
        Ok(val) => Ok(val),
        Err(crate::resource_pool::ResourcePoolDatabaseError::ResourcePool(
            resource_pool::ResourcePoolError::Empty,
        )) => {
            tracing::error!(
                owner_id,
                pool = "secondary-vtep-ip",
                "Pool exhausted, cannot allocate"
            );
            Err(DatabaseError::ResourceExhausted(
                "pool secondary-vtep-ip".to_string(),
            ))
        }
        Err(err) => {
            tracing::error!(owner_id, error = %err, pool = "secondary-vtep-ip", "Error allocating from resource pool");
            Err(err.into())
        }
    }
}

pub async fn find_by_validation_id(
    txn: &mut PgConnection,
    validation_id: &MachineValidationId,
) -> Result<Option<Machine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            r#"{}
                WHERE m.discovery_machine_validation_id = $1
                OR m.cleanup_machine_validation_id = $1
                OR m.on_demand_machine_validation_id = $1"#,
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(validation_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

/// set_firmware_autoupdate flags a machine ID as explicitly having firmware upgrade enabled or disabled, or use config files if None.
pub async fn set_firmware_autoupdate(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    state: Option<bool>,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET firmware_autoupdate = $1 WHERE id = $2";
    sqlx::query(query)
        .bind(state)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_rack_fw_details(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    details: Option<&model::rack::RackFirmwareUpgradeStatus>,
) -> Result<(), DatabaseError> {
    let query =
        "UPDATE machines SET rack_fw_details = $1, updated = NOW() WHERE id = $2 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(details.map(|d| sqlx::types::Json(d.clone())))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn set_machine_validation_request(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    machine_validation_request: bool,
) -> Result<(), DatabaseError> {
    let query =
        "UPDATE machines SET on_demand_machine_validation_request=$2 WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(machine_validation_request)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_dpu_asns(
    db_pool: &Pool<Postgres>,
    common_pools: &CommonPools,
) -> Result<(), DatabaseError> {
    let mut txn = Transaction::begin(db_pool).await?;

    if crate::resource_pool::stats(txn.as_pgconn(), common_pools.ethernet.pool_fnn_asn.name())
        .await?
        .free
        == 0
    {
        tracing::info!(
            "Skipping update of DPU ASNs.  FNN ASN pool not configured or fully allocated"
        );
        return Ok(());
    }
    // Get all DPU IP addresses except the requester DPU machine
    let query = "SELECT id FROM machines WHERE starts_with(id, $1) AND asn IS NULL";

    let dpu_ids: Vec<MachineId> = sqlx::query_as(query)
        .bind(MachineType::Dpu.id_prefix())
        .fetch_all(txn.as_pgconn())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if !dpu_ids.is_empty() {
        tracing::info!(dpu_count = dpu_ids.len(), "Updating missing ASN of DPUs");
    }

    for dpu_machine_id in dpu_ids.iter() {
        let asn: i64 = crate::resource_pool::allocate(
            &common_pools.ethernet.pool_fnn_asn,
            &mut txn,
            resource_pool::OwnerType::Machine,
            &dpu_machine_id.to_string(),
            None,
        )
        .await? as i64;

        let query = "UPDATE machines set asn=$1 WHERE id=$2 and asn is null";

        sqlx::query(query)
            .bind(asn)
            .bind(dpu_machine_id)
            .execute(txn.as_pgconn())
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
    }

    txn.commit().await?;

    Ok(())
}

pub async fn assign_sku(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    sku_id: &str,
) -> Result<MachineId, DatabaseError> {
    let query = "UPDATE machines SET hw_sku=$1 WHERE id=$2 RETURNING id";

    let id = sqlx::query_as(query)
        .bind(sku_id)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("assign sku to machine", e))?;

    Ok(id)
}

pub async fn unassign_sku(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<MachineId, DatabaseError> {
    let query = "UPDATE machines SET hw_sku=NULL WHERE id=$1 RETURNING id";

    let id = sqlx::query_as(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("assign sku to machine", e))?;

    Ok(id)
}

pub async fn update_sku_status_last_match_attempt(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET hw_sku_status=jsonb_set(coalesce(hw_sku_status, '{}'), '{last_match_attempt}', $1) WHERE id=$2 RETURNING id";

    sqlx::query_as::<_, ()>(query)
        .bind(sqlx::types::Json(Utc::now()))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update sku last match attempt", e))?;

    Ok(())
}

pub async fn update_sku_status_last_generate_attempt(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET hw_sku_status=jsonb_set(coalesce(hw_sku_status, '{}'), '{last_generate_attempt}', $1) WHERE id=$2 RETURNING id";

    sqlx::query_as::<_, ()>(query)
        .bind(sqlx::types::Json(Utc::now()))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update sku last generate attempt", e))?;

    Ok(())
}

pub async fn update_sku_status_verify_request_time(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET hw_sku_status=jsonb_set(coalesce(hw_sku_status, '{}'), '{verify_request_time}', $1) WHERE id=$2 RETURNING id";

    sqlx::query_as::<_, ()>(query)
        .bind(sqlx::types::Json(Utc::now()))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update sku status", e))?;

    Ok(())
}

pub async fn update_sku_status_verify_request_time_for_sku(
    txn: &mut PgConnection,
    sku_id: &str,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET hw_sku_status=jsonb_set(coalesce(hw_sku_status, '{}'), '{verify_request_time}', $1) WHERE hw_sku=$2 RETURNING id";

    let ids: Vec<MachineId> = sqlx::query_as(query)
        .bind(sqlx::types::Json(Utc::now()))
        .bind(sku_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("update sku status", e))?;

    tracing::info!(machine_ids=?ids, "SKU updated, requesting verify for affected machines");
    Ok(())
}

pub async fn find_machine_ids_by_sku_ids(
    db_reader: impl DbReader<'_>,
    sku_ids: &[String],
) -> Result<HashMap<String, Vec<MachineId>>, DatabaseError> {
    let query = r#"SELECT skus.id, COALESCE(m.machine_ids, '[]'::jsonb) as associated_machine_ids
  FROM machine_skus AS skus
  LEFT JOIN LATERAL (
    SELECT jsonb_agg(m.id) AS machine_ids
    FROM machines AS m
    WHERE m.hw_sku = skus.id
  ) AS m ON TRUE
  WHERE skus.id=ANY($1)"#;

    #[derive(FromRow)]
    struct Result {
        id: String,
        associated_machine_ids: sqlx::types::Json<Vec<String>>,
    }

    let ids: Vec<Result> = sqlx::query_as(query)
        .bind(sku_ids)
        .fetch_all(db_reader)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // Build a HashMap of Sku ID to Vec<MachineId>
    Ok(ids
        .into_iter()
        .map(|result| {
            (
                result.id,
                result
                    .associated_machine_ids
                    .0
                    .into_iter()
                    .filter_map(|machine_id_str| machine_id_str.parse().ok())
                    .collect(),
            )
        })
        .collect())
}

pub async fn get_network_config(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Versioned<ManagedHostNetworkConfig>, DatabaseError> {
    #[derive(FromRow)]
    struct QueryResult {
        network_config: sqlx::types::Json<ManagedHostNetworkConfig>,
        network_config_version: ConfigVersion,
    }

    let query = "SELECT network_config, network_config_version FROM machines WHERE id=$1";

    let QueryResult {
        network_config,
        network_config_version,
    } = sqlx::query_as(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(Versioned::new(network_config.0, network_config_version))
}

pub async fn get_quarantine_state(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Option<ManagedHostQuarantineState>, DatabaseError> {
    let network_config = get_network_config(txn, machine_id).await?;
    Ok(network_config.value.quarantine_state)
}

pub async fn set_quarantine_state(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    quarantine_state: ManagedHostQuarantineState,
) -> Result<Option<ManagedHostQuarantineState>, DatabaseError> {
    let (mut network_config, network_config_version) =
        get_network_config(&mut *txn, machine_id).await?.take();
    let old_quarantine_state = network_config.quarantine_state.clone();
    network_config.quarantine_state = Some(quarantine_state);
    try_update_network_config(txn, machine_id, network_config_version, &network_config).await?;
    Ok(old_quarantine_state)
}

pub async fn clear_quarantine_state(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<Option<ManagedHostQuarantineState>, DatabaseError> {
    let (mut network_config, network_config_version) =
        get_network_config(&mut *txn, machine_id).await?.take();
    let old_quarantine_state = network_config.quarantine_state.clone();
    network_config.quarantine_state = None;
    try_update_network_config(txn, machine_id, network_config_version, &network_config).await?;
    Ok(old_quarantine_state)
}

pub async fn modify_dpf_state(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    status: bool,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines set dpf = jsonb_set(dpf, '{enabled}', to_jsonb($1)) WHERE id=$2";
    sqlx::query(query)
        .bind(status)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn mark_machine_ingestion_done_with_dpf(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines set dpf = jsonb_set(dpf, '{used_for_ingestion}', to_jsonb(true)) WHERE id=$1";
    sqlx::query(query)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_nvlink_info(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    nvlink_info: MachineNvLinkInfo,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET nvlink_info=$1 WHERE id=$2 RETURNING id";
    let _id: MachineId = sqlx::query_as(query)
        .bind(sqlx::types::Json(nvlink_info))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct _HealthReportWrapper {
    hardware_health_report: Option<HealthReport>,
}
impl<'r> FromRow<'r, PgRow> for _HealthReportWrapper {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let hardware_health_report: sqlx::types::Json<Option<HealthReport>> =
            row.try_get("health_report")?;
        Ok(Self {
            hardware_health_report: hardware_health_report.0,
        })
    }
}

/// RMS identity for a compute tray machine, including rack profile context for
/// node type resolution.
#[derive(Debug, sqlx::FromRow)]
pub struct MachineRmsIdentity {
    pub id: String,
    pub bmc_ip: IpAddr,
    pub bmc_mac_address: MacAddress,
    pub rack_id: Option<RackId>,
    pub rack_profile_id: Option<RackProfileId>,
}

/// Look up RMS identities and rack profile context for compute tray machines by
/// their BMC IP addresses.
pub async fn find_rms_identities_by_bmc_ips(
    db: impl crate::db_read::DbReader<'_>,
    bmc_ips: &[IpAddr],
) -> DatabaseResult<Vec<MachineRmsIdentity>> {
    let ip_strings: Vec<String> = bmc_ips.iter().map(ToString::to_string).collect();
    let sql = r#"
        SELECT
            m.id::text,
            mia.address AS bmc_ip,
            mi.mac_address AS bmc_mac_address,
            m.rack_id,
            r.rack_profile_id
        FROM machines m
        LEFT JOIN racks r ON r.id = m.rack_id
        JOIN machine_interfaces mi
            ON mi.machine_id = m.id
            AND mi.interface_type = 'Bmc'
        JOIN machine_interface_addresses mia
            ON mia.interface_id = mi.id
        WHERE host(mia.address) = ANY($1)
    "#;

    let rows: Vec<MachineRmsIdentity> = sqlx::query_as(sql)
        .bind(ip_strings)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::new("machine::find_rms_identities_by_bmc_ips", err))?;

    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    for row in &rows {
        if !seen.insert(row.bmc_ip) {
            return Err(DatabaseError::internal(format!(
                "duplicate machine RMS identity mapping for bmc_ip={}",
                row.bmc_ip
            )));
        }
    }
    Ok(rows)
}

pub fn count_healthy_unhealthy_host_machines(
    all_machines: &HashMap<MachineId, model::machine::ManagedHostStateSnapshot>,
) -> (i32, i32) {
    let without_fault_count = all_machines
        .iter()
        .filter(|(_, x)| {
            !x.aggregate_health
                .alerts
                .iter()
                .any(|x| x.id != *model::machine_update_module::HOST_UPDATE_HEALTH_PROBE_ID)
        })
        .count();

    (
        all_machines.len() as i32,
        (all_machines.len() - without_fault_count) as i32,
    )
}

#[cfg(test)]
mod test {
    use std::net::IpAddr;
    use std::str::FromStr;

    use carbide_uuid::machine::{MachineId, MachineInterfaceId};
    use carbide_uuid::network::NetworkSegmentId;
    use model::allocation_type::AllocationType;
    use model::bmc_info::BmcInfo;
    use model::hardware_info::HardwareInfo;
    use model::machine::ManagedHostState;
    use model::machine::machine_search_config::MachineSearchConfig;
    use model::machine::topology::{DiscoveryData, TopologyData};

    /// The machine snapshot reports the BMC IP from the *live* `machine_interface_addresses`
    /// row, and reports `None` once that address is gone -- it must never fall back to the
    /// stale copy cached in `machine_topologies.topology.bmc_info.ip`.
    ///
    /// Exercises the BMC subquery in `machine_snapshots.sql.template`, which now reads
    /// `host(bmc_addr.address)` directly instead of coalescing onto the topology copy.
    #[crate::sqlx_test]
    async fn test_snapshot_bmc_ip_follows_live_interface_not_stale_topology(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let live_bmc_ip: IpAddr = "10.1.2.3".parse()?;
        // A different value in the topology copy makes a fallback observable: if the
        // snapshot ever read the stale copy we'd see this instead of the live address.
        let stale_topology_ip: IpAddr = "10.9.9.9".parse()?;

        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;

        // A BMC interface attached to the machine, plus its live address (a DHCP lease).
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version) VALUES ($1, 'V1-T0') RETURNING id",
        )
        .bind("bmc-live-vs-stale")
        .fetch_one(txn.as_mut())
        .await?;
        let bmc_interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                 (machine_id, association_type, segment_id, mac_address,
                  primary_interface, hostname, interface_type)
             VALUES ($1, 'Machine', $2, $3::macaddr, false, 'bmc-live-vs-stale', 'Bmc')
             RETURNING id",
        )
        .bind(machine_id)
        .bind(segment_id)
        .bind("02:00:00:00:00:01")
        .fetch_one(txn.as_mut())
        .await?;
        crate::machine_interface_address::insert(
            txn.as_mut(),
            bmc_interface_id,
            live_bmc_ip,
            AllocationType::Dhcp,
        )
        .await?;

        // A topology row that *also* carries a (stale) BMC IP -- the removed fallback.
        let topology = TopologyData {
            discovery_data: DiscoveryData {
                info: HardwareInfo::default(),
            },
            bmc_info: BmcInfo {
                ip: Some(stale_topology_ip),
                ..Default::default()
            },
        };
        sqlx::query("INSERT INTO machine_topologies (machine_id, topology) VALUES ($1, $2::jsonb)")
            .bind(machine_id)
            .bind(sqlx::types::Json(&topology))
            .execute(txn.as_mut())
            .await?;

        // With a live address present, the snapshot reports it.
        let machine = super::find_one(txn.as_mut(), &machine_id, MachineSearchConfig::default())
            .await?
            .expect("machine snapshot should load");
        assert_eq!(machine.bmc_info.ip, Some(live_bmc_ip));

        // Release the lease: drop the live address but keep the interface and the topology
        // row (with its stale bmc_info.ip) intact.
        crate::machine_interface_address::delete(txn.as_mut(), &bmc_interface_id).await?;

        // The key assertion: with no live address, the snapshot reports None rather than
        // falling back to the stale topology copy.
        let machine = super::find_one(txn.as_mut(), &machine_id, MachineSearchConfig::default())
            .await?
            .expect("machine snapshot should load");
        assert_eq!(machine.bmc_info.ip, None);

        // Now drop the BMC interface row entirely; the topology row (with its stale
        // bmc_info.ip) stays. Even with no live BMC interface at all, the snapshot must
        // still report None -- it must not fall back to the stale copy via the outer COALESCE.
        sqlx::query("DELETE FROM machine_interfaces WHERE id = $1")
            .bind(bmc_interface_id)
            .execute(txn.as_mut())
            .await?;
        let machine = super::find_one(txn.as_mut(), &machine_id, MachineSearchConfig::default())
            .await?
            .expect("machine snapshot should load");
        assert_eq!(machine.bmc_info.ip, None);

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]

    async fn test_set_firmware_autoupdate(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await.unwrap();
        let id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        super::create(&mut txn, None, &id, ManagedHostState::Ready, None, 2).await?;
        super::set_firmware_autoupdate(&mut txn, &id, Some(true)).await?;
        txn.commit().await?;
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await.unwrap();

        let host = crate::machine::find_one(&mut *txn, &id, MachineSearchConfig::default())
            .await
            .unwrap()
            .unwrap();
        assert!(host.firmware_autoupdate.is_some());

        txn.commit().await?;
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await.unwrap();
        super::set_firmware_autoupdate(&mut txn, &id, None).await?;
        let host = crate::machine::find_one(&mut *txn, &id, MachineSearchConfig::default())
            .await
            .unwrap()
            .unwrap();
        assert!(host.firmware_autoupdate.is_none());
        Ok(())
    }
}
