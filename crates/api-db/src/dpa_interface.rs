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

use std::collections::HashSet;
use std::net::IpAddr;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use carbide_uuid::dpa_interface::{DpaInterfaceId, NULL_DPA_INTERFACE_ID};
use carbide_uuid::machine::HostMachineId;
use config_version::ConfigVersion;
use mac_address::MacAddress;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::dpa_interface::{
    DpaInterface, DpaInterfaceControllerState, DpaInterfaceNetworkConfig, DpaSearchConfig,
    NewDpaInterface,
};
use sqlx::PgConnection;

use super::DatabaseError;
use crate::db_read::DbReader;

pub async fn persist(
    value: NewDpaInterface,
    txn: &mut PgConnection,
) -> Result<DpaInterface, DatabaseError> {
    let network_config_version = ConfigVersion::initial();
    let network_config = DpaInterfaceNetworkConfig::default();
    let state_version = ConfigVersion::initial();
    let state = DpaInterfaceControllerState::Provisioning;
    let description = value.device_description.unwrap_or_default();

    let query = "INSERT INTO dpa_interfaces (machine_id, mac_address, network_config_version, network_config, controller_state_version, controller_state, device_type, pci_name, device_description, interface_type)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING row_to_json(dpa_interfaces.*)";

    sqlx::query_as(query)
        .bind(value.machine_id.to_string())
        .bind(value.mac_address)
        .bind(network_config_version)
        .bind(sqlx::types::Json(&network_config))
        .bind(state_version)
        .bind(sqlx::types::Json(&state))
        .bind(value.device_type)
        .bind(value.pci_name)
        .bind(description)
        .bind(value.interface_type)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// ensure inserts a new interface if one doesn't already exist for
/// the given (machine_id, mac_address), or returns the existing one if
/// it does. This makes it so repeated processing of device reports from
/// scout are handled gracefully.
pub async fn ensure(
    value: NewDpaInterface,
    txn: &mut PgConnection,
) -> Result<DpaInterface, DatabaseError> {
    let network_config_version = ConfigVersion::initial();
    let network_config = DpaInterfaceNetworkConfig::default();
    let state_version = ConfigVersion::initial();
    let state = DpaInterfaceControllerState::Provisioning;
    let description = value.device_description.unwrap_or_default();

    let insert_query = "INSERT INTO dpa_interfaces (machine_id, mac_address, network_config_version, network_config, controller_state_version, controller_state, device_type, pci_name, device_description, interface_type)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (machine_id, mac_address) DO NOTHING RETURNING row_to_json(dpa_interfaces.*)";

    let result: Option<DpaInterface> = sqlx::query_as(insert_query)
        .bind(value.machine_id.to_string())
        .bind(value.mac_address)
        .bind(network_config_version)
        .bind(sqlx::types::Json(&network_config))
        .bind(state_version)
        .bind(sqlx::types::Json(&state))
        .bind(value.device_type)
        .bind(value.pci_name)
        .bind(description)
        .bind(value.interface_type)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(insert_query, e))?;

    // A new interface was inserted, return it!
    if let Some(interface) = result {
        return Ok(interface);
    }

    // ...nope, we got nothin. Fetch and return
    // the existing interface instead.
    let select_query = "SELECT row_to_json(m.*) FROM (SELECT * FROM dpa_interfaces WHERE deleted IS NULL AND machine_id = $1 AND mac_address = $2) m";
    sqlx::query_as(select_query)
        .bind(value.machine_id)
        .bind(value.mac_address)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(select_query, e))
}

