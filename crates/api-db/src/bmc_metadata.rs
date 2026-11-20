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

use std::net::IpAddr;

use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use model::bmc_info::BmcInfo;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::network_segment::NetworkSegmentType;
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
        .ok_or_else(|| DatabaseError::NotFoundError {
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

/// Attaches the observed unowned Underlay BMC interface when a machine has no recorded BMC, allowing API-first discovery without following a reassigned address.
/// Missing, changed or ineligible candidates remain unassociated so unrelated reconciliation can continue.
///
/// Callers must serialize BMC assignment for the machine (Site Explorer uses its iteration work lock)
/// and finish interface writes before acquiring machine-row locks.
pub async fn associate_missing_bmc_interface(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    observed_interface_id: MachineInterfaceId,
    bmc_ip: IpAddr,
) -> DatabaseResult<()> {
    // Preserve both owned BMCs and retained BMC identities whose interface is temporarily unavailable.
    let query = "SELECT EXISTS (
                     SELECT 1 FROM machine_interfaces
                     WHERE machine_id = $1 AND interface_type = 'Bmc'
                 ) OR EXISTS (
                     SELECT 1 FROM machine_topologies WHERE machine_id = $1 AND (
                         topology #>> '{bmc_info,machine_interface_id}' IS NOT NULL
                         OR topology #>> '{bmc_info,mac}' IS NOT NULL
                         OR topology #>> '{bmc_info,ip}' IS NOT NULL
                     )
                 )";
    let recorded: bool = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if recorded {
        return Ok(());
    }

    // Recheck the observed identity under the address lock so a replacement lease cannot redirect BMC management.
    let Some(interface) =
        crate::machine_interface::find_optional_for_update_by_ip(txn, bmc_ip).await?
    else {
        tracing::debug!(%machine_id, %bmc_ip, "BMC address is unavailable for association");
        return Ok(());
    };
    if interface.id != observed_interface_id
        || interface.network_segment_type != Some(NetworkSegmentType::Underlay)
        || interface.machine_id.is_some()
        || interface.switch_id.is_some()
        || interface.power_shelf_id.is_some()
        || interface.attached_dpu_machine_id.is_some()
    {
        tracing::debug!(%machine_id, %bmc_ip, %observed_interface_id, current_interface_id = %interface.id,
            "BMC interface changed or is unavailable for association");
        return Ok(());
    }
    crate::machine_interface::associate_bmc_interface(
        &interface.id,
        MachineInterfaceAssociation::Machine(*machine_id),
        txn,
    )
    .await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use model::address_selection_strategy::AddressSelectionStrategy;
    use model::hardware_info::HardwareInfo;
    use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
    use model::network_segment::NetworkSegmentControllerState;

    use super::*;
    use crate::TransactionVending as _;

    /// Leaves recorded BMC authority and foreign or reassigned endpoints unchanged, returning success so unrelated reconciliation can proceed.
    #[crate::sqlx_test]
    async fn missing_bmc_association_preserves_recorded_and_foreign_ownership(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (index, (scenario, owned_bmc, retained_bmc, foreign_candidate, moved_address)) in [
            // A complete owned BMC must remain selected when a different endpoint is reported.
            ("owned BMC", true, false, false, false),
            // Retained identity remains authoritative even when its interface is no longer associated.
            ("retained BMC", false, true, false, false),
            // An otherwise eligible endpoint must not be taken from another machine.
            ("foreign endpoint", false, false, true, false),
            // Moving the report's IP to another unowned interface must not redirect BMC association.
            ("reassigned address", false, false, false, true),
        ]
        .into_iter()
        .enumerate()
        {
            // Give each case two machines and Underlay leases to isolate recorded authority, ownership or address reassignment.
            let index = u8::try_from(index).expect("small case table");
            let machine_id = MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [index + 1; 32],
                MachineType::Dpu,
            );
            let foreign_id = MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [index + 10; 32],
                MachineType::Dpu,
            );
            let mut setup = pool.txn_begin().await?;
            for id in [machine_id, foreign_id] {
                crate::machine::create(
                    setup.as_pgconn(),
                    None,
                    &id,
                    ManagedHostState::Created,
                    None,
                    CURRENT_STATE_MODEL_VERSION,
                )
                .await?;
            }
            crate::machine_topology::create_or_update(
                setup.as_pgconn(),
                &machine_id,
                &HardwareInfo::default(),
            )
            .await?;
            let mut subnet = crate::test_support::network_segment::admin_segment(
                scenario,
                &format!("192.0.{}.0/24", index + 10),
                &format!("192.0.{}.1", index + 10),
                3,
            );
            subnet.segment_type = NetworkSegmentType::Underlay;
            let segment = crate::network_segment::persist(
                subnet,
                setup.as_pgconn(),
                NetworkSegmentControllerState::Ready,
            )
            .await?;
            let candidate = crate::machine_interface::create(
                setup.as_pgconn(),
                std::slice::from_ref(&segment),
                &mac_address::MacAddress::new([2, 0, 0, 0, index, 1]),
                false,
                AddressSelectionStrategy::NextAvailableIp,
                None,
            )
            .await?;
            if owned_bmc || retained_bmc {
                let previous = crate::machine_interface::create(
                    setup.as_pgconn(),
                    std::slice::from_ref(&segment),
                    &mac_address::MacAddress::new([2, 0, 0, 0, index, 2]),
                    false,
                    AddressSelectionStrategy::NextAvailableIp,
                    None,
                )
                .await?;
                if owned_bmc {
                    crate::machine_interface::associate_bmc_interface(
                        &previous.id,
                        MachineInterfaceAssociation::Machine(machine_id),
                        setup.as_pgconn(),
                    )
                    .await?;
                } else {
                    // Retain the selected ID without attaching its row, modeling an incomplete established association.
                    sqlx::query("UPDATE machine_topologies SET topology = jsonb_set(topology,
                        '{bmc_info,machine_interface_id}', to_jsonb($2::text)) WHERE machine_id = $1")
                        .bind(machine_id).bind(previous.id.to_string())
                        .execute(setup.as_pgconn()).await?;
                }
            }
            if foreign_candidate {
                crate::machine_interface::associate_interface_with_machine(
                    &candidate.id,
                    MachineInterfaceAssociation::Machine(foreign_id),
                    setup.as_pgconn(),
                )
                .await?;
            }
            if moved_address {
                // Move the observed lease through the same static-assignment writer used by the public RPC.
                let replacement = crate::machine_interface::create(
                    setup.as_pgconn(),
                    std::slice::from_ref(&segment),
                    &mac_address::MacAddress::new([2, 0, 0, 0, index, 3]),
                    false,
                    AddressSelectionStrategy::NextAvailableIp,
                    None,
                )
                .await?;
                crate::machine_interface_address::assign_static(
                    setup.as_pgconn(),
                    candidate.id,
                    format!("192.0.{}.200", index + 10).parse()?,
                )
                .await?;
                crate::machine_interface_address::assign_static(
                    setup.as_pgconn(),
                    replacement.id,
                    candidate.addresses[0],
                )
                .await?;
            }
            setup.commit().await?;
            let query = "SELECT jsonb_build_object(
                'interfaces', (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.id) FROM machine_interfaces i),
                'addresses', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM machine_interface_addresses a),
                'topologies', (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.machine_id) FROM machine_topologies t))";
            let before: serde_json::Value = sqlx::query_scalar(query).fetch_one(&pool).await?;

            // Commit every skipped adoption so rollback cannot conceal an accidental change or an unrelated-work failure.
            let mut txn = pool.txn_begin().await?;
            associate_missing_bmc_interface(
                txn.as_pgconn(),
                &machine_id,
                candidate.id,
                candidate.addresses[0],
            )
            .await?;
            txn.commit().await?;

            // Find persisted interfaces and topology again to prove existing authority and foreign ownership are unchanged.
            let after: serde_json::Value = sqlx::query_scalar(query).fetch_one(&pool).await?;
            assert_eq!(after, before, "{scenario}");
        }
        Ok(())
    }
}
