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
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use carbide_uuid::UuidConversionError;
use carbide_uuid::machine::MachineInterfaceId;
use ipnetwork::{Ipv4Network, Ipv6Network};
use mac_address::MacAddress;
use rpc::InterfaceFunctionType;
use rpc::errors::RpcDataConversionError;
use rpc::forge::ManagedHostNetworkConfigResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    /// DHCPv4 boot address. Omit together with `carbide_dhcp_server` to
    /// disable DHCPv4; an incomplete pair is invalid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carbide_provisioning_server_ipv4: Option<Ipv4Addr>,
    /// IPv6 provisioning address used to generate default DHCPv6 HTTP boot URLs.
    ///
    /// Omission disables URL generation. An explicit interface `booturl`,
    /// including an empty one, takes precedence. DHCPv4 uses its own address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carbide_provisioning_server_ipv6: Option<Ipv6Addr>,
    /// DHCPv4 server address, absent when DHCPv4 is disabled. Supplies the
    /// legacy DHCPv6 identity when `dhcpv6_server_id` is omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carbide_dhcp_server: Option<Ipv4Addr>,
    /// Configured DHCPv6 identity as a YAML/JSON array of integer bytes.
    /// Omission or `null` permits derivation from `carbide_dhcp_server`;
    /// an empty or malformed array is rejected during deserialization.
    /// The server resolves any saved identity separately before serving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcpv6_server_id: Option<DhcpV6ServerId>,
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
    /// Preference emitted only in DHCPv6 ADVERTISE messages.
    ///
    /// `None` preserves legacy configuration behavior (effective preference
    /// zero); `Some(0)` is an explicit configured value and must not collapse
    /// into omission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcpv6_server_preference: Option<u8>,
}

#[derive(thiserror::Error, Debug)]
pub enum DhcpDataError {
    /// A server identifier is not a bounded NVIDIA DUID-EN.
    #[error("invalid DHCPv6 server identifier")]
    InvalidServerIdentifier,
    /// The DPU remote ID cannot form a bounded DHCPv6 server identifier;
    /// the payload is its UTF-8 length in bytes.
    #[error("invalid DPU remote_id length {0}: expected 1 to 120 UTF-8 bytes")]
    InvalidRemoteIdLength(usize),
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
    /// A service reservation contains a MAC address that cannot be parsed.
    #[error("DhcpDataError: MacParseError: {0}")]
    MacParseError(#[from] mac_address::MacParseError),
    /// A service reservation replacement violates an identity or link invariant.
    #[error("invalid service reservation configuration: {0}")]
    InvalidServiceConfiguration(String),
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

            carbide_provisioning_server_ipv4: None,
            carbide_dhcp_server: None,
            dhcpv6_server_id: None,
            carbide_provisioning_server_ipv6: None,
            carbide_nameservers_v6: vec![],
            carbide_ntpservers_v6: vec![],
            carbide_dhcp_server_v6: None,
            dhcpv6_preferred_lifetime_secs: 0,
            dhcpv6_valid_lifetime_secs: 0,
            dhcpv6_server_preference: None,
        }
    }
}

impl DhcpConfig {
    /// `ipv4` returns the complete DHCPv4 configuration, or `None` when
    /// both addresses are omitted. A partial pair is an error.
    pub fn ipv4(&self) -> Result<Option<DhcpV4Config>, DhcpDataError> {
        match (
            self.carbide_dhcp_server,
            self.carbide_provisioning_server_ipv4,
        ) {
            (None, None) => Ok(None),
            (Some(server), Some(provisioning_server)) => Ok(Some(DhcpV4Config {
                server,
                provisioning_server,
            })),
            _ => Err(DhcpDataError::ParameterMissing(
                "complete IPv4 DHCP configuration",
            )),
        }
    }

    /// `validate` checks the IPv4 address pair and availability of a DHCPv6
    /// identity. It does not check host settings, lifetimes, or boot sources.
    pub fn validate(&self) -> Result<(), DhcpDataError> {
        self.ipv4()?;
        self.server_identifier()?;
        Ok(())
    }

    /// `server_identifier` uses the configured identity when supplied, otherwise
    /// reproduces the legacy identifier from the IPv4 server address.
    pub fn server_identifier(&self) -> Result<DhcpV6ServerId, DhcpDataError> {
        self.dhcpv6_server_id
            .clone()
            .or_else(|| self.carbide_dhcp_server.map(DhcpV6ServerId::from_ipv4))
            .ok_or(DhcpDataError::ParameterMissing("DHCPv6 server identifier"))
    }

