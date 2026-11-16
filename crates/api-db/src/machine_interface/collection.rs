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
use mac_address::MacAddress;
use model::machine::{
    MachineInterfaceSnapshot, pick_boot_interface, pick_boot_prediction,
    pick_default_boot_interface,
};
use model::machine_boot_interface::{MachineBootInterface, canonical_redfish_boot_interface_id};
use model::predicted_machine_interface::PredictedMachineInterface;
use sqlx::PgConnection;

use crate::DatabaseResult;

/// A machine's non-BMC interfaces and pending interface predictions.
/// Every real row is retained, including same-MAC rows in different segments.
pub struct MachineInterfaces {
    /// Owned non-BMC interface rows, in the database query's order.
    pub interfaces: Vec<MachineInterfaceSnapshot>,
    /// Pending predictions owned by the machine, including underlay predictions.
    pub predictions: Vec<PredictedMachineInterface>,
}

impl MachineInterfaces {
    /// Loads both collections in the caller's transaction with one query per table.
    /// Missing machines produce empty collections. Selection methods perform no I/O.
    pub async fn load(txn: &mut PgConnection, machine_id: &MachineId) -> DatabaseResult<Self> {
        let mut interfaces_by_machine =
            crate::machine_interface::find_by_machine_ids(txn, &[*machine_id]).await?;
        let interfaces = interfaces_by_machine.remove(machine_id).unwrap_or_default();
        let predictions =
            crate::predicted_machine_interface::find_by_machine_id(txn, machine_id).await?;
        Ok(Self {
            interfaces,
            predictions,
        })
    }

    /// Selects an owned boot interface: primary first, otherwise the lowest-MAC non-underlay row.
    pub fn boot_interface(&self) -> Option<&MachineInterfaceSnapshot> {
        pick_boot_interface(&self.interfaces)
    }

    /// Selects the lowest-MAC non-underlay row, ignoring primary designations.
    pub fn default_boot_interface(&self) -> Option<&MachineInterfaceSnapshot> {
        pick_default_boot_interface(&self.interfaces)
    }

    /// Selects a primary prediction, otherwise the sole non-underlay prediction.
    /// Several non-primary predictions offer no unambiguous candidate.
    pub fn boot_prediction(&self) -> Option<&PredictedMachineInterface> {
        pick_boot_prediction(&self.predictions)
    }

    /// Returns a MAC/Redfish-ID pair, preferring unambiguous owned IDs over predictions.
    /// Conflicting owned IDs block prediction fallback. Blank IDs are ignored and others trimmed.
    pub fn boot_interface_for_mac(&self, mac_address: MacAddress) -> Option<MachineBootInterface> {
        self.resolve_interface_id(mac_address)
            .map(|interface_id| MachineBootInterface {
                mac_address,
                interface_id,
            })
    }

    /// Resolves an unambiguous Redfish ID, preferring owned interfaces.
    /// Ambiguous owned IDs block prediction fallback.
    fn resolve_interface_id(&self, mac_address: MacAddress) -> Option<String> {
        let owned = unique_interface_id(
            self.interfaces
                .iter()
                .filter(|interface| interface.mac_address == mac_address)
                .filter_map(|interface| interface.boot_interface_id.as_deref()),
        );
        match owned {
            InterfaceIdResolution::Unique(interface_id) => return Some(interface_id.to_string()),
            InterfaceIdResolution::Ambiguous => return None,
            InterfaceIdResolution::Missing => {}
        }

        match unique_interface_id(
            self.predictions
                .iter()
                .filter(|prediction| prediction.mac_address == mac_address)
                .filter_map(|prediction| prediction.boot_interface_id.as_deref()),
        ) {
            InterfaceIdResolution::Unique(interface_id) => Some(interface_id.to_string()),
            InterfaceIdResolution::Missing | InterfaceIdResolution::Ambiguous => None,
        }
    }
}

enum InterfaceIdResolution<'a> {
    Missing,
    Unique(&'a str),
    Ambiguous,
}

