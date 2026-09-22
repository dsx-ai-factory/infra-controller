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
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use carbide_uuid::UuidConversionError;
use carbide_uuid::machine::MachineInterfaceId;
use ipnetwork::{Ipv4Network, Ipv6Network};
use rpc::InterfaceFunctionType;
use rpc::errors::RpcDataConversionError;
use rpc::forge::ManagedHostNetworkConfigResponse;
use serde::{Deserialize, Serialize};

/// This structure is used in dhcp-server and dpu-agent. dpu-agent passes these information to
/// dhcp-server. dhcp-server uses it for handling packet.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DhcpConfig {
    pub lease_time_secs: u32,
    pub renewal_time_secs: u32,
    pub rebinding_time_secs: u32,
    pub carbide_nameservers: Vec<Ipv4Addr>,
    // Mandatory for Controller mode.
    pub carbide_api_url: Option<String>,
    pub carbide_ntpservers: Vec<Ipv4Addr>,
    pub carbide_provisioning_server_ipv4: Ipv4Addr,
    pub carbide_dhcp_server: Ipv4Addr,
    #[serde(default)]
    pub carbide_nameservers_v6: Vec<Ipv6Addr>,
    #[serde(default)]
    pub carbide_ntpservers_v6: Vec<Ipv6Addr>,
    /// Optional DHCPv6 server-address configuration.
    ///
    /// Listener admission does not depend on it, and sockets bind `[::]:547`
    /// per interface.
    #[serde(default)]
    pub carbide_dhcp_server_v6: Option<Ipv6Addr>,
    #[serde(default)]
    pub dhcpv6_preferred_lifetime_secs: u32,
    #[serde(default)]
    pub dhcpv6_valid_lifetime_secs: u32,
}

