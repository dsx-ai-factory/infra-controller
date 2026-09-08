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

use carbide_uuid::machine::{MachineId, MachineIdSubtypeTrait};
use chrono::{TimeDelta, Utc};
use itertools::Itertools;
use model::bmc_info::BmcInfo;
use model::hardware_info::HardwareInfo;
use model::machine::topology::{DiscoveryData, MachineTopology, TopologyData};
use sqlx::PgConnection;

use super::DatabaseError;
use crate::DatabaseResult;
use crate::db_read::DbReader;

async fn update(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    hardware_info: &HardwareInfo,
) -> DatabaseResult<MachineTopology> {
    let discovery_data = DiscoveryData {
        info: hardware_info.clone(),
    };

    tracing::info!(
        %machine_id,
        "Discovery data for machine already exists. Updating now.",
    );
    let query = "UPDATE machine_topologies SET topology=jsonb_set(topology, '{discovery_data}', $2::jsonb), topology_update_needed=false, updated=NOW() WHERE machine_id=$1 RETURNING *";
    let res = sqlx::query_as(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(&discovery_data))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(res)
}

pub async fn create_or_update(
    txn: &mut PgConnection,
    machine_id: &impl MachineIdSubtypeTrait,
    hardware_info: &HardwareInfo,
) -> DatabaseResult<MachineTopology> {
    let machine_id = machine_id.to_machine_id();
    let topology_data = find_latest_by_machine_ids(txn, &[machine_id]).await?;
    let topology_data = topology_data.get(&machine_id);

    if let Some(topology) = topology_data {
        if topology.topology_update_needed {
            return update(txn, &machine_id, hardware_info).await;
        }
        return Ok(topology.clone());
    }

    let topology_data = TopologyData {
        discovery_data: DiscoveryData {
            info: hardware_info.clone(),
        },
        bmc_info: BmcInfo {
            machine_interface_id: None,
            ip: None,
            port: None,
            mac: None,
            version: None,
            firmware_version: None,
        },
    };

    tracing::info!(
        %machine_id,
        "Discovery data for machine did not exist. Creating now.",
    );

    let query = "INSERT INTO machine_topologies VALUES ($1, $2::json) RETURNING *";
    let res = sqlx::query_as(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(&topology_data))
        .fetch_one(txn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db_err)
                if db_err.constraint() == Some("machine_topologies_machine_id_fkey") =>
            {
                tracing::error!(
                    %machine_id,
                    "Machine discovery failed: hardware reports a different machine id \
                    (Caused by installing a TPM without force-deleting the machine). Power off the machine, force-delete it, \
                    then re-ingest."
                );
                DatabaseError::FailedPrecondition(format!(
                    "Machine topology machine_id foreign key violation: {e}"
                ))
            }
            _ => DatabaseError::query(query, e),
        })?;

    Ok(res)
}

//  Wrapper for create_or_update to set topology_update_needed to true if bom_validation is enabled and
//  the last update was older than 1 day.
pub async fn create_or_update_with_bom_validation(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    hardware_info: &HardwareInfo,
    bom_validation_enabled: bool,
) -> DatabaseResult<MachineTopology> {
    let topology_data = find_latest_by_machine_ids(txn, &[*machine_id]).await?;
    let topology_data = topology_data.get(machine_id);

    if let Some(topology) = topology_data {
        let age = Utc::now() - topology.updated;
        if bom_validation_enabled && age > TimeDelta::days(1) {
            tracing::debug!(
                machine_id = %machine_id,
                inventory_age_days = age.num_days(),
                "Received stale inventory update while BOM validation is enabled",
            );
            set_topology_update_needed(txn, machine_id, true).await?;
        }
    }

    create_or_update(txn, machine_id, hardware_info).await
}