fn unique_interface_id<'a>(
    interface_ids: impl Iterator<Item = &'a str>,
) -> InterfaceIdResolution<'a> {
    let mut interface_ids = interface_ids.filter_map(canonical_redfish_boot_interface_id);
    match interface_ids.next() {
        None => InterfaceIdResolution::Missing,
        Some(first) if interface_ids.any(|id| id != first) => InterfaceIdResolution::Ambiguous,
        Some(first) => InterfaceIdResolution::Unique(first),
    }
}

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use carbide_uuid::network::NetworkSegmentId;
    use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
    use model::machine_interface::InterfaceType;
    use model::network_segment::NetworkSegmentType;
    use model::predicted_machine_interface::NewPredictedMachineInterface;

    use super::*;

    /// Scope both tables to one owner without collapsing same-MAC segment rows.
    #[crate::sqlx_test]
    async fn load_preserves_scope_and_ambiguous_rows(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let id = |seed| {
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [seed; 32],
                MachineType::Host,
            )
        };
        let owner = id(0x41);
        let other = id(0x42);
        let mac = |suffix| MacAddress::new([0x02, 0, 0, 0, 0x41, suffix]);
        let mut txn = pool.begin().await?;
        for machine_id in [owner, other] {
            crate::machine::create(
                txn.as_mut(),
                None,
                &machine_id,
                ManagedHostState::Ready,
                None,
                CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
        }
        let segments: Vec<NetworkSegmentId> = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('collection-a', 'V1-T0'), ('collection-b', 'V1-T0') RETURNING id",
        )
        .fetch_all(txn.as_mut())
        .await?;
        for (index, (machine_id, segment, address, interface_type, boot_id)) in [
            (
                Some(owner),
                segments[0],
                mac(1),
                InterfaceType::Data,
                Some("NIC.Real.1"),
            ),
            (
                Some(owner),
                segments[1],
                mac(1),
                InterfaceType::Data,
                Some("NIC.Real.2"),
            ),
            (Some(owner), segments[0], mac(2), InterfaceType::Bmc, None),
            (Some(other), segments[0], mac(3), InterfaceType::Data, None),
            (None, segments[0], mac(4), InterfaceType::Data, None),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query(
                "INSERT INTO machine_interfaces (machine_id, association_type, segment_id,
                    mac_address, hostname, interface_type, primary_interface, boot_interface_id)
                 VALUES ($1, 'Machine', $2, $3, $4, $5, $6, $7)",
            )
            .bind(machine_id)
            .bind(segment)
            .bind(address)
            .bind(format!("collection-{index}"))
            .bind(interface_type)
            .bind(index == 0)
            .bind(boot_id)
            .execute(txn.as_mut())
            .await?;
        }
        let predictions = [
            (owner, mac(1), NetworkSegmentType::HostInband, false),
            (owner, mac(6), NetworkSegmentType::HostInband, true),
            (owner, mac(7), NetworkSegmentType::Underlay, false),
            (other, mac(8), NetworkSegmentType::HostInband, false),
        ];
        crate::predicted_machine_interface::create_many(
            &predictions
                .iter()
                .map(
                    |(machine_id, address, segment_type, primary)| NewPredictedMachineInterface {
                        machine_id,
                        mac_address: *address,
                        expected_network_segment_type: *segment_type,
                        boot_interface_id: Some(" NIC.Predicted "),
                        primary_interface: *primary,
                    },
                )
                .collect::<Vec<_>>(),
            txn.as_mut(),
        )
        .await?;

        let loaded = MachineInterfaces::load(txn.as_mut(), &owner).await?;
        assert_eq!(loaded.interfaces.len(), 2);
        assert!(
            loaded
                .interfaces
                .iter()
                .all(|row| row.machine_id == Some(owner)
                    && row.mac_address == mac(1)
                    && row.interface_type == InterfaceType::Data)
        );
        let mut predicted_macs: Vec<_> = loaded
            .predictions
            .iter()
            .map(|row| row.mac_address)
            .collect();
        predicted_macs.sort_unstable();
        assert_eq!(predicted_macs, [mac(1), mac(6), mac(7)]);
        assert!(
            loaded.boot_interface_for_mac(mac(1)).is_none(),
            "conflicting owned IDs must still block prediction fallback"
        );
        assert_eq!(loaded.boot_prediction().unwrap().mac_address, mac(6));
        assert_eq!(
            loaded.boot_interface_for_mac(mac(6)).unwrap().interface_id,
            "NIC.Predicted"
        );

        let absent = MachineInterfaces::load(txn.as_mut(), &id(0x43)).await?;
        assert!(absent.interfaces.is_empty() && absent.predictions.is_empty());
        txn.rollback().await?;
        Ok(())
    }
}
