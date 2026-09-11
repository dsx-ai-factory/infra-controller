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
use carbide_rpc_utils::dhcp::{HostConfig, InterfaceInfo, InterfaceInfoV6};
use carbide_uuid::machine::MachineInterfaceId;
use lru::LruCache;
use rpc::forge::{DhcpDiscovery, DhcpRecord, MessageKind};
use tonic::async_trait;

use super::{DhcpMode, V6Outcome, v6_message_kind};
use crate::Config;
use crate::cache::CacheEntry;
use crate::errors::DhcpError;
use crate::packet_handler::DecodedPacket;

#[derive(Debug)]
pub struct Dpu {}

fn from_host_conf(
    value: &InterfaceInfo,
    interface_id: MachineInterfaceId,
) -> Result<DhcpRecord, DhcpError> {
    let address = value
        .address
        .ok_or_else(|| DhcpError::InvalidInput("IPv4 address is not configured".to_string()))?;
    let gateway = value
        .gateway
        .ok_or_else(|| DhcpError::InvalidInput("IPv4 gateway is not configured".to_string()))?;
    let prefix = value
        .prefix
        .clone()
        .ok_or_else(|| DhcpError::InvalidInput("IPv4 prefix is not configured".to_string()))?;

    // Fill only needed fields. Rest are left empty or none.
    Ok(DhcpRecord {
        machine_id: None,
        machine_interface_id: Some(interface_id),
        segment_id: None,
        subdomain_id: None,
        fqdn: value.fqdn.clone(),
        mac_address: "dummy".to_string(),
        address: address.to_string(),
        mtu: 0,
        prefix,
        gateway: Some(gateway.to_string()),
        booturl: value.booturl.clone(),
        last_invalidation_time: None,
        ntp_servers: vec![],
    })
}

/// Build the family-neutral API record consumed by the DHCPv6 encoder.
fn from_host_conf_v6(
    value: &InterfaceInfo,
    ipv6: &InterfaceInfoV6,
    interface_id: MachineInterfaceId,
) -> Result<DhcpRecord, DhcpError> {
    let mtu = value
        .mtu
        .map(i32::try_from)
        .transpose()
        .map_err(|_| DhcpError::InvalidInput("DHCPv6 MTU exceeds the API range".to_string()))?
        .unwrap_or_default();

    Ok(DhcpRecord {
        machine_id: None,
        machine_interface_id: Some(interface_id),
        segment_id: None,
        subdomain_id: None,
        fqdn: value.fqdn.clone(),
        mac_address: String::new(),
        address: ipv6
            .address
            .map_or_else(String::new, |address| address.to_string()),
        mtu,
        prefix: ipv6.prefix.clone(),
        gateway: None,
        booturl: None,
        last_invalidation_time: None,
        ntp_servers: vec![],
    })
}

#[async_trait]
impl DhcpMode for Dpu {
    async fn discover_dhcp(
        &self,
        discovery_request: DhcpDiscovery,
        config: &Config,
        _machine_cache: &mut std::sync::Arc<tokio::sync::Mutex<LruCache<String, CacheEntry>>>,
    ) -> Result<DhcpRecord, DhcpError> {
        let Some(circuit_id) = discovery_request.circuit_id else {
            return Err(DhcpError::MissingArgument(
                "Missing circuit id.".to_string(),
            ));
        };

        let ip_details = config
            .host_config
            .as_ref()
            .ok_or_else(|| DhcpError::InvalidInput("host input is invalid.".to_string()))?
            .host_ip_addresses
            .get(&circuit_id)
            .ok_or_else(|| {
                DhcpError::MissingArgument(format!("Could not find IP details for {circuit_id}"))
            })?;

        let Some(host_config) = &config.host_config else {
            return Err(DhcpError::MissingArgument(
                "host_config is missing.".to_string(),
            ));
        };

        from_host_conf(ip_details, host_config.host_interface_id)
    }

