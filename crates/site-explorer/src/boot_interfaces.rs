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
use std::net::IpAddr;

use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::machine::HostMachineId;
use db::machine_interface::MachineInterfaces;
use db::{DatabaseError, Transaction};
use mac_address::MacAddress;
use model::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterface, MachineBootInterfaceTarget,
};
use model::site_explorer::{ExploredEndpoint, HostPrimaryInterfaceSelection};
use sqlx::PgConnection;

use crate::errors::SiteExplorerResult;
use crate::machine_interfaces::MachineInterfacesExt;

/// Selected boot interfaces for host endpoints.
pub(super) struct HostBootInterfaces {
    interfaces: Vec<HostBootInterface>,
}

impl HostBootInterfaces {
    pub(super) fn new() -> Self {
        Self {
            interfaces: Vec::new(),
        }
    }

    pub(super) fn add(&mut self, endpoint: &ExploredEndpoint, mac_address: MacAddress) {
        self.interfaces
            .extend(HostBootInterface::new(endpoint, mac_address));
    }

    pub(super) async fn persist(mut self, txn: &mut Transaction<'_>) -> Result<(), DatabaseError> {
        // Any transaction touching multiple explored_endpoints needs to sort them the same way to
        // avoid deadlocks: sort by IP.
        self.interfaces.sort_by_key(|interface| interface.address);
        for interface in self.interfaces {
            db::explored_endpoints::set_boot_interface(
                interface.address,
                &interface.boot_interface,
                txn,
            )
            .await?;
        }
        Ok(())
    }
}

/// The selected boot interface for one host endpoint.
struct HostBootInterface {
    address: IpAddr,
    boot_interface: MachineBootInterface,
}

impl HostBootInterface {
    /// A MAC without a Redfish interface id must not replace the stored pair.
    fn new(endpoint: &ExploredEndpoint, mac_address: MacAddress) -> Option<Self> {
        if let Some(interface_id) = endpoint.report.find_interface_id_for_mac(mac_address) {
            Some(Self {
                address: endpoint.address,
                boot_interface: MachineBootInterface {
                    mac_address,
                    interface_id: interface_id.to_string(),
                },
            })
        } else {
            tracing::debug!(
                bmc_ip_address = %endpoint.address,
                %mac_address,
                "boot interface MAC has no matching Redfish interface id in the report; keeping last-known-good stored boot interface",
            );
            None
        }
    }
}

/// Complete MAC/Redfish id pairs from all explored host NICs.
pub(super) struct NicBootInterfaces {
    interfaces: Vec<MachineBootInterface>,
}

impl NicBootInterfaces {
    pub(super) fn new(explored_hosts: &HashMap<IpAddr, ExploredEndpoint>) -> Self {
        let mut interfaces = Vec::new();

        // Refresh every explored host, including hosts that cannot yet pair
        // with their DPUs. Only complete pairs replace last-known-good ids.
        for ep in explored_hosts.values() {
            for iface in ep
                .report
                .systems
                .iter()
                .flat_map(|s| s.ethernet_interfaces.iter())
            {
                if let Some(interface) =
                    MachineBootInterface::from_parts(iface.mac_address, iface.id.clone())
                {
                    interfaces.push(interface);
                } else if iface.mac_address.is_some()
                    || iface.id.as_deref().none_if_empty().is_some()
                {
                    // Keep fully empty NIC records quiet.
                    tracing::info!(
                        bmc_ip_address = %ep.address,
                        interface_mac_address = ?iface.mac_address,
                        interface_id = ?iface.id,
                        "site-explorer: cannot build host NIC boot interface from MAC and interface id; skipping",
                    );
                }
            }
        }

        Self { interfaces }
    }

    pub(super) async fn persist(self, txn: &mut Transaction<'_>) -> Result<(), DatabaseError> {
        // Record each host NIC's Redfish id on its machine_interfaces row so the
        // primary-flagged row is the host's complete boot interface (MAC + id).
        // Pending predicted interfaces get the same refresh, so a prediction
        // minted before the report resolved the id stays as current as the
        // live report until DHCP promotes it.
        for boot_interface in self.interfaces {
            db::machine_interface::set_boot_interface_id(
                boot_interface.mac_address,
                &boot_interface.interface_id,
                txn,
            )
            .await?;
            db::predicted_machine_interface::set_boot_interface_id(
                txn,
                boot_interface.mac_address,
                &boot_interface.interface_id,
            )
            .await?;
        }

        Ok(())
    }
}

