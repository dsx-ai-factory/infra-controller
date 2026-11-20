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
use std::collections::hash_map::Entry;

use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use db::ObjectColumnFilter;
use itertools::Itertools;
use mac_address::MacAddress;
use model::machine::MachineInterfaceSnapshot;
use model::machine_boot_interface::InterfaceSelectionSource;
use model::machine_interface::InterfaceType;
use model::network_segment::NetworkSegmentType;
use model::predicted_machine_interface::{NewPredictedMachineInterface, PredictedMachineInterface};
use sqlx::PgConnection;

use crate::errors::{SiteExplorerError, SiteExplorerResult};

/// Persisted interfaces for the MACs in one host report, including unowned DHCP rows.
struct HostInterfaces {
    interfaces: HashMap<MacAddress, MachineInterfaceSnapshot>,
    predictions: HashMap<MacAddress, PredictedMachineInterface>,
}

impl HostInterfaces {
    /// Loads real and predicted interfaces with one query per table.
    async fn load(txn: &mut PgConnection, macs: &[MacAddress]) -> SiteExplorerResult<Self> {
        let rows = db::machine_interface::find_by_mac_addresses(&mut *txn, macs).await?;
        let predictions = db::predicted_machine_interface::find_by(
            txn,
            ObjectColumnFilter::List(db::predicted_machine_interface::MacAddressColumn, macs),
        )
        .await?;
        // One preferred real row per MAC for ownership and primary-flag updates.
        let mut interfaces: HashMap<_, MachineInterfaceSnapshot> = HashMap::new();
        for row in rows {
            // A MAC may exist in multiple segments. Preserve the previous preference
            // for a non-BMC row, including an unowned row waiting for ingestion.
            match interfaces.entry(row.mac_address) {
                Entry::Vacant(entry) => {
                    entry.insert(row);
                }
                Entry::Occupied(mut entry)
                    if entry.get().interface_type == InterfaceType::Bmc
                        && row.interface_type != InterfaceType::Bmc =>
                {
                    entry.insert(row);
                }
                Entry::Occupied(_) => {}
            }
        }
        // Pending NICs whose ownership, primary flag, or boot ID may need updating.
        let predictions: HashMap<_, _> = predictions
            .into_iter()
            .map(|row| (row.mac_address, row))
            .collect();
        Ok(Self {
            interfaces,
            predictions,
        })
    }

    /// Plans ownership and primary changes without accessing the database.
    /// Pending predictions preserve the leased primary until DHCP promotes them.
    fn plan<'a>(
        &self,
        machine_id: &MachineId,
        macs: &[MacAddress],
        boot_ids: &[(MacAddress, &'a str)],
        primary_mac: Option<MacAddress>,
    ) -> SiteExplorerResult<Vec<InterfaceTask<'a>>> {
        // Only a MAC present in this report can become the selected primary.
        let selected = primary_mac.filter(|mac| macs.contains(mac));
        // A leased non-BMC interface that this host owns or can adopt as primary.
        let primary_row = selected
            .and_then(|mac| self.interfaces.get(&mac))
            .filter(|row| {
                row.interface_type != InterfaceType::Bmc
                    && row.machine_id.is_none_or(|owner| owner == *machine_id)
            });
        // Clear the old primary before the selected leased NIC takes over.
        let demote_real =
            primary_row.is_some_and(|row| row.machine_id.is_none() || !row.primary_interface);
        let mut tasks = Vec::new();
        if demote_real {
            tasks.push(InterfaceTask::DemoteOwnedInterfaces);
        }
        if let Some(mac) = selected {
            tasks.push(InterfaceTask::DemoteOtherPredictions(mac));
        }

        for mac in macs.iter().copied().unique() {
            let is_primary = selected == Some(mac);
            if let Some(row) = self.interfaces.get(&mac) {
                match row.machine_id {
                    Some(owner) if owner != *machine_id => {
                        return Err(SiteExplorerError::AlreadyFoundError {
                            kind: "MachineInterface",
                            id: mac.to_string(),
                        });
                    }
                    Some(_) => {
                        let want_primary = is_primary && row.interface_type != InterfaceType::Bmc;
                        // Account for the planned machine-wide demotion before comparing flags.
                        let current_primary = row.primary_interface && !demote_real;
                        if (primary_row.is_some() || row.interface_type == InterfaceType::Bmc)
                            && current_primary != want_primary
                        {
                            tasks.push(InterfaceTask::SetPrimary(row.id, want_primary));
                        }
                    }
                    None => {
                        if row.primary_interface != is_primary {
                            tasks.push(InterfaceTask::SetPrimary(row.id, is_primary));
                        }
                        tasks.push(InterfaceTask::Associate(row.id));
                    }
                }
                continue;
            }
            let boot_id = boot_ids
                .iter()
                .find(|(address, _)| *address == mac)
                .map(|(_, id)| *id);
            match self.predictions.get(&mac) {
                Some(row) if row.machine_id != *machine_id => {
                    return Err(SiteExplorerError::AlreadyFoundError {
                        kind: "PredictedMachineInterface",
                        id: mac.to_string(),
                    });
                }
                Some(row) => {
                    if is_primary && !row.primary_interface {
                        tasks.push(InterfaceTask::PromotePrediction(mac));
                    }
                    if let Some(id) = boot_id
                        && row.boot_interface_id.as_deref() != Some(id)
                    {
                        tasks.push(InterfaceTask::SetBootId(mac, id));
                    }
                }
                None => tasks.push(InterfaceTask::CreatePrediction(mac, boot_id, is_primary)),
            }
        }
        Ok(tasks)
    }
}