    /// Resolve DHCPv6 directly from the interface block delivered in host.yaml.
    async fn discover_dhcp_v6(
        &self,
        discovery_request: DhcpDiscovery,
        config: &Config,
        _machine_cache: &mut std::sync::Arc<tokio::sync::Mutex<LruCache<String, CacheEntry>>>,
    ) -> Result<V6Outcome, DhcpError> {
        let message_kind = v6_message_kind(&discovery_request)?;
        let Some(circuit_id) = discovery_request.circuit_id else {
            return Err(DhcpError::MissingArgument("DHCPv6 circuit id".to_string()));
        };

        // DPU mode selects the precomputed host entry by the receiving interface,
        // matching its DHCPv4 path.
        let host_config = config
            .host_config
            .as_ref()
            .ok_or_else(|| DhcpError::InvalidInput("host input is invalid".to_string()))?;
        let interface = host_config
            .host_ip_addresses
            .get(&circuit_id)
            .ok_or_else(|| {
                DhcpError::MissingArgument(format!("could not find IP details for {circuit_id}"))
            })?;
        let Some(ipv6) = interface.ipv6.as_ref() else {
            // No IPv6 block means this interface is v6-disabled. This is
            // distinct from a SLAAC-only block whose address is absent.
            return Err(DhcpError::NoIpv6Configuration(circuit_id));
        };

        let record = || from_host_conf_v6(interface, ipv6, host_config.host_interface_id);

        match message_kind {
            // Stateless requests receive configuration regardless of whether
            // the interface also owns a stateful address.
            MessageKind::V6InfoRequest => Ok(V6Outcome::OptionsOnly(record()?)),
            MessageKind::V6Solicit | MessageKind::V6Request if ipv6.address.is_some() => {
                Ok(V6Outcome::Stateful(record()?))
            }
            // A present prefix with no address is the explicit SLAAC-only
            // contract. The encoder retains the wire type to choose the RFC status.
            MessageKind::V6Solicit | MessageKind::V6Request => Ok(V6Outcome::NoAddress),
            _ => Err(DhcpError::InvalidInput(
                "non-DHCPv6 message kind passed to DHCPv6 mode".to_string(),
            )),
        }
    }

    /// Here circuit is interface name. This is what dhcp-relay used to fill.
    fn get_circuit_id(&self, _packet: &DecodedPacket, circuit_id: &str) -> Option<String> {
        Some(circuit_id.to_string())
    }

    fn should_be_relayed(&self) -> bool {
        false
    }
}

fn validate_host_config(host_config: &HostConfig) -> Result<(), DhcpError> {
    for (circuit_id, interface) in &host_config.host_ip_addresses {
        match (&interface.address, &interface.gateway, &interface.prefix) {
            (Some(_), Some(_), Some(prefix)) if !prefix.is_empty() => {}
            (None, None, None) => {}
            _ => {
                return Err(DhcpError::InvalidInput(format!(
                    "IPv4 address, gateway, and non-empty prefix for {circuit_id} must be configured together"
                )));
            }
        }
    }

    Ok(())
}