    /// `from_forge_dhcp_config` builds settings from the agent's service
    /// addresses. IPv4 addresses must both be present or both absent; callers
    /// must also supply a DHCPv6 identity before serving without IPv4.
    pub fn from_forge_dhcp_config(
        carbide_provisioning_server_ipv4: Option<Ipv4Addr>,
        carbide_ntpservers: Vec<Ipv4Addr>,
        carbide_nameservers: Vec<Ipv4Addr>,
        carbide_nameservers_v6: Vec<Ipv6Addr>,
        loopback_ip: Option<Ipv4Addr>,
    ) -> Result<Self, DhcpDataError> {
        let config = DhcpConfig {
            carbide_nameservers,
            carbide_nameservers_v6,
            carbide_ntpservers,
            carbide_provisioning_server_ipv4,
            carbide_dhcp_server: loopback_ip,
            ..Default::default()
        };
        config.ipv4()?;
        Ok(config)
    }
}

/// `DhcpV4Config` contains the two addresses required to serve DHCPv4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpV4Config {
    /// Server identifier emitted in DHCPv4 responses.
    pub server: Ipv4Addr,
    /// Address used for DHCPv4 boot URLs and the next-server field.
    pub provisioning_server: Ipv4Addr,
}

/// Stores immutable service reservations by receiving interface and client MAC so DHCP lookup
/// cannot cross service slots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceConfig {
    interfaces: BTreeMap<String, BTreeMap<MacAddress, ServiceReservation>>,
}

/// Groups service reservations that are reachable only through one provisioned DHCP receiving
/// interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInterfaceReservations {
    /// Listener interface whose ingress identity forms half of every reservation key.
    pub receiving_interface: String,
    /// Exact client bindings reserved on this interface; an empty list explicitly blocks host
    /// fallback.
    pub reservations: Vec<ServiceReservation>,
}

/// Identifies one service attachment and its exact client MAC binding so tenant host traffic cannot
/// claim the address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceReservation {
    /// Stable API identity of the service attachment that owns this binding.
    pub attachment_id: Uuid,
    /// Stable interface position within the attachment.
    pub interface_ordinal: u32,
    /// Internal identity used to correlate this binding across replacement snapshots.
    pub internal_uuid: Uuid,
    /// DHCP chaddr required to select this binding.
    pub mac_address: MacAddress,
    /// Fixed point-to-point address-family configuration; DHCPv4 emits only the IPv4 variant.
    pub link: ServiceLink,
}

/// Selects the address-family contract for a service reservation while keeping one exact lookup key
/// for both families.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceLink {
    /// A DHCPv4-managed `/31` link using HBN as its default gateway.
    Ipv4(ServiceLinkV4),
    /// A validated `/127` link retained independently from the DHCPv4-only service response path.
    Ipv6(ServiceLinkV6),
}

/// Describes the fixed IPv4 point-to-point link emitted in DHCPv4 responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLinkV4 {
    /// Service-side address (the second address in `prefix`).
    pub address: Ipv4Addr,
    /// Canonical `/31` containing the HBN and service endpoints.
    pub prefix: Ipv4Network,
    /// HBN-side address (the base address in `prefix`).
    pub gateway: Ipv4Addr,
}

/// Describes the fixed IPv6 point-to-point link retained in replacement state; DHCPv4 rejects this
/// family explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLinkV6 {
    /// Service-side address (the second address in `prefix`).
    pub address: Ipv6Addr,
    /// Canonical `/127` containing the HBN and service endpoints.
    pub prefix: Ipv6Network,
    /// HBN-side address (the base address in `prefix`).
    pub hbn_peer: Ipv6Addr,
}