#[derive(Debug, PartialEq)]
enum InterfaceTask<'a> {
    DemoteOwnedInterfaces,
    DemoteOtherPredictions(MacAddress),
    SetPrimary(MachineInterfaceId, bool),
    Associate(MachineInterfaceId),
    PromotePrediction(MacAddress),
    SetBootId(MacAddress, &'a str),
    CreatePrediction(MacAddress, Option<&'a str>, bool),
}

/// Groups a plan into writes. Demotions precede promotions and association so
/// the unique primary-interface constraint also holds between statements.
async fn apply(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    tasks: Vec<InterfaceTask<'_>>,
) -> SiteExplorerResult<()> {
    // Real interfaces that must end up non-primary.
    let mut demote = Vec::new();
    // Real interfaces that must end up primary.
    let mut promote = Vec::new();
    // Unowned real interfaces to attach to this host.
    let mut associate = Vec::new();
    // Existing predictions to mark primary, ready for DHCP promotion.
    let mut predictions = Vec::new();
    // New report IDs to store on existing predictions; omitted IDs stay unchanged.
    let mut boot_ids = Vec::new();
    // Report NICs with neither a real row nor a prediction: create pending rows.
    let mut create = Vec::new();
    for task in tasks {
        match task {
            InterfaceTask::DemoteOwnedInterfaces => {
                db::machine_interface::demote_primary_interfaces_for_machine(machine_id, txn)
                    .await?
            }
            InterfaceTask::DemoteOtherPredictions(mac) => {
                db::predicted_machine_interface::demote_other_primary_interfaces_for_machine(
                    machine_id, mac, txn,
                )
                .await?
            }
            InterfaceTask::SetPrimary(id, false) => demote.push(id),
            InterfaceTask::SetPrimary(id, true) => promote.push(id),
            InterfaceTask::Associate(id) => associate.push(id),
            InterfaceTask::PromotePrediction(mac) => predictions.push(mac),
            InterfaceTask::SetBootId(mac, id) => boot_ids.push((mac, id)),
            InterfaceTask::CreatePrediction(mac_address, boot_interface_id, primary_interface) => {
                create.push(NewPredictedMachineInterface {
                    machine_id,
                    mac_address,
                    boot_interface_id,
                    primary_interface,
                    expected_network_segment_type: NetworkSegmentType::HostInband,
                })
            }
        }
    }
    db::machine_interface::set_primary_interfaces(&demote, false, txn).await?;
    db::machine_interface::set_primary_interfaces(&promote, true, txn).await?;
    db::machine_interface::associate_interfaces_with_machine(&associate, machine_id, txn).await?;
    db::predicted_machine_interface::promote_interfaces(&predictions, txn).await?;
    db::predicted_machine_interface::set_boot_interface_ids(&boot_ids, txn).await?;
    // Only live report IDs are copied: retained IDs must keep their expiry check at DHCP.
    db::predicted_machine_interface::create_many(&create, txn).await?;
    Ok(())
}

/// Adds report NICs to a new or existing predicted host. The caller owns the
/// transaction and the locks shared with DHCP and admin-address repair.
pub(super) async fn reconcile(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    macs: &[MacAddress],
    boot_ids: &[(MacAddress, &str)],
    primary_mac: Option<MacAddress>,
    source: InterfaceSelectionSource,
) -> SiteExplorerResult<()> {
    let interfaces = HostInterfaces::load(txn, macs).await?;
    let tasks = interfaces.plan(machine_id, macs, boot_ids, primary_mac)?;
    apply(txn, machine_id, tasks).await?;
    // Publish networking provenance only when a leased NIC actually becomes primary.
    if primary_mac
        .filter(|mac| macs.contains(mac))
        .and_then(|mac| interfaces.interfaces.get(&mac))
        .filter(|row| {
            row.interface_type != InterfaceType::Bmc
                && row.machine_id.is_none_or(|owner| owner == *machine_id)
                && (row.machine_id.is_none() || !row.primary_interface)
        })
        .is_some()
    {
        db::machine_interface::set_primary_interface_selection(txn, machine_id, source, true)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_harness::prelude::sqlx_testing;

    use super::*;

    /// A bulk snapshot predates all writes, including the machine-wide demotion.
    /// Planning must account for that demotion when the selected row is re-promoted.
    #[test]
    fn primary_handoff_from_loaded_state() {
        let machine_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap();
        let old_mac = MacAddress::new([2, 0, 0, 0, 0, 1]);
        let selected_mac = MacAddress::new([2, 0, 0, 0, 0, 2]);
        let old = MachineInterfaceSnapshot {
            machine_id: Some(machine_id),
            ..MachineInterfaceSnapshot::mock_with_mac(old_mac)
        };
        let selected = MachineInterfaceSnapshot {
            id: uuid::Uuid::new_v4().into(),
            machine_id: Some(machine_id),
            primary_interface: false,
            ..MachineInterfaceSnapshot::mock_with_mac(selected_mac)
        };
        let selected_id = selected.id;
        let state = HostInterfaces {
            interfaces: [(old_mac, old), (selected_mac, selected)].into(),
            predictions: HashMap::new(),
        };
        for (name, primary, expected) in [
            ("no declaration preserves the owned primary", None, vec![]),
            (
                "leased NIC takes over after demotion",
                Some(selected_mac),
                vec![
                    InterfaceTask::DemoteOwnedInterfaces,
                    InterfaceTask::DemoteOtherPredictions(selected_mac),
                    InterfaceTask::SetPrimary(selected_id, true),
                ],
            ),
        ] {
            assert_eq!(
                state
                    .plan(&machine_id, &[old_mac, selected_mac], &[], primary)
                    .unwrap(),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn pending_primary_and_missing_report_id_preserve_settled_state() {
        let machine_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap();
        let old_mac = MacAddress::new([2, 0, 0, 0, 0, 1]);
        let selected_mac = MacAddress::new([2, 0, 0, 0, 0, 2]);
        let old = MachineInterfaceSnapshot {
            machine_id: Some(machine_id),
            ..MachineInterfaceSnapshot::mock_with_mac(old_mac)
        };
        let prediction = PredictedMachineInterface {
            id: uuid::Uuid::nil(),
            machine_id,
            mac_address: selected_mac,
            expected_network_segment_type: NetworkSegmentType::HostInband,
            boot_interface_id: Some("NIC.Known".into()),
            primary_interface: false,
        };
        let state = HostInterfaces {
            interfaces: [(old_mac, old)].into(),
            predictions: [(selected_mac, prediction)].into(),
        };
        for (name, boot_ids, expected) in [
            (
                "missing ID preserves stored value",
                vec![],
                vec![
                    InterfaceTask::DemoteOtherPredictions(selected_mac),
                    InterfaceTask::PromotePrediction(selected_mac),
                ],
            ),
            (
                "new report ID refreshes prediction",
                vec![(selected_mac, "NIC.New")],
                vec![
                    InterfaceTask::DemoteOtherPredictions(selected_mac),
                    InterfaceTask::PromotePrediction(selected_mac),
                    InterfaceTask::SetBootId(selected_mac, "NIC.New"),
                ],
            ),
        ] {
            assert_eq!(
                state
                    .plan(
                        &machine_id,
                        &[old_mac, selected_mac],
                        &boot_ids,
                        Some(selected_mac)
                    )
                    .unwrap(),
                expected,
                "{name}"
            );
        }
    }

    /// Persists the report's networking source when adopting a leased primary and retains its decision time on repeated reconciliation, preventing ordinary exploration from rewriting selection history.
    #[carbide_test_harness::prelude::sqlx_test]
    async fn reconcile_persists_primary_selection_without_refreshing_repeated_decision(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::machine::{MachineIdSource, MachineType};
        use model::address_selection_strategy::AddressSelectionStrategy;
        use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
        use model::machine_interface_address::MachineInterfaceAssociation;
        use model::network_prefix::NewNetworkPrefix;
        use model::network_segment::{
            AllocationStrategy, NetworkSegmentControllerState, NewNetworkSegment,
        };

        // An unowned leased primary isolates adoption from flag promotion; the old owned primary supplies the handoff.
        let host_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x86; 32],
            MachineType::PredictedHost,
        );
        let previous_mac = MacAddress::new([2, 0, 0, 0, 0x86, 1]);
        let selected_mac = MacAddress::new([2, 0, 0, 0, 0x86, 2]);
        let mut setup = pool.begin().await?;
        db::machine::create(
            &mut setup,
            None,
            &host_id,
            ManagedHostState::Created,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        let segment = db::network_segment::persist(
            NewNetworkSegment {
                id: uuid::Uuid::new_v4().into(),
                name: "host-primary-selection".to_string(),
                subdomain_id: None,
                vpc_id: None,
                mtu: 1500,
                prefixes: vec![NewNetworkPrefix {
                    prefix: "192.0.2.0/24".parse()?,
                    gateway: Some("192.0.2.1".parse()?),
                    dhcpv6_link_address: None,
                    num_reserved: 3,
                }],
                vlan_id: None,
                vni: None,
                segment_type: NetworkSegmentType::HostInband,
                can_stretch: None,
                allocation_strategy: AllocationStrategy::Dynamic,
                infer_slaac_eui64_addresses: false,
            },
            &mut setup,
            NetworkSegmentControllerState::Ready,
        )
        .await?;
        let previous = db::machine_interface::create(
            &mut setup,
            std::slice::from_ref(&segment),
            &previous_mac,
            true,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        db::machine_interface::associate_interface_with_machine(
            &previous.id,
            MachineInterfaceAssociation::Machine(host_id),
            &mut setup,
        )
        .await?;
        let selected = db::machine_interface::create(
            &mut setup,
            std::slice::from_ref(&segment),
            &selected_mac,
            true, // Adoption must record the source even though this leased row is already primary.
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        setup.commit().await?;

        // Apply the report's selected MAC and source through the full load, plan and persistence operation.
        let mut discovery = pool.begin().await?;
        reconcile(
            &mut discovery,
            &host_id,
            &[previous_mac, selected_mac],
            &[],
            Some(selected_mac),
            InterfaceSelectionSource::ExpectedMachine,
        )
        .await?;
        discovery.commit().await?;

        // Reload the host and both interfaces to prove attribution belongs to the newly adopted networking primary.
        let host = db::machine::find_one(&pool, &host_id, Default::default())
            .await?
            .expect("reconciled host");
        let selection = host
            .config
            .primary_interface_selection
            .expect("networking source");
        assert_eq!(selection.source, InterfaceSelectionSource::ExpectedMachine);
        assert!(selection.updated_at.is_some());
        let primary = db::machine_interface::find_one(&pool, selected.id).await?;
        assert_eq!(primary.machine_id, Some(host_id));
        assert!(primary.primary_interface);
        assert!(
            !db::machine_interface::find_one(&pool, previous.id)
                .await?
                .primary_interface
        );

        // Give the persisted decision a fixed earlier time so any repeat write is observable without sleeping.
        let decision_time: chrono::DateTime<chrono::Utc> = "2020-01-02T03:04:05Z".parse()?;
        sqlx::query(
            "UPDATE machine_interfaces SET primary_selection_updated_at = $2 WHERE id = $1",
        )
        .bind(selected.id)
        .bind(decision_time)
        .execute(&pool)
        .await?;

        // Reconcile the same report after its association and primary flags have already settled.
        let mut repeated = pool.begin().await?;
        reconcile(
            &mut repeated,
            &host_id,
            &[previous_mac, selected_mac],
            &[],
            Some(selected_mac),
            InterfaceSelectionSource::ExpectedMachine,
        )
        .await?;
        repeated.commit().await?;

        // A fresh host read must retain the recorded source and original decision time.
        let repeated = db::machine::find_one(&pool, &host_id, Default::default())
            .await?
            .expect("host after repeated reconciliation");
        assert_eq!(
            repeated.config.primary_interface_selection,
            Some(model::machine_boot_interface::InterfaceSelection {
                source: InterfaceSelectionSource::ExpectedMachine,
                updated_at: Some(decision_time),
            })
        );
        Ok(())
    }
}