#[derive(thiserror::Error, Debug)]
pub enum DhcpDataError {
    #[error("DhcpDataError: AddressParseError: {0}")]
    AddressParseError(#[from] std::net::AddrParseError),
    #[error("DhcpDataError: missing: {0}")]
    ParameterMissing(&'static str),
    #[error("DhcpDataError: IpNetworkError: {0}")]
    IpNetworkError(#[from] ipnetwork::IpNetworkError),
    #[error("DhcpDataError: RpcDataConversionError: {0}")]
    RpcConversion(#[from] RpcDataConversionError),
    #[error("DhcpDataError: UuidConversionError: {0}")]
    UuidConversion(#[from] UuidConversionError),
    #[error("DhcpDataError: UuidParseError: {0}")]
    UuidParseError(#[from] carbide_uuid::typed_uuids::UuidError),
}

impl Default for DhcpConfig {
    fn default() -> Self {
        Self {
            // Use some sane defaults
            lease_time_secs: 604800,
            renewal_time_secs: 3600,
            rebinding_time_secs: 432000,
            carbide_nameservers: vec![],
            carbide_api_url: None,
            carbide_ntpservers: vec![],

            // These two must be updated with valid values.
            carbide_provisioning_server_ipv4: Ipv4Addr::from([127, 0, 0, 1]),
            carbide_dhcp_server: Ipv4Addr::from([127, 0, 0, 1]),
            carbide_nameservers_v6: vec![],
            carbide_ntpservers_v6: vec![],
            carbide_dhcp_server_v6: None,
            dhcpv6_preferred_lifetime_secs: 0,
            dhcpv6_valid_lifetime_secs: 0,
        }
    }
}

impl DhcpConfig {
    pub fn from_forge_dhcp_config(
        carbide_provisioning_server_ipv4: Ipv4Addr,
        carbide_ntpservers: Vec<Ipv4Addr>,
        carbide_nameservers: Vec<Ipv4Addr>,
        carbide_nameservers_v6: Vec<Ipv6Addr>,
        loopback_ip: Ipv4Addr,
    ) -> Result<Self, DhcpDataError> {
        Ok(DhcpConfig {
            carbide_nameservers,
            carbide_nameservers_v6,
            carbide_ntpservers,
            carbide_provisioning_server_ipv4,
            carbide_dhcp_server: loopback_ip,
            ..Default::default()
        })
    }
}

type CircuitId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    pub host_interface_id: MachineInterfaceId,
    // BTreeMap is needed because we want ordered map. Due to unordered nature of HashMap, the
    // serialized output was changing very frequently and it was causing dpu-agent to restart dhcp-server
    // very frequently although no config was changed.
    pub host_ip_addresses: BTreeMap<CircuitId, InterfaceInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterfaceInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    pub fqdn: String,
    pub booturl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<InterfaceInfoV6>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterfaceInfoV6 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Ipv6Addr>,
    pub prefix: String,
}

impl HostConfig {
    pub fn try_from(
        value: ManagedHostNetworkConfigResponse,
        physical_rep: &str,
        virt_rep_begin: &str,
        sf_id: &str,
        is_dpu_os: bool,
    ) -> Result<Self, DhcpDataError> {
        let mut host_ip_addresses = BTreeMap::new();
        let virtualization_type = value.network_virtualization_type();

        let interface_configs = if value.use_admin_network {
            let Some(interface_config) = value.admin_interface else {
                return Err(DhcpDataError::ParameterMissing("AdminInterface"));
            };
            vec![interface_config]
        } else {
            value.tenant_interfaces
        };

        for interface in interface_configs {
            let interface_name = if (virtualization_type
                == ::rpc::forge::VpcVirtualizationType::Fnn
                && !interface.is_l2_segment)
                || !is_dpu_os
            {
                if interface.function_type() == InterfaceFunctionType::Physical {
                    // pf0hpf_sf/if
                    physical_rep.to_string()
                } else {
                    // pf0vf{0-15}_sf/if
                    format!(
                        "{}{}{}",
                        virt_rep_begin,
                        interface.virtual_function_id(),
                        sf_id
                    )
                }
            } else {
                format!("vlan{}", interface.vlan_id)
            };
            host_ip_addresses.insert(interface_name, InterfaceInfo::try_from(interface)?);
        }

        Ok(HostConfig {
            host_interface_id: value
                .host_interface_id
                .ok_or(DhcpDataError::ParameterMissing("HostInterfaceId"))?
                .parse()?,
            host_ip_addresses,
        })
    }
}

// This conversion continues to consume the IPv4 compatibility fields during
// the address-list rollout.
#[allow(deprecated)]
impl TryFrom<::rpc::forge::FlatInterfaceConfig> for InterfaceInfo {
    type Error = DhcpDataError;
    fn try_from(value: ::rpc::forge::FlatInterfaceConfig) -> Result<Self, Self::Error> {
        let (address, gateway, prefix) = match (&value.ip, &value.gateway, &value.prefix) {
            (None, None, None) => (None, None, None),
            (Some(ip), Some(gateway), Some(prefix)) => (
                Some(ip.parse()?),
                Some(Ipv4Network::from_str(gateway)?.ip()),
                Some(prefix.clone()),
            ),
            _ => {
                return Err(DhcpDataError::ParameterMissing(
                    "complete IPv4 interface configuration",
                ));
            }
        };

        let ipv6 = value
            .addresses
            .iter()
            .find(|address| address.address_family == i32::from(::rpc::forge::AddressFamily::V6))
            // Routing-only loopback or SVI data does not configure DHCP on this interface.
            .filter(|address| !address.ip.is_empty() || !address.interface_prefix.is_empty())
            .map(|ipv6| -> Result<InterfaceInfoV6, DhcpDataError> {
                // An empty address preserves an explicitly enabled SLAAC-only
                // prefix without manufacturing a stateful host binding.
                let prefix = Ipv6Network::from_str(&ipv6.interface_prefix)?;
                Ok(InterfaceInfoV6 {
                    address: if ipv6.ip.is_empty() {
                        None
                    } else {
                        Some(ipv6.ip.parse()?)
                    },
                    prefix: prefix.to_string(),
                })
            })
            .transpose()?;

        Ok(InterfaceInfo {
            address,
            gateway,
            prefix,
            fqdn: value.fqdn,
            booturl: value.booturl,
            mtu: value.mtu,
            ipv6,
        })
    }
}

const DHCP_TIMESTAMP_FILE_HBN: &str = "/var/support/forge-dhcp/logs/dhcp_timestamps.json";
const DHCP_TIMESTAMP_FILE_HBN_TMP: &str = "/var/support/forge-dhcp/logs/dhcp_timestamps.json.tmp";
const DHCP_TIMESTAMP_FILE_DPU: &str =
    "/var/lib/hbn/var/support/forge-dhcp/logs/dhcp_timestamps.json";
const DHCP_TIMESTAMP_FILE_TEST: &str = "/tmp/timestamps.json";
#[derive(Serialize, Deserialize)]
pub struct DhcpTimestamps {
    timestamps: HashMap<MachineInterfaceId, String>,

    #[serde(skip)]
    path: DhcpTimestampsFilePath,
}

#[derive(Default)]
pub enum DhcpTimestampsFilePath {
    HbnTmp,
    Hbn,
    Dpu,
    Test,
    #[default]
    NotSet,
}

impl DhcpTimestampsFilePath {
    pub fn path_str(&self) -> &str {
        match self {
            Self::HbnTmp => DHCP_TIMESTAMP_FILE_HBN_TMP,
            Self::Hbn => DHCP_TIMESTAMP_FILE_HBN,
            Self::Dpu => DHCP_TIMESTAMP_FILE_DPU,
            Self::Test => DHCP_TIMESTAMP_FILE_TEST,
            Self::NotSet => "Not set",
        }
    }
}

impl DhcpTimestamps {
    pub fn new(filepath: DhcpTimestampsFilePath) -> Self {
        Self {
            timestamps: HashMap::new(),
            path: filepath,
        }
    }

    pub fn add_timestamp(&mut self, host_id: MachineInterfaceId, timestamp: String) {
        self.timestamps.insert(host_id, timestamp);
    }

    pub fn get_timestamp(&self, host_id: &MachineInterfaceId) -> Option<&String> {
        self.timestamps.get(host_id)
    }

    pub fn write(&self) -> eyre::Result<()> {
        if let DhcpTimestampsFilePath::NotSet = self.path {
            // No-op
            return Ok(());
        }
        let timestamp_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(self.path.path_str())?;

        serde_json::to_writer(timestamp_file, self)?;
        if let DhcpTimestampsFilePath::HbnTmp = self.path {
            // Rename the file.
            fs::rename(DHCP_TIMESTAMP_FILE_HBN_TMP, DHCP_TIMESTAMP_FILE_HBN)?;
        }
        Ok(())
    }

    pub fn read(&mut self) -> eyre::Result<()> {
        if let DhcpTimestampsFilePath::NotSet = self.path {
            // No-op
            return Ok(());
        }
        let timestamp_file = fs::OpenOptions::new()
            .read(true)
            .open(self.path.path_str())?;
        *self = serde_json::from_reader(timestamp_file)?;
        Ok(())
    }
}

impl Default for DhcpTimestamps {
    fn default() -> Self {
        Self::new(DhcpTimestampsFilePath::default())
    }
}

impl IntoIterator for DhcpTimestamps {
    type Item = (MachineInterfaceId, String);
    type IntoIter = std::collections::hash_map::IntoIter<MachineInterfaceId, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.timestamps.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use rpc::forge::{
        AddressFamily, FlatInterfaceConfig, InterfaceAddressConfig, InterfaceFunctionType,
        ManagedHostNetworkConfigResponse, VpcVirtualizationType,
    };

    use super::*;

    const HOST_INTERFACE_ID: &str = "11111111-1111-1111-1111-111111111111";
    const DEFAULT_LEASE_TIME_SECS: u32 = 7 * 24 * 60 * 60;

    #[derive(Debug, PartialEq)]
    struct DhcpConfigSummary {
        provisioning_server: Ipv4Addr,
        dhcp_server: Ipv4Addr,
        ntpservers: Vec<Ipv4Addr>,
        nameservers: Vec<Ipv4Addr>,
        nameservers_v6: Vec<Ipv6Addr>,
        lease_time_secs: u32,
    }

    #[derive(Debug, PartialEq)]
    struct InterfaceSummary {
        address: Option<Ipv4Addr>,
        gateway: Option<Ipv4Addr>,
        prefix: Option<String>,
        fqdn: String,
        booturl: Option<String>,
        mtu: Option<u32>,
    }

    #[derive(Debug, PartialEq)]
    struct HostConfigSummary {
        host_interface_id: String,
        host_ip_addresses: Vec<(String, InterfaceSummary)>,
    }

    fn host_interface_id() -> MachineInterfaceId {
        HOST_INTERFACE_ID.parse().unwrap()
    }

    // Builds the deprecated compatibility shape consumed by this conversion.
    #[allow(deprecated)]
    fn interface_config(
        function_type: InterfaceFunctionType,
        vlan_id: u32,
        virtual_function_id: Option<u32>,
        is_l2_segment: bool,
        ip: &str,
        gateway: &str,
    ) -> FlatInterfaceConfig {
        FlatInterfaceConfig {
            function_type: function_type as i32,
            vlan_id,
            gateway: (!gateway.is_empty()).then(|| gateway.to_string()),
            ip: (!ip.is_empty()).then(|| ip.to_string()),
            virtual_function_id,
            prefix: Some("192.0.2.0/24".to_string()),
            fqdn: "host.example.com".to_string(),
            booturl: Some("http://boot.example.com/ipxe".to_string()),
            is_l2_segment,
            mtu: Some(9000),
            ..Default::default()
        }
    }

    /// Build caller-selected family-neutral IPv6 interface data.
    fn interface_config_with_ipv6(address: &str, prefix: &str) -> FlatInterfaceConfig {
        FlatInterfaceConfig {
            addresses: vec![InterfaceAddressConfig {
                address_family: AddressFamily::V6.into(),
                ip: address.to_string(),
                interface_prefix: prefix.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// Build deprecated compatibility input without IPv4 addressing.
    #[allow(deprecated)]
    fn ipv6_only_interface_config() -> FlatInterfaceConfig {
        let mut config =
            interface_config(InterfaceFunctionType::Virtual, 100, Some(3), false, "", "");
        config.prefix = None;
        config
    }

    fn host_network_config(
        use_admin_network: bool,
        admin_interface: Option<FlatInterfaceConfig>,
        tenant_interfaces: Vec<FlatInterfaceConfig>,
        virtualization_type: VpcVirtualizationType,
        host_interface_id: Option<String>,
    ) -> ManagedHostNetworkConfigResponse {
        ManagedHostNetworkConfigResponse {
            use_admin_network,
            admin_interface,
            tenant_interfaces,
            network_virtualization_type: Some(virtualization_type as i32),
            host_interface_id,
            ..Default::default()
        }
    }

    fn summarize_interface(interface: InterfaceInfo) -> InterfaceSummary {
        InterfaceSummary {
            address: interface.address,
            gateway: interface.gateway,
            prefix: interface.prefix,
            fqdn: interface.fqdn,
            booturl: interface.booturl,
            mtu: interface.mtu,
        }
    }

    fn summarize_host_config(
        (config, is_dpu_os): (ManagedHostNetworkConfigResponse, bool),
    ) -> Result<HostConfigSummary, &'static str> {
        HostConfig::try_from(config, "p0", "vf", "sf", is_dpu_os)
            .map(|host_config| HostConfigSummary {
                host_interface_id: host_config.host_interface_id.to_string(),
                host_ip_addresses: host_config
                    .host_ip_addresses
                    .into_iter()
                    .map(|(name, interface)| (name, summarize_interface(interface)))
                    .collect(),
            })
            .map_err(dhcp_error_kind)
    }

    fn summarize_dhcp_config(
        (provisioning_server, ntpservers, nameservers, nameservers_v6, dhcp_server): (
            Ipv4Addr,
            Vec<Ipv4Addr>,
            Vec<Ipv4Addr>,
            Vec<Ipv6Addr>,
            Ipv4Addr,
        ),
    ) -> Result<DhcpConfigSummary, &'static str> {
        DhcpConfig::from_forge_dhcp_config(
            provisioning_server,
            ntpservers,
            nameservers,
            nameservers_v6,
            dhcp_server,
        )
        .map(|config| DhcpConfigSummary {
            provisioning_server: config.carbide_provisioning_server_ipv4,
            dhcp_server: config.carbide_dhcp_server,
            ntpservers: config.carbide_ntpservers,
            nameservers: config.carbide_nameservers,
            nameservers_v6: config.carbide_nameservers_v6,
            lease_time_secs: config.lease_time_secs,
        })
        .map_err(dhcp_error_kind)
    }

    fn summarize_flat_interface(
        config: FlatInterfaceConfig,
    ) -> Result<InterfaceSummary, &'static str> {
        InterfaceInfo::try_from(config)
            .map(summarize_interface)
            .map_err(dhcp_error_kind)
    }

    /// Convert only the IPv6 sidecar so table cases remain focused on its contract.
    fn summarize_flat_interface_ipv6(
        config: FlatInterfaceConfig,
    ) -> Result<Option<InterfaceInfoV6>, &'static str> {
        InterfaceInfo::try_from(config)
            .map(|interface| interface.ipv6)
            .map_err(dhcp_error_kind)
    }

    fn dhcp_error_kind(error: DhcpDataError) -> &'static str {
        match error {
            DhcpDataError::AddressParseError(_) => "address-parse",
            DhcpDataError::ParameterMissing(_) => "parameter-missing",
            DhcpDataError::IpNetworkError(_) => "ip-network",
            DhcpDataError::RpcConversion(_) => "rpc-conversion",
            DhcpDataError::UuidConversion(_) => "uuid-conversion",
            DhcpDataError::UuidParseError(_) => "uuid-parse",
        }
    }

    #[test]
    fn builds_dhcp_config_from_forge_values() {
        scenarios!(summarize_dhcp_config:
            "configured addresses" {
                (
                    Ipv4Addr::new(192, 0, 2, 10),
                    vec![Ipv4Addr::new(192, 0, 2, 20)],
                    vec![Ipv4Addr::new(192, 0, 2, 53)],
                    vec!["2001:db8::53".parse::<Ipv6Addr>().unwrap()],
                    Ipv4Addr::new(127, 0, 0, 2),
                ) => Yields(DhcpConfigSummary {
                    provisioning_server: Ipv4Addr::new(192, 0, 2, 10),
                    dhcp_server: Ipv4Addr::new(127, 0, 0, 2),
                    ntpservers: vec![Ipv4Addr::new(192, 0, 2, 20)],
                    nameservers: vec![Ipv4Addr::new(192, 0, 2, 53)],
                    nameservers_v6: vec!["2001:db8::53".parse::<Ipv6Addr>().unwrap()],
                    lease_time_secs: DEFAULT_LEASE_TIME_SECS,
                }),
            }
        );
    }

    #[test]
    fn converts_flat_interface_config() {
        scenarios!(summarize_flat_interface:
            "valid interface" {
                interface_config(
                    InterfaceFunctionType::Virtual,
                    100,
                    Some(3),
                    false,
                    "192.0.2.50",
                    "192.0.2.1/24",
                ) => Yields(InterfaceSummary {
                    address: Some(Ipv4Addr::new(192, 0, 2, 50)),
                    gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                    prefix: Some("192.0.2.0/24".to_string()),
                    fqdn: "host.example.com".to_string(),
                    booturl: Some("http://boot.example.com/ipxe".to_string()),
                    mtu: Some(9000),
                }),
                ipv6_only_interface_config() => Yields(InterfaceSummary {
                    address: None,
                    gateway: None,
                    prefix: None,
                    fqdn: "host.example.com".to_string(),
                    booturl: Some("http://boot.example.com/ipxe".to_string()),
                    mtu: Some(9000),
                }),
            }

            "invalid or incomplete addresses" {
                interface_config(
                    InterfaceFunctionType::Virtual,
                    100,
                    Some(3),
                    false,
                    "not an ip",
                    "192.0.2.1/24",
                ) => FailsWith("address-parse"),
                interface_config(
                    InterfaceFunctionType::Virtual,
                    100,
                    Some(3),
                    false,
                    "192.0.2.50",
                    "not a network",
                ) => FailsWith("ip-network"),
                interface_config(
                    InterfaceFunctionType::Virtual,
                    100,
                    Some(3),
                    false,
                    "",
                    "192.0.2.1/24",
                ) => FailsWith("parameter-missing"),
            }
        );
    }

    #[test]
    fn converts_host_network_config() {
        scenarios!(summarize_host_config:
            "admin network uses vlan circuit id" {
                (
                    host_network_config(
                        true,
                        Some(interface_config(
                            InterfaceFunctionType::Physical,
                            100,
                            None,
                            true,
                            "192.0.2.10",
                            "192.0.2.1/24",
                        )),
                        vec![],
                        VpcVirtualizationType::EthernetVirtualizer,
                        Some(HOST_INTERFACE_ID.to_string()),
                    ),
                    true,
                ) => Yields(HostConfigSummary {
                    host_interface_id: HOST_INTERFACE_ID.to_string(),
                    host_ip_addresses: vec![(
                        "vlan100".to_string(),
                        InterfaceSummary {
                            address: Some(Ipv4Addr::new(192, 0, 2, 10)),
                            gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                            prefix: Some("192.0.2.0/24".to_string()),
                            fqdn: "host.example.com".to_string(),
                            booturl: Some("http://boot.example.com/ipxe".to_string()),
                            mtu: Some(9000),
                        },
                    )],
                }),
            }

            "fnn virtual functions use representor circuit id" {
                (
                    host_network_config(
                        false,
                        None,
                        vec![interface_config(
                            InterfaceFunctionType::Virtual,
                            200,
                            Some(3),
                            false,
                            "192.0.2.20",
                            "192.0.2.1/24",
                        )],
                        VpcVirtualizationType::Fnn,
                        Some(HOST_INTERFACE_ID.to_string()),
                    ),
                    true,
                ) => Yields(HostConfigSummary {
                    host_interface_id: HOST_INTERFACE_ID.to_string(),
                    host_ip_addresses: vec![(
                        "vf3sf".to_string(),
                        InterfaceSummary {
                            address: Some(Ipv4Addr::new(192, 0, 2, 20)),
                            gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                            prefix: Some("192.0.2.0/24".to_string()),
                            fqdn: "host.example.com".to_string(),
                            booturl: Some("http://boot.example.com/ipxe".to_string()),
                            mtu: Some(9000),
                        },
                    )],
                }),
            }

            "non dpu os uses physical representor" {
                (
                    host_network_config(
                        false,
                        None,
                        vec![interface_config(
                            InterfaceFunctionType::Physical,
                            300,
                            None,
                            true,
                            "192.0.2.30",
                            "192.0.2.1/24",
                        )],
                        VpcVirtualizationType::EthernetVirtualizer,
                        Some(HOST_INTERFACE_ID.to_string()),
                    ),
                    false,
                ) => Yields(HostConfigSummary {
                    host_interface_id: HOST_INTERFACE_ID.to_string(),
                    host_ip_addresses: vec![(
                        "p0".to_string(),
                        InterfaceSummary {
                            address: Some(Ipv4Addr::new(192, 0, 2, 30)),
                            gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                            prefix: Some("192.0.2.0/24".to_string()),
                            fqdn: "host.example.com".to_string(),
                            booturl: Some("http://boot.example.com/ipxe".to_string()),
                            mtu: Some(9000),
                        },
                    )],
                }),
            }

            "missing required fields" {
                (
                    host_network_config(
                        true,
                        None,
                        vec![],
                        VpcVirtualizationType::EthernetVirtualizer,
                        Some(HOST_INTERFACE_ID.to_string()),
                    ),
                    true,
                ) => FailsWith("parameter-missing"),
                (
                    host_network_config(
                        false,
                        None,
                        vec![interface_config(
                            InterfaceFunctionType::Physical,
                            400,
                            None,
                            true,
                            "192.0.2.40",
                            "192.0.2.1/24",
                        )],
                        VpcVirtualizationType::EthernetVirtualizer,
                        None,
                    ),
                    true,
                ) => FailsWith("parameter-missing"),
            }
        );
    }

    /// Verifies DHCP config IPv6 fields round-trip and old configs default them.
    #[test]
    fn dhcp_config_v6_fields_round_trip_and_default_when_absent() {
        let config = DhcpConfig {
            carbide_nameservers_v6: vec!["2001:db8::53".parse().unwrap()],
            carbide_ntpservers_v6: vec!["2001:db8::123".parse().unwrap()],
            carbide_dhcp_server_v6: Some("2001:db8::1".parse().unwrap()),
            dhcpv6_preferred_lifetime_secs: 3600,
            dhcpv6_valid_lifetime_secs: 7200,
            ..Default::default()
        };

        // Serialize a populated config and verify the IPv6 fields survive.
        let wire = serde_json::to_string(&config).expect("dhcp config serializes");
        let recovered: DhcpConfig = serde_json::from_str(&wire).expect("dhcp config deserializes");
        assert_eq!(
            recovered.carbide_nameservers_v6,
            vec![Ipv6Addr::from_str("2001:db8::53").unwrap()]
        );
        assert_eq!(
            recovered.carbide_ntpservers_v6,
            vec![Ipv6Addr::from_str("2001:db8::123").unwrap()]
        );
        assert_eq!(
            recovered.carbide_dhcp_server_v6,
            Some(Ipv6Addr::from_str("2001:db8::1").unwrap())
        );
        assert_eq!(recovered.dhcpv6_preferred_lifetime_secs, 3600);
        assert_eq!(recovered.dhcpv6_valid_lifetime_secs, 7200);

        // Deserialize old-style JSON and verify the new fields default cleanly.
        let old_wire = r#"{
            "lease_time_secs": 604800,
            "renewal_time_secs": 3600,
            "rebinding_time_secs": 432000,
            "carbide_nameservers": [],
            "carbide_api_url": null,
            "carbide_ntpservers": [],
            "carbide_provisioning_server_ipv4": "127.0.0.1",
            "carbide_dhcp_server": "127.0.0.1"
        }"#;
        let old_config: DhcpConfig =
            serde_json::from_str(old_wire).expect("old dhcp config deserializes");
        assert!(old_config.carbide_nameservers_v6.is_empty());
        assert!(old_config.carbide_ntpservers_v6.is_empty());
        assert_eq!(old_config.carbide_dhcp_server_v6, None);
        assert_eq!(old_config.dhcpv6_preferred_lifetime_secs, 0);
        assert_eq!(old_config.dhcpv6_valid_lifetime_secs, 0);
    }

    /// Verifies per-interface IPv6 details round-trip and old host configs default them.
    #[test]
    fn interface_info_ipv6_round_trip_and_defaults_when_absent() {
        let interface = InterfaceInfo {
            address: Some(Ipv4Addr::new(192, 0, 2, 10)),
            gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
            prefix: Some("192.0.2.0/24".to_string()),
            fqdn: "host.example.com".to_string(),
            booturl: None,
            mtu: Some(9000),
            ipv6: Some(InterfaceInfoV6 {
                address: Some("2001:db8::10".parse().unwrap()),
                prefix: "2001:db8::/64".to_string(),
            }),
        };

        // Serialize a populated interface and verify the IPv6 sub-record survives.
        let wire = serde_json::to_string(&interface).expect("interface serializes");
        let recovered: InterfaceInfo = serde_json::from_str(&wire).expect("interface deserializes");
        assert_eq!(recovered.ipv6, interface.ipv6);

        // Deserialize old-style JSON and verify the IPv6 field defaults to absent.
        let old_wire = r#"{
            "address": "192.0.2.10",
            "gateway": "192.0.2.1",
            "prefix": "192.0.2.0/24",
            "fqdn": "host.example.com",
            "booturl": null
        }"#;
        let old_interface: InterfaceInfo =
            serde_json::from_str(old_wire).expect("old interface deserializes");
        assert_eq!(old_interface.address, interface.address);
        assert_eq!(old_interface.gateway, interface.gateway);
        assert_eq!(old_interface.prefix, interface.prefix);
        assert_eq!(old_interface.ipv6, None);

        let ipv6_only_wire = r#"{
            "fqdn": "host.example.com",
            "booturl": null
        }"#;
        let ipv6_only_interface: InterfaceInfo = serde_json::from_str(ipv6_only_wire)
            .expect("interface without IPv4 fields deserializes");
        assert_eq!(ipv6_only_interface.address, None);
        assert_eq!(ipv6_only_interface.gateway, None);
        assert_eq!(ipv6_only_interface.prefix, None);
    }

    /// Verifies family-neutral IPv6 data is validated before becoming the
    /// host.yaml sidecar consumed by DHCP.
    #[test]
    fn converts_family_neutral_ipv6_config() {
        scenarios!(summarize_flat_interface_ipv6:
            "valid IPv6 interface configuration" {
                // A stateful address and its validated prefix are both retained.
                interface_config_with_ipv6(
                    "2001:db8::20",
                    "2001:db8::/64",
                ) => Yields(Some(InterfaceInfoV6 {
                    address: Some("2001:db8::20".parse().unwrap()),
                    prefix: "2001:db8::/64".to_string(),
                })),
                // An explicit prefix without an address remains SLAAC-only.
                interface_config_with_ipv6(
                    "",
                    "2001:db8:1::/64",
                ) => Yields(Some(InterfaceInfoV6 {
                    address: None,
                    prefix: "2001:db8:1::/64".to_string(),
                })),
                // An operator loopback alone does not enable DHCPv6 on the host interface.
                FlatInterfaceConfig {
                    addresses: vec![InterfaceAddressConfig {
                        address_family: AddressFamily::V6.into(),
                        tenant_vrf_loopback_ip: Some("2001:db8::3".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                } => Yields(None),
                // An SVI address alone belongs to routing and cannot configure DHCPv6.
                FlatInterfaceConfig {
                    addresses: vec![InterfaceAddressConfig {
                        address_family: AddressFamily::V6.into(),
                        svi_ip: Some("2001:db8::4".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                } => Yields(None),
            }

            "invalid IPv6 interface prefix" {
                // Malformed prefixes fail conversion instead of reaching packet handling.
                interface_config_with_ipv6(
                    "2001:db8::20",
                    "not-an-ipv6-prefix",
                ) => FailsWith("ip-network"),
            }
        );
    }

    #[test]
    fn reports_timestamp_file_paths() {
        value_scenarios!(
            run = |path| path.path_str().to_string();
            "known paths" {
                DhcpTimestampsFilePath::HbnTmp => "/var/support/forge-dhcp/logs/dhcp_timestamps.json.tmp".to_string(),
                DhcpTimestampsFilePath::Hbn => "/var/support/forge-dhcp/logs/dhcp_timestamps.json".to_string(),
                DhcpTimestampsFilePath::Dpu => "/var/lib/hbn/var/support/forge-dhcp/logs/dhcp_timestamps.json".to_string(),
                DhcpTimestampsFilePath::Test => "/tmp/timestamps.json".to_string(),
                DhcpTimestampsFilePath::NotSet => "Not set".to_string(),
            }
        );
    }

    #[test]
    fn stores_timestamps_by_host_interface_id() {
        let id = host_interface_id();
        let mut timestamps = DhcpTimestamps::default();

        timestamps.add_timestamp(id, "2026-06-13T00:00:00Z".to_string());

        assert_eq!(
            timestamps.get_timestamp(&id),
            Some(&"2026-06-13T00:00:00Z".to_string())
        );
        assert_eq!(timestamps.into_iter().count(), 1);
    }
}
