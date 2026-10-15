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

use mac_address::MacAddress;
use model::site_explorer::NetworkAdapter as ModelNetworkAdapter;
use nv_redfish::Bmc;
use nv_redfish::chassis::{Chassis, NetworkAdapter};
use nv_redfish::network_device_function::NetworkDeviceFunction;
use nv_redfish::port::Port;

use crate::Error;

pub(crate) struct Config {
    pub(crate) need_network_device_fns: bool,
}

pub(crate) struct ExploredNetworkAdapterCollection<B: Bmc> {
    members: Vec<ExploredNetworkAdapter<B>>,
}

impl<B: Bmc> ExploredNetworkAdapterCollection<B> {
    pub(crate) async fn explore(chassis: &Chassis<B>, config: &Config) -> Result<Self, Error<B>> {
        match chassis.network_adapters().await {
            Ok(Some(network_adapters)) => {
                let mut members = Vec::new();
                for na in network_adapters {
                    members.push(ExploredNetworkAdapter::explore(na, config).await?);
                }
                Ok(Self { members })
            }
            Ok(None) => Ok(Self { members: vec![] }),
            Err(err) => Err(Error::NvRedfish {
                context: "chassis network adapters",
                err,
            }),
        }
    }

    // Find adapater by MAC address. To make it work network adapters
    // must be explored with need_network_device_fns set to true.
    pub(crate) fn find_by_mac(
        &self,
        mac: MacAddress,
    ) -> Option<(&ExploredNetworkAdapter<B>, &NetworkDeviceFunction<B>)> {
        self.members
            .iter()
            .find_map(|a| a.find_by_mac(mac).map(|f| (a, f)))
    }

    pub(crate) fn to_model(&self) -> Vec<ModelNetworkAdapter> {
        self.members.iter().map(|v| v.to_model()).collect()
    }

    pub(crate) fn members(&self) -> &[ExploredNetworkAdapter<B>] {
        &self.members
    }

    /// `fetch_ports` fetches `Port` resources for every adapter in this
    /// collection.
    ///
    /// Port inventory is supplemental, so an error reading one adapter or Port
    /// is logged without failing BMC exploration.
    pub(crate) async fn fetch_ports(&mut self) {
        for adapter in &mut self.members {
            adapter.fetch_ports().await;
        }
    }
}

pub(crate) struct ExploredNetworkAdapter<B: Bmc> {
    pub(crate) adapter: NetworkAdapter<B>,
    pub(crate) functions: Option<Vec<NetworkDeviceFunction<B>>>,
    ports: Vec<Port<B>>,
}

impl<B: Bmc> ExploredNetworkAdapter<B> {
    async fn explore(adapter: NetworkAdapter<B>, config: &Config) -> Result<Self, Error<B>> {
        let functions = if config.need_network_device_fns {
            if let Some(collection) = adapter
                .network_device_functions()
                .await
                .map_err(Error::nv_redfish("network device function collection"))?
            {
                Some(collection.members().await.map_err(Error::nv_redfish(
                    "network device function collection members",
                ))?)
            } else {
                None
            }
        } else {
            None
        };
        Ok(Self {
            adapter,
            functions,
            ports: Vec::new(),
        })
    }

    /// `fetch_ports` keeps every `Port` this adapter can read.
    ///
    /// `member_links()` lets us fetch members independently. Using `members()`
    /// here would let one stale link discard otherwise usable sibling ports.
    async fn fetch_ports(&mut self) {
        let ports = match self.adapter.ports().await {
            Ok(Some(ports)) => ports,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(
                    adapter_id = %self.adapter.raw().id,
                    error = %error,
                    "Failed to fetch network adapter Ports"
                );
                return;
            }
        };

        for port_link in ports.member_links() {
            let port_id = port_link.odata_id().clone();
            match port_link.upgrade::<Port<B>>().await {
                Ok(port) => self.ports.push(port),
                Err(error) => tracing::warn!(
                    adapter_id = %self.adapter.raw().id,
                    %port_id,
                    error = %error,
                    "Failed to fetch network adapter Port"
                ),
            }
        }
    }

    fn find_by_mac(&self, mac: MacAddress) -> Option<&NetworkDeviceFunction<B>> {
        self.functions.iter().flatten().find(|f| {
            f.ethernet_permanent_mac_address()
                .and_then(|mac| mac.as_str().parse::<MacAddress>().ok())
                .is_some_and(|v| v == mac)
        })
    }

    fn to_model(&self) -> ModelNetworkAdapter {
        let hw_id = self.adapter.hardware_id();
        ModelNetworkAdapter {
            id: self.adapter.raw().id.clone(),
            manufacturer: hw_id.manufacturer.map(|v| v.to_string()),
            model: hw_id.model.map(|v| v.to_string()),
            part_number: hw_id.part_number.map(|v| v.to_string()),
            serial_number: Some(
                hw_id
                    .serial_number
                    .map(|v| v.inner().trim())
                    .unwrap_or("")
                    .to_owned(),
            ),
            port_mac_addresses: self.port_mac_addresses(),
        }
    }

    /// `port_mac_addresses` returns MAC addresses reported by this adapter's
    /// fetched `Port` resources.
    ///
    /// Standard `AssociatedMACAddresses` takes precedence for each `Port`.
    /// Some Lenovo XCC firmware reports only OEM `PhysicalPortMacAddress`, so
    /// that value is considered when the standard list contains no usable MAC.
    /// Malformed values are logged and skipped; duplicates retain their
    /// first-seen order.
    fn port_mac_addresses(&self) -> Vec<MacAddress> {
        let mut result = Vec::new();
        for port in &self.ports {
            let mut port_mac_addresses = port
                .associated_mac_addresses()
                .iter()
                .filter_map(|address| self.parse_port_mac_address(port, address.as_str()))
                .collect::<Vec<_>>();

            if port_mac_addresses.is_empty() {
                let oem_address = match port.oem_lenovo() {
                    Ok(Some(lenovo)) => lenovo
                        .physical_port_mac_address()
                        .map(|address| address.as_str().to_owned()),
                    Ok(None) => None,
                    Err(error) => {
                        tracing::warn!(
                            adapter_id = %self.adapter.raw().id,
                            port_id = %port.raw().id,
                            error = %error,
                            "Failed to parse Lenovo network adapter Port data"
                        );
                        continue;
                    }
                };
                port_mac_addresses.extend(
                    oem_address
                        .as_deref()
                        .and_then(|address| self.parse_port_mac_address(port, address)),
                );
            }

            for mac_address in port_mac_addresses {
                if !result.contains(&mac_address) {
                    result.push(mac_address);
                }
            }
        }
        result
    }

    fn parse_port_mac_address(&self, port: &Port<B>, address: &str) -> Option<MacAddress> {
        match address.parse() {
            Ok(mac_address) => Some(mac_address),
            Err(error) => {
                tracing::warn!(
                    adapter_id = %self.adapter.raw().id,
                    port_id = %port.raw().id,
                    mac_address = %address,
                    error = %error,
                    "Failed to parse network adapter Port MAC address"
                );
                None
            }
        }
    }
}