// update_firmware_version_by_machine_id updates the stored firmware version info for a machine.
pub async fn update_firmware_version_by_machine_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    bmc_version: &str,
    bios_version: &str,
) -> DatabaseResult<()> {
    // The IS NOT NULL checks that we're not partially creating stuff under an Option when adding a bios_version.
    let query = r#"UPDATE machine_topologies mt SET topology =
                        jsonb_set(jsonb_set(topology, '{bmc_info}',
                            jsonb_set(topology->'bmc_info', '{firmware_version}', $2)),
                            '{discovery_data}',
                                 jsonb_set(topology->'discovery_data', '{Info}',
                                            jsonb_set(topology->'discovery_data'->'Info', '{dmi_data}',
                                                         jsonb_set(topology->'discovery_data'->'Info'->'dmi_data', '{bios_version}', $3))
                        ))
                    WHERE mt.machine_id = $1
                        AND topology->'discovery_data'->'Info'->'dmi_data'->'bios_version' IS NOT NULL;"#;

    sqlx::query(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(bmc_version))
        .bind(sqlx::types::Json(bios_version))
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Lock every topology row for a machine.
///
/// Site Explorer updates topology rows before the corresponding
/// `explored_endpoints` row. Callers that touch both tables must acquire them
/// in the same order to avoid deadlocks.
pub async fn lock_by_machine_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> DatabaseResult<()> {
    let query = "SELECT machine_id FROM machine_topologies WHERE machine_id = $1 FOR UPDATE";
    sqlx::query(query)
        .bind(machine_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn find_by_machine_ids<ID: MachineIdSubtypeTrait>(
    txn: &mut PgConnection,
    machine_ids: &[ID],
) -> Result<HashMap<ID, Vec<MachineTopology>>, DatabaseError> {
    // TODO: Actually this shouldn't be able to return multiple entries,
    // since there is a check in create that for existing interfaces
    // But due to race conditions we can likely still have multiple of those interfaces
    let str_ids: Vec<String> = machine_ids.iter().map(|id| id.to_string()).collect();
    let query = "SELECT * FROM machine_topologies WHERE machine_id=ANY($1)";
    let topologies = sqlx::query_as(query)
        .bind(str_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .into_iter()
        .filter_map(|t: MachineTopology| Some((ID::try_from(t.machine_id).ok()?, t)))
        .into_group_map();
    Ok(topologies)
}

pub async fn find_latest_by_machine_ids<ID: MachineIdSubtypeTrait>(
    txn: &mut PgConnection,
    machine_ids: &[ID],
) -> Result<HashMap<ID, MachineTopology>, DatabaseError> {
    // TODO: So far this just moved code around
    // This way of doing fetching the latest topology is inefficient, because it will still fetch all
    // information. We can change the query - however if we store information
    // later on directly as part of the Machine or instance this might
    // be unnecessary.
    let all = find_by_machine_ids(txn, machine_ids).await?;

    let mut result = HashMap::new();
    for (id, mut topos) in all {
        let topo = topos
            .drain(..)
            .reduce(|t1, t2| if t1.created() > t2.created() { t1 } else { t2 });
        if let Some(topo) = topo {
            result.insert(id, topo);
        }
    }

    Ok(result)
}

pub async fn find_machine_id_by_bmc_ip(
    txn: impl DbReader<'_>,
    address: &str,
) -> Result<Option<MachineId>, DatabaseError> {
    let query = r#"
        SELECT mi.machine_id
        FROM machine_interfaces mi
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        WHERE mi.interface_type = 'Bmc'
            AND mi.machine_id IS NOT NULL
            AND mia.address = $1::inet
    "#;
    sqlx::query_as(query)
        .bind(address)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_machine_id_by_bmc_mac(
    txn: &mut PgConnection,
    mac_address: mac_address::MacAddress,
) -> Result<Option<MachineId>, DatabaseError> {
    let query = r#"
        SELECT machine_id
        FROM machine_interfaces
        WHERE interface_type = 'Bmc'
            AND machine_id IS NOT NULL
            AND mac_address = $1::macaddr
    "#;
    sqlx::query_as(query)
        .bind(mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_machine_bmc_pairs(
    txn: impl DbReader<'_>,
    bmc_ips: Vec<String>,
) -> Result<Vec<(MachineId, String)>, DatabaseError> {
    let query = r#"
        SELECT mi.machine_id, host(mia.address)
        FROM machine_interfaces mi
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        WHERE mi.interface_type = 'Bmc'
            AND mi.machine_id IS NOT NULL
            AND host(mia.address) = ANY($1)
    "#;
    sqlx::query_as(query)
        .bind(bmc_ips)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("machine_topologies find_machine_bmc_pairs", e))
}

/// Find the BMC IP address for each of the given machine IDs.
///
/// Returns a list of (machine_id, bmc_ip) pairs from BMC interface links.
///
/// The BMC IP is returned as `Option<String>`:
/// - `Some(ip)` if the topology has a valid BMC IP
/// - `None` if the linked interface exists but has no BMC IP (caller can log/handle this case)
pub async fn find_machine_bmc_pairs_by_machine_id(
    txn: &mut PgConnection,
    machine_ids: Vec<MachineId>,
) -> Result<Vec<(MachineId, Option<String>)>, DatabaseError> {
    let query = r#"
        SELECT DISTINCT ON (mi.machine_id) mi.machine_id, host(mia.address)
        FROM machine_interfaces mi
        LEFT JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        WHERE mi.interface_type = 'Bmc'
            AND mi.machine_id = ANY($1)
        ORDER BY mi.machine_id, family(mia.address), mia.address
    "#;
    sqlx::query_as(query)
        .bind(machine_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| {
            DatabaseError::new("machine_topologies find_machine_bmc_pairs_by_machine_id", e)
        })
}

/// Find any topology with a product, chassis, or board serial number exactly matching the input.
///
/// NOTE: This query must exactly match the index machine_topologies_serial_numbers_idx, which
/// will make this a fast operation that doesn't need to sequentially scan. DO NOT change this
/// query without also changing the index!
pub async fn find_by_serial(
    txn: impl DbReader<'_>,
    to_find: &str,
) -> Result<Vec<MachineId>, DatabaseError> {
    let query = r#"
            SELECT machine_id
            FROM   machine_topologies
            WHERE
            (
                jsonb_path_query_array(topology,
                    '$.discovery_data.Info.dmi_data.product_serial')
            ||
                jsonb_path_query_array(topology,
                    '$.discovery_data.Info.dmi_data.board_serial')
            ||
                jsonb_path_query_array(topology,
                    '$.discovery_data.Info.dmi_data.chassis_serial')
            ) @> to_jsonb(ARRAY[$1]);
        "#;
    sqlx::query_as::<_, MachineId>(query)
        .bind(to_find)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("machine_topologies find_by_serial", e))
}

/// Returns a machine's hardware serial -- product, then board, then chassis --
/// from its discovered topology, or `None` if no topology (or no serial) has
/// been recorded. Reads the same JSON path that [`find_by_serial`] matches.
pub async fn serial_for_machine(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> DatabaseResult<Option<String>> {
    let query = r#"
        SELECT COALESCE(
            NULLIF(topology #>> '{discovery_data,Info,dmi_data,product_serial}', ''),
            NULLIF(topology #>> '{discovery_data,Info,dmi_data,board_serial}', ''),
            NULLIF(topology #>> '{discovery_data,Info,dmi_data,chassis_serial}', '')
        )
        FROM machine_topologies
        WHERE machine_id = $1
    "#;
    sqlx::query_scalar::<_, Option<String>>(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map(Option::flatten)
        .map_err(|e| DatabaseError::query(query, e))
}

/// Search the topologyfor a string anywhere in the JSON.
/// Used by the serial number finder for non-exact matches
pub async fn find_freetext(
    txn: impl DbReader<'_>,
    to_find: &str,
) -> Result<Vec<MachineId>, DatabaseError> {
    let query =
        "SELECT machine_id FROM machine_topologies WHERE topology::text ilike '%' || $1 || '%'";
    sqlx::query_as::<_, MachineId>(query)
        .bind(to_find)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("machine_topologies find_freetext", e))
}

pub async fn set_topology_update_needed(
    txn: &mut PgConnection,
    machine_id: &impl MachineIdSubtypeTrait,
    value: bool,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machine_topologies SET topology_update_needed=$2 WHERE machine_id=$1 RETURNING machine_id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id.as_machine_id())
        .bind(value)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}