impl ServiceConfig {
    /// Builds an exact-match reservation index and rejects ambiguous or unsafe service link
    /// definitions before activation.
    pub fn try_new(
        interface_groups: Vec<ServiceInterfaceReservations>,
    ) -> Result<Self, DhcpDataError> {
        let mut interfaces = BTreeMap::new();
        let mut attachment_interfaces = BTreeSet::new();
        let mut internal_uuids = BTreeSet::new();

        // Validate each group before insertion so a later duplicate cannot silently replace an earlier reservation.
        for group in interface_groups {
            if group.receiving_interface.is_empty() {
                return Err(invalid_service_config(
                    "receiving interface must not be empty",
                ));
            }
            if interfaces.contains_key(&group.receiving_interface) {
                return Err(invalid_service_config(format!(
                    "duplicate receiving interface {}",
                    group.receiving_interface
                )));
            }

            let mut reservations = BTreeMap::new();
            for reservation in group.reservations {
                validate_service_reservation(&reservation)?;
                if !attachment_interfaces
                    .insert((reservation.attachment_id, reservation.interface_ordinal))
                {
                    return Err(invalid_service_config(format!(
                        "duplicate attachment interface {} ordinal {}",
                        reservation.attachment_id, reservation.interface_ordinal
                    )));
                }
                if !internal_uuids.insert(reservation.internal_uuid) {
                    return Err(invalid_service_config(format!(
                        "duplicate service internal_uuid {}",
                        reservation.internal_uuid
                    )));
                }
                let mac_address = reservation.mac_address;
                if reservations.insert(mac_address, reservation).is_some() {
                    return Err(invalid_service_config(format!(
                        "duplicate MAC {mac_address} on receiving interface {}",
                        group.receiving_interface
                    )));
                }
            }
            interfaces.insert(group.receiving_interface, reservations);
        }

        Ok(Self { interfaces })
    }

    /// Returns whether an interface owns service slots so unknown MACs can be rejected without
    /// falling back to host DHCP.
    pub fn has_interface(&self, receiving_interface: &str) -> bool {
        self.interfaces.contains_key(receiving_interface)
    }

    /// Finds the reservation for an exact receiving-interface and client-MAC pair to preserve
    /// service-slot isolation.
    pub fn get(
        &self,
        receiving_interface: &str,
        mac_address: &MacAddress,
    ) -> Option<&ServiceReservation> {
        self.interfaces
            .get(receiving_interface)
            .and_then(|reservations| reservations.get(mac_address))
    }

    /// Rejects reservations for interfaces outside the provisioned listener inventory so typos
    /// cannot create unreachable service slots.
    pub fn validate_receiving_interfaces(
        &self,
        provisioned_interfaces: &[String],
    ) -> Result<(), DhcpDataError> {
        let provisioned_interfaces: BTreeSet<_> =
            provisioned_interfaces.iter().map(String::as_str).collect();
        if let Some(interface) = self
            .interfaces
            .keys()
            .find(|interface| !provisioned_interfaces.contains(interface.as_str()))
        {
            return Err(invalid_service_config(format!(
                "receiving interface {interface} is not provisioned"
            )));
        }
        Ok(())
    }
}

/// Validates one service reservation so packet handlers can construct replies without repeating
/// domain checks.
fn validate_service_reservation(reservation: &ServiceReservation) -> Result<(), DhcpDataError> {
    // Reject identities that cannot name one stable unicast service endpoint.
    if reservation.attachment_id.is_nil() || reservation.internal_uuid.is_nil() {
        return Err(invalid_service_config(
            "attachment_id and internal_uuid must not be nil",
        ));
    }
    let mac = reservation.mac_address.bytes();
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err(invalid_service_config(format!(
            "MAC {} must be a nonzero unicast address",
            reservation.mac_address
        )));
    }

    // Validate the fixed HBN and service positions once so packet serving needs no repair or fallback.
    match &reservation.link {
        ServiceLink::Ipv4(link) => {
            if link.prefix.prefix() != 31
                || link.prefix.ip() != link.prefix.network()
                || link.gateway != link.prefix.network()
                || link.address != link.prefix.broadcast()
            {
                return Err(invalid_service_config(
                    "IPv4 service link must use the /31 base address as gateway and base+1 as the service address",
                ));
            }
        }
        ServiceLink::Ipv6(link) => {
            if link.prefix.prefix() != 127 || link.prefix.ip() != link.prefix.network() {
                return Err(invalid_service_config(
                    "IPv6 service link must use the /127 base address as HBN peer and base+1 as the service address",
                ));
            }
            let mut service_octets = link.prefix.network().octets();
            service_octets[15] += 1;
            if link.hbn_peer != link.prefix.network()
                || link.address != Ipv6Addr::from(service_octets)
            {
                return Err(invalid_service_config(
                    "IPv6 service link must use the /127 base address as HBN peer and base+1 as the service address",
                ));
            }
        }
    }
    Ok(())
}

/// Creates a consistent validation error so callers can map all service contract failures to
/// INVALID_ARGUMENT.
fn invalid_service_config(message: impl Into<String>) -> DhcpDataError {
    DhcpDataError::InvalidServiceConfiguration(message.into())
}

