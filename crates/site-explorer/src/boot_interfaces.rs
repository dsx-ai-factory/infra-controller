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
use db::{DatabaseError, Transaction};
use itertools::Itertools;
use mac_address::MacAddress;
use model::machine::{MachineInterfaceSnapshot, pick_boot_interface, pick_boot_prediction};
use model::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterface, MachineBootInterfaceTarget,
    canonical_redfish_boot_interface_id,
};
use model::predicted_machine_interface::PredictedMachineInterface;
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

/// Initializes the durable desired boot interface from Site Explorer's settled
/// interface ownership, or enriches a persisted target that has only a MAC
/// with a newly observed Redfish interface ID.
///
/// The initial read does not lock, so a completed pair does not serialize
/// unrelated machine updates on every exploration. The mutation helpers lock
/// and recheck the two allowed transitions: unset to initialized, and a
/// `MacOnly` target to a `Pair` for the same MAC.
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

    // `find_by_machine_ids` removes BMC rows. Keeping that boundary in the
    // database query also lets `pick_boot_interface` borrow this list directly.
    let mut interfaces_by_machine =
        db::machine_interface::find_by_machine_ids(txn, &[*machine_id]).await?;
    let interfaces = interfaces_by_machine.remove(machine_id).unwrap_or_default();
    let predictions = db::predicted_machine_interface::find_by_machine_id(txn, machine_id).await?;
    let update = desired_boot_interface_update(
        desired.as_ref().map(|desired| &desired.value),
        &interfaces,
        &predictions,
    );

    match (desired.as_ref().map(|desired| &desired.value), update) {
        (None, Some(target)) => {
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

/// Returns the only desired-target transition Site Explorer should attempt.
///
/// An existing pair is already complete. An existing MAC-only value is
/// already designated, so only a unique id for that same MAC may enrich it.
/// With no value, an explicitly primary prediction outranks the interim owned
/// interface (the integrated-NIC-before-first-DHCP case); otherwise owned
/// interfaces win, with an unambiguous prediction as the final fallback.
/// Once the MAC is selected, all same-MAC records resolve its id so a current
/// owned id can complete an id-less prediction.
fn desired_boot_interface_update(
    desired: Option<&MachineBootInterfaceTarget>,
    interfaces: &[MachineInterfaceSnapshot],
    predictions: &[PredictedMachineInterface],
) -> Option<MachineBootInterfaceTarget> {
    match desired {
        Some(MachineBootInterfaceTarget::Pair(_)) => None,
        Some(MachineBootInterfaceTarget::MacOnly(mac_address)) => {
            resolve_interface_id(*mac_address, interfaces, predictions).map(|interface_id| {
                MachineBootInterfaceTarget::Pair(MachineBootInterface {
                    mac_address: *mac_address,
                    interface_id,
                })
            })
        }
        None => {
            let prediction = pick_boot_prediction(predictions);
            let selected_mac = prediction
                .filter(|prediction| prediction.primary_interface)
                .map(|prediction| prediction.mac_address)
                .or_else(|| pick_boot_interface(interfaces).map(|interface| interface.mac_address))
                .or_else(|| prediction.map(|prediction| prediction.mac_address));
            selected_mac.map(|mac_address| {
                target_with_resolved_interface_id(mac_address, interfaces, predictions)
            })
        }
    }
}

fn target_with_resolved_interface_id(
    mac_address: MacAddress,
    interfaces: &[MachineInterfaceSnapshot],
    predictions: &[PredictedMachineInterface],
) -> MachineBootInterfaceTarget {
    match resolve_interface_id(mac_address, interfaces, predictions) {
        Some(interface_id) => MachineBootInterfaceTarget::Pair(MachineBootInterface {
            mac_address,
            interface_id,
        }),
        None => MachineBootInterfaceTarget::MacOnly(mac_address),
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
    let mut interface_ids = interface_ids
        .filter_map(canonical_redfish_boot_interface_id)
        .unique();
    match (interface_ids.next(), interface_ids.next()) {
        (None, _) => InterfaceIdResolution::Missing,
        (Some(interface_id), None) => InterfaceIdResolution::Unique(interface_id),
        (Some(_), Some(_)) => InterfaceIdResolution::Ambiguous,
    }
}

fn resolve_interface_id(
    mac_address: MacAddress,
    interfaces: &[MachineInterfaceSnapshot],
    predictions: &[PredictedMachineInterface],
) -> Option<String> {
    let owned = unique_interface_id(
        interfaces
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
        predictions
            .iter()
            .filter(|prediction| prediction.mac_address == mac_address)
            .filter_map(|prediction| prediction.boot_interface_id.as_deref()),
    ) {
        InterfaceIdResolution::Unique(interface_id) => Some(interface_id.to_string()),
        InterfaceIdResolution::Missing | InterfaceIdResolution::Ambiguous => None,
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::{Check, check_values};
    use carbide_uuid::machine::MachineId;
    use model::machine_interface::InterfaceType;
    use model::network_segment::NetworkSegmentType;

    use super::*;

    /// `desired_boot_interface_update` only initializes a missing target or
    /// completes the same designated MAC. These cases keep the
    /// prediction/owned precedence and the ambiguity rules together.
    #[test]
    fn desired_boot_interface_update_cases() {
        let mac = |last| MacAddress::new([0x02, 0, 0, 0, 0, last]);
        let interface =
            |mac_address, interface_type, primary_interface, interface_id: Option<&str>| {
                MachineInterfaceSnapshot {
                    interface_type,
                    primary_interface,
                    boot_interface_id: interface_id.map(str::to_string),
                    network_segment_type: Some(NetworkSegmentType::HostInband),
                    ..MachineInterfaceSnapshot::mock_with_mac(mac_address)
                }
            };
        let prediction = |mac_address, primary_interface, interface_id: Option<&str>| {
            PredictedMachineInterface {
                id: uuid::Uuid::nil(),
                machine_id: MachineId::from_str(
                    "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng",
                )
                .unwrap(),
                mac_address,
                expected_network_segment_type: NetworkSegmentType::HostInband,
                boot_interface_id: interface_id.map(str::to_string),
                primary_interface,
            }
        };
        let pair = |mac_address, interface_id: &str| {
            MachineBootInterfaceTarget::Pair(MachineBootInterface {
                mac_address,
                interface_id: interface_id.to_string(),
            })
        };

        let owned_mac = mac(1);
        let predicted_mac = mac(2);
        let desired_mac = mac(3);

        check_values(
            [
                Check {
                    scenario: "explicit primary prediction outranks an owned primary",
                    input: (
                        None,
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(predicted_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "selected prediction uses a same-MAC owned id",
                    input: (
                        None,
                        vec![interface(
                            predicted_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, None)],
                    ),
                    expect: Some(pair(predicted_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "owned candidate outranks a non-primary prediction",
                    input: (
                        None,
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(owned_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "a sole prediction initializes a MAC-only target",
                    input: (None, vec![], vec![prediction(predicted_mac, false, None)]),
                    expect: Some(MachineBootInterfaceTarget::MacOnly(predicted_mac)),
                },
                Check {
                    scenario: "a whitespace-only id initializes a MAC-only target",
                    input: (
                        None,
                        vec![],
                        vec![prediction(predicted_mac, false, Some("\t\n"))],
                    ),
                    expect: Some(MachineBootInterfaceTarget::MacOnly(predicted_mac)),
                },
                Check {
                    scenario: "several non-primary predictions do not initialize a target",
                    input: (
                        None,
                        vec![],
                        vec![
                            prediction(predicted_mac, false, Some("NIC.Predicted")),
                            prediction(desired_mac, false, Some("NIC.Other")),
                        ],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "an existing pair is never rewritten",
                    input: (
                        Some(pair(desired_mac, "NIC.Remembered")),
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "a MAC-only target uses a unique owned id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            desired_mac,
                            InterfaceType::Data,
                            false,
                            Some("NIC.Remembered"),
                        )],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Remembered")),
                },
                Check {
                    scenario: "a MAC-only target falls back to a predicted id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(desired_mac, InterfaceType::Data, false, None)],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "a whitespace-only owned id falls back to a predicted id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            desired_mac,
                            InterfaceType::Data,
                            false,
                            Some("\t\n"),
                        )],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "ambiguous owned ids stop enrichment",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned.1")),
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned.2")),
                        ],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "duplicate owned ids are still one unique id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![
                            interface(
                                desired_mac,
                                InterfaceType::Data,
                                false,
                                Some(" \tNIC.Owned\n "),
                            ),
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned")),
                        ],
                        vec![],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "ambiguous predicted ids stop enrichment",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![],
                        vec![
                            prediction(desired_mac, false, Some("NIC.Predicted.1")),
                            prediction(desired_mac, false, Some("NIC.Predicted.2")),
                        ],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "a different designated MAC is preserved",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
            ],
            |(desired, interfaces, predictions)| {
                desired_boot_interface_update(desired.as_ref(), &interfaces, &predictions)
            },
        );
    }

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
