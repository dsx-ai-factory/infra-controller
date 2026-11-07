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

use carbide_uuid::machine::MachineId;
use model::bmc_info::BmcInfo;
use serde_json::json;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

async fn update_bmc_network_into_topologies(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    bmc_info: &BmcInfo,
) -> DatabaseResult<()> {
    if bmc_info.mac.is_none() {
        return Err(DatabaseError::internal(format!(
            "BMC Info in machine_topologies does not have a MAC address for machine {machine_id}"
        )));
    }
    tracing::info!(
        bmc_info = ?bmc_info,
        "Updating BMC info",
    );

    // A entry with same machine id is already created by discover_machine call.
    // Just update json by adding a ipmi_ip entry.
    let query = "UPDATE machine_topologies SET topology = jsonb_set(topology, '{bmc_info}', $1, true) WHERE machine_id=$2 RETURNING machine_id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(json!(bmc_info))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
        .ok_or(DatabaseError::NotFoundError {
            kind: "machine_topologies.machine_id",
            id: machine_id.to_string(),
        })?;
    Ok(())
}

pub async fn update_bmc_network_into_machine_interfaces(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    bmc_info: &mut BmcInfo,
) -> DatabaseResult<()> {
    let Some(bmc_mac_address) = bmc_info.mac else {
        return Err(DatabaseError::internal(format!(
            "BMC Info does not have a MAC address for machine {machine_id}"
        )));
    };

    let interface = if let Some(interface_id) = bmc_info.machine_interface_id {
        crate::machine_interface::find_one(&mut *txn, interface_id).await?
    } else if let Some(bmc_ip) = bmc_info.ip.as_ref() {
        crate::machine_interface::find_by_ip(&mut *txn, *bmc_ip)
            .await?
            .ok_or_else(|| DatabaseError::NotFoundError {
                kind: "machine_interfaces.address",
                id: bmc_ip.to_string(),
            })?
    } else {
        crate::machine_interface::find_by_mac_address(&mut *txn, bmc_mac_address)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| DatabaseError::NotFoundError {
                kind: "machine_interfaces.mac_address",
                id: bmc_mac_address.to_string(),
            })?
    };

    if interface.mac_address != bmc_mac_address {
        return Err(DatabaseError::internal(format!(
            "BMC interface {} MAC {} does not match BMC Info MAC {} for machine {machine_id}",
            interface.id, interface.mac_address, bmc_mac_address
        )));
    }

    crate::machine_interface::associate_bmc_interface(
        &interface.id,
        model::machine_interface_address::MachineInterfaceAssociation::Machine(*machine_id),
        txn,
    )
    .await?;
    bmc_info.machine_interface_id = Some(interface.id);

    update_bmc_network_into_topologies(txn, machine_id, bmc_info).await
}

// enrich_mac_address queries the MachineInterfaces table to populate the BMC mac address of the BmcMetaDataInfo structure in memory if it does not exist
// If this function populates the BMC mac address, and persist is speciifed as true, the function will update the machine_topologies table
// with the mac address for that BMC
pub async fn enrich_mac_address(
    bmc_info: &mut BmcInfo,
    caller: String,
    txn: &mut PgConnection,
    machine_id: &MachineId,
    persist: bool,
) -> DatabaseResult<()> {
    if bmc_info.ip.is_none() {
        return Err(DatabaseError::internal(format!(
            "{caller} cannot enrich BMC Info without a valid BMC IP address for machine {machine_id}: {bmc_info:#?}"
        )));
    }

    let bmc_ip_address = bmc_info.ip.unwrap();
    if bmc_info.mac.is_none() {
        if let Some(bmc_machine_interface) =
            crate::machine_interface::find_by_ip(&mut *txn, bmc_ip_address).await?
        {
            let bmc_mac_address = bmc_machine_interface.mac_address;

            tracing::info!(
                caller = %caller,
                machine_id = %machine_id,
                mac_address = ?bmc_machine_interface.mac_address,
                "Enriching BMC information",
            );
            bmc_info.mac = Some(bmc_mac_address);
            bmc_info.machine_interface_id = Some(bmc_machine_interface.id);
            if persist {
                update_bmc_network_into_topologies(txn, machine_id, bmc_info).await?;
            }
        } else {
            // This should never happen. Should we return an error here?
            tracing::info!(
                caller = %caller,
                machine_id = %machine_id,
                bmc_ip_address = %bmc_ip_address,
                "Failed to enrich BMC information: machine interface not found",
            );
        }
    }
    Ok(())
}
