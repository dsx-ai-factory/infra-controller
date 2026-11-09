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
use carbide_uuid::machine::PredictedHostMachineId;
use db::{DatabaseError, Transaction};
use mac_address::MacAddress;
use model::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterface, MachineBootInterfaceTarget,
};
use model::site_explorer::{ExploredEndpoint, HostPrimaryInterfaceSelection};
use sqlx::PgConnection;

use crate::errors::SiteExplorerResult;

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

/// Returns `ExpectedMachine` when the declared MAC became durable, otherwise
/// returns the report source only when its MAC matches the durable target.
/// `LegacyUnknown` covers every other target, including one chosen by a report
/// fallback when Redfish could not resolve the declared interface and the
/// fallback selected a different MAC.
pub(super) fn initial_selection_source(
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

/// Selects declared or retained primary intent while preserving an existing primary.
pub(super) async fn zero_dpu_primary_mac(
    txn: &mut PgConnection,
    existing_machine_id: Option<PredictedHostMachineId>,
    mac_addresses: &[MacAddress],
    declared_primary: Option<MacAddress>,
    retained_boot_interface_window: Option<chrono::Duration>,
) -> SiteExplorerResult<Option<MacAddress>> {
    let has_existing_primary = if declared_primary.is_none()
        && let Some(machine_id) = existing_machine_id
    {
        db::machine_interface::machine_has_primary_interface(&machine_id, &mut *txn).await?
            || db::predicted_machine_interface::find_by_machine_id(&mut *txn, &machine_id)
                .await?
                .iter()
                .any(|prediction| prediction.primary_interface)
    } else {
        false
    };
    Ok(match declared_primary {
        Some(declared) => Some(declared),
        None if !has_existing_primary => {
            let mut recovered = None;
            for mac_address in mac_addresses {
                if db::retained_boot_interface::find_by_mac(
                    &mut *txn,
                    *mac_address,
                    retained_boot_interface_window,
                )
                .await?
                .is_some()
                {
                    recovered = Some(*mac_address);
                    break;
                }
            }
            recovered
        }
        None => None,
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