/// `DhcpV6ServerId` is an NVIDIA DUID-EN containing 7 through 130 bytes.
/// The header is type 2 and enterprise 5703 (`00 02 00 00 16 47`), followed
/// by a nonempty identifier. YAML and JSON encode it as an array of integer bytes;
/// deserialization and `TryFrom<Vec<u8>>` reject other headers or lengths with
/// `DhcpDataError::InvalidServerIdentifier`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct DhcpV6ServerId(Vec<u8>);

impl DhcpV6ServerId {
    const DUID_EN: u16 = 2;
    const NVIDIA_ENTERPRISE_NUMBER: u32 = 5703;
    const HEADER: [u8; 6] = {
        let kind = Self::DUID_EN.to_be_bytes();
        let enterprise = Self::NVIDIA_ENTERPRISE_NUMBER.to_be_bytes();
        [
            kind[0],
            kind[1],
            enterprise[0],
            enterprise[1],
            enterprise[2],
            enterprise[3],
        ]
    };

    /// `from_ipv4` preserves the exact identifier emitted by older servers.
    fn from_ipv4(address: Ipv4Addr) -> Self {
        Self([Self::HEADER.as_slice(), &address.octets()].concat())
    }

    /// `from_remote_id` derives a stable identifier from the DPU's remote ID.
    /// Empty IDs and IDs over 120 UTF-8 bytes return
    /// `DhcpDataError::InvalidRemoteIdLength`.
    /// The `dpu:` prefix plus a nonempty ID cannot collide with the four-byte
    /// legacy IPv4 identifiers. The remote ID is visible in DHCPv6 packets.
    pub fn from_remote_id(remote_id: &str) -> Result<Self, DhcpDataError> {
        if remote_id.is_empty() || remote_id.len() > 120 {
            return Err(DhcpDataError::InvalidRemoteIdLength(remote_id.len()));
        }
        Self::try_from([Self::HEADER.as_slice(), b"dpu:", remote_id.as_bytes()].concat())
    }

    /// `as_bytes` returns the complete DUID for the DHCPv6 ServerId option.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl TryFrom<Vec<u8>> for DhcpV6ServerId {
    type Error = DhcpDataError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        if !(7..=130).contains(&bytes.len()) || !bytes.starts_with(&Self::HEADER) {
            return Err(DhcpDataError::InvalidServerIdentifier);
        }
        Ok(Self(bytes))
    }
}