// Update the last_hb_time field with the current timestamp for the given DPA interface
// and return the DPA Interface ID
pub async fn update_last_hb_time(
    value: &DpaInterface,
    txn: &mut PgConnection,
) -> Result<DpaInterfaceId, DatabaseError> {
    let query = "UPDATE dpa_interfaces SET last_hb_time = NOW() WHERE id = $1::uuid
                RETURNING id";

    sqlx::query_as(query)
        .bind(value.id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

// Update the underlay or the overlay ip address of the given DPA interface object
pub async fn update_ip(
    value: DpaInterface,
    underlay: bool,
    txn: &mut PgConnection,
) -> Result<DpaInterfaceId, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("Update dpa_interfaces SET ");

    if underlay {
        builder.push(" underlay_ip=");
        builder.push_bind(value.underlay_ip.unwrap());
    } else {
        builder.push(" overlay_ip=");
        builder.push_bind(value.overlay_ip.unwrap());
    }

    builder.push(" WHERE id=");
    builder.push_bind(value.id);

    builder.push(" RETURNING id");

    builder
        .build_query_as()
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

pub async fn find_ids(txn: impl DbReader<'_>) -> Result<Vec<DpaInterfaceId>, DatabaseError> {
    let query = "SELECT id from dpa_interfaces WHERE deleted is NULL";

    let results: Vec<DpaInterfaceId> = {
        sqlx::query_as(query)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
    };

    Ok(results)
}

// Given an IP address, find and return the DPA interface that has the given IP
// as its underlay or overlay IP address.
pub async fn find_by_ip(
    txn: impl DbReader<'_>,
    ipaddr: IpAddr,
) -> Result<Vec<DpaInterface>, DatabaseError> {
    let query = "SELECT row_to_json(m.*) from (select * from dpa_interfaces
        WHERE deleted is NULL AND underlay_ip = $1 or overlay_ip = $2) m";

    let results: Vec<DpaInterface> = {
        sqlx::query_as(query)
            .bind(ipaddr)
            .bind(ipaddr)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
    };

    Ok(results)
}

// get_for_pci_name gets the DpaInterface for a specific device
// on a machine, based on its PCI name, which may be either the PCIe
// address or /dev/mst address.
//
// Returns exactly one DpaInterface, or an error if none or multiple
// are found, because multiple would not make sense.
pub async fn get_for_pci_name(
    txn: impl DbReader<'_>,
    machine_id: &HostMachineId,
    pci_name: &str,
) -> Result<DpaInterface, DatabaseError> {
    let query = "SELECT row_to_json(m.*) from (select * from dpa_interfaces WHERE deleted is NULL AND machine_id = $1 AND pci_name = $2) m";

    let results: Vec<DpaInterface> = sqlx::query_as(query)
        .bind(machine_id)
        .bind(pci_name)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    match results.len() {
        0 => Err(DatabaseError::NotFoundError {
            kind: "dpa_interface",
            id: format!("machine_id={machine_id}, pci_name={pci_name}"),
        }),
        1 => Ok(results.into_iter().next().unwrap()),
        n => Err(DatabaseError::Internal {
            message: format!(
                "expected 1 dpa_interface for machine_id={machine_id}, pci_name={pci_name}, found {n}"
            ),
        }),
    }
}

// Find a DPA Interface given its mac address. When we receive messages from the MQTT broker,
// the topic contains the mac address, and we look up the interface based on that mac address.
pub async fn find_by_mac_addr(
    txn: impl DbReader<'_>,
    maddr: &MacAddress,
) -> Result<Vec<DpaInterface>, DatabaseError> {
    let query = "SELECT row_to_json(m.*) from (select * from dpa_interfaces WHERE deleted is NULL AND mac_address = $1) m";

    let results: Vec<DpaInterface> = {
        sqlx::query_as(query)
            .bind(maddr)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
    };

    Ok(results)
}

/// update_device_info updates the device_info JSONB column and
/// timestamp for a DPA interface, storing the full MlxDeviceInfo as
/// reported by the device. Called on every device report so that the
/// latest hardware info (part_number, psid, fw_version_current, etc)
/// is always available.
pub async fn update_device_info(
    txn: &mut PgConnection,
    machine_id: HostMachineId,
    pci_name: &str,
    device_info: &MlxDeviceInfo,
) -> Result<(), DatabaseError> {
    let query = "UPDATE dpa_interfaces SET device_info = $1::jsonb, device_info_ts = NOW() WHERE machine_id = $2 AND pci_name = $3 AND deleted IS NULL";
    sqlx::query(query)
        .bind(sqlx::types::Json(device_info))
        .bind(machine_id)
        .bind(pci_name)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_card_state(
    txn: &mut PgConnection,
    value: DpaInterface,
) -> Result<DpaInterfaceId, DatabaseError> {
    let query = "UPDATE dpa_interfaces SET card_state = $1::json WHERE id = $2::uuid
                RETURNING id";

    sqlx::query_as(query)
        .bind(sqlx::types::Json(&value.card_state))
        .bind(value.id.to_string())
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// The `only_svpc` and `only_astra` filters are mutually exclusive.
fn validate_search_config(search_config: &DpaSearchConfig) -> Result<(), DatabaseError> {
    if search_config.only_svpc && search_config.only_astra {
        return Err(DatabaseError::Internal {
            message: "only_svpc and only_astra cannot be true at the same time".to_string(),
        });
    }
    Ok(())
}

// Used by the machine statemachine controller to find all DPAs associated with a given machine
pub async fn find_by_machine_id(
    txn: impl DbReader<'_>,
    machine_id: HostMachineId,
    search_config: DpaSearchConfig,
) -> Result<Vec<DpaInterface>, DatabaseError> {
    validate_search_config(&search_config)?;

    let mut builder = sqlx::QueryBuilder::new(
        "SELECT row_to_json(m.*) from (select * from dpa_interfaces WHERE deleted is NULL AND machine_id = $1",
    );

    if search_config.only_svpc {
        builder.push(" AND interface_type = 'Svpc'");
    }

    if search_config.only_astra {
        builder.push(" AND interface_type = 'Astra'");
    }

    builder.push(") m");

    let results: Vec<DpaInterface> = {
        builder
            .build_query_as()
            .bind(machine_id)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(builder.sql(), e))?
    };

    Ok(results)
}

/// Batch-load DPA interfaces for many machines in a single query.
///
/// This is the set-oriented counterpart to [`find_by_machine_id`]: callers that
/// have already batch-loaded a group of hosts can attach every host's DPA
/// interfaces with one round trip instead of one query per machine. The returned
/// map is keyed by machine id; a machine with no interfaces simply has no entry
/// (callers default such machines to an empty list).
pub async fn find_by_machine_ids(
    txn: impl DbReader<'_>,
    machine_ids: &[HostMachineId],
    search_config: DpaSearchConfig,
) -> Result<std::collections::HashMap<HostMachineId, Vec<DpaInterface>>, DatabaseError> {
    validate_search_config(&search_config)?;

    // No machines means no interfaces; skip the round trip entirely.
    if machine_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let mut builder = sqlx::QueryBuilder::new(
        "SELECT row_to_json(m.*) from (select * from dpa_interfaces WHERE deleted is NULL AND machine_id = ANY(",
    );
    builder.push_bind(machine_ids);
    builder.push(")");

    if search_config.only_svpc {
        builder.push(" AND interface_type = 'Svpc'");
    }

    if search_config.only_astra {
        builder.push(" AND interface_type = 'Astra'");
    }

    builder.push(") m");

    let interfaces: Vec<DpaInterface> = builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))?;

    Ok(interfaces.into_iter().fold(
        std::collections::HashMap::<HostMachineId, Vec<DpaInterface>>::new(),
        |mut by_machine, interface| {
            by_machine
                .entry(interface.machine_id)
                .or_default()
                .push(interface);
            by_machine
        },
    ))
}

