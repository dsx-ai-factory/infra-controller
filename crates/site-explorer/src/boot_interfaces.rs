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
use db::{DatabaseError, Transaction};
use mac_address::MacAddress;
use model::machine_boot_interface::MachineBootInterface;
use model::site_explorer::ExploredEndpoint;

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