impl From<DhcpV6ServerId> for Vec<u8> {
    fn from(identifier: DhcpV6ServerId) -> Self {
        identifier.0
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

    /// Returns the durable timestamp path so initialization checks the HBN file produced by the
    /// temporary-file rename.
    fn persisted_path_str(&self) -> &str {
        match self {
            Self::HbnTmp => DHCP_TIMESTAMP_FILE_HBN,
            path => path.path_str(),
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

    /// Loads or creates durable timestamp history for configured paths so generation restarts
    /// preserve host activity; `NotSet` performs no file I/O.
    pub fn initialize_file(&mut self) -> eyre::Result<()> {
        if let DhcpTimestampsFilePath::NotSet = self.path {
            return Ok(());
        }
        match fs::metadata(self.path.persisted_path_str()) {
            Ok(_) => self.read(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.write(),
            Err(error) => Err(error.into()),
        }
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
            .open(self.path.persisted_path_str())?;
        let persisted: Self = serde_json::from_reader(timestamp_file)?;
        self.timestamps = persisted.timestamps;
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

    /// Builds one valid service reservation so validation cases can change only the invariant they
    /// exercise.
    fn service_reservation(link: ServiceLink) -> ServiceReservation {
        ServiceReservation {
            attachment_id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
            interface_ordinal: 0,
            internal_uuid: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
            mac_address: "02:00:00:00:00:01".parse().unwrap(),
            link,
        }
    }

    /// Builds the valid IPv4 point-to-point fixture used to isolate service identity and topology
    /// validation failures.
    fn service_link_v4() -> ServiceLinkV4 {
        ServiceLinkV4 {
            address: "192.0.2.11".parse().unwrap(),
            prefix: "192.0.2.10/31".parse().unwrap(),
            gateway: "192.0.2.10".parse().unwrap(),
        }
    }

    #[derive(Debug, PartialEq)]
    struct DhcpConfigSummary {
        provisioning_server: Option<Ipv4Addr>,
        dhcp_server: Option<Ipv4Addr>,
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
            service_interfaces: vec![],
            service_vpc_slot_inventory: None,
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
            Some(provisioning_server),
            ntpservers,
            nameservers,
            nameservers_v6,
            Some(dhcp_server),
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
            DhcpDataError::InvalidServerIdentifier => "server-identifier",
            DhcpDataError::InvalidRemoteIdLength(_) => "remote-id-length",
            DhcpDataError::AddressParseError(_) => "address-parse",
            DhcpDataError::ParameterMissing(_) => "parameter-missing",
            DhcpDataError::IpNetworkError(_) => "ip-network",
            DhcpDataError::RpcConversion(_) => "rpc-conversion",
            DhcpDataError::UuidConversion(_) => "uuid-conversion",
            DhcpDataError::UuidParseError(_) => "uuid-parse",
            DhcpDataError::MacParseError(_) => "mac-parse",
            DhcpDataError::InvalidServiceConfiguration(_) => "service-configuration",
        }
    }

    /// Verifies service reservations reject ambiguous identity and topology while accepting the
    /// exact dual-family contract so packet paths can rely on canonical, globally unique lookup
    /// state.
    #[test]
    fn validates_service_reservation_contract() {
        struct Case {
            name: &'static str,
            groups: Vec<ServiceInterfaceReservations>,
            expected: Result<(), &'static str>,
        }

        // Build the shared valid fixtures before changing one field for each rejected invariant.
        let valid_v4 = service_reservation(ServiceLink::Ipv4(service_link_v4()));
        let valid_v6 = service_reservation(ServiceLink::Ipv6(ServiceLinkV6 {
            address: "2001:db8::11".parse().unwrap(),
            prefix: "2001:db8::10/127".parse().unwrap(),
            hbn_peer: "2001:db8::10".parse().unwrap(),
        }));
        // Remove only the attachment identity so the ownership requirement selects this rejection.
        let mut nil_identity = valid_v4.clone();
        nil_identity.attachment_id = Uuid::nil();
        // Remove only the internal identity so replacement correlation selects this rejection.
        let mut nil_internal_uuid = valid_v4.clone();
        nil_internal_uuid.internal_uuid = Uuid::nil();
        // Replace only the client key with the unspecified MAC while leaving ownership and topology valid.
        let mut zero_mac = valid_v4.clone();
        zero_mac.mac_address = "00:00:00:00:00:00".parse().unwrap();
        // Replace only the client key with a multicast MAC to isolate the unicast requirement.
        let mut multicast_mac = valid_v4.clone();
        multicast_mac.mac_address = "01:00:5e:00:00:01".parse().unwrap();
        // Widen only the IPv4 prefix and realign its endpoints so no later topology rule can cause rejection.
        let mut wrong_v4_prefix = service_link_v4();
        wrong_v4_prefix.prefix = "192.0.2.8/30".parse().unwrap();
        wrong_v4_prefix.address = "192.0.2.11".parse().unwrap();
        wrong_v4_prefix.gateway = "192.0.2.8".parse().unwrap();
        // Preserve /31 width while setting host bits so only canonical-prefix validation rejects it.
        let mut noncanonical_v4_prefix = service_link_v4();
        noncanonical_v4_prefix.prefix = "192.0.2.11/31".parse().unwrap();
        // Move only the service endpoint to the HBN address so the base + 1 requirement rejects it.
        let mut wrong_service_address = service_link_v4();
        wrong_service_address.address = "192.0.2.10".parse().unwrap();
        // Move only the gateway to the service endpoint so the fixed base position rejects it.
        let mut wrong_v4_gateway = service_link_v4();
        wrong_v4_gateway.gateway = wrong_v4_gateway.address;
        // Use a /128 ending in 0xff so prefix-length rejection precedes the former base-plus-one overflow.
        let mut wrong_v6_prefix = valid_v6.clone();
        wrong_v6_prefix.link = ServiceLink::Ipv6(ServiceLinkV6 {
            address: "2001:db8::ff".parse().unwrap(),
            prefix: "2001:db8::ff/128".parse().unwrap(),
            hbn_peer: "2001:db8::ff".parse().unwrap(),
        });
        // Preserve /127 width while setting host bits so only canonical-prefix validation rejects it.
        let mut noncanonical_v6_prefix = valid_v6.clone();
        noncanonical_v6_prefix.link = ServiceLink::Ipv6(ServiceLinkV6 {
            address: "2001:db8::11".parse().unwrap(),
            prefix: "2001:db8::11/127".parse().unwrap(),
            hbn_peer: "2001:db8::10".parse().unwrap(),
        });
        // Move only the HBN peer to the service endpoint so the fixed base position rejects it.
        let mut wrong_v6_peer = valid_v6.clone();
        let ServiceLink::Ipv6(link) = &mut wrong_v6_peer.link else {
            unreachable!();
        };
        link.hbn_peer = "2001:db8::11".parse().unwrap();
        // Move only the service endpoint to the HBN peer so the base + 1 requirement rejects it.
        let mut wrong_v6_address = valid_v6.clone();
        let ServiceLink::Ipv6(link) = &mut wrong_v6_address.link else {
            unreachable!();
        };
        link.address = link.hbn_peer;
        // Preserve the client MAC while changing both stable identities to isolate exact-key uniqueness.
        let mut duplicate_mac = valid_v4.clone();
        duplicate_mac.attachment_id = "33333333-3333-3333-3333-333333333333".parse().unwrap();
        duplicate_mac.internal_uuid = "44444444-4444-4444-4444-444444444444".parse().unwrap();
        // Preserve the internal identity while changing attachment ownership and MAC to isolate identity uniqueness.
        let mut duplicate_identity = valid_v4.clone();
        duplicate_identity.attachment_id = "33333333-3333-3333-3333-333333333333".parse().unwrap();
        duplicate_identity.mac_address = "02:00:00:00:00:02".parse().unwrap();
        // Preserve the attachment ordinal while changing internal identity and MAC to isolate interface uniqueness.
        let mut duplicate_attachment_interface = valid_v4.clone();
        duplicate_attachment_interface.internal_uuid =
            "44444444-4444-4444-4444-444444444444".parse().unwrap();
        duplicate_attachment_interface.mac_address = "02:00:00:00:00:03".parse().unwrap();

        let cases = vec![
            // Accepts canonical /31 data because DHCPv4 can emit the fixed link without repair.
            Case {
                name: "valid IPv4 reservation",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![valid_v4.clone()],
                }],
                expected: Ok(()),
            },
            // Accepts canonical /127 data independently of DHCPv4 so validation retains both address-family contracts.
            Case {
                name: "valid IPv6 reservation",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![valid_v6],
                }],
                expected: Ok(()),
            },
            // Rejects an empty ingress key because packet lookup cannot distinguish or bind an unnamed listener.
            Case {
                name: "empty receiving interface",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: String::new(),
                    reservations: vec![valid_v4.clone()],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects nil attachment identity because downstream lifecycle operations require stable ownership.
            Case {
                name: "nil attachment identity",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![nil_identity],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects nil internal identity because replacement snapshots could not correlate the service binding.
            Case {
                name: "nil internal identity",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![nil_internal_uuid],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects the all-zero MAC because it cannot name a unicast service endpoint.
            Case {
                name: "zero client MAC",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![zero_mac],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects multicast MACs because the exact client key must identify one service endpoint.
            Case {
                name: "multicast client MAC",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![multicast_mac],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects prefixes wider than /31 because they do not encode the fixed two-address service link.
            Case {
                name: "invalid IPv4 prefix length",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![service_reservation(ServiceLink::Ipv4(wrong_v4_prefix))],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects host bits in a /31 prefix because base and service positions must be unambiguous.
            Case {
                name: "noncanonical IPv4 prefix",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![service_reservation(ServiceLink::Ipv4(
                        noncanonical_v4_prefix,
                    ))],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects a service address on the HBN side of the /31 to preserve the fixed base-plus-one assignment.
            Case {
                name: "wrong IPv4 service address",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![service_reservation(ServiceLink::Ipv4(
                        wrong_service_address,
                    ))],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects a gateway on the service side because HBN must own the base address.
            Case {
                name: "wrong IPv4 link gateway",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![service_reservation(ServiceLink::Ipv4(wrong_v4_gateway))],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects /128 before computing base+1, preventing malformed caller input from overflowing validation.
            Case {
                name: "invalid IPv6 prefix length",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![wrong_v6_prefix],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects host bits in a /127 prefix because HBN must own the canonical base address.
            Case {
                name: "noncanonical IPv6 prefix",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![noncanonical_v6_prefix],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects an HBN peer on the service side of the /127 because endpoint roles are fixed.
            Case {
                name: "wrong IPv6 HBN peer",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![wrong_v6_peer],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects a service endpoint on the HBN side while retaining the canonical /127 and correct peer.
            Case {
                name: "wrong IPv6 service address",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![wrong_v6_address],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects duplicate exact keys instead of allowing input order to decide which attachment receives an address.
            Case {
                name: "duplicate service MAC",
                groups: vec![ServiceInterfaceReservations {
                    receiving_interface: "pf0sf1".to_string(),
                    reservations: vec![valid_v4.clone(), duplicate_mac],
                }],
                expected: Err("service-configuration"),
            },
            // Rejects one endpoint identity under multiple MACs because replacement and diagnostics must correlate one reservation unambiguously.
            Case {
                name: "duplicate service identity",
                groups: vec![
                    ServiceInterfaceReservations {
                        receiving_interface: "pf0sf1".to_string(),
                        reservations: vec![valid_v4.clone()],
                    },
                    ServiceInterfaceReservations {
                        receiving_interface: "pf0sf2".to_string(),
                        reservations: vec![duplicate_identity],
                    },
                ],
                expected: Err("service-configuration"),
            },
            // Rejects one attachment ordinal under multiple endpoint identities because attachment replacement must address one interface deterministically.
            Case {
                name: "duplicate attachment interface",
                groups: vec![
                    ServiceInterfaceReservations {
                        receiving_interface: "pf0sf1".to_string(),
                        reservations: vec![valid_v4],
                    },
                    ServiceInterfaceReservations {
                        receiving_interface: "pf0sf2".to_string(),
                        reservations: vec![duplicate_attachment_interface],
                    },
                ],
                expected: Err("service-configuration"),
            },
            // Rejects repeated receiving-interface groups because splitting one slot across groups would make replacement order significant.
            Case {
                name: "duplicate receiving interface",
                groups: vec![
                    ServiceInterfaceReservations {
                        receiving_interface: "pf0sf1".to_string(),
                        reservations: vec![],
                    },
                    ServiceInterfaceReservations {
                        receiving_interface: "pf0sf1".to_string(),
                        reservations: vec![],
                    },
                ],
                expected: Err("service-configuration"),
            },
        ];

        // Construct each index and compare only the admission result because lookup behavior has a separate focused proof.
        for case in cases {
            let actual = ServiceConfig::try_new(case.groups)
                .map(drop)
                .map_err(dhcp_error_kind);
            assert_eq!(actual, case.expected, "{}", case.name);
        }
    }

    /// Verifies lookup includes the receiving interface and listener validation rejects
    /// reservations for unprovisioned interfaces.
    #[test]
    fn service_reservations_are_scoped_to_provisioned_interfaces() {
        // Build one valid reservation whose MAC would collide if the receiving interface were ignored.
        let reservation = service_reservation(ServiceLink::Ipv4(service_link_v4()));
        let client_mac = reservation.mac_address;

        // Place the same MAC in one service interface to prove another interface cannot retrieve it.
        let config = ServiceConfig::try_new(vec![ServiceInterfaceReservations {
            receiving_interface: "pf0sf1".to_string(),
            reservations: vec![reservation],
        }])
        .unwrap();

        // Exact interface lookup succeeds, cross-interface lookup fails, and listener inventory is authoritative.
        assert!(config.get("pf0sf1", &client_mac).is_some());
        assert!(config.get("pf0sf2", &client_mac).is_none());
        assert!(
            config
                .validate_receiving_interfaces(&["pf0sf1".to_string()])
                .is_ok()
        );
        assert!(
            config
                .validate_receiving_interfaces(&["pf0sf2".to_string()])
                .is_err()
        );
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
                    provisioning_server: Some(Ipv4Addr::new(192, 0, 2, 10)),
                    dhcp_server: Some(Ipv4Addr::new(127, 0, 0, 2)),
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
            dhcpv6_server_id: Some(DhcpV6ServerId::from_remote_id("test-dpu").unwrap()),
            carbide_provisioning_server_ipv6: Some("2001:db8::80".parse().unwrap()),
            carbide_nameservers_v6: vec!["2001:db8::53".parse().unwrap()],
            carbide_ntpservers_v6: vec!["2001:db8::123".parse().unwrap()],
            carbide_dhcp_server_v6: Some("2001:db8::1".parse().unwrap()),
            dhcpv6_preferred_lifetime_secs: 3600,
            dhcpv6_valid_lifetime_secs: 7200,
            dhcpv6_server_preference: Some(0),
            ..Default::default()
        };

        // Serialize a populated config and verify the IPv6 fields survive.
        let wire = serde_json::to_string(&config).expect("dhcp config serializes");
        let recovered: DhcpConfig = serde_json::from_str(&wire).expect("dhcp config deserializes");
        recovered.validate().unwrap();
        assert_eq!(recovered.ipv4().unwrap(), None);
        assert_eq!(recovered.dhcpv6_server_id, config.dhcpv6_server_id);
        assert!(!wire.contains("carbide_dhcp_server\""));
        assert!(!wire.contains("carbide_provisioning_server_ipv4"));
        assert_eq!(
            recovered.carbide_provisioning_server_ipv6,
            Some(Ipv6Addr::from_str("2001:db8::80").unwrap())
        );
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
        assert_eq!(recovered.dhcpv6_server_preference, Some(0));

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
        old_config.validate().unwrap();
        assert_eq!(old_config.dhcpv6_server_id, None);
        assert_eq!(
            old_config.server_identifier().unwrap().as_bytes(),
            &[0, 2, 0, 0, 0x16, 0x47, 127, 0, 0, 1],
        );
        assert!(old_config.carbide_nameservers_v6.is_empty());
        assert!(old_config.carbide_ntpservers_v6.is_empty());
        assert_eq!(old_config.carbide_provisioning_server_ipv6, None);
        assert!(
            !serde_json::to_string(&old_config)
                .expect("legacy dhcp config serializes")
                .contains("carbide_provisioning_server_ipv6")
        );
        assert_eq!(old_config.carbide_dhcp_server_v6, None);
        assert_eq!(old_config.dhcpv6_preferred_lifetime_secs, 0);
        assert_eq!(old_config.dhcpv6_valid_lifetime_secs, 0);
        assert_eq!(old_config.dhcpv6_server_preference, None);
    }

    #[test]
    fn requires_complete_ipv4_settings_or_an_explicit_identity() {
        value_scenarios!(run = |(server, boot, identity): (Option<Ipv4Addr>, Option<Ipv4Addr>, bool)| {
                DhcpConfig {
                    carbide_dhcp_server: server,
                    carbide_provisioning_server_ipv4: boot,
                    dhcpv6_server_id: identity.then(|| DhcpV6ServerId::from_ipv4(Ipv4Addr::LOCALHOST)),
                    ..Default::default()
                }.validate().is_ok()
            };
            "incomplete IPv4 is invalid even with a DHCPv6 identity" {
                (Some(Ipv4Addr::LOCALHOST), None, true) => false,
                (None, Some(Ipv4Addr::LOCALHOST), true) => false,
            }
            "IPv6-only needs an identity" {
                (None, None, false) => false,
                (None, None, true) => true,
            }
        );
    }

    #[test]
    fn validates_persisted_server_identity_and_remote_id_boundaries() {
        let id = DhcpV6ServerId::from_remote_id("test-dpu").unwrap();
        assert_eq!(id.as_bytes(), b"\x00\x02\x00\x00\x16\x47dpu:test-dpu");
        assert_ne!(id, DhcpV6ServerId::from_remote_id("another-dpu").unwrap());
        assert_ne!(id, DhcpV6ServerId::from_ipv4(Ipv4Addr::LOCALHOST));
        let maximum = DhcpV6ServerId::from_remote_id(&"x".repeat(120)).unwrap();
        assert_eq!(maximum.as_bytes().len(), 130);
        assert!(matches!(
            DhcpV6ServerId::from_remote_id(&"x".repeat(121)),
            Err(DhcpDataError::InvalidRemoteIdLength(121))
        ));
        assert!(matches!(
            DhcpV6ServerId::from_remote_id(""),
            Err(DhcpDataError::InvalidRemoteIdLength(0))
        ));

        value_scenarios!(run = |bytes: Vec<u8>| {
                serde_json::from_value::<DhcpV6ServerId>(serde_json::json!(bytes)).is_ok()
            };
            "persisted identity" {
                id.as_bytes().to_vec() => true,
                DhcpV6ServerId::HEADER.to_vec() => false,
                vec![0; 10] => false,
                [maximum.as_bytes(), &[1]].concat() => false,
            }
        );
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

    /// Verifies HBN temporary-write mode maps reads to the durable pathname because rename-based
    /// persistence never leaves history at the temporary path.
    #[test]
    fn hbn_temporary_write_mode_selects_durable_read_path() {
        assert_eq!(
            DhcpTimestampsFilePath::HbnTmp.persisted_path_str(),
            DHCP_TIMESTAMP_FILE_HBN
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