/// A SpectrumX attachment-selector group projected from non-deleted DPA interfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpectrumXDeviceCapability {
    /// The non-empty device description used as the attachment `device` selector.
    pub device: String,
    /// The number of interfaces in the group. Valid `device_instance` selectors
    /// are `0..count`, resolved in PCI-name order during attachment allocation.
    pub count: u32,
}

/// Batch-load SpectrumX attachment-selector inventory for a set of host machines.
///
/// This intentionally projects and aggregates only the fields needed by
/// machine capability discovery instead of loading complete DPA-interface rows,
/// whose configuration and device-info JSON can be substantially wider. This
/// inventory does not represent site-level SpectrumX enablement or readiness.
pub async fn find_spectrum_x_capabilities_by_machine_ids(
    txn: impl DbReader<'_>,
    machine_ids: &[HostMachineId],
) -> Result<std::collections::HashMap<HostMachineId, Vec<SpectrumXDeviceCapability>>, DatabaseError>
{
    if machine_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    #[derive(sqlx::FromRow)]
    struct SpectrumXCapabilityRow {
        machine_id: HostMachineId,
        device: String,
        count: i64,
    }

    let query = "SELECT machine_id, device_description AS device, COUNT(*) AS count
        FROM dpa_interfaces
        WHERE deleted IS NULL
          AND machine_id = ANY($1)
          AND device_description IS NOT NULL
          AND device_description <> ''
        GROUP BY machine_id, device_description
        ORDER BY machine_id, device_description";
    let rows = sqlx::query_as::<_, SpectrumXCapabilityRow>(query)
        .bind(machine_ids)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    let mut capabilities_by_machine =
        std::collections::HashMap::<HostMachineId, Vec<SpectrumXDeviceCapability>>::new();
    for row in rows {
        let count = row
            .count
            .try_into()
            .map_err(|error| DatabaseError::Internal {
                message: format!(
                    "SpectrumX interface count {} for machine {} and device {:?} does not fit in u32: {error}",
                    row.count, row.machine_id, row.device,
                ),
            })?;
        capabilities_by_machine
            .entry(row.machine_id)
            .or_default()
            .push(SpectrumXDeviceCapability {
                device: row.device,
                count,
            });
    }

    Ok(capabilities_by_machine)
}