/// This config is fetched by dpu-agent from controller periodically. In case of any change in
/// this configuration, dpu-agent MUST restart dhcp-server.
pub async fn get_host_config(
    host_config_path: Option<String>,
) -> Result<Option<HostConfig>, DhcpError> {
    let Some(host_config) = host_config_path else {
        return Err(DhcpError::MissingArgument(
            "--host_config is missing.".to_string(),
        ));
    };

    let f = tokio::fs::read_to_string(host_config).await?;
    let host_config: HostConfig = serde_yaml::from_str(&f)?;
    validate_host_config(&host_config)?;

    Ok(Some(host_config))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use carbide_rpc_utils::dhcp::InterfaceInfoV6;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    fn summarize_ipv4_config(
        interface: InterfaceInfo,
    ) -> Result<(String, Option<String>, String), ()> {
        let interface_id = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        from_host_conf(&interface, interface_id)
            .map(|record| (record.address, record.gateway, record.prefix))
            .map_err(drop)
    }

    fn validate_interface_presence(interface: InterfaceInfo) -> Result<(), ()> {
        let host_config = HostConfig {
            host_interface_id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
            host_ip_addresses: [("vlan100".to_string(), interface)].into(),
        };
        validate_host_config(&host_config).map_err(drop)
    }

    fn validate_yaml_interface_prefix(prefix: &str) -> Result<(), ()> {
        let yaml = format!(
            "address: 192.0.2.10\ngateway: 192.0.2.1\nprefix: \"{prefix}\"\nfqdn: host.example.com\nbooturl: null\n"
        );
        let interface = serde_yaml::from_str(&yaml).map_err(drop)?;
        validate_interface_presence(interface)
    }

    #[test]
    fn host_config_requires_ipv4_for_dhcpv4() {
        scenarios!(run = summarize_ipv4_config;
            "complete IPv4 configuration" {
                InterfaceInfo {
                    address: Some(Ipv4Addr::new(192, 0, 2, 10)),
                    gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Yields((
                    "192.0.2.10".to_string(),
                    Some("192.0.2.1".to_string()),
                    "192.0.2.0/24".to_string(),
                )),
            }
            "IPv6-only configuration" {
                InterfaceInfo {
                    ipv6: Some(InterfaceInfoV6 {
                        address: Some("2001:db8::10".parse().unwrap()),
                        prefix: "2001:db8::/64".to_string(),
                    }),
                    ..Default::default()
                } => Fails,
            }
            "missing IPv4 gateway" {
                InterfaceInfo {
                    address: Some(Ipv4Addr::new(192, 0, 2, 10)),
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "missing IPv4 prefix" {
                InterfaceInfo {
                    address: Some(Ipv4Addr::new(192, 0, 2, 10)),
                    gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                    ..Default::default()
                } => Fails,
            }
        );
    }

    #[test]
    fn host_config_rejects_partial_ipv4_configuration() {
        scenarios!(run = validate_interface_presence;
            "complete IPv4 configuration" {
                InterfaceInfo {
                    address: Some(Ipv4Addr::new(192, 0, 2, 10)),
                    gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Yields(()),
            }
            "all IPv4 fields absent in IPv6-only configuration" {
                InterfaceInfo {
                    ipv6: Some(InterfaceInfoV6 {
                        address: Some("2001:db8::10".parse().unwrap()),
                        prefix: "2001:db8::/64".to_string(),
                    }),
                    ..Default::default()
                } => Yields(()),
            }
            "missing IPv4 address" {
                InterfaceInfo {
                    gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "missing IPv4 gateway" {
                InterfaceInfo {
                    address: Some(Ipv4Addr::new(192, 0, 2, 10)),
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Fails,
            }
            "missing IPv4 prefix" {
                InterfaceInfo {
                    address: Some(Ipv4Addr::new(192, 0, 2, 10)),
                    gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                    ..Default::default()
                } => Fails,
            }
            "IPv4 address only" {
                InterfaceInfo {
                    address: Some(Ipv4Addr::new(192, 0, 2, 10)),
                    ..Default::default()
                } => Fails,
            }
            "IPv4 gateway only" {
                InterfaceInfo {
                    gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                    ..Default::default()
                } => Fails,
            }
            "IPv4 prefix only" {
                InterfaceInfo {
                    prefix: Some("192.0.2.0/24".to_string()),
                    ..Default::default()
                } => Fails,
            }
        );
    }

    #[test]
    fn host_config_rejects_empty_ipv4_prefix_from_yaml() {
        scenarios!(run = validate_yaml_interface_prefix;
            "configured IPv4 prefix" {
                "192.0.2.0/24" => Yields(()),
            }
            "empty IPv4 prefix" {
                "" => Fails,
            }
        );
    }
}