/// Initializes the durable desired boot interface from Site Explorer's settled
/// interface ownership, or enriches a persisted target that has only a MAC
/// with a newly observed Redfish interface ID.
///
/// The initial read does not lock, so a completed pair does not serialize
/// unrelated machine updates on every exploration. The mutation helpers lock
/// and recheck the two allowed transitions: unset to initialized, and a
/// `MacOnly` target to a `Pair` for the same MAC.
/// Before either mutation, try the group after the caller's interface work.
/// A busy member aborts the transaction rather than waiting while holding
/// rows another network writer may need. Site Explorer retries in a later run.
pub(super) async fn reconcile_desired_boot_interface(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    primary_interface_selection: Option<HostPrimaryInterfaceSelection>,
    declared_primary: Option<MacAddress>,
) -> SiteExplorerResult<()> {
    let desired = db::machine_desired_boot_interface::get(&mut *txn, machine_id).await?;

    if matches!(
        desired.as_ref().map(|desired| &desired.value),
        Some(MachineBootInterfaceTarget::Pair(_))
    ) {
        return Ok(());
    }

    let interfaces = MachineInterfaces::load(txn, machine_id).await?;
    let proposed_target =
        interfaces.proposed_boot_interface_target(desired.as_ref().map(|desired| &desired.value));

    match (
        desired.as_ref().map(|desired| &desired.value),
        proposed_target,
    ) {
        (None, Some(target)) => {
            if !db::machine::try_lock_group_for_update(txn, machine_id.as_machine_id()).await? {
                return Err(db::DatabaseError::FailedPrecondition(format!(
                    "machine group for {machine_id} changed or is busy; retry",
                ))
                .into());
            }
            let selection_source =
                initial_selection_source(&target, primary_interface_selection, declared_primary);
            db::machine_desired_boot_interface::initialize_if_unset(
                txn,
                machine_id,
                &target,
                selection_source,
            )
            .await?;
        }
        (
            Some(MachineBootInterfaceTarget::MacOnly(mac_address)),
            Some(MachineBootInterfaceTarget::Pair(target)),
        ) => {
            debug_assert_eq!(*mac_address, target.mac_address);
            if !db::machine::try_lock_group_for_update(txn, machine_id.as_machine_id()).await? {
                return Err(db::DatabaseError::FailedPrecondition(format!(
                    "machine group for {machine_id} changed or is busy; retry",
                ))
                .into());
            }
            db::machine_desired_boot_interface::enrich_interface_id(
                txn,
                machine_id,
                *mac_address,
                &target.interface_id,
            )
            .await?;
        }
        _ => {}
    }

    Ok(())
}

/// Returns `ExpectedMachine` when the declared MAC became durable, otherwise
/// returns the report source only when its MAC matches the durable target.
/// `LegacyUnknown` covers every other target, including one chosen by a report
/// fallback when Redfish could not resolve the declared interface and the
/// fallback selected a different MAC.
fn initial_selection_source(
    target: &MachineBootInterfaceTarget,
    report_selection: Option<HostPrimaryInterfaceSelection>,
    declared_primary: Option<MacAddress>,
) -> BootInterfaceSelectionSource {
    let target_mac = target.mac_address();
    if declared_primary == Some(target_mac) {
        return BootInterfaceSelectionSource::ExpectedMachine;
    }

    report_selection
        .filter(|selection| selection.mac_address == target_mac)
        .map_or(BootInterfaceSelectionSource::LegacyUnknown, |selection| {
            selection.source
        })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn initial_selection_source_must_match_the_durable_target() {
        let target_mac = MacAddress::new([0x02, 0, 0, 0, 0, 1]);
        let other_mac = MacAddress::new([0x02, 0, 0, 0, 0, 2]);
        let target = MachineBootInterfaceTarget::MacOnly(target_mac);
        let selection = |mac_address, source| HostPrimaryInterfaceSelection {
            mac_address,
            source,
        };

        check_values(
            [
                Check {
                    scenario: "matching ExpectedMachine declaration wins over report fallback",
                    input: (
                        Some(selection(
                            other_mac,
                            BootInterfaceSelectionSource::RedfishSerialNumber,
                        )),
                        Some(target_mac),
                    ),
                    expect: BootInterfaceSelectionSource::ExpectedMachine,
                },
                Check {
                    scenario: "matching UEFI selection is retained",
                    input: (
                        Some(selection(
                            target_mac,
                            BootInterfaceSelectionSource::RedfishUefiPci,
                        )),
                        None,
                    ),
                    expect: BootInterfaceSelectionSource::RedfishUefiPci,
                },
                Check {
                    scenario: "matching serial fallback is retained",
                    input: (
                        Some(selection(
                            target_mac,
                            BootInterfaceSelectionSource::RedfishSerialNumber,
                        )),
                        None,
                    ),
                    expect: BootInterfaceSelectionSource::RedfishSerialNumber,
                },
                Check {
                    scenario: "different report selection is not attributed to target",
                    input: (
                        Some(selection(
                            other_mac,
                            BootInterfaceSelectionSource::RedfishUefiPci,
                        )),
                        None,
                    ),
                    expect: BootInterfaceSelectionSource::LegacyUnknown,
                },
                Check {
                    scenario: "unexplained target stays explicitly unknown",
                    input: (None, None),
                    expect: BootInterfaceSelectionSource::LegacyUnknown,
                },
            ],
            |(report_selection, declared_primary)| {
                initial_selection_source(&target, report_selection, declared_primary)
            },
        );
    }
}