pub async fn find_by_ids(
    txn: impl DbReader<'_>,
    dpa_ids: &[DpaInterfaceId],
    include_history: bool,
) -> Result<Vec<DpaInterface>, DatabaseError> {
    let mut builder = if include_history {
        sqlx::QueryBuilder::new("select row_to_json(m.*) from
                (SELECT si.*, COALESCE(history_agg.json, '[]'::json) AS history FROM dpa_interfaces si
                LEFT JOIN LATERAL (
                SELECT h.object_id, json_agg(json_build_object('interface_id', h.object_id, 'state', h.state::text, 'state_version', h.state_version,
                'timestamp', h.timestamp) ORDER BY h.id ASC) AS json FROM dpa_interface_state_history h WHERE h.object_id = si.id::text GROUP BY h.object_id ) AS history_agg ON true
                WHERE deleted is NULL")
    } else {
        sqlx::QueryBuilder::new(
            "SELECT row_to_json(m.*) from (select * from dpa_interfaces WHERE deleted is NULL",
        )
    };

    builder.push(" AND id = ANY(");
    builder.push_bind(dpa_ids);
    builder.push(")) m");

    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|err: sqlx::Error| DatabaseError::query(builder.sql(), err))
}

// Given a DPA Interface ID, return a vector of the DPA Interface structures
// of all the DPAs in the same host machine.
pub async fn get_dpas_in_machine(
    txn: &mut PgConnection,
    id: DpaInterfaceId,
) -> Result<Vec<DpaInterface>, DatabaseError> {
    // let query = "SELECT * row_to_json(m.*) from (select * from dpa_interfaces e
    // deleted is NULL AND machine_id = (SELECT machine_id from dpa_interfaces WHERE id = $1)) m";
    let query = "SELECT row_to_json(m)
                        FROM dpa_interfaces m
                        JOIN dpa_interfaces d ON d.id = $1
                        WHERE m.deleted IS NULL
                        AND m.machine_id = d.machine_id";

    let results: Vec<DpaInterface> = {
        sqlx::query_as(query)
            .bind(id)
            .fetch_all(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
    };

    Ok(results)
}

// Return true if all the dpas in the same machine as the given dpa (self) are in the
// same state. Return false otherwise.
// Use this in places where we need to move the DPAs in lockstep (i.e. all the DPAs have
// to be in the same state before we move to the next state).
pub async fn all_dpa_states_in_sync(
    value: &DpaInterface,
    txn: &mut PgConnection,
) -> Result<bool, DatabaseError> {
    let dpas_vec = get_dpas_in_machine(txn, value.id).await?;

    for dpa in &dpas_vec {
        if dpa.controller_state.value != value.controller_state.value {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Updates the dpa interface state that is owned by the state controller
/// under the premise that the current controller state version didn't change.
///
/// Returns `true` if the state could be updated, and `false` if the object
/// either doesn't exist anymore or is at a different version.
pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    id: DpaInterfaceId,
    expected_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &DpaInterfaceControllerState,
) -> Result<bool, DatabaseError> {
    let query = "UPDATE dpa_interfaces SET controller_state_version=$1, controller_state=$2::json where id=$3::uuid AND controller_state_version=$4 returning id";
    let result = sqlx::query_as::<_, DpaInterfaceId>(query)
        .bind(new_version)
        .bind(sqlx::types::Json(new_state))
        .bind(id)
        .bind(expected_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(result.is_some())
}

pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    id: DpaInterfaceId,
    outcome: PersistentStateHandlerOutcome,
) -> Result<(), DatabaseError> {
    let query = "UPDATE dpa_interfaces SET controller_state_outcome=$1::json WHERE id=$2";
    sqlx::query(query)
        .bind(sqlx::types::Json(outcome))
        .bind(id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn delete(value: DpaInterface, txn: &mut PgConnection) -> Result<(), DatabaseError> {
    let query = "delete from dpa_interfaces where id=$1";
    sqlx::query(query)
        .bind(value.id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
        .map(|_| ())
}

pub async fn is_machine_dpa_capable(
    txn: &mut PgConnection,
    machine_id: HostMachineId,
) -> Result<bool, DatabaseError> {
    let result = batch_is_machine_dpa_capable(txn, &[machine_id]).await?;
    Ok(result.contains(&machine_id))
}

/// Batch check which machines are DPA capable.
/// Returns a HashSet of machine IDs that have DPA interfaces.
pub async fn batch_is_machine_dpa_capable(
    txn: &mut PgConnection,
    machine_ids: &[HostMachineId],
) -> Result<HashSet<HostMachineId>, DatabaseError> {
    if machine_ids.is_empty() {
        return Ok(HashSet::new());
    }

    let query = "SELECT DISTINCT machine_id FROM dpa_interfaces
                 WHERE deleted IS NULL AND machine_id = ANY($1)";

    Ok(sqlx::query_scalar(query)
        .bind(machine_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .into_iter()
        .collect())
}

/// Updates the desired network configuration for a host
pub async fn try_update_network_config(
    txn: &mut PgConnection,
    interface_id: &DpaInterfaceId,
    expected_version: ConfigVersion,
    new_state: &DpaInterfaceNetworkConfig,
) -> Result<DpaInterfaceId, DatabaseError> {
    let next_version = expected_version.increment();

    let query = "UPDATE dpa_interfaces SET network_config_version=$1, network_config=$2::json
            WHERE id=$3::uuid AND network_config_version=$4
            RETURNING id";
    let query_result: Result<DpaInterfaceId, _> = sqlx::query_as(query)
        .bind(next_version)
        .bind(sqlx::types::Json(new_state))
        .bind(interface_id)
        .bind(expected_version)
        .fetch_one(txn)
        .await;

    match query_result {
        Ok(interface_id) => Ok(interface_id),
        Err(sqlx::Error::RowNotFound) => Ok(NULL_DPA_INTERFACE_ID),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use carbide_libmlx_model::device::info::MlxDeviceInfo;
    use carbide_test_support::query_counter::count_queries;
    use carbide_uuid::machine::{
        HostMachineId as MachineId, MachineId as GenericMachineId, MachineIdSource, MachineType,
    };
    use mac_address::MacAddress;
    use model::dpa_interface::{
        DpaInterfaceControllerState, DpaInterfaceType, DpaSearchConfig, NewDpaInterface,
    };
    use model::machine::ManagedHostState;

    use crate::machine;

    /// Query-count regression guard for the batched DPA-interface loader.
    ///
    /// This test is the deliverable of the N+1 fix: it seeds several machines,
    /// each with one interface, and asserts that loading them via the
    /// per-machine [`find_by_machine_id`] loop issues one query *per machine*
    /// (the N+1), while the batched [`find_by_machine_ids`] issues exactly one
    /// query regardless of how many machines are involved. The per-machine
    /// count is asserted to equal N so that a future regression that quietly
    /// reintroduces per-row queries fails loudly.
    #[crate::sqlx_test]
    async fn find_by_machine_ids_issues_one_query(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Seed helper: create `n` distinct machines, each with exactly one
        // dpa_interface, and return their ids. Each MachineId is minted from a
        // per-index hardware hash (the same shape as the `test_machine_id`
        // helpers elsewhere in the tree), so every id is distinct and valid.
        async fn seed(
            pool: &sqlx::PgPool,
            offset: usize,
            n: usize,
        ) -> Result<Vec<MachineId>, Box<dyn std::error::Error>> {
            let mut ids = Vec::with_capacity(n);
            let mut txn = pool.begin().await?;
            for i in offset..(offset + n) {
                let mut hash = [0u8; 32];
                hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
                let id: MachineId = GenericMachineId::new(
                    MachineIdSource::ProductBoardChassisSerial,
                    hash,
                    MachineType::Host,
                )
                .try_into()
                .unwrap();
                machine::create(&mut txn, None, &id, ManagedHostState::Ready, None, 2).await?;
                crate::dpa_interface::persist(
                    NewDpaInterface {
                        machine_id: id,
                        mac_address: MacAddress::from([
                            0x00,
                            0x11,
                            0x22,
                            0x33,
                            (i >> 8) as u8,
                            i as u8,
                        ]),
                        device_type: "Bluefield 3".to_string(),
                        pci_name: format!("{i:02x}:00.0"),
                        device_description: None,
                        interface_type: DpaInterfaceType::Svpc,
                    },
                    &mut txn,
                )
                .await?;
                ids.push(id);
            }
            txn.commit().await?;
            Ok(ids)
        }

        // ---- Scenario 1: N = 5 ----
        const N: usize = 5;
        let ids = seed(&pool, 0, N).await?;

        // BEFORE: the per-machine loop (the N+1 pattern).
        let (per_machine, before): (std::collections::HashMap<MachineId, Vec<_>>, usize) = {
            let pool = &pool;
            let ids = &ids;
            count_queries(async move {
                let mut out = std::collections::HashMap::new();
                for id in ids {
                    let v = crate::dpa_interface::find_by_machine_id(
                        pool,
                        *id,
                        DpaSearchConfig::default(),
                    )
                    .await
                    .unwrap();
                    out.insert(*id, v);
                }
                out
            })
            .await
        };
        println!("BEFORE (per-machine loop, N={N}): {before} queries");

        // BITE-CHECK: the per-machine loop MUST issue one query per machine.
        // If this reads 0, the counter is not observing sqlx events; if it
        // reads 1, the counter is under-counting. Either way, fail loudly.
        assert_eq!(
            before, N,
            "per-machine loop should issue exactly N={N} queries (the N+1 pattern being fixed); \
             a count of 0 means the query counter isn't seeing sqlx::query events"
        );

        // AFTER: the batched loader.
        let (batched, after) = {
            let pool = &pool;
            let ids = &ids;
            count_queries(async move {
                crate::dpa_interface::find_by_machine_ids(pool, ids, DpaSearchConfig::default())
                    .await
                    .unwrap()
            })
            .await
        };
        println!("AFTER (batched find_by_machine_ids, N={N}): {after} queries");
        assert_eq!(after, 1, "batched loader must issue exactly one query");

        // Correctness: the batched map holds the same interfaces (by id) as the
        // per-machine loop produced, for every machine.
        for id in &ids {
            let mut expected: Vec<_> = per_machine[id].iter().map(|i| i.id).collect();
            let mut got: Vec<_> = batched
                .get(id)
                .map(|v| v.iter().map(|i| i.id).collect())
                .unwrap_or_default();
            expected.sort();
            got.sort();
            assert_eq!(
                got, expected,
                "batched result for machine {id} must match the per-machine loop"
            );
        }

        // ---- Scenario 2: N = 10, batched count STILL 1 (constant, not linear) ----
        const N2: usize = 10;
        let ids2 = seed(&pool, 100, N2).await?;

        // Re-confirm the per-machine loop scales linearly at N2 as well, so the
        // "constant vs linear" contrast is anchored on both sides.
        let ((), before2) = {
            let pool = &pool;
            let ids2 = &ids2;
            count_queries(async move {
                for id in ids2 {
                    crate::dpa_interface::find_by_machine_id(pool, *id, DpaSearchConfig::default())
                        .await
                        .unwrap();
                }
            })
            .await
        };
        println!("BEFORE (per-machine loop, N={N2}): {before2} queries");
        assert_eq!(
            before2, N2,
            "per-machine loop should issue exactly N={N2} queries"
        );

        let (batched2, after2) = {
            let pool = &pool;
            let ids2 = &ids2;
            count_queries(async move {
                crate::dpa_interface::find_by_machine_ids(pool, ids2, DpaSearchConfig::default())
                    .await
                    .unwrap()
            })
            .await
        };
        println!("AFTER (batched find_by_machine_ids, N={N2}): {after2} queries");
        assert_eq!(
            after2, 1,
            "batched loader must issue exactly one query regardless of N"
        );
        assert_eq!(
            batched2.len(),
            N2,
            "every seeded machine should appear in the batched result"
        );

        // ---- Scenario 3: N = 0, no query at all ----
        // An empty id slice short-circuits before building the query, matching
        // the sibling batch helpers.
        let (batched_empty, empty_count) = {
            let pool = &pool;
            count_queries(async move {
                crate::dpa_interface::find_by_machine_ids(pool, &[], DpaSearchConfig::default())
                    .await
                    .unwrap()
            })
            .await
        };
        println!("EMPTY (batched find_by_machine_ids, N=0): {empty_count} queries");
        assert_eq!(
            empty_count, 0,
            "empty input must return without issuing any query"
        );
        assert!(
            batched_empty.is_empty(),
            "empty input must produce an empty map"
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_find_interfaces(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await.unwrap();

        let id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;

        machine::create(&mut txn, None, &id, ManagedHostState::Ready, None, 2).await?;

        let new_intf = NewDpaInterface {
            mac_address: MacAddress::from_str("00:11:22:33:44:55")?,
            machine_id: id,
            device_type: "Bluefield 3".to_string(),
            pci_name: "5e:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        };

        let intf = crate::dpa_interface::persist(new_intf, &mut txn).await?;

        let ids = crate::dpa_interface::find_ids(txn.as_mut()).await?;

        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], intf.id);

        let db_intf = crate::dpa_interface::find_by_ids(txn.as_mut(), &[ids[0]], false).await?;

        assert_eq!(db_intf.len(), 1);
        assert_eq!(db_intf[0].id, intf.id);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn deleting_interface_retains_state_history(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        machine::create(
            &mut txn,
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;

        let interface = crate::dpa_interface::persist(
            NewDpaInterface {
                mac_address: MacAddress::from_str("00:11:22:33:44:55")?,
                machine_id,
                device_type: "Bluefield 3".to_string(),
                pci_name: "5e:00.0".to_string(),
                device_description: None,
                interface_type: DpaInterfaceType::Svpc,
            },
            &mut txn,
        )
        .await?;
        crate::state_history::persist(
            &mut txn,
            crate::state_history::StateHistoryTableId::DpaInterface,
            &interface.id,
            &DpaInterfaceControllerState::Provisioning,
            config_version::ConfigVersion::initial(),
        )
        .await?;
        crate::state_history::persist(
            &mut txn,
            crate::state_history::StateHistoryTableId::DpaInterface,
            &interface.id,
            &DpaInterfaceControllerState::Ready,
            config_version::ConfigVersion::new(2),
        )
        .await?;

        let expected_history = crate::state_history::for_object(
            txn.as_mut(),
            crate::state_history::StateHistoryTableId::DpaInterface,
            &interface.id,
        )
        .await?;

        let interfaces_with_history =
            crate::dpa_interface::find_by_ids(txn.as_mut(), &[interface.id], true).await?;
        assert_eq!(interfaces_with_history.len(), 1);
        assert_eq!(
            interfaces_with_history[0]
                .history
                .iter()
                .map(|record| record.state.as_str())
                .collect::<Vec<_>>(),
            expected_history
                .iter()
                .map(|record| record.state.as_str())
                .collect::<Vec<_>>(),
            "DPA include-history query should use the shared object_id column and ordering",
        );

        crate::dpa_interface::delete(interface.clone(), &mut txn).await?;

        assert!(
            crate::dpa_interface::find_by_ids(txn.as_mut(), &[interface.id], false)
                .await?
                .is_empty(),
            "DPA interface should be deleted",
        );
        let history = crate::state_history::for_object(
            txn.as_mut(),
            crate::state_history::StateHistoryTableId::DpaInterface,
            &interface.id,
        )
        .await?;
        assert_eq!(history.len(), 2, "DPA state history should be retained");

        Ok(())
    }

    #[crate::sqlx_test]
    async fn deleting_machine_retains_dpa_interface_state_history(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        machine::create(
            &mut txn,
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;

        let interface = crate::dpa_interface::persist(
            NewDpaInterface {
                mac_address: MacAddress::from_str("00:11:22:33:44:55")?,
                machine_id,
                device_type: "Bluefield 3".to_string(),
                pci_name: "5e:00.0".to_string(),
                device_description: None,
                interface_type: DpaInterfaceType::Svpc,
            },
            &mut txn,
        )
        .await?;
        crate::state_history::persist(
            &mut txn,
            crate::state_history::StateHistoryTableId::DpaInterface,
            &interface.id,
            &DpaInterfaceControllerState::Provisioning,
            config_version::ConfigVersion::initial(),
        )
        .await?;

        machine::force_cleanup(&mut txn, &machine_id).await?;

        assert!(
            crate::dpa_interface::find_by_ids(txn.as_mut(), &[interface.id], false)
                .await?
                .is_empty(),
            "DPA interface should be deleted with its machine",
        );
        let history = crate::state_history::for_object(
            txn.as_mut(),
            crate::state_history::StateHistoryTableId::DpaInterface,
            &interface.id,
        )
        .await?;
        assert_eq!(
            history.len(),
            1,
            "DPA state history should survive machine cleanup",
        );

        Ok(())
    }

    // test_ensure_idempotent verifies that calling ensure() twice with
    // the same (machine_id, mac_address) returns the same DpaInterface
    // both times without error, ensuring that ensure ensures as ensured!
    #[crate::sqlx_test]
    async fn test_ensure_idempotent(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await.unwrap();

        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        machine::create(
            &mut txn,
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;

        let new_intf = NewDpaInterface {
            machine_id,
            mac_address: MacAddress::from_str("00:11:22:33:44:55")?,
            device_type: "BlueField3".to_string(),
            pci_name: "01:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        };

        // First call should insert a new interface.
        let first = crate::dpa_interface::ensure(new_intf, &mut txn).await?;
        assert_eq!(first.machine_id, machine_id);
        assert_eq!(
            first.mac_address,
            MacAddress::from_str("00:11:22:33:44:55")?
        );
        assert_eq!(first.pci_name, "01:00.0");

        // Second call with the same (machine_id, mac_address) should
        // return the existing interface, not fail.
        let second_intf = NewDpaInterface {
            machine_id,
            mac_address: MacAddress::from_str("00:11:22:33:44:55")?,
            device_type: "BlueField3".to_string(),
            pci_name: "01:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        };
        let second = crate::dpa_interface::ensure(second_intf, &mut txn).await?;
        assert_eq!(second.id, first.id);
        assert_eq!(second.machine_id, first.machine_id);
        assert_eq!(second.mac_address, first.mac_address);

        Ok(())
    }

    // test_device_info_roundtrip is a super basic test that inserts
    // a new DPA + corresponding MlxDeviceInfo (as a JSONB blob) into
    // the database, and makes sure it gets read back out successfully.
    // Since this drives things like doing firmware updates, it seemed
    // like a nice test to have.
    #[crate::sqlx_test]
    async fn test_device_info_roundtrip(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await.unwrap();

        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        machine::create(
            &mut txn,
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;

        let pci_name = "01:00.0";
        let new_intf = NewDpaInterface {
            machine_id,
            mac_address: MacAddress::from_str("00:11:22:33:44:55")?,
            device_type: "BlueField3".to_string(),
            pci_name: pci_name.to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        };

        crate::dpa_interface::persist(new_intf, &mut txn).await?;

        // Verify device_info starts as None, because in this case,
        // one hasn't been reported yet (and also allows for backwards
        // compatibility checks from before this existed).
        let dpa_search_config = DpaSearchConfig {
            only_svpc: true,
            only_astra: false,
        };
        let intfs =
            crate::dpa_interface::find_by_machine_id(txn.as_mut(), machine_id, dpa_search_config)
                .await?;
        assert_eq!(intfs.len(), 1);
        assert!(intfs[0].device_info.is_none());
        assert!(intfs[0].device_info_ts.is_none());

        // Store a device info report.
        let device_info = MlxDeviceInfo {
            pci_name: pci_name.to_string(),
            device_type: "BlueField3".to_string(),
            psid: Some("MT_0000001069".to_string()),
            device_description: Some(
                "Nvidia BlueField-3 B3140H E-series HHHL SuperNIC".to_string(),
            ),
            part_number: Some("900-9D3D4-00EN-HA0".to_string()),
            fw_version_current: Some("32.43.1014".to_string()),
            pxe_version_current: None,
            uefi_version_current: None,
            uefi_version_virtio_blk_current: None,
            uefi_version_virtio_net_current: None,
            base_mac: Some(MacAddress::from_str("00:11:22:33:44:55")?),
            status: Some("OK".to_string()),
        };

        crate::dpa_interface::update_device_info(txn.as_mut(), machine_id, pci_name, &device_info)
            .await?;

        // Read back and verify everything we put into
        // the database came back as we originally put it.
        let dpa_search_config = DpaSearchConfig {
            only_svpc: true,
            only_astra: false,
        };
        let intfs =
            crate::dpa_interface::find_by_machine_id(txn.as_mut(), machine_id, dpa_search_config)
                .await?;
        assert_eq!(intfs.len(), 1);

        let info = intfs[0]
            .device_info
            .as_ref()
            .expect("device_info should be set");
        assert_eq!(info.part_number.as_deref(), Some("900-9D3D4-00EN-HA0"));
        assert_eq!(info.psid.as_deref(), Some("MT_0000001069"));
        assert_eq!(info.fw_version_current.as_deref(), Some("32.43.1014"));
        assert_eq!(info.pci_name, pci_name);
        assert_eq!(
            info.base_mac,
            Some(MacAddress::from_str("00:11:22:33:44:55")?)
        );
        assert!(intfs[0].device_info_ts.is_some());

        Ok(())
    }
}
