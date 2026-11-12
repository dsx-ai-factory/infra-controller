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
use std::ffi::CStr;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;
use std::{fmt, fs, io};

use ::rpc::InterfaceFunctionType;
use ::rpc::forge::{
    self as rpc, FlatInterfaceConfig, ManagedHostNetworkConfigResponse,
    NetworkSecurityGroupRuleAction, NetworkSecurityGroupRuleProtocol,
};
use carbide_network::ip::prefix::{IpNet, Ipv6Net, aggregate};
use carbide_network::virtualization::{VpcVirtualizationType, build_dual_stack_list};
use carbide_rpc_utils::dhcp::{
    DhcpConfig, DhcpV6ServerId, HostConfig, ServiceConfig, ServiceDhcpInterfaceReservations,
    ServiceDhcpReservation, ServiceLink, ServiceLinkV4, ServiceLinkV6,
};
use eyre::WrapErr;
use mac_address::MacAddress;
use nvue_client::client::{NvueClient, NvueClientError};
use nvue_client::config::{NvueConfig, NvueConfigWithHeader};
use serde::Deserialize;
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

use crate::nvue::NetworkSecurityGroupRule;
use crate::{HBNDeviceNames, acl_rules, dhcp, hbn, nvue, service_network};

/// Bounds NVUE startup-file reads and writes unless admin recovery forces replacement.
const MAX_EXPECTED_SIZE: u64 = 10 * 1024 * 1024; // 10 MiB

/// Retains forced admin replacement above the original 1 MiB file limit.
const ADMIN_FORCE_WRITE_THRESHOLD: u64 = 1024 * 1024; // 1 MiB

/// Bounds DHCP and ACL files independently of the NVUE startup-file allowance.
const MAX_DHCP_ACL_SIZE: u64 = 1024 * 1024; // 1 MiB

/// ACL to prevent access to nvued's API
const NVUED_BLOCK_RULE: &str = r"
[iptables]
# Block access to nvued API
-A INPUT -p tcp --dport 8765 -j DROP
";

#[derive(PartialEq, Debug, Clone)]
enum InterfaceState {
    Up,
    Down,
}

impl FromStr for InterfaceState {
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.contains("DOWN") {
            return Ok(InterfaceState::Down);
        }
        Ok(InterfaceState::Up)
    }

    type Err = eyre::Report;
}

impl InterfaceState {
    const HOST_INTERFACE_NAME: &str = "pf0hpf";
    fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("ip");
        cmd.arg("link")
            .arg("set")
            .arg(InterfaceState::HOST_INTERFACE_NAME);
        if InterfaceState::Up == *self {
            cmd.arg("up");
        } else {
            cmd.arg("down");
        }
        cmd
    }

    async fn update_state(needed_state: &Self) -> eyre::Result<()> {
        let current_state = get_interface_state(InterfaceState::HOST_INTERFACE_NAME).await?;

        if current_state != *needed_state {
            // Execute command only if interface state is changed.
            let mut cmd = needed_state.command();
            tracing::info!(
                current_interface_state = ?current_state,
                target_interface_state = ?needed_state,
                command = ?cmd,
                "Updating interface state"
            );
            let result = cmd.output().await?;
            if !result.status.success() {
                return Err(eyre::eyre!(
                    "failed to update interface state: {}",
                    result.status
                ));
            }

            // Let's check if interface state is updated or not.
            let new_state = get_interface_state(InterfaceState::HOST_INTERFACE_NAME).await?;
            if &new_state != needed_state {
                return Err(eyre::eyre!(
                    r#"state is not updated after command execution. will try in next iteration. 
                needed {needed_state:?}, after updating {new_state:?}, interface: {}"#,
                    InterfaceState::HOST_INTERFACE_NAME
                ));
            }
        }

        // Return new state.
        Ok(())
    }
}

struct DhcpServerPaths {
    server: FPath,
    config: FPath,
    host_config: FPath,
}

/// Stores dual-stack PXE/UEFI HTTP, NTP, and DNS service addresses.
///
/// These are remote dependent services advertised as DHCP options; none is a
/// local listener address.
// TODO(dhcpv6-server-address): Populate `carbide_dhcp_server_v6` only after a
// non-gating consumer and authoritative server-address source are defined.
pub(super) struct ServiceAddresses {
    pub(super) pxe_ips: Vec<IpAddr>,
    pub(super) ntpservers: Vec<IpAddr>,
    pub(super) nameservers: Vec<IpAddr>,
}

/// Split a dual-stack address list into its IPv4 and IPv6 members.
fn split_addresses_by_family(addresses: &[IpAddr]) -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
    addresses
        .iter()
        .copied()
        .fold((Vec::new(), Vec::new()), |(mut v4, mut v6), addr| {
            match addr {
                IpAddr::V4(v4_addr) => v4.push(v4_addr),
                IpAddr::V6(v6_addr) => v6.push(v6_addr),
            }
            (v4, v6)
        })
}

/// Converts the presence-bearing Core value without collapsing explicit zero
/// into the legacy omitted-field behavior.
fn dhcpv6_server_preference(
    network_config: &rpc::ManagedHostNetworkConfigResponse,
) -> eyre::Result<Option<u8>> {
    network_config
        .dhcpv6_server_preference
        .map(u8::try_from)
        .transpose()
        .wrap_err("DHCPv6 server preference must be between 0 and 255")
}

/// Resolve DHCP NTP options, letting each valid site-configured family replace its service-discovered fallback.
fn build_dhcp_ntp_servers(
    nc: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
) -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
    // Start with the NTP servers from the service addresses, which is read from carbide-ntp.forge.
    let (mut ntpservers_v4, mut ntpservers_v6) =
        split_addresses_by_family(&service_addrs.ntpservers);

    // Site-configured addresses replace service-discovered addresses only for the same family.
    if !nc.ntp_servers.is_empty() {
        let site_addrs: Vec<IpAddr> = nc
            .ntp_servers
            .iter()
            .filter_map(|server| match server.parse::<IpAddr>() {
                Ok(address) => Some(address),
                Err(error) => {
                    tracing::debug!(
                        ntp_server = %server,
                        error = %error,
                        "Invalid NTP server from ManagedHostNetworkConfigResponse, ignoring"
                    );
                    None
                }
            })
            .collect();
        let (site_v4, site_v6) = split_addresses_by_family(&site_addrs);

        if !site_v4.is_empty() {
            ntpservers_v4 = site_v4;
        }
        if !site_v6.is_empty() {
            ntpservers_v6 = site_v6;
        }
    }

    (ntpservers_v4, ntpservers_v6)
}

/// How we tell HBN to notice the new file we wrote
#[derive(Debug)]
struct PostAction {
    cmd: &'static str,
}

pub(super) enum NvueUpdateFlavor<'a> {
    StartupFile {
        hbn_root: &'a Path,
        skip_post: bool,
    },
    RestApi {
        nvue_context: &'a mut NvueClientContext,
    },
}

/// The NVUE client and other information associated with it.
pub(super) struct NvueClientContext {
    pub(super) nvue_client: NvueClient,
    last_applied_hash: Option<u64>,
}

impl NvueClientContext {
    pub(super) fn new(nvue_client: NvueClient) -> Self {
        let last_applied_hash = None;
        Self {
            nvue_client,
            last_applied_hash,
        }
    }

    // Wrap the inner nvue_client's `push_config()` and try to avoid re-applying
    // a configuration we're already using. Returns Ok(Some(revision_id)) when
    // a revision was applied, Ok(None) if the config was unchanged, and clears
    // the retained hash before passing through errors from the inner client.
    async fn update_config(
        &mut self,
        config: &NvueConfig,
    ) -> Result<Option<String>, NvueClientError> {
        let new_hash = config.u64_hash();

        if let Some(last_applied_hash) = self.last_applied_hash
            && new_hash == last_applied_hash
        {
            Ok(None)
        } else {
            let result = self.nvue_client.push_config(config).await;
            self.record_update_result(new_hash, result)
        }
    }

    /// Records a confirmed NVUE result or discards the retained hash because a failed push cannot
    /// prove which configuration remains applied.
    fn record_update_result(
        &mut self,
        new_hash: u64,
        result: Result<Option<String>, NvueClientError>,
    ) -> Result<Option<String>, NvueClientError> {
        match result {
            Ok(revision_id) => {
                self.last_applied_hash.replace(new_hash);
                Ok(revision_id)
            }
            Err(error) => {
                // Treat every failed push as ambiguous because an error can arrive after NVUE
                // accepted the apply request but before its resulting revision was observed.
                self.last_applied_hash.take();
                Err(error)
            }
        }
    }
}

/// Converts one concrete RPC route target while preserving legacy absent-VNI zero behavior.
fn route_target_from_rpc(
    target: &::rpc::common::RouteTarget,
) -> eyre::Result<nvue::RouteTargetConfig> {
    eyre::ensure!(
        !target.vni_self,
        "received unresolved route target with vni_self set"
    );
    let target = nvue::RouteTargetConfig {
        asn: target.asn,
        vni: target.vni.unwrap_or_default(),
    };
    target.validate()?;
    Ok(target)
}

/// Converts a concrete RPC routing profile while rejecting Core-owned symbolic values.
impl TryFrom<&rpc::RoutingProfile> for nvue::RoutingProfile {
    type Error = eyre::Report;

    fn try_from(profile: &rpc::RoutingProfile) -> Result<Self, Self::Error> {
        // Preserve the API-provided routing policy without applying template concerns here.
        let route_targets_on_exports_by_prefix = profile
            .route_targets_on_exports_by_prefix
            .iter()
            .map(|entry| {
                let match_type = rpc::ExportPrefixMatchType::try_from(entry.match_type)?;
                let entry = nvue::PrefixRouteTargetConfig {
                    prefix: entry.prefix.parse::<IpNet>()?.trunc(),
                    match_type: match_type.into(),
                    max_prefix_length: entry.max_prefix_length,
                    route_targets: entry
                        .route_targets
                        .iter()
                        .map(route_target_from_rpc)
                        .collect::<eyre::Result<_>>()?,
                };
                entry.validate()?;
                Ok(entry)
            })
            .collect::<eyre::Result<Vec<_>>>()?;
        nvue::PrefixRouteTargetConfig::validate_entries(&route_targets_on_exports_by_prefix)?;
        Ok(nvue::RoutingProfile {
            route_targets_on_exports_by_prefix,
            leak_default_route_from_underlay: profile.leak_default_route_from_underlay,
            leak_tenant_host_routes_to_underlay: profile.leak_tenant_host_routes_to_underlay,
            tenant_leak_communities_accepted: profile.tenant_leak_communities_accepted,
            route_target_imports: profile
                .route_target_imports
                .iter()
                .map(route_target_from_rpc)
                .collect::<eyre::Result<_>>()?,
            route_targets_on_exports: profile
                .route_targets_on_exports
                .iter()
                .map(route_target_from_rpc)
                .collect::<eyre::Result<_>>()?,
            accepted_leaks_from_underlay: profile
                .accepted_leaks_from_underlay
                .iter()
                .map(|l| l.prefix.to_owned())
                .collect(),
            allowed_anycast_prefixes: profile
                .allowed_anycast_prefixes
                .iter()
                .map(|p| p.prefix.to_owned())
                .collect(),
        })
    }
}

/// Converts an RPC interface routing profile into the NVUE renderer model.
impl From<&rpc::FlatInterfaceRoutingProfile> for nvue::InterfaceRoutingProfile {
    fn from(profile: &rpc::FlatInterfaceRoutingProfile) -> Self {
        nvue::InterfaceRoutingProfile {
            allowed_anycast_prefixes: profile
                .allowed_anycast_prefixes
                .iter()
                .map(|p| p.prefix.to_owned())
                .collect(),
        }
    }
}

/// `parse_managed_host_loopback_ips` types the string-valued RPC fields before
/// NVUE rendering. In particular, parsing `loopback_ip_v6` as `Ipv6Addr` keeps
/// an IPv4 value from slipping through just because protobuf stores it as a
/// string.
fn parse_managed_host_loopback_ips(
    config: &rpc::ManagedHostNetworkConfig,
) -> eyre::Result<(IpAddr, Option<Ipv6Addr>)> {
    if config.loopback_ip.is_empty() {
        return Err(eyre::eyre!("missing loopback IP"));
    }

    let loopback_ip = config
        .loopback_ip
        .parse()
        .wrap_err_with(|| format!("invalid primary loopback IP: {}", config.loopback_ip))?;
    let loopback_ip_v6 = config
        .loopback_ip_v6
        .as_deref()
        .map(str::parse)
        .transpose()
        .wrap_err("invalid IPv6 loopback IP")?;

    Ok((loopback_ip, loopback_ip_v6))
}

/// Returns peer VNIs only when Core explicitly marks them as policy-filtered.
///
/// The protobuf default is false, so configurations from Core versions that
/// predate the marker cannot reactivate peerings during a rolling upgrade.
fn vpc_peer_vnis_for_rendering(authoritative: bool, vnis: &[u32]) -> Vec<u32> {
    if authoritative {
        vnis.to_vec()
    } else {
        Vec::new()
    }
}

/// Selects the isolation prefixes for the active virtualizer.
///
/// New Core versions resolve FNN null-route policy before transmission and
/// preserve explicit prefix boundaries in the presence-bearing field. During
/// an agent-first rolling upgrade, an older Core omits that field and the
/// agent reduces the legacy site-prefix list to its minimal exact union before
/// using it as the fallback.
fn site_isolation_prefixes_for_rendering(
    virtualization_type: VpcVirtualizationType,
    config: &rpc::ManagedHostNetworkConfigResponse,
) -> eyre::Result<Vec<String>> {
    if virtualization_type == VpcVirtualizationType::Fnn {
        if let Some(prefixes) = config.site_fabric_null_routes.as_ref() {
            Ok(prefixes.items.clone())
        } else {
            let legacy_prefixes = config
                .site_fabric_prefixes
                .iter()
                .map(|prefix| {
                    prefix.parse::<IpNet>().map_err(|error| {
                        eyre::eyre!("invalid legacy site-fabric prefix {prefix}: {error}")
                    })
                })
                .collect::<eyre::Result<Vec<_>>>()?;
            Ok(aggregate(legacy_prefixes)
                .into_iter()
                .map(|prefix| prefix.to_string())
                .collect())
        }
    } else {
        Ok(config.site_fabric_prefixes.clone())
    }
}

/// Builds tenant RA inputs from Core's mode-specific allocation without
/// confusing the stateful tenant `/128` with its containing `/127` linknet.
fn tenant_ipv6_router_advertisement(
    interface: &FlatInterfaceConfig,
    rdnss_servers: &[Ipv6Addr],
) -> Option<nvue::Ipv6RouterAdvertisementConfig> {
    let address = interface
        .addresses
        .iter()
        .find(|address| address.address_family == i32::from(rpc::AddressFamily::V6))?;
    let IpNet::V6(prefix) = address.prefix.parse().ok()? else {
        return None;
    };
    let IpNet::V6(interface_prefix) = address.interface_prefix.parse().ok()? else {
        return None;
    };
    let mode = if address.ip.is_empty() {
        // VPC-level SLAAC is explicit only when Core supplies the allocated /64.
        if prefix.prefix_len() != 64
            || interface_prefix.prefix_len() != 64
            || interface_prefix.network() != prefix.network()
        {
            return None;
        }
        nvue::Ipv6RouterAdvertisementMode::Slaac
    } else {
        // Stateful FNN persists the second /127 endpoint as a tenant /128;
        // the first endpoint remains the DPU address derived from the linknet.
        let host_address = address.ip.parse::<Ipv6Addr>().ok()?;
        if prefix.prefix_len() != 127
            || interface_prefix.prefix_len() != 128
            || interface_prefix.network() != host_address
            || !prefix.contains(&host_address)
            || host_address == prefix.network()
        {
            return None;
        }
        nvue::Ipv6RouterAdvertisementMode::Stateful
    };

    Some(nvue::Ipv6RouterAdvertisementConfig {
        prefix: prefix.to_string(),
        mode,
        rdnss_servers: rdnss_servers.to_vec(),
    })
}

/// Returns Core's authoritative IPv6 address-list entry, when present.
fn canonical_ipv6_address(interface: &FlatInterfaceConfig) -> Option<&rpc::InterfaceAddressConfig> {
    interface
        .addresses
        .iter()
        .find(|address| address.address_family == i32::from(rpc::AddressFamily::V6))
}

/// Parses and network-normalizes the segment prefix in a canonical V6 entry.
fn normalized_ipv6_segment_prefix(address: &rpc::InterfaceAddressConfig) -> Option<Ipv6Net> {
    let Ok(IpNet::V6(prefix)) = address.prefix.parse() else {
        return None;
    };
    Some(prefix.trunc())
}

/// Selects Core's authoritative segment prefix, falling back to the deprecated
/// sidecar only when the canonical V6 entry is absent.
fn ipv6_segment_prefix(interface: &FlatInterfaceConfig, legacy_fallback: &str) -> Option<String> {
    let Some(address) = canonical_ipv6_address(interface) else {
        return Some(legacy_fallback.to_owned());
    };
    normalized_ipv6_segment_prefix(address).map(|prefix| prefix.to_string())
}

/// Builds stateful RA inputs from an admin host `/128` and its containing segment.
///
/// Callers must select admin ports only on the primary DPU.
/// The canonical address entry keeps the host `/128` distinct from the
/// containing segment so the host route cannot become the advertised prefix.
fn admin_ipv6_router_advertisement(
    virtualization_type: VpcVirtualizationType,
    address: &rpc::InterfaceAddressConfig,
    rdnss_servers: &[Ipv6Addr],
) -> Option<nvue::Ipv6RouterAdvertisementConfig> {
    // The caller supplies admin ports only on the primary DPU. Keep the
    // render-side FNN guard for both the VLAN SVI and the routed PF.
    if virtualization_type != VpcVirtualizationType::Fnn {
        return None;
    }

    let prefix = normalized_ipv6_segment_prefix(address)?;
    let host_address = address.ip.parse::<Ipv6Addr>().ok()?;
    let interface_prefix = address.interface_prefix.parse::<Ipv6Net>().ok()?;

    if interface_prefix.prefix_len() != 128
        || interface_prefix.network() != host_address
        || !prefix.contains(&host_address)
    {
        return None;
    }

    Some(nvue::Ipv6RouterAdvertisementConfig {
        prefix: prefix.to_string(),
        mode: nvue::Ipv6RouterAdvertisementMode::Stateful,
        rdnss_servers: rdnss_servers.to_vec(),
    })
}

/// Selects the family-neutral host VRF loopback with legacy IPv4 fallback so rendering and
/// shared-VPC validation use the same address.
/// Callers must validate the family-neutral address entries before selecting the loopback.
#[allow(deprecated)]
pub(super) fn tenant_vrf_loopback_ip(interface: &FlatInterfaceConfig) -> Option<&str> {
    interface
        .addresses
        .iter()
        .find_map(|address| address.tenant_vrf_loopback_ip.as_deref())
        .or(interface.tenant_vrf_loopback_ip.as_deref())
}

/// Update the NVUE network config, returning whether NVUE applied a change.
/// With `StartupFile` and `skip_post`, only save the desired file and return
/// whether that file was replaced. Startup-file YAML must fit within 10 MiB after
/// supplemental merging unless an admin update replaces an existing file above 1 MiB.
/// Errors from saving or applying the desired configuration are returned to the caller.
/// `fmds_ipv6_gateway` is resolved from the local agent configuration, not
/// the Core response, so both startup-file and REST updates use the same peer.
// The fetcher projects `addresses` into these compatibility fields before rendering.
#[allow(deprecated)]
pub(super) async fn update_nvue(
    vpc_virtualization_type: VpcVirtualizationType,
    update_flavor: NvueUpdateFlavor<'_>,
    nc: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
    hbn_device_names: HBNDeviceNames,
    supplemental_config: Option<&str>,
    fmds_ipv6_gateway: Option<ipnetwork::Ipv6Network>,
) -> eyre::Result<bool> {
    let hbn_version = match update_flavor {
        NvueUpdateFlavor::StartupFile { .. } => hbn::read_version().await?,
        NvueUpdateFlavor::RestApi { ref nvue_context } => nvue_context
            .nvue_client
            .system_build_info()
            .await
            .map_err(|e| eyre::eyre!("couldn't get HBN version from NVUE: {e}"))
            .and_then(|build_value| hbn::parse_nvue_build_as_hbn_version(&build_value))?,
    };

    let managed_host_config = nc
        .managed_host_config
        .as_ref()
        .ok_or_else(|| eyre::eyre!("missing managed_host_config in response"))?;
    let (loopback_ip, loopback_ip_v6) = parse_managed_host_loopback_ips(managed_host_config)?;

    let access_vlans = if nc.use_admin_network {
        let admin_interface = nc
            .admin_interface
            .as_ref()
            .ok_or_else(|| eyre::eyre!("missing admin_interface"))?;
        let admin_ipv6 = canonical_ipv6_address(admin_interface);
        vec![nvue::VlanConfig {
            vlan_id: admin_interface.vlan_id,
            network: admin_interface.interface_prefix.clone().unwrap_or_default(),
            ip: admin_interface.ip.clone().unwrap_or_default(),
            ipv6_vlan_config: admin_ipv6.map(|ipv6| nvue::Ipv6VlanConfig {
                network: ipv6.interface_prefix.clone(),
                ip: ipv6.ip.clone(),
            }),
        }]
    } else {
        let mut access_vlans = Vec::with_capacity(nc.tenant_interfaces.len());
        for net in &nc.tenant_interfaces {
            access_vlans.push(nvue::VlanConfig {
                vlan_id: net.vlan_id,
                network: net.interface_prefix.clone().unwrap_or_default(),
                ip: net.ip.clone().unwrap_or_default(),
                ipv6_vlan_config: net.ipv6_interface_config.as_ref().map(|v6| {
                    nvue::Ipv6VlanConfig {
                        network: v6.interface_prefix.clone(),
                        ip: v6.ip.clone(),
                    }
                }),
            });
        }
        access_vlans
    };

    let service_networks = service_network::validate_and_build_ports(nc)?;
    let (has_stateful_nsg, network_security_groups) = build_network_security_group_rules(nc)?;

    // Host port selection follows use_admin_network and is_primary_dpu. Service ports retain their
    // accepted VRFs during boot and release on secondary DPUs, whose Admin responses contain no host
    // port. Core must omit service endpoints from primary Admin responses.
    let tenancy_enabled =
        !nc.use_admin_network || nc.is_primary_dpu || !nc.service_interfaces.is_empty();

    let physical_name = hbn_device_names.reps[0].to_string();
    let (_, rdnss_servers) = split_addresses_by_family(&service_addrs.nameservers);
    let mut networks = if nc.use_admin_network {
        if nc.is_primary_dpu {
            let admin_interface = nc
                .admin_interface
                .as_ref()
                .ok_or_else(|| eyre::eyre!("missing admin_interface"))?;
            let admin_ipv6 = canonical_ipv6_address(admin_interface);
            vec![nvue::PortConfig {
                interface_name: physical_name,
                is_phy: true,
                host_ip: admin_interface.ip.clone().unwrap_or_default(),
                host_route: admin_interface.interface_prefix.clone().unwrap_or_default(),
                host_ipv6: admin_ipv6.map(|ipv6| ipv6.ip.clone()),
                host_ipv6_route: admin_ipv6.map(|ipv6| ipv6.interface_prefix.clone()),
                vlan: admin_interface.vlan_id as u16,
                vni: if nc.network_virtualization_type() == ::rpc::forge::VpcVirtualizationType::Fnn
                {
                    Some(admin_interface.vni)
                } else {
                    None
                },
                l3_vni: if nc.network_virtualization_type()
                    == ::rpc::forge::VpcVirtualizationType::Fnn
                {
                    Some(admin_interface.vpc_vni)
                } else {
                    None
                },
                gateway_cidrs: admin_interface
                    .gateway
                    .iter()
                    .filter(|address| !address.is_empty())
                    .cloned()
                    .collect(),
                ipv6_port_config: admin_ipv6.map(|ipv6| nvue::Ipv6PortConfig {
                    gateway_cidrs: normalized_ipv6_segment_prefix(ipv6)
                        .map(|prefix| vec![prefix.to_string()])
                        .unwrap_or_default(),
                    svi_ip: ipv6.svi_ip.clone(),
                    router_advertisement: admin_ipv6_router_advertisement(
                        vpc_virtualization_type,
                        ipv6,
                        &rdnss_servers,
                    ),
                }),
                vpc_prefixes: admin_interface.vpc_prefixes.clone(),
                vpc_peer_prefixes: admin_interface.vpc_peer_prefixes.clone(),
                vpc_peer_vnis: vpc_peer_vnis_for_rendering(
                    nc.vpc_peer_vnis_authoritative,
                    &admin_interface.vpc_peer_vnis,
                ),
                svi_ip: admin_interface.svi_ip.clone(),
                tenant_vrf_loopback_ip: tenant_vrf_loopback_ip(admin_interface).map(str::to_owned),
                network_security_group_id: None, // NSGs are not applied on the admin network.
                routing_profile: admin_interface
                    .vpc_routing_profile
                    .as_ref()
                    .map(nvue::RoutingProfile::try_from)
                    .transpose()?,
                interface_routing_profile: admin_interface
                    .interface_routing_profile
                    .as_ref()
                    .map(nvue::InterfaceRoutingProfile::from),
                is_l2_segment: if nc.network_virtualization_type()
                    == ::rpc::forge::VpcVirtualizationType::Fnn
                {
                    admin_interface.is_l2_segment
                } else {
                    // Why false in legacy case? ¯\_(ツ)_/¯
                    false
                },
                is_service: false,
            }]
        } else {
            vec![]
        }
    } else {
        let mut ifs = Vec::with_capacity(nc.tenant_interfaces.len());
        for net in &nc.tenant_interfaces {
            let name = if net.function_type == rpc::InterfaceFunctionType::Physical as i32 {
                physical_name.clone()
            } else {
                match net.virtual_function_id {
                    Some(id) => hbn_device_names.build_virt(id),
                    None => {
                        eyre::bail!("missing virtual function id");
                    }
                }
            };

            // Core owns the IPv6 link prefix. The DPU address is its first
            // address, while stateful DHCPv6 assigns the second /127 endpoint
            // carried independently as the tenant /128.
            ifs.push(nvue::PortConfig {
                interface_name: name,
                is_phy: net.function_type == rpc::InterfaceFunctionType::Physical as i32,
                vlan: net.vlan_id as u16,
                host_ip: net.ip.clone().unwrap_or_default(),
                host_route: net.interface_prefix.clone().unwrap_or_default(),
                host_ipv6: net.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
                host_ipv6_route: net
                    .ipv6_interface_config
                    .as_ref()
                    .map(|v6| v6.interface_prefix.clone()),
                vni: Some(net.vni), // TODO should this be nc.vni_device?
                l3_vni: Some(net.vpc_vni),
                gateway_cidrs: net
                    .gateway
                    .iter()
                    .filter(|address| !address.is_empty())
                    .cloned()
                    .collect(),
                ipv6_port_config: net.ipv6_interface_config.as_ref().map(|v6| {
                    // New configs derive the DPU address from Core's one
                    // authoritative prefix. Only configs without a V6 entry
                    // retain the legacy sidecar mapping during upgrades.
                    let gateway_cidr =
                        ipv6_segment_prefix(net, &v6.interface_prefix).unwrap_or_default();
                    nvue::Ipv6PortConfig {
                        gateway_cidrs: (!gateway_cidr.is_empty())
                            .then_some(gateway_cidr)
                            .into_iter()
                            .collect(),
                        svi_ip: v6.svi_ip.clone(),
                        router_advertisement: if vpc_virtualization_type
                            == VpcVirtualizationType::Fnn
                            && !net.is_l2_segment
                        {
                            tenant_ipv6_router_advertisement(net, &rdnss_servers)
                        } else {
                            None
                        },
                    }
                }),
                vpc_prefixes: net.vpc_prefixes.clone(),
                vpc_peer_prefixes: net.vpc_peer_prefixes.clone(),
                vpc_peer_vnis: vpc_peer_vnis_for_rendering(
                    nc.vpc_peer_vnis_authoritative,
                    &net.vpc_peer_vnis,
                ),
                svi_ip: net.svi_ip.clone(),
                tenant_vrf_loopback_ip: tenant_vrf_loopback_ip(net).map(str::to_owned),
                network_security_group_id: net
                    .network_security_group
                    .as_ref()
                    .map(|n| n.id.clone()),
                routing_profile: net
                    .vpc_routing_profile
                    .as_ref()
                    .map(nvue::RoutingProfile::try_from)
                    .transpose()?,
                interface_routing_profile: net
                    .interface_routing_profile
                    .as_ref()
                    .map(nvue::InterfaceRoutingProfile::from),
                is_l2_segment: net.is_l2_segment,
                is_service: false,
            });
        }
        ifs
    };
    networks.extend(service_networks);

    // We should explicitly guard against the absence of interfaces.
    // A follow-up should probably do some work to split out tenant enabled vs. disabled DPUs more clearly.
    if tenancy_enabled && networks.is_empty() {
        return Err(eyre::eyre!(
            "BUG: network config provided without interfaces"
        ));
    }

    // FNN requires a routing profile per rendered port, unless an older
    // response-level compatibility profile is present.
    if vpc_virtualization_type == VpcVirtualizationType::Fnn
        && nc.routing_profile.is_none()
        && !networks
            .iter()
            .all(|network| network.routing_profile.is_some())
    {
        return Err(eyre::eyre!(
            "BUG: FNN config provided without routing-profile"
        ));
    }

    // Currently there's only one quarantine mode, BlockAllTraffic, so we block everything if it's set at all.
    let is_quarantined = nc
        .managed_host_config
        .as_ref()
        .is_some_and(|c| c.quarantine_state.is_some());

    let network_security_policy_override_rules = if is_quarantined {
        tracing::info!("managed host is quarantined! Disabling network access via nvue");

        build_quarantined_network_security_group_rules()
    } else {
        nc.network_security_policy_overrides
            .iter()
            .map(|r| r.try_into())
            .collect::<Result<Vec<NetworkSecurityGroupRule>, eyre::Error>>()?
    };

    let hostname = hostname().wrap_err("gethostname error")?;
    let is_dpu_os = matches!(update_flavor, NvueUpdateFlavor::StartupFile { .. });
    let dhcp_servers = nc
        .dhcp_servers
        .iter()
        .map(|ip| ip.parse::<IpAddr>())
        .collect::<Result<Vec<_>, _>>()
        .wrap_err("invalid DHCP server IP")?;
    let route_servers = nc
        .route_servers
        .iter()
        .map(|ip| ip.parse::<IpAddr>())
        .collect::<Result<Vec<_>, _>>()
        .wrap_err("invalid route server IP")?;
    let conf = nvue::NvueConfig {
        fmds_ipv6_gateway,
        is_fnn: false,
        is_dpu_os,
        fmds_gateway_vlan: if !is_dpu_os {
            nc.tenant_interfaces
                .iter()
                .find(|i| i.function_type == rpc::InterfaceFunctionType::Physical as i32)
                .map(|i| i.vlan_id as u16)
        } else {
            None
        },
        vpc_virtualization_type,
        site_global_vpc_vni: nc.site_global_vpc_vni,
        use_admin_network: nc.use_admin_network,
        tenancy_enabled,
        loopback_ip,
        loopback_ip_v6,
        asn: nc.asn,
        datacenter_asn: nc.datacenter_asn,
        common_internal_route_target: nc
            .common_internal_route_target
            .as_ref()
            .map(route_target_from_rpc)
            .transpose()?,
        additional_route_target_imports: nc
            .additional_route_target_imports
            .iter()
            .map(route_target_from_rpc)
            .collect::<eyre::Result<_>>()?,
        dpu_hostname: hostname.hostname,
        dpu_search_domain: hostname.search_domain,
        hbn_version: Some(hbn_version),
        uplinks: hbn_device_names
            .uplinks
            .into_iter()
            .map(String::from)
            .collect(),
        dhcp_servers,
        route_servers,
        ct_port_configs: networks,
        ct_vrf_name: format!("vpc_{}", nc.vpc_vni.unwrap_or_default()),
        ct_access_vlans: access_vlans,
        deny_prefixes: nc.deny_prefixes.clone(),
        site_fabric_prefixes: site_isolation_prefixes_for_rendering(vpc_virtualization_type, nc)?,
        anycast_site_prefixes: nc.anycast_site_prefixes.clone(),
        tenant_host_asn: nc.tenant_host_asn,
        stateful_acls_enabled: nc.stateful_acls_enabled && has_stateful_nsg,

        // For now, the isolation options boil down to a boolean,
        // but the match will make sure we catch and adjust accordingly
        // if that changes in the future.
        use_vpc_isolation: match nc.vpc_isolation_behavior() {
            rpc::VpcIsolationBehaviorType::VpcIsolationInvalid => {
                return Err(eyre::eyre!("received invalid VPC-isolation config"));
            }
            rpc::VpcIsolationBehaviorType::VpcIsolationMutual => true,
            //  There's no isolation.
            rpc::VpcIsolationBehaviorType::VpcIsolationOpen => false,
        },

        network_security_policy_override_rules,
        network_security_groups,
        ct_l3_vni: nc.vpc_vni,
        ct_vrf_loopback: "FNN".to_string(),
        l3_domains: vec![],
        ct_routing_profile: nc
            .routing_profile
            .as_ref()
            .map(nvue::RoutingProfile::try_from)
            .transpose()?,
        bgp_leaf_session_password: nc.bgp_leaf_session_password.clone(),
    };

    // next_contents is a YAML-serialized NVUE config.
    let next_contents = nvue::build(conf)?;

    // Merging before the write/push keeps the supplemental content part of the
    // same atomic NVUE revision on both apply flavors. A blank file (empty or
    // whitespace-only, hence the trim: a pre-created ConfigMap or a stray
    // trailing newline) means "no patch" rather than failing reconciliation;
    // a malformed one fails loudly instead of being silently dropped.
    let next_contents = match supplemental_config.map(str::trim) {
        Some(patch) if !patch.is_empty() => {
            crate::supplemental_config::merge_into_nvue_yaml(&next_contents, patch)
                .wrap_err("merging supplemental network config")?
        }
        _ => next_contents,
    };

    match update_flavor {
        NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post,
        } => {
            let startup_path = hbn_root.join(nvue::PATH);
            // If switching to the admin network, we want to just force the write.
            // We've seen a past incident where a tenant managed to create a config
            // that exceeded the original 1 MiB limit. Because of the diff check failing, it
            // also prevented a successful termination because the NVUE config couldn't
            // be switched to the admin network.
            let force = nc.use_admin_network
                && startup_path.exists()
                && startup_path.metadata()?.len() > ADMIN_FORCE_WRITE_THRESHOLD;

            // Bound startup files before cleanup or writes unless admin recovery forces replacement.
            // Keep REST submissions outside this file-size check.
            eyre::ensure!(
                force || next_contents.len() <= MAX_EXPECTED_SIZE as usize,
                "NVUE configuration exceeds maximum size: {} > {} bytes",
                next_contents.len(),
                MAX_EXPECTED_SIZE,
            );

            // Cleanup non-NVUE ACL files
            // We can remove this once az01 is upgraded
            cleanup_old_acls(hbn_root);

            // Write the extra ACL config
            let path_acl = FPath(hbn_root.join(nvue::PATH_ACL));
            path_acl.cleanup();
            let mut rules = NVUED_BLOCK_RULE.to_string();
            rules.push_str(acl_rules::ARP_SUPPRESSION_RULE);
            match write(rules, &path_acl, "NVUE ACL", MAX_DHCP_ACL_SIZE, false) {
                Ok(true) => {
                    if !skip_post {
                        let cmd = acl_rules::RELOAD_CMD;
                        if let Err(err) = hbn::run_in_container_shell(cmd).await {
                            tracing::error!(
                                command = %cmd,
                                error = format!("{err:#}"),
                                "running nvue extra acl post"
                            );
                        }
                        path_acl.del("BAK");
                    }
                }
                // ACLs didn't need changing, should be always this except on first boot
                Ok(false) => {}
                // Log the error but continue so that we get network working
                Err(err) => tracing::error!(error = format!("{err:#}"), "write nvue extra ACL"),
            }

            // nvue can save a copy of the config here. If that exists nvue uses it on boot.
            // We always want to use the most recent `nv config apply`, so ensure this doesn't exist.
            let saved_config = hbn_root.join(nvue::SAVE_PATH);
            if saved_config.exists()
                && let Err(err) = fs::remove_file(&saved_config)
            {
                tracing::warn!(
                    saved_config_path = %saved_config.display(),
                    error = format!("{err:#}"),
                    "Failed removing old startup.yaml"
                );
            }

            // Write the config we're going to apply
            let path = FPath(startup_path);
            path.cleanup();
            let file_changed = write(next_contents, &path, "NVUE", MAX_EXPECTED_SIZE, force)
                .wrap_err(format!("NVUE config at {path}"))?;

            if !skip_post {
                // The agent can restart after saving the file but before
                // applying it. Check NVUE even when the file is unchanged;
                // `apply` skips the live update when NVUE reports no semantic diff.
                return nvue::apply(hbn_root, &path).await;
            }
            Ok(file_changed)
        }
        NvueUpdateFlavor::RestApi { nvue_context } => {
            let config = NvueConfigWithHeader::from_yaml(&next_contents)
                .map(|config_with_header| config_with_header.into_nvue_config())
                .map_err(|e| eyre::eyre!("couldn't parse NVUE config as YAML: {e}"))?;
            let revision_id = nvue_context
                .update_config(&config)
                .await
                .map_err(|e| eyre::eyre!("couldn't push new config to NVUE server: {e}"))?;
            if let Some(revision_id) = revision_id {
                tracing::debug!(revision_id, "Applied NVUE config via REST API");
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

/// Builds one NVUE policy per NSG ID so host and service interfaces share one ACL index.
fn build_network_security_group_rules(
    config: &ManagedHostNetworkConfigResponse,
) -> eyre::Result<(bool, Vec<nvue::NetworkSecurityGroup>)> {
    let mut network_security_groups = BTreeMap::<String, nvue::NetworkSecurityGroup>::new();
    let mut has_stateful = false;

    for nsg in config
        .tenant_interfaces
        .iter()
        .filter_map(|interface| interface.network_security_group.as_ref())
        .chain(
            config
                .service_interfaces
                .iter()
                .filter_map(|interface| interface.network_security_group.as_ref()),
        )
    {
        let rules = nsg
            .rules
            .iter()
            .map(NetworkSecurityGroupRule::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        has_stateful |= nsg.stateful_egress;

        network_security_groups
            .entry(nsg.id.clone())
            .or_insert_with(|| nvue::NetworkSecurityGroup {
                id: nsg.id.clone(),
                rules,
                stateful_egress: nsg.stateful_egress,
            });
    }

    Ok((
        has_stateful,
        network_security_groups.into_values().collect(),
    ))
}

/// Build a set of security group rules that deny all traffic.
///
/// Builds rules for ipv6 and ipv4, both ingress and ingress, denying traffic to all address
/// prefixes.
fn build_quarantined_network_security_group_rules() -> Vec<NetworkSecurityGroupRule> {
    let build_rule = |ingress, ipv6| {
        let catchall_prefix = if ipv6 {
            vec!["::/0".to_string()]
        } else {
            vec!["0.0.0.0/0".to_string()]
        };

        nvue::NetworkSecurityGroupRule {
            id: format!(
                "quarantine_{}_{}",
                if ipv6 { "ipv6" } else { "ipv4" },
                if ingress { "ingress" } else { "egress" }
            ),
            ingress,
            ipv6,
            priority: 0,
            src_port_start: None,
            src_port_end: None,
            dst_port_start: None,
            dst_port_end: None,
            can_match_any_protocol: true,
            can_be_stateful: false,
            protocol: NetworkSecurityGroupRuleProtocol::to_string_from_enum_i32(
                NetworkSecurityGroupRuleProtocol::NsgRuleProtoAny.into(),
            )
            .expect("BUG: cannot convert `any` protocol to string?")
            .to_lowercase(),
            action: NetworkSecurityGroupRuleAction::to_string_from_enum_i32(
                NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
            )
            .expect("BUG: cannot convert deny action to string?")
            .to_lowercase(),
            src_prefixes: catchall_prefix.clone(),
            dst_prefixes: catchall_prefix,
        }
    };

    vec![
        build_rule(false, false),
        build_rule(false, true),
        build_rule(true, false),
        build_rule(true, true),
    ]
}

async fn get_interface_state(interface_name: &str) -> eyre::Result<InterfaceState> {
    let mut cmd = tokio::process::Command::new("ip");
    cmd.arg("link").arg("show").arg(interface_name);
    let output = cmd.output().await?;

    if !output.status.success() {
        return Err(eyre::eyre!(
            "failed to get interface state: {}",
            output.status
        ));
    }

    let output = String::from_utf8_lossy(&output.stdout);
    InterfaceState::from_str(&output)
}

fn needed_interface_state(is_primary_dpu: bool, use_admin_network: bool) -> InterfaceState {
    // Interface is always UP on primary DPU.
    if is_primary_dpu {
        return InterfaceState::Up;
    }

    // If secondary DPU and on tenant network, enable the interface.
    if !use_admin_network {
        return InterfaceState::Up;
    }

    // If secondary DPU and on admin network, disable the interface.
    InterfaceState::Down
}

pub(super) async fn update_interface_state(
    nc: &ManagedHostNetworkConfigResponse,
) -> eyre::Result<()> {
    let needed_state = needed_interface_state(nc.is_primary_dpu, nc.use_admin_network);

    InterfaceState::update_state(&needed_state).await
}

/// Stops the DHCP process via the dhcp-server gRPC control service.
///
/// The gRPC control server remains up after this call so that a future
/// [`update_dhcp_via_grpc`] call can restart the DHCP process.
///
/// Returns `Ok(false)` (matching the file-write path convention) to signal
/// that no active DHCP service reload occurred.
async fn stop_dhcp_via_grpc(grpc_addr: &str) -> eyre::Result<bool> {
    crate::dhcp_server_grpc_client::stop_server(grpc_addr)
        .await
        .wrap_err_with(|| format!("stop_dhcp_via_grpc({grpc_addr})"))?;
    Ok(false)
}

/// `managed_host_ipv4_loopback` selects the primary loopback for DHCPv4.
/// IPv6 primary addresses disable DHCPv4; missing or malformed loopbacks fail.
pub(super) fn managed_host_ipv4_loopback(
    config: &rpc::ManagedHostNetworkConfig,
) -> eyre::Result<Option<Ipv4Addr>> {
    match parse_managed_host_loopback_ips(config)?.0 {
        IpAddr::V4(address) => Ok(Some(address)),
        IpAddr::V6(_) => Ok(None),
    }
}

/// Build the same DHCP options for file and gRPC delivery.
fn build_dhcp_server_config(
    network_config: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
) -> eyre::Result<DhcpConfig> {
    let Some(mh_nc) = &network_config.managed_host_config else {
        eyre::bail!("loopback IP is missing. can't write dhcp-server config");
    };
    let loopback_ip = managed_host_ipv4_loopback(mh_nc)?;

    let (nameservers_v4, nameservers_v6) = split_addresses_by_family(&service_addrs.nameservers);

    let (ntpservers_v4, ntpservers_v6) = build_dhcp_ntp_servers(network_config, service_addrs);

    let pxe_ip_v4 = match loopback_ip {
        Some(_) => Some(
            service_addrs
                .pxe_ips
                .iter()
                .find_map(|address| match address {
                    IpAddr::V4(address) => Some(*address),
                    IpAddr::V6(_) => None,
                })
                .ok_or_else(|| {
                    eyre::eyre!(
                        "DHCPv4 server config requires an IPv4 PXE/UEFI HTTP boot address, but none found in {:?}",
                        service_addrs.pxe_ips
                    )
                })?,
        ),
        None => None,
    };

    let pxe_ip_v6 = service_addrs
        .pxe_ips
        .iter()
        .find_map(|address| match address {
            IpAddr::V6(address) => Some(*address),
            IpAddr::V4(_) => None,
        });

    // Check interface boot URLs only when host DHCP is enabled so secondary-admin service
    // listeners do not depend on `admin_interface.booturl`.
    if (!network_config.use_admin_network || network_config.is_primary_dpu)
        && pxe_ip_v4.is_none()
        && pxe_ip_v6.is_none()
    {
        let interfaces = if network_config.use_admin_network {
            network_config.admin_interface.as_slice()
        } else {
            network_config.tenant_interfaces.as_slice()
        };
        // Explicit interface boot URLs do not use the discovered PXE address;
        // an explicitly empty URL disables boot URL generation.
        // Keep requiring one if any interface still needs a generated URL.
        if interfaces
            .iter()
            .any(|interface| interface.booturl.is_none())
        {
            eyre::bail!(
                "PXE/UEFI HTTP boot server has no address usable by this DPU; resolved addresses: {:?}",
                service_addrs.pxe_ips
            );
        }
    }

    let mut dhcp_config = DhcpConfig::from_forge_dhcp_config(
        pxe_ip_v4,
        ntpservers_v4,
        nameservers_v4,
        nameservers_v6,
        loopback_ip,
    )?;

    dhcp_config.carbide_provisioning_server_ipv6 = pxe_ip_v6;
    dhcp_config.carbide_ntpservers_v6 = ntpservers_v6;
    dhcp_config.dhcpv6_preferred_lifetime_secs = dhcp::DHCPV6_PREFERRED_LIFETIME_SECS;
    dhcp_config.dhcpv6_valid_lifetime_secs = dhcp::DHCPV6_VALID_LIFETIME_SECS;
    dhcp_config.dhcpv6_server_preference = dhcpv6_server_preference(network_config)?;
    if loopback_ip.is_none() {
        dhcp_config.dhcpv6_server_id =
            Some(DhcpV6ServerId::from_remote_id(&network_config.remote_id)?);
    }
    dhcp_config.validate()?;
    Ok(dhcp_config)
}

/// Holds one fixed DHCP listener and its VPC assignment while service reservations are assembled.
struct DhcpServiceSlot {
    receiving_interface: String,
    vpc_id: Option<carbide_uuid::vpc::VpcId>,
    reservations: Vec<ServiceDhcpReservation>,
}

/// Converts Core's fixed service-slot inventory and endpoint bindings into one validated DHCP
/// replacement, preserving explicit empty listeners before activation and after final removal.
fn build_dhcp_service_config(
    network_config: &rpc::ManagedHostNetworkConfigResponse,
    interface_translation_mode: Option<&InterfaceTranslationMode>,
) -> eyre::Result<(Option<ServiceConfig>, Vec<String>)> {
    let Some(inventory) = &network_config.service_vpc_slot_inventory else {
        eyre::ensure!(
            network_config.service_interfaces.is_empty(),
            "service interfaces require a service VPC slot inventory"
        );
        return Ok((None, Vec::new()));
    };

    // Index every supplied slot before assigning reservations so endpoint order cannot affect the
    // replacement and an unknown slot cannot be inferred from its numeric value.
    let mut slots = BTreeMap::new();
    for slot in &inventory.slots {
        eyre::ensure!(
            !slot.hbn_interface_name.is_empty(),
            "service VPC slot {} has an empty HBN interface name",
            slot.slot_index
        );
        let receiving_interface = interface_translation_mode.map_or_else(
            || slot.hbn_interface_name.clone(),
            |translation| translation.translate(&slot.hbn_interface_name),
        );
        eyre::ensure!(
            slots
                .insert(
                    slot.slot_index,
                    DhcpServiceSlot {
                        receiving_interface,
                        vpc_id: None,
                        reservations: Vec::new(),
                    },
                )
                .is_none(),
            "duplicate service VPC slot index {}",
            slot.slot_index
        );
    }

    // Resolve each endpoint through the inventory and retain one VPC per local HBN slot because
    // one interface cannot participate in multiple VRFs at the same time.
    for service_interface in &network_config.service_interfaces {
        let slot = slots
            .get_mut(&service_interface.slot_index)
            .ok_or_else(|| {
                eyre::eyre!(
                    "service interface {} ordinal {} references unknown slot {}",
                    service_interface.service_id,
                    service_interface.interface_ordinal,
                    service_interface.slot_index
                )
            })?;
        let vpc_id = service_interface.vpc_id.ok_or_else(|| {
            eyre::eyre!(
                "service interface {} ordinal {} is missing its VPC ID",
                service_interface.service_id,
                service_interface.interface_ordinal
            )
        })?;
        if let Some(existing_vpc_id) = slot.vpc_id {
            eyre::ensure!(
                existing_vpc_id == vpc_id,
                "service VPC slot {} contains endpoints from VPCs {} and {}",
                service_interface.slot_index,
                existing_vpc_id,
                vpc_id
            );
        } else {
            slot.vpc_id = Some(vpc_id);
        }

        let link = match service_interface
            .link_prefix
            .parse::<ipnetwork::IpNetwork>()
            .wrap_err_with(|| {
                format!(
                    "invalid service link prefix for {} ordinal {}",
                    service_interface.service_id, service_interface.interface_ordinal
                )
            })? {
            ipnetwork::IpNetwork::V4(prefix) => {
                eyre::ensure!(
                    prefix.prefix() == 31 && prefix.ip() == prefix.network(),
                    "IPv4 service link {} must be a canonical /31",
                    service_interface.link_prefix
                );
                ServiceLink::Ipv4(ServiceLinkV4 {
                    address: prefix.broadcast(),
                    prefix,
                    gateway: prefix.network(),
                })
            }
            ipnetwork::IpNetwork::V6(prefix) => {
                eyre::ensure!(
                    prefix.prefix() == 127 && prefix.ip() == prefix.network(),
                    "IPv6 service link {} must be a canonical /127",
                    service_interface.link_prefix
                );
                // A canonical /127 has an even final byte, so this increment cannot overflow.
                let mut address = prefix.network().octets();
                address[15] += 1;
                ServiceLink::Ipv6(ServiceLinkV6 {
                    address: Ipv6Addr::from(address),
                    prefix,
                    hbn_peer: prefix.network(),
                })
            }
        };
        slot.reservations.push(ServiceDhcpReservation {
            attachment_id: service_interface
                .attachment_id
                .as_ref()
                .ok_or_else(|| {
                    eyre::eyre!(
                        "service interface {} ordinal {} is missing its attachment ID",
                        service_interface.service_id,
                        service_interface.interface_ordinal
                    )
                })?
                .try_into()
                .wrap_err("invalid service attachment ID")?,
            interface_ordinal: service_interface.interface_ordinal,
            internal_uuid: service_interface
                .internal_uuid
                .as_ref()
                .ok_or_else(|| {
                    eyre::eyre!(
                        "service interface {} ordinal {} is missing its internal UUID",
                        service_interface.service_id,
                        service_interface.interface_ordinal
                    )
                })?
                .try_into()
                .wrap_err("invalid service internal UUID")?,
            mac_address: service_interface.mac_address.parse().wrap_err_with(|| {
                format!(
                    "invalid service MAC for {} ordinal {}",
                    service_interface.service_id, service_interface.interface_ordinal
                )
            })?,
            link,
        });
    }

    let interface_groups = slots
        .into_values()
        .map(|slot| ServiceDhcpInterfaceReservations {
            receiving_interface: slot.receiving_interface,
            reservations: slot.reservations,
        })
        .collect::<Vec<_>>();
    let receiving_interfaces = interface_groups
        .iter()
        .map(|group| group.receiving_interface.clone())
        .collect();
    Ok((
        Some(ServiceConfig::try_new(interface_groups)?),
        receiving_interfaces,
    ))
}

/// Carries one complete DHCP control replacement after host and service listeners have been
/// translated and deduplicated from the same accepted network response.
struct PreparedDhcpUpdate {
    dhcp_config: DhcpConfig,
    host_config: Option<HostConfig>,
    service_config: Option<ServiceConfig>,
    interfaces: Vec<String>,
}

/// Builds one complete DHCP control replacement or selects the explicit stop operation when no
/// known host or fixed service listener should remain active.
fn prepare_dhcp_grpc_update(
    network_config: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
    hbn_device_names: HBNDeviceNames,
    interface_translation_mode: Option<&InterfaceTranslationMode>,
) -> eyre::Result<Option<PreparedDhcpUpdate>> {
    let (service_config, service_interfaces) =
        build_dhcp_service_config(network_config, interface_translation_mode)?;
    let host_dhcp_enabled = !network_config.use_admin_network || network_config.is_primary_dpu;
    let selected_host_interfaces_exist = if network_config.use_admin_network {
        network_config.admin_interface.is_some()
    } else {
        !network_config.tenant_interfaces.is_empty()
    };
    let service_only =
        network_config.service_vpc_slot_inventory.is_some() && !selected_host_interfaces_exist;
    let mut host_config = if host_dhcp_enabled && !service_only {
        Some(HostConfig::try_from(
            network_config.clone(),
            hbn_device_names.reps[0],
            hbn_device_names.virt_rep_begin,
            hbn_device_names.sf_id,
            false,
        )?)
    } else {
        None
    };

    // Translate host names at the same step as fixed slot names so the union sent to the
    // DHCP server contains each receiving interface in its target namespace exactly once.
    if let (Some(translation_mode), Some(host_config)) =
        (interface_translation_mode, host_config.as_mut())
    {
        host_config.host_ip_addresses = std::mem::take(&mut host_config.host_ip_addresses)
            .into_iter()
            .map(|(name, info)| (translation_mode.translate(&name), info))
            .collect();
    }
    let interfaces = host_config
        .iter()
        .flat_map(|config| config.host_ip_addresses.keys().cloned())
        .chain(service_interfaces)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    // A known empty topology or disabled secondary-admin host service must use StopServer because
    // UpdateAndReloadConfig does not stop remembered listeners for interfaces=[].
    if interfaces.is_empty()
        && (!host_dhcp_enabled || network_config.service_vpc_slot_inventory.is_some())
    {
        return Ok(None);
    }

    let dhcp_config = build_dhcp_server_config(network_config, service_addrs)?;
    // Refuse IPv4 service bindings without a real DPU IPv4 loopback because the DHCP server opens
    // no IPv4 socket in that configuration and fabricating an address would make replies invalid.
    eyre::ensure!(
        !service_config
            .as_ref()
            .is_some_and(ServiceConfig::has_ipv4_reservations)
            || dhcp_config.ipv4()?.is_some(),
        "IPv4 service reservations require an IPv4 DPU loopback"
    );

    Ok(Some(PreparedDhcpUpdate {
        dhcp_config,
        host_config,
        service_config,
        interfaces,
    }))
}

/// Sends DHCP settings, optional host settings, complete service reservations, and listener names
/// through the control service, using its explicit stop operation for known empty topology or
/// secondary-admin responses without service listeners.
///
/// Matching configuration and interfaces do not restart a running server, so
/// this is safe to call on every tick. Returns `Ok(true)` after the control update succeeds and
/// `Ok(false)` after an accepted stop request. With no interfaces, file-only changes remain staged
/// while a changed service replacement applies on remembered listeners; success does not
/// guarantee every listener has bound.
async fn update_dhcp_via_grpc(
    grpc_addr: &str,
    network_config: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
    hbn_device_names: HBNDeviceNames,
    interface_translation_mode: Option<&InterfaceTranslationMode>,
) -> eyre::Result<bool> {
    let Some(update) = prepare_dhcp_grpc_update(
        network_config,
        service_addrs,
        hbn_device_names,
        interface_translation_mode,
    )?
    else {
        return stop_dhcp_via_grpc(grpc_addr).await;
    };

    crate::dhcp_server_grpc_client::update_and_reload(
        grpc_addr,
        update.dhcp_config,
        update.host_config,
        update.service_config,
        update.interfaces,
    )
    .await
    .wrap_err_with(|| format!("update_dhcp_via_grpc({grpc_addr})"))?;
    Ok(true)
}

/// Updates DHCP server configuration using either the gRPC or file-write path.
///
/// When `dhcp_grpc_server` is `Some`, delegates to [`update_dhcp_via_grpc`], which submits DHCP
/// settings, optional host settings, complete service reservations, and listener names, or
/// explicitly stops a drained listener inventory.
///
/// When `dhcp_grpc_server` is `None`, validates IPv6-only configuration with
/// the installed server before replacing files and restarting supervisord.
/// Write-only updates (`skip_post`) defer that check until application. Stopping
/// a secondary admin DHCP server preserves DHCP configuration and identity
/// without preparing a replacement. Failed writes or reloads attempt to
/// restore the previous files and report any restoration failure. Populated
/// service topology is rejected because the file protocol has no representation
/// for service reservations.
///
/// Returns `Ok(true)` after changed files are saved, a pending file update is
/// applied, or a control update succeeds (including staging with no interfaces).
/// With `skip_post`, saved files still need application on a later call.
/// Returns `Ok(false)` for unchanged files or a successful gRPC stop.
pub(super) async fn update_dhcp(
    hbn_root: &Path,
    network_config: &rpc::ManagedHostNetworkConfigResponse,
    // if true don't run the reload/restart commands after file update
    skip_post: bool,
    service_addrs: &ServiceAddresses,
    hbn_device_names: HBNDeviceNames,
    dhcp_grpc_server: Option<String>,
    interface_translation_mode: Option<&InterfaceTranslationMode>,
) -> eyre::Result<bool> {
    // DPU-backed admin DHCP is authoritative only on the primary DPU. API-side
    // reconciliation may move the active admin DHCP row between DPU-backed host
    // interfaces, so secondary DPUs must not answer with stale host config.
    let stop_server = network_config.use_admin_network && !network_config.is_primary_dpu;
    if let Some(ref addr) = dhcp_grpc_server {
        return update_dhcp_via_grpc(
            addr,
            network_config,
            service_addrs,
            hbn_device_names,
            interface_translation_mode,
        )
        .await;
    }

    // The file-backed server has no durable or reloadable service-reservation input. Reject fixed
    // service topology instead of acknowledging a replacement that the server can never receive.
    eyre::ensure!(
        network_config.service_interfaces.is_empty()
            && network_config
                .service_vpc_slot_inventory
                .as_ref()
                .is_none_or(|inventory| inventory.slots.is_empty()),
        "service VPC DHCP topology requires the gRPC control transport"
    );

    let path_dhcp_relay = FPath(hbn_root.join(dhcp::RELAY_PATH));
    let path_dhcp_relay_nvue = FPath(hbn_root.join(dhcp::RELAY_PATH_NVUE));
    let paths_dhcp_server = DhcpServerPaths {
        server: FPath(hbn_root.join(dhcp::SERVER_PATH)),
        config: FPath(hbn_root.join(dhcp::SERVER_CONFIG_PATH)),
        host_config: FPath(hbn_root.join(dhcp::SERVER_HOST_CONFIG_PATH)),
    };
    // Stopping must not depend on a replacement configuration or identity.
    // Keep the live DHCP files so the next start can recover the same DUID.
    let mut config = if stop_server {
        None
    } else {
        Some(prepare_dhcp_server_config(
            network_config,
            service_addrs,
            &hbn_device_names,
        )?)
    };
    let files = match config.as_mut() {
        Some(config) => {
            if config.dhcp.ipv4()?.is_none() {
                config.preserve_server_identifier(&paths_dhcp_server.config)?;
            }
            config.files(&path_dhcp_relay, &paths_dhcp_server, &path_dhcp_relay_nvue)?
        }
        None => {
            let supervisor =
                dhcp::build_server_supervisord_config(dhcp::DhcpServerSupervisordConfig {
                    interfaces: Vec::new(),
                    autostart: false,
                })?;
            [
                (&path_dhcp_relay, Some(dhcp::blank())),
                (&paths_dhcp_server.server, Some(supervisor)),
                (&path_dhcp_relay_nvue, None),
            ]
            .into_iter()
            .map(|(path, next)| DhcpFileUpdate::new(path, next))
            .collect::<eyre::Result<Vec<_>>>()?
        }
    };
    let pending_apply = paths_dhcp_server.config.with_ext("PENDING");
    if !files.iter().any(|file| file.previous != file.next)
        && (skip_post || !pending_apply.try_exists()?)
    {
        return Ok(false);
    }
    if let Some(config) = &config
        && config.dhcp.ipv4()?.is_none()
        && !skip_post
    {
        // Check the installed binary before replacing files. An older binary
        // rejects this flag without seeing any changed live configuration.
        config
            .validate_in_hbn(hbn_root, &paths_dhcp_server.config)
            .await?;
    }
    // Matching files do not prove supervisord loaded them. Keep this marker
    // until reload succeeds, including across an interrupted agent update.
    fs::write(&pending_apply, [])
        .wrap_err_with(|| format!("record pending DHCP reload: {}", pending_apply.display()))?;
    let mut written = 0;
    let apply_result = async {
        for file in &files {
            file.write(file.next.as_deref())?;
            written += 1;
        }
        if !skip_post {
            let command = if stop_server {
                dhcp::STOP_DHCP_SERVER
            } else {
                dhcp::RELOAD_DHCP_SERVER
            };
            hbn::run_in_container_shell(command).await?;
        }
        Ok::<_, eyre::Report>(())
    }
    .await;
    if let Err(error) = apply_result {
        let mut rollback_errors = Vec::new();
        for file in files[..written].iter().rev() {
            if let Err(rollback_error) = file.write(file.previous.as_deref()) {
                rollback_errors.push(format!("{}: {rollback_error:#}", file.path));
            }
        }
        if !rollback_errors.is_empty() {
            return Err(error.wrap_err(format!(
                "restore DHCP files: {}",
                rollback_errors.join(", ")
            )));
        }
        return Err(error);
    }
    if !skip_post {
        fs::remove_file(&pending_apply)
            .wrap_err_with(|| format!("complete DHCP reload: {}", pending_apply.display()))?;
    }
    for file in &files {
        file.path.cleanup();
    }
    Ok(true)
}

/// Interfaces to report back to server
// The fetcher projects `addresses` into these compatibility fields before status reporting.
#[allow(deprecated)]
pub(super) async fn interfaces(
    network_config: &rpc::ManagedHostNetworkConfigResponse,
    factory_mac_address: MacAddress,
    nvue_client: Option<&NvueClient>,
) -> eyre::Result<Vec<rpc::InstanceInterfaceStatusObservation>> {
    let mut interfaces = vec![];
    if network_config.use_admin_network {
        let Some(iface) = network_config.admin_interface.as_ref() else {
            eyre::bail!("use_admin_network is true but admin interface is missing");
        };
        let addresses = build_dual_stack_list(
            iface.ip.clone(),
            iface.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
        );
        let prefixes = build_dual_stack_list(
            iface.interface_prefix.clone(),
            iface
                .ipv6_interface_config
                .as_ref()
                .map(|v6| v6.interface_prefix.clone()),
        );
        interfaces.push(rpc::InstanceInterfaceStatusObservation {
            function_type: iface.function_type,
            virtual_function_id: None,
            mac_address: Some(factory_mac_address.to_string()),
            addresses,
            prefixes,
            gateways: build_dual_stack_list(iface.gateway.clone(), None),
            network_security_group: None,
            internal_uuid: iface.internal_uuid.clone(),
        });
    } else {
        // Only load virtual interface details if there are any
        let fdb = if network_config
            .tenant_interfaces
            .iter()
            .any(|iface| iface.function_type == rpc::InterfaceFunctionType::Virtual as i32)
        {
            match nvue_client {
                Some(nvue_client) => {
                    let mac_table = nvue_client.bridge_mac_table("br_default").await?;
                    vlan_fdb_map_from_nvue_mac_table(mac_table)
                }
                None => {
                    let fdb_json = hbn::run_in_container(
                        &hbn::get_hbn_container_id().await?,
                        &["bridge", "-j", "fdb", "show"],
                        true,
                    )
                    .await?;
                    parse_fdb(&fdb_json)?
                }
            }
        } else {
            HashMap::new()
        };

        for iface in network_config.tenant_interfaces.iter() {
            let mac = if iface.function_type == rpc::InterfaceFunctionType::Physical as i32 {
                Some(factory_mac_address.to_string())
            } else {
                match fdb.get(&iface.vlan_id) {
                    Some(vlan_fdb) => match tenant_vf_mac(vlan_fdb).await {
                        Ok(mac) => Some(mac.to_string()),
                        Err(err) => {
                            tracing::error!(
                                error = %err,
                                vlan_id = iface.vlan_id,
                                "Error fetching tenant VF MAC"
                            );
                            None
                        }
                    },
                    None => {
                        tracing::error!(
                            vlan_id = iface.vlan_id,
                            "Missing fdb bridge info for vlan"
                        );
                        None
                    }
                }
            };

            let network_security_group =
                iface
                    .network_security_group
                    .as_ref()
                    .map(|nsg| rpc::NetworkSecurityGroupStatus {
                        id: nsg.id.clone(),
                        // If a network security group was set, then this
                        // field must be be a valid non-default value.
                        // The default value will be (correctly) rejected by
                        // the server.
                        source: nsg.source().into(),
                        version: nsg.version.clone(),
                    });

            let addresses = build_dual_stack_list(
                iface.ip.clone(),
                iface.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
            );
            let prefixes = build_dual_stack_list(
                iface.interface_prefix.clone(),
                iface
                    .ipv6_interface_config
                    .as_ref()
                    .map(|v6| v6.interface_prefix.clone()),
            );
            interfaces.push(rpc::InstanceInterfaceStatusObservation {
                function_type: iface.function_type,
                virtual_function_id: iface.virtual_function_id,
                mac_address: mac,
                addresses,
                prefixes,
                gateways: build_dual_stack_list(iface.gateway.clone(), None),
                network_security_group,
                internal_uuid: iface.internal_uuid.clone(),
            });
        }
    }
    Ok(interfaces)
}

// The fetcher projects `addresses` into the compatibility IPv4 field before health checks.
#[allow(deprecated)]
pub(super) fn tenant_peers(network_config: &rpc::ManagedHostNetworkConfigResponse) -> Vec<&str> {
    network_config
        .tenant_interfaces
        .iter()
        .filter_map(|iface| iface.ip.as_deref())
        .collect()
}

/// Reset networking to blank.
/// Clear DHCP and NVUE config files.
pub(super) async fn reset(hbn_root: &Path, skip_post: bool) {
    tracing::debug!("Setting network config to blank");

    let mut errs = vec![];
    let mut post_actions = vec![];
    let dhcp_relay_path = FPath(hbn_root.join(dhcp::RELAY_PATH));
    match write(
        dhcp::blank(),
        &dhcp_relay_path,
        "DHCP relay",
        MAX_DHCP_ACL_SIZE,
        false,
    ) {
        Ok(true) => post_actions.push(PostAction {
            cmd: dhcp::RELOAD_CMD,
        }),
        Ok(false) => {}
        Err(err) => errs.push(format!("Write blank DHCP relay: {err:#}")),
    }
    let dhcp_server_path = FPath(hbn_root.join(dhcp::SERVER_PATH));
    match write(
        dhcp::blank(),
        &dhcp_server_path,
        "DHCP server",
        MAX_DHCP_ACL_SIZE,
        false,
    ) {
        Ok(true) => post_actions.push(PostAction {
            cmd: dhcp::RELOAD_CMD,
        }),
        Ok(false) => {}
        Err(err) => errs.push(format!("Write blank DHCP server: {err:#}")),
    }

    // Clean up NVUE config
    let nvue_path = FPath(hbn_root.join(nvue::PATH));
    if nvue_path.0.exists()
        && let Err(err) = fs::remove_file(&nvue_path.0)
    {
        errs.push(format!("remove NVUE config {nvue_path}: {err:#}"));
    }

    if !skip_post {
        for post in post_actions {
            if let Err(err) = hbn::run_in_container_shell(post.cmd).await {
                errs.push(format!("reload '{}': {err}", post.cmd))
            }
        }
    }

    let err_message = errs.join(", ");
    if !err_message.is_empty() {
        tracing::error!(error = %err_message, "Failed to reset network configuration");
    }
}

fn prepare_dhcp_server_config(
    nc: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
    hbn_device_names: &HBNDeviceNames,
) -> eyre::Result<PreparedDhcpServerConfig> {
    let interfaces = if nc.use_admin_network {
        let interface_name = nc
            .admin_interface
            .as_ref()
            .map(|interface| {
                // Routed admin ports have no VLAN SVI for the DHCP listener.
                if nc.network_virtualization_type() == ::rpc::forge::VpcVirtualizationType::Fnn
                    && !interface.is_l2_segment
                {
                    hbn_device_names.reps[0].to_string()
                } else {
                    format!("vlan{}", interface.vlan_id)
                }
            })
            .ok_or_else(|| eyre::eyre!("admin interface missing on admin network"))?;
        vec![interface_name]
    } else {
        let mut interfaces = Vec::with_capacity(nc.tenant_interfaces.len());
        for interface in &nc.tenant_interfaces {
            let interface_name = if nc.network_virtualization_type()
                == ::rpc::forge::VpcVirtualizationType::Fnn
                && !interface.is_l2_segment
            {
                if interface.function_type() == InterfaceFunctionType::Physical {
                    // pf0hpf_sf/if
                    hbn_device_names.reps[0].to_string()
                } else {
                    // pf0vf{0-15}_sf/if
                    format!(
                        "{}{}{}",
                        hbn_device_names.virt_rep_begin,
                        interface.virtual_function_id(),
                        hbn_device_names.sf_id
                    )
                }
            } else {
                format!("vlan{}", interface.vlan_id)
            };
            interfaces.push(interface_name);
        }

        if interfaces.is_empty() {
            // In case of secondary DPU, tenant interface will be empty.
            // To keep the dhcp-server alive, we need to pass a valid interface.
            interfaces.push("lo".to_string());
        }

        interfaces
    };

    let dhcp = build_dhcp_server_config(nc, service_addrs)?;
    let supervisor = dhcp::build_server_supervisord_config(dhcp::DhcpServerSupervisordConfig {
        interfaces,
        autostart: (!nc.use_admin_network || nc.is_primary_dpu),
    })?;
    let host = dhcp::build_server_host_config(nc.clone(), hbn_device_names)?;
    Ok(PreparedDhcpServerConfig {
        dhcp,
        supervisor,
        host,
    })
}

struct PreparedDhcpServerConfig {
    dhcp: DhcpConfig,
    supervisor: String,
    host: String,
}

impl PreparedDhcpServerConfig {
    fn preserve_server_identifier(&mut self, live_config: &FPath) -> eyre::Result<()> {
        let identity_path = format!("{live_config}.duid");
        match fs::read(&identity_path) {
            Ok(bytes) => {
                // A saved identity lets us replace damaged YAML without
                // changing the server identity that clients already know.
                self.dhcp.dhcpv6_server_id = Some(
                    DhcpV6ServerId::try_from(bytes)
                        .wrap_err_with(|| format!("read DHCP identity {identity_path}"))?,
                );
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).wrap_err_with(|| format!("read DHCP identity {identity_path}"));
            }
        }
        let previous = match read_limited(live_config, MAX_DHCP_ACL_SIZE) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).wrap_err_with(|| format!("read DHCP config {live_config}"));
            }
        };
        let previous: DhcpConfig = serde_yaml::from_str(&previous)
            .wrap_err_with(|| format!("parse previous DHCP config {live_config}"))?;
        // Without a saved identity, capture the IPv4-derived DUID before
        // removing its only input.
        self.dhcp.dhcpv6_server_id =
            Some(previous.server_identifier().wrap_err_with(|| {
                format!("read server identity from DHCP config {live_config}")
            })?);
        Ok(())
    }

    async fn validate_in_hbn(&self, hbn_root: &Path, live_config: &FPath) -> eyre::Result<()> {
        let directory = live_config
            .0
            .parent()
            .ok_or_else(|| eyre::eyre!("DHCP config has no parent directory"))?;
        let mut candidate = tempfile::NamedTempFile::new_in(directory)?;
        candidate.write_all(serde_yaml::to_string(&self.dhcp)?.as_bytes())?;
        let mut host = tempfile::NamedTempFile::new_in(directory)?;
        host.write_all(self.host.as_bytes())?;
        let in_container = |path: &Path| -> eyre::Result<String> {
            Ok(Path::new("/")
                .join(path.strip_prefix(hbn_root)?)
                .to_string_lossy()
                .into_owned())
        };
        let candidate_path = in_container(candidate.path())?;
        let host_path = in_container(host.path())?;
        let live_path = in_container(&live_config.0)?;
        let container = hbn::get_hbn_container_id().await?;
        hbn::run_in_container(
            &container,
            &[
                "/var/support/forge-dhcp/bin/forge-dhcp-server",
                "--validate-config",
                &candidate_path,
                "--host-config",
                &host_path,
                "--dhcp-config",
                &live_path,
            ],
            true,
        )
        .await
        .wrap_err("validate IPv6-only DHCP config with installed server")?;
        Ok(())
    }

    fn files(
        &self,
        relay: &FPath,
        server: &DhcpServerPaths,
        nvue_relay: &FPath,
    ) -> eyre::Result<Vec<DhcpFileUpdate>> {
        [
            (relay, Some(dhcp::blank())),
            (&server.server, Some(self.supervisor.clone())),
            (&server.config, Some(serde_yaml::to_string(&self.dhcp)?)),
            (&server.host_config, Some(self.host.clone())),
            (nvue_relay, None),
        ]
        .into_iter()
        .map(|(path, next)| DhcpFileUpdate::new(path, next))
        .collect()
    }
}

struct DhcpFileUpdate {
    path: FPath,
    previous: Option<String>,
    next: Option<String>,
}

impl DhcpFileUpdate {
    fn new(path: &FPath, next: Option<String>) -> eyre::Result<Self> {
        let previous = match read_limited(path, MAX_DHCP_ACL_SIZE) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).wrap_err_with(|| format!("read DHCP file {path}")),
        };
        Ok(Self {
            path: path.clone(),
            previous,
            next,
        })
    }

    fn write(&self, contents: Option<&str>) -> eyre::Result<()> {
        match contents {
            Some(contents) => {
                write(
                    contents.to_owned(),
                    &self.path,
                    "DHCP configuration",
                    MAX_DHCP_ACL_SIZE,
                    false,
                )?;
            }
            None => match fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).wrap_err_with(|| format!("remove DHCP file {}", self.path));
                }
            },
        }
        Ok(())
    }
}

// Update configuration file
// Returns true if the file has changes, false otherwise.
fn write(
    // What to write into the file
    next_contents: String,
    // The file to write to
    path: &FPath,
    // Human readable description of the file, for error messages
    file_type: &str,
    // Maximum file size in bytes; force bypasses size checks and bounded comparison reads.
    maximum_size: u64,
    force: bool,
) -> eyre::Result<bool> {
    let path_tmp = path.temp();
    fs::write(&path_tmp, next_contents.clone())
        .wrap_err_with(|| format!("fs::write {}", path_tmp.display()))?;

    if !force {
        let path_tmp_size = path_tmp.metadata()?.len();
        if path_tmp_size > maximum_size {
            return Err(eyre::eyre!(
                "new content for '{}' would exceed maximum size: {} > {}",
                path_tmp.display(),
                path_tmp_size,
                maximum_size
            ));
        }
    }

    let has_changed = if !force && path.0.exists() {
        let current =
            read_limited(path, maximum_size).wrap_err_with(|| format!("read_limited {path}"))?;
        current != next_contents
    } else {
        true
    };
    if !has_changed {
        return Ok(false);
    }
    tracing::debug!(%file_type, "Applying new config");

    let path_bak = path.backup();
    if path.0.exists() {
        fs::copy(&path.0, path_bak).wrap_err("copying file to .BAK")?;
    }

    fs::rename(&path_tmp, path).wrap_err("rename")?;

    Ok(true)
}

#[derive(Deserialize, Debug, Clone)]
struct Fdb {
    mac: String,
    ifname: String,
    state: String,
    vlan: Option<u32>,
}

impl Fdb {
    fn is_permanent(&self) -> bool {
        self.state == "permanent"
    }
}

impl From<nvue_client::types::MacTableEntry> for Fdb {
    fn from(mac_table_entry: nvue_client::types::MacTableEntry) -> Self {
        let nvue_client::types::MacTableEntry {
            mac,
            interface,
            entry_type,
            vlan,
        } = mac_table_entry;
        let vlan = vlan.map(u32::from);
        Self {
            mac,
            ifname: interface,
            state: entry_type,
            vlan,
        }
    }
}

fn vlan_fdb_map_from_nvue_mac_table(
    mac_table: Vec<nvue_client::types::MacTableEntry>,
) -> HashMap<u32, Vec<Fdb>> {
    let entries_by_vlan = mac_table.into_iter().filter_map(|table_entry| {
        let fdb = Fdb::from(table_entry);
        if let Some(vlan_id) = fdb.vlan
            && !fdb.is_permanent()
        {
            Some((vlan_id, fdb))
        } else {
            None
        }
    });

    use std::collections::hash_map::Entry;
    let mut fdb_table: HashMap<_, Vec<_>> = HashMap::new();
    for (vlan_id, fdb_entry) in entries_by_vlan {
        match fdb_table.entry(vlan_id) {
            Entry::Occupied(mut occupied_entry) => {
                occupied_entry.get_mut().push(fdb_entry);
            }
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(vec![fdb_entry]);
            }
        }
    }
    fdb_table
}

#[derive(Deserialize, Debug)]
// This has many more fields, only parse the one we check
struct IpShow {
    address: String,
}

fn parse_fdb(fdb_json: &str) -> eyre::Result<HashMap<u32, Vec<Fdb>>> {
    let all_fdb: Vec<Fdb> = serde_json::from_str(fdb_json)?;
    let mut out: HashMap<u32, Vec<Fdb>> = HashMap::new();
    for fdb in all_fdb.into_iter() {
        let Some(vlan) = fdb.vlan else {
            continue;
        };
        if fdb.state == "permanent" {
            continue;
        }
        out.entry(vlan)
            .and_modify(|v| v.push(fdb.clone()))
            .or_insert_with(|| vec![fdb]);
    }

    Ok(out)
}

/// The host/tenant side MAC address of a VF
///
/// To use a VF a tenant needs to do this on their host:
///  - echo 16 > /sys/class/net/eth0/device/sriov_numvfs
///  - ip link set <name> up
///    DPU side this must say 16 but discovery should take care of that:
///    mlxconfig -d /dev/mst/mt41686_pciconf0 query NUM_OF_VFS
async fn tenant_vf_mac(vlan_fdb: &[Fdb]) -> eyre::Result<&str> {
    // We're expecting only the host side and our side
    if vlan_fdb.len() != 2 {
        eyre::bail!("expected two fdb entries, got {vlan_fdb:?}");
    }
    if vlan_fdb[0].ifname != vlan_fdb[1].ifname {
        eyre::bail!(
            "both entries must have the same ifname, got '{}' and '{}'",
            vlan_fdb[0].ifname,
            vlan_fdb[1].ifname
        );
    }

    // Find our side - both will have the same ifname
    let ovs_side = format!("{}_r", vlan_fdb[0].ifname);
    let mut cmd = TokioCommand::new("ip");
    cmd.kill_on_drop(true);
    let cmd = cmd.args(["-j", "address", "show", &ovs_side.to_string()]);
    let cmd_str = super::pretty_cmd(cmd.as_std());

    let cmd_res = timeout(Duration::from_secs(5), cmd.output())
        .await
        .wrap_err_with(|| format!("timeout calling {cmd_str}"))?;
    let ip_out = cmd_res.wrap_err(cmd_str.to_string())?;

    if !ip_out.status.success() {
        tracing::debug!(
            command = %super::pretty_cmd(cmd.as_std()),
            stderr = %String::from_utf8_lossy(&ip_out.stderr),
            "STDERR"
        );
        return Err(eyre::eyre!(
            "{} for cmd '{}'",
            ip_out.status, // includes the string "exit status"
            super::pretty_cmd(cmd.as_std())
        ));
    }

    let ip_json = String::from_utf8_lossy(&ip_out.stdout).to_string();
    let ip_show: Vec<IpShow> = serde_json::from_str(&ip_json)?;
    if ip_show.len() != 1 {
        eyre::bail!("getting local side MAC should return 1 entry, got {ip_show:?}");
    }

    // Ignore our side
    let remote_side: Vec<&Fdb> = vlan_fdb
        .iter()
        .filter(|&f| f.mac != ip_show[0].address)
        .collect();

    if remote_side.len() != 1 {
        eyre::bail!("after all removals there should be 1 entry, got {remote_side:?}");
    }
    Ok(&remote_side[0].mac)
}

// std::fs::read_to_string but limited to maximum_size bytes for safety
fn read_limited<P: AsRef<Path>>(path: P, maximum_size: u64) -> io::Result<String> {
    let f = File::open(path)?;
    let l = f.metadata()?.len();
    if l > maximum_size {
        return Err(io::Error::other(
            // ErrorKind::FileTooLarge but it's nightly only
            format!("{l} > {maximum_size} bytes"),
        ));
    }
    // in case it changes as we read
    let mut f_limit = f.take(maximum_size);
    let mut s = String::with_capacity(l as usize);
    f_limit.read_to_string(&mut s)?;
    Ok(s)
}

// Ask the OS for its hostname.
//
// On a DPU this is correctly set to the DB hostname of the first interface, the hyphenated
// two-word randomly generated name.
fn hostname() -> eyre::Result<Hostname> {
    let mut buf = vec![0u8; 64 + 1]; // Linux HOST_NAME_MAX is 64
    // SAFETY: `buf` is live and exclusively writable for all `buf.len()` bytes. `u8` and
    // `c_char` have the same size and alignment, and `gethostname` writes at most that length.
    let res = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if res != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let cstr = CStr::from_bytes_until_nul(&buf)?;
    let fqdn = cstr.to_string_lossy().into_owned();
    let hostname = fqdn
        .split('.')
        .next()
        .map(|s| s.to_owned())
        .ok_or(eyre::eyre!("empty hostname?"))?;
    let search_domain = fqdn.split('.').skip(1).collect::<Vec<&str>>().join(".");
    Ok(Hostname {
        hostname,
        search_domain,
        #[cfg(test)]
        fqdn,
    })
}

struct Hostname {
    hostname: String,
    search_domain: String,
    #[cfg(test)]
    fqdn: String,
}

#[derive(Debug, Clone)]
pub struct FPath(pub PathBuf);
impl FPath {
    /// The previous config, in case we need to revert
    pub fn backup(&self) -> PathBuf {
        self.with_ext("BAK")
    }

    /// The new config before we apply it
    pub fn temp(&self) -> PathBuf {
        self.with_ext("TMP")
    }

    /// `.TEST` is an old path that was used when migrating from Go VPC,
    /// and briefly re-appears in Jan/Feb 2024. Clean it up.
    ///
    /// `.TMP` is the pending config before it is applied. It should be removed
    /// on drop.
    ///
    /// `.BAK` holds the previous file contents and is removed after successful
    /// application. Callers handle restoration before invoking cleanup.
    pub fn cleanup(&self) -> bool {
        let mut has_deleted = self.del("TEST");
        has_deleted = has_deleted || self.del("TMP");
        has_deleted = has_deleted || self.del("BAK");
        has_deleted
    }

    pub fn del(&self, ext: &'static str) -> bool {
        let p = self.with_ext(ext);
        if p.exists() {
            match fs::remove_file(&p) {
                Ok(_) => true,
                Err(err) => {
                    tracing::warn!(
                        file_path = %p.display(),
                        error = %err,
                        "Failed to remove file"
                    );
                    false
                }
            }
        } else {
            false
        }
    }

    pub fn with_ext(&self, ext: &'static str) -> PathBuf {
        let mut p = self.0.clone();
        p.set_extension(ext);
        p
    }
}

impl AsRef<Path> for FPath {
    fn as_ref(&self) -> &Path {
        self.0.as_ref()
    }
}

impl Drop for FPath {
    fn drop(&mut self) {
        self.del("TMP");
    }
}

impl fmt::Display for FPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// Delete the non-NVUE ACL rules so that they don't interfere with NVUE.
/// Also delete the very old VPC migration ACL rules, which used a non-standard naming convention
fn cleanup_old_acls(hbn_root: &Path) {
    let old_acls = hbn_root.join(acl_rules::PATH);

    let mut old_acls_test = old_acls.clone();
    old_acls_test.as_mut_os_string().push(".TEST");

    let mut old_acls_tmp = old_acls.clone();
    old_acls_tmp.as_mut_os_string().push(".TMP");

    // not see in the wild, but just in case
    let mut old_acls_bak = old_acls.clone();
    old_acls_bak.as_mut_os_string().push(".BAK");

    for p in [&old_acls, &old_acls_test, &old_acls_tmp, &old_acls_bak] {
        if p.exists() {
            match fs::remove_file(p) {
                Ok(_) => {
                    tracing::info!(acl_file_path = %p.display(), "Cleaned up old ACL file");
                }
                Err(err) => {
                    tracing::warn!(
                        acl_file_path = %p.display(),
                        error = %err,
                        "Failed removing old ACL file."
                    );
                }
            }
        }
    }
}

// In some cases (e.g. different container namespaces), the other services we
// send configuration data to might see different interface names from the ones
// HBN sees. This allows us to translate them.
pub(super) enum InterfaceTranslationMode {
    // The translated interface is just the input interface with a string prepended.
    Prepend(String),
}

impl InterfaceTranslationMode {
    fn translate(&self, input_interface_name: &str) -> String {
        use InterfaceTranslationMode::*;
        match self {
            Prepend(prefix) => {
                format!("{prefix}{input_interface_name}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::{Path, PathBuf};
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ::rpc::{common as rpc_common, forge as rpc};
    use axum::Router;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use carbide_network::virtualization::{VpcVirtualizationType, get_svi_ip};
    use carbide_rpc_utils::dhcp::{DhcpConfig, HostConfig, ServiceLink};
    use eyre::WrapErr;
    use ipnetwork::IpNetwork;

    use super::*;
    use crate::ethernet_virtualization::{
        InterfaceState, ServiceAddresses, needed_interface_state,
    };
    use crate::{HBNDeviceNames, dhcp, nvue};

    /// Verifies the agent preserves legacy zero while rejecting symbolic values
    /// that only Core can resolve.
    /// Also rejects concrete targets with two oversized components while accepting
    /// either component at 65535 so both ASN-specific encodings stay available.
    #[test]
    fn route_target_conversion_handles_legacy_and_unresolved_values() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::scenarios;

        scenarios!(run = |target| {
            route_target_from_rpc(&target).map_err(|error| error.to_string())
        };
            "agent compatibility" {
                // Older Core versions can omit proto3 field 2 to represent zero.
                rpc_common::RouteTarget {
                    asn: 65_000,
                    vni: None, // Exercise legacy omission rather than explicit zero.
                    vni_self: false,
                } => Yields(nvue::RouteTargetConfig {
                    asn: 65_000,
                    vni: 0,
                }),

                // VPC-derived values must never be guessed by the DPU agent.
                rpc_common::RouteTarget {
                    asn: 429_000_000,
                    vni: None,
                    vni_self: true, // Core must resolve this before agent delivery.
                } => Fails,
            }
            "agent encoding" {
                // Two oversized components fit neither encoding and must fail before rendering.
                rpc_common::RouteTarget {
                    asn: 65_536, // Requires the four-byte ASN encoding.
                    vni: Some(65_536), // Exceeds its two-byte local-administrator limit.
                    vni_self: false,
                } => Fails,

                // A four-byte ASN accepts the inclusive two-byte local-administrator limit.
                rpc_common::RouteTarget {
                    asn: 65_536, // Selects the four-byte ASN encoding.
                    vni: Some(65_535), // Retains the largest two-byte local administrator.
                    vni_self: false,
                } => Yields(nvue::RouteTargetConfig {
                    asn: 65_536,
                    vni: 65_535,
                }),

                // A two-byte ASN accepts a local administrator requiring four bytes.
                rpc_common::RouteTarget {
                    asn: 65_535, // Retains the inclusive two-byte ASN limit.
                    vni: Some(65_536), // Requires the four-byte local-administrator encoding.
                    vni_self: false,
                } => Yields(nvue::RouteTargetConfig {
                    asn: 65_535,
                    vni: 65_536,
                }),
            }
        );
    }

    /// Reapplies A after NVUE accepts B but its status request fails, preventing a retained A hash
    /// from skipping recovery while B remains active.
    #[tokio::test]
    async fn nvue_push_error_reapplies_previously_confirmed_config() {
        use std::sync::Arc;

        use axum::body::to_bytes;
        use axum::extract::{Request, State};
        use axum::http::{Method, StatusCode};
        use axum::response::IntoResponse;
        use axum::{Json, Router};
        use nvue_client::client::NvueServerAddress;
        use serde_json::{Value, json};
        use tokio::net::UnixListener;
        use tokio::sync::{Mutex, oneshot};

        /// Keeps staged and applied NVUE configuration separate so an observation error cannot undo
        /// the accepted apply that makes restoring A necessary.
        #[derive(Default)]
        struct NvueState {
            requests: Vec<String>,
            revision_count: u32,
            pending_config: Value,
            applied_config: Value,
            fail_next_observation: bool,
        }

        // Keep one server and context alive across A, ambiguous B, and restored A.
        let directory = tempfile::tempdir().expect("NVUE socket directory should be created");
        let socket_path = directory.path().join("nvue.sock");
        let listener = UnixListener::bind(&socket_path).expect("NVUE socket should bind");
        let state = Arc::new(Mutex::new(NvueState {
            pending_config: json!({}),
            applied_config: json!({}),
            ..Default::default()
        }));
        let app = Router::new()
            .fallback(
                |State(state): State<Arc<Mutex<NvueState>>>, request: Request| async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, 1024 * 1024)
                        .await
                        .expect("NVUE request body should be readable");
                    let mut state = state.lock().await;
                    state
                        .requests
                        .push(format!("{} {}", parts.method, parts.uri));
                    let response = match (parts.method, parts.uri.path()) {
                        (Method::POST, "/nvue_v1/revision") => {
                            state.revision_count += 1;
                            json!({ (state.revision_count.to_string()): {} })
                        }
                        (Method::DELETE, "/nvue_v1/") => {
                            state.pending_config = json!({});
                            json!({})
                        }
                        (Method::PATCH, "/nvue_v1/") => {
                            state.pending_config = serde_json::from_slice(&body)
                                .expect("staged NVUE configuration should be JSON");
                            json!({})
                        }
                        (Method::GET, "/nvue_v1/") => {
                            if state.pending_config == state.applied_config {
                                json!({})
                            } else {
                                json!({"changed": true})
                            }
                        }
                        // Activate the staged configuration before any status request can fail.
                        (Method::PATCH, path) if path.starts_with("/nvue_v1/revision/") => {
                            state.applied_config = state.pending_config.clone();
                            json!({"state": "applied"})
                        }
                        (Method::GET, path) if path.starts_with("/nvue_v1/revision/") => {
                            // Fail only observation, preserving the configuration accepted above.
                            if std::mem::take(&mut state.fail_next_observation) {
                                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                            }
                            json!({"state": "applied"})
                        }
                        _ => return StatusCode::NOT_FOUND.into_response(),
                    };
                    Json(response).into_response()
                },
            )
            .with_state(Arc::clone(&state));
        let (shutdown, shutdown_received) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_received.await;
                })
                .await
                .expect("NVUE server should stop cleanly");
        });
        let client = NvueClient::new(NvueServerAddress::UnixSocket { socket_path })
            .expect("NVUE client should be constructed");
        let mut context = NvueClientContext::new(client);

        // Only the hostname differs; the other fields provide an otherwise empty NVUE configuration.
        let config_a = NvueConfig {
            system: Some(json!({"hostname": "config-a"})), // Give A a distinct content hash.
            bridge: None,
            evpn: None,
            interface: None,
            nve: None,
            router: None,
            vrf: None,
            acl: None,
        };
        let config_b = NvueConfig {
            system: Some(json!({"hostname": "config-b"})), // B must be applied rather than skipped.
            ..config_a.clone()
        };
        let expected_a = serde_json::to_value(&config_a).expect("A should serialize");
        let expected_b = serde_json::to_value(&config_b).expect("B should serialize");

        // Confirm A through the real client before arming the next status-request failure.
        assert_eq!(
            context
                .update_config(&config_a)
                .await
                .expect("A should apply"),
            Some("1".to_string())
        );
        {
            let mut state = state.lock().await;
            assert_eq!(state.applied_config, expected_a);
            // Arm only B's status failure after confirming A is active.
            state.fail_next_observation = true;
        }

        // Let B become active, then return the deliberately failed status observation to the context.
        let error = context
            .update_config(&config_b)
            .await
            .expect_err("B observation should fail after its apply succeeds");
        // Verify B stayed active when its revision status request failed.
        let NvueClientError::RequestFailed(failure) = error else {
            panic!("expected an HTTP observation failure, got {error:?}");
        };
        assert_eq!(failure.method, Method::GET);
        assert_eq!(failure.url.path(), "/nvue_v1/revision/2");
        assert_eq!(
            failure.source.status(),
            Some(StatusCode::INTERNAL_SERVER_ERROR)
        );
        assert_eq!(state.lock().await.applied_config, expected_b);

        // Return to A through the same context, proving the failure cannot leave its old hash trusted.
        assert_eq!(
            context
                .update_config(&config_a)
                .await
                .expect("A should be restored"),
            Some("3".to_string())
        );
        assert_eq!(state.lock().await.applied_config, expected_a);

        // Each push must create, stage, diff, apply, and observe a revision, including restored A.
        let expected_requests = (1..=3)
            .flat_map(|revision| {
                [
                    "POST /nvue_v1/revision".to_string(),
                    format!("DELETE /nvue_v1/?rev={revision}"),
                    format!("PATCH /nvue_v1/?rev={revision}"),
                    format!("GET /nvue_v1/?diff=applied&rev={revision}&filled=false"),
                    format!("PATCH /nvue_v1/revision/{revision}"),
                    format!("GET /nvue_v1/revision/{revision}"),
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(state.lock().await.requests, expected_requests);

        // Close the client and join the server before removing its Unix socket directory.
        drop(context);
        shutdown.send(()).expect("NVUE server should be running");
        server.await.expect("NVUE server task should finish");
    }

    /// Verifies unknown conditional match enums fail at the RPC conversion before GE can be assumed.
    #[test]
    fn conditional_export_conversion_rejects_unknown_match() {
        // Keep the target concrete so only the unknown wire enum drives rejection.
        let profile = rpc::RoutingProfile {
            route_targets_on_exports_by_prefix: vec![rpc::PrefixRouteTarget {
                prefix: "0.0.0.0/0".into(),
                match_type: 99, // Prost preserves this unsupported value.
                route_targets: vec![rpc_common::RouteTarget {
                    asn: 64512,
                    vni: Some(100),
                    vni_self: false,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        // Convert through the same adapter used by update_nvue.
        assert!(nvue::RoutingProfile::try_from(&profile).is_err());
    }

    /// Verifies unresolved Core-owned route targets stop `update_nvue` before
    /// publication, preventing the DPU from guessing the missing VNI.
    #[tokio::test]
    async fn unresolved_route_target_stops_nvue_before_publication() -> eyre::Result<()> {
        // Make only a nested conditional export target unresolved.
        let mut network_config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        network_config
            .routing_profile
            .as_mut()
            .expect("fixture response-level routing profile")
            .route_targets_on_exports_by_prefix = vec![rpc::PrefixRouteTarget {
            prefix: "0.0.0.0/0".into(),
            route_targets: vec![rpc_common::RouteTarget {
                asn: 64512,
                vni: None, // Core must resolve the nested target before publication.
                vni_self: true,
            }],
            ..Default::default()
        }];

        // Precreate every destination directory so publication could succeed.
        let tempdir = tempfile::tempdir()?;
        let hbn_root = tempdir.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // Render through the complete response-to-NVUE update path.
        let error = super::update_nvue(
            VpcVirtualizationType::Fnn,
            NvueUpdateFlavor::StartupFile {
                hbn_root,
                skip_post: true,
            },
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await
        .expect_err("unresolved route target must stop NVUE publication");

        // Rejection must precede both configuration and ACL publication.
        assert!(
            format!("{error:#}").contains("received unresolved route target with vni_self set")
        );
        assert!(!hbn_root.join(nvue::PATH).exists());
        assert!(!hbn_root.join(nvue::PATH_ACL).exists());
        Ok(())
    }

    /// Builds twelve 256-action VPC profiles so publication tests exercise aggregate YAML size
    /// independently of the per-profile expansion limit.
    #[allow(deprecated)]
    fn large_conditional_export_config() -> rpc::ManagedHostNetworkConfigResponse {
        // Retain ordinary host networking while varying only conditional policy and VPC membership.
        let mut config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        // Set FNN explicitly because netconf preserves an omitted network_virtualization_type.
        config.network_virtualization_type = Some(rpc::VpcVirtualizationType::Fnn as i32);
        let conditional_export = rpc::PrefixRouteTarget {
            // Use a full-width IPv6 prefix with a non-exact maximum to retain the LE stanza.
            prefix: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe/127".into(),
            match_type: rpc::ExportPrefixMatchType::Le as i32,
            max_prefix_length: Some(128),
            route_targets: (0..256)
                .map(|target| rpc_common::RouteTarget {
                    asn: u32::MAX,
                    vni: Some(target), // Keep every four-byte-ASN target encodable and distinct.
                    vni_self: false,
                })
                .collect(),
        };

        // Give the required PF and its VFs separate VPCs and disjoint subnets without prefix reuse.
        let interface = config.tenant_interfaces[0].clone();
        config.tenant_interfaces = (0_u32..12)
            .map(|index| {
                let mut interface = interface.clone();
                interface.virtual_function_id = index.checked_sub(1);
                if index == 0 {
                    interface.function_type = rpc::InterfaceFunctionType::Physical.into();
                }
                interface.vlan_id = 200 + index;
                interface.vni = 1000 + index;
                interface.vpc_vni = 2000 + index;
                let subnet = format!("10.217.{}", 100 + index);
                interface.ip = Some(format!("{subnet}.2"));
                interface.interface_prefix = Some(format!("{subnet}.2/32"));
                interface.prefix = Some(format!("{subnet}.0/29"));
                interface.gateway = Some(format!("{subnet}.1/29"));
                interface.svi_ip = Some(format!("{subnet}.3/29"));
                interface.vpc_prefixes = vec![format!("{subnet}.0/24")];
                // Clear copied peer prefixes and VNIs to avoid describing the cloned VPC's peerings.
                interface.vpc_peer_prefixes.clear();
                interface.vpc_peer_vnis.clear();
                interface
                    .vpc_routing_profile
                    .as_mut()
                    .expect("fixture VPC routing profile")
                    .route_targets_on_exports_by_prefix = vec![conditional_export.clone()];
                interface
            })
            .collect();
        config
    }

    /// Rejects oversized startup-file YAML before file changes while preserving REST submission
    /// above 10 MiB so the legacy file limit does not restrict the REST publication contract.
    #[tokio::test]
    async fn oversized_nvue_config_is_rejected_only_for_startup_files() -> eyre::Result<()> {
        use nvue_client::client::NvueServerAddress;
        use tokio::net::UnixListener;

        // Retain ordinary host networking without conditional targets and add supplemental content above 10 MiB.
        let ordinary_config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        let oversized_patch = serde_json::json!({
            "system": {"hostname": "x".repeat(MAX_EXPECTED_SIZE as usize + 1)}
        })
        .to_string();

        // Seed files that the StartupFile path can change before writing its desired YAML.
        let directory = tempfile::tempdir()?;
        let hbn_root = directory.path();
        let startup = FPath(hbn_root.join(nvue::PATH));
        let protected_paths = [
            startup.0.clone(),
            startup.temp(),
            startup.backup(),
            hbn_root.join(nvue::PATH_ACL),
            hbn_root.join(nvue::SAVE_PATH),
            hbn_root.join(acl_rules::PATH),
        ];
        for path in &protected_paths {
            fs::create_dir_all(path.parent().expect("fixture file has a directory"))?;
            fs::write(path, "previous configuration")?;
        }

        // Capture revision submission and report no semantic diff without simulating vendor acceptance.
        let socket_path = hbn_root.join("nvue.sock");
        let listener = UnixListener::bind(&socket_path)?;
        let publication_requests = Arc::new(AtomicUsize::new(0));
        let revision_requests = Arc::clone(&publication_requests);
        let submitted_body = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let captured_body = Arc::clone(&submitted_body);
        let app = Router::new()
            .route(
                "/nvue_v1/system",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({"build": "HBN 2.3.0"}))
                }),
            )
            .route(
                "/nvue_v1/revision",
                axum::routing::post(move || {
                    let revision_requests = Arc::clone(&revision_requests);
                    async move {
                        revision_requests.fetch_add(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({"1": {}}))
                    }
                }),
            )
            .route(
                "/nvue_v1/",
                axum::routing::delete(|| async { axum::Json(serde_json::json!({})) })
                    .patch(move |request: axum::extract::Request| {
                        let captured_body = Arc::clone(&captured_body);
                        async move {
                            // Read the raw request above Axum's default JSON body limit.
                            let body = axum::body::to_bytes(
                                request.into_body(),
                                2 * MAX_EXPECTED_SIZE as usize,
                            )
                            .await
                            .expect("bounded oversized NVUE request");
                            *captured_body.lock().await = body.to_vec();
                            axum::Json(serde_json::json!({}))
                        }
                    })
                    .get(|| async { axum::Json(serde_json::json!({})) }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("NVUE test server should run");
        });
        let client = NvueClient::new(NvueServerAddress::UnixSocket { socket_path })?;
        let mut context = NvueClientContext::new(client);
        // Seed the prior REST hash before exercising either publication path.
        context.last_applied_hash = Some(7);

        for (scenario, config, supplemental, use_rest) in [
            // StartupFile rejection must precede cleanup or replacement of existing artifacts.
            (
                "supplemental StartupFile YAML",
                &ordinary_config,
                Some(oversized_patch.as_str()),
                false,
            ),
            // REST must submit the same oversized supplemental content without applying a local file limit.
            (
                "supplemental REST YAML",
                &ordinary_config,
                Some(oversized_patch.as_str()),
                true,
            ),
        ] {
            // Exercise each real publication path without allowing a live container apply.
            let flavor = if use_rest {
                NvueUpdateFlavor::RestApi {
                    nvue_context: &mut context,
                }
            } else {
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                }
            };
            let result = update_nvue(
                VpcVirtualizationType::Fnn,
                flavor,
                config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                supplemental,
                None,
            )
            .await;

            // Verify REST submits the complete content while StartupFile rejects before publication.
            if use_rest {
                assert!(!result?, "mocked no-diff revision must skip apply");
                assert_eq!(publication_requests.load(Ordering::SeqCst), 1, "{scenario}");
                let body = submitted_body.lock().await;
                assert!(body.len() > MAX_EXPECTED_SIZE as usize);
                let submitted: NvueConfig = serde_json::from_slice(&body)?;
                let submitted_json: serde_json::Value = serde_json::from_slice(&body)?;
                assert_eq!(
                    submitted_json["system"]["hostname"],
                    "x".repeat(MAX_EXPECTED_SIZE as usize + 1)
                );
                assert_eq!(context.last_applied_hash, Some(submitted.u64_hash()));
            } else {
                let error = result.expect_err("oversized startup YAML must stop publication");
                assert!(
                    format!("{error:#}").contains("NVUE configuration exceeds maximum size"),
                    "{scenario}: {error:#}"
                );
                assert_eq!(publication_requests.load(Ordering::SeqCst), 0, "{scenario}");
                assert_eq!(context.last_applied_hash, Some(7), "{scenario}");
            }

            // Neither publication path may change the pre-existing local artifacts in this scenario.
            for path in &protected_paths {
                assert_eq!(
                    fs::read_to_string(path)?,
                    "previous configuration",
                    "{scenario}: {} changed",
                    path.display()
                );
            }
        }
        server.abort();
        Ok(())
    }

    /// Rejects DHCP files above 1 MiB on writes and previous-file reads so oversized DHCP
    /// state cannot pass file staging or rollback preparation.
    #[test]
    fn dhcp_files_retain_existing_size_limit() -> eyre::Result<()> {
        // Create a DHCP candidate one byte above 1 MiB in an empty directory.
        let directory = tempfile::tempdir()?;
        let path = FPath(directory.path().join("dhcp.yaml"));
        let oversized = "x".repeat(1024 * 1024 + 1);
        let update = DhcpFileUpdate::new(&path, Some(oversized.clone()))?;

        // Verify the DHCP writer rejects the candidate before creating the live file.
        assert!(update.write(update.next.as_deref()).is_err());
        assert!(!path.0.exists());

        // Seed an oversized live DHCP file using the same contents.
        fs::write(&path, &oversized)?;

        // Verify previous-file loading rejects oversized state before preparing rollback contents.
        assert!(DhcpFileUpdate::new(&path, None).is_err());
        Ok(())
    }

    /// Publishes NVUE YAML above 1 MiB and applies supplemental deletions so large configurations
    /// remain readable during reconciliation and retain merge-patch support.
    #[tokio::test]
    async fn larger_nvue_configs_support_readback_and_supplemental_deletions() -> eyre::Result<()> {
        // Use twelve profiles with 256 IPv6 actions each and ordinary host networking.
        let config = large_conditional_export_config();
        let directory = tempfile::tempdir()?;
        let hbn_root = directory.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // Publish all twelve profiles within the NVUE startup-file budget.
        assert!(
            update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                None,
                None,
            )
            .await?
        );

        // Read back an artifact above the former limit to prove the larger configuration persisted.
        let published = read_limited(hbn_root.join(nvue::PATH), MAX_EXPECTED_SIZE)?;
        assert!(published.len() > 1024 * 1024);
        assert!(published.len() <= MAX_EXPECTED_SIZE as usize);
        assert!(published.contains("EXPORT_RT_"));

        // Reconcile the same response to exercise comparison against an existing large file.
        assert!(
            !update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                None,
                None,
            )
            .await?
        );

        // Build a patch removing only conditional export maps, their prefix lists, and their VRF references.
        let mut prefix_lists = serde_json::Map::new();
        let mut route_maps = serde_json::Map::new();
        let mut vrfs = serde_json::Map::new();
        for interface in &config.tenant_interfaces {
            let vrf = format!("vpc_{}", interface.vpc_vni);
            let route_map = format!("EXPORT_RT_{vrf}_V6");
            for index in 1..=256 {
                prefix_lists.insert(format!("{route_map}_{index}"), serde_json::Value::Null);
            }
            route_maps.insert(route_map, serde_json::Value::Null);
            vrfs.insert(
                vrf,
                serde_json::json!({
                    "router": {"bgp": {"address-family": {
                        "ipv6-unicast": {"route-export": {"to-evpn": {"route-map": null}}}
                    }}}
                }),
            );
        }
        let supplemental = serde_json::json!({
            "router": {"policy": {"prefix-list": prefix_lists, "route-map": route_maps}},
            "vrf": vrfs,
        })
        .to_string();

        // Publish the complete replacement after supplemental merging has removed those actions.
        assert!(
            update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                Some(&supplemental),
                None,
            )
            .await?
        );

        // Read the saved artifact to prove the final document fits and retained VPC state survives.
        let published = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        assert!(published.len() <= MAX_EXPECTED_SIZE as usize);
        assert!(published.contains("vpc_2000:"));
        assert!(!published.contains("EXPORT_RT_"));
        Ok(())
    }

    /// Verifies admin updates force replacement only when the old file exceeds 1 MiB, bypassing
    /// new-document size checks and bounded comparison reads so size limits cannot block saving
    /// the admin configuration during release. Other updates retain the 10 MiB document guard.
    #[tokio::test]
    async fn nvue_size_guard_preserves_admin_recovery() -> eyre::Result<()> {
        // Vary admin selection and file sizes while retaining the ordinary response-level profile fallback.
        let mut config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        // Set FNN explicitly because netconf preserves an omitted network_virtualization_type.
        config.network_virtualization_type = Some(rpc::VpcVirtualizationType::Fnn as i32);

        // Add supplemental content above 10 MiB without enabling conditional export targets.
        let oversized_size = MAX_EXPECTED_SIZE as usize + 1;
        let oversized_hostname = "x".repeat(oversized_size);
        let oversized_patch = serde_json::json!({
            "system": {"hostname": oversized_hostname.as_str()}
        })
        .to_string();
        // Fix the original limit independently of the production constant to catch threshold changes.
        let original_limit = 1024 * 1024;

        for (scenario, previous_size, use_admin_network, oversized, should_replace) in [
            // The original threshold is exclusive, so an admin update at exactly 1 MiB keeps the guard.
            ("admin at original limit", original_limit, true, true, false),
            // Crossing the original limit must force an admin write even below the new 10 MiB allowance.
            (
                "admin above original limit",
                original_limit + 1,
                true,
                true,
                true,
            ),
            // An admin replacement may exceed 10 MiB even when bounded comparison cannot read the old file.
            (
                "admin oversized replacement",
                oversized_size,
                true,
                true,
                true,
            ),
            // A bounded admin replacement must still recover an old file rejected by bounded comparison.
            (
                "admin bounded replacement",
                oversized_size,
                true,
                false,
                true,
            ),
            // An oversized old file cannot grant tenant updates the admin-only bypass.
            (
                "tenant oversized replacement",
                oversized_size,
                false,
                true,
                false,
            ),
        ] {
            // Select admin or tenant networking independently for each candidate.
            config.use_admin_network = use_admin_network;

            // Seed exact old-file lengths independently; comparison does not parse previous YAML.
            let directory = tempfile::tempdir()?;
            let hbn_root = directory.path();
            fs::create_dir_all(hbn_root.join("var/support"))?;
            fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
            let startup = hbn_root.join(nvue::PATH);
            let previous = "x".repeat(previous_size);
            fs::write(&startup, &previous)?;

            // Confirm old files above 10 MiB cannot pass the ordinary bounded comparison.
            if previous_size > MAX_EXPECTED_SIZE as usize {
                assert!(
                    read_limited(&startup, MAX_EXPECTED_SIZE).is_err(),
                    "{scenario}"
                );
            }

            // Save the candidate without applying it to a container; these checks prove local file behavior.
            let result = update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                oversized.then_some(oversized_patch.as_str()),
                None,
            )
            .await;

            if should_replace {
                // Read the saved document to prove replacement retained admin configuration at either size.
                assert!(result?, "{scenario}");
                let saved = fs::read_to_string(&startup)?;
                assert_eq!(
                    saved.len() > MAX_EXPECTED_SIZE as usize,
                    oversized,
                    "{scenario}"
                );
                let saved_config: serde_yaml::Value = serde_yaml::from_str(&saved)?;
                let saved_set = &saved_config[1]["set"];
                assert!(saved_set["vrf"]["vpc_1002"].is_mapping(), "{scenario}");
                assert_eq!(
                    saved_set["interface"]["vlan1"]["ip"]["vrf"].as_str(),
                    Some("vpc_1002"),
                    "{scenario}"
                );

                // Compare the complete supplemental value so forced writes cannot silently truncate it.
                if oversized {
                    assert!(
                        saved_set["system"]["hostname"].as_str()
                            == Some(oversized_hostname.as_str()),
                        "{scenario}: complete supplemental hostname must persist"
                    );
                }
            } else {
                // Reject an unforced oversized candidate without changing any previous startup-file content.
                let error = result.expect_err("unforced oversized candidate must be rejected");
                assert!(
                    format!("{error:#}").contains("NVUE configuration exceeds maximum size"),
                    "{scenario}: {error:#}"
                );
                assert_eq!(fs::read_to_string(&startup)?, previous, "{scenario}");
            }
        }
        Ok(())
    }

    /// Supplies stable PXE, NTP, and DNS discovery results to NVUE and DHCP tests so RDNSS and
    /// DHCPv6 can be compared against one agent-local source.
    /// The DHCPv4 server address comes from the managed-host loopback.
    fn test_service_addresses() -> ServiceAddresses {
        ServiceAddresses {
            pxe_ips: vec!["192.0.2.10".parse().unwrap()],
            ntpservers: vec![],
            nameservers: vec![
                "192.0.2.53".parse().unwrap(),
                "2001:db8::53".parse().unwrap(),
                "2001:db8::54".parse().unwrap(),
            ],
        }
    }

    /// Builds one service-interface fixture with caller-selected attachment ID, interface ordinal,
    /// slot index, VPC ID, MAC address, link prefix, and internal UUID so DHCP conversion tests can
    /// change one contract field without rebuilding unrelated RPC data.
    fn service_interface(
        attachment_id: &str,
        interface_ordinal: u32,
        slot_index: u32,
        vpc_id: &str,
        link_prefix: &str,
        mac_address: &str,
        internal_uuid: &str,
    ) -> rpc::ManagedHostServiceInterfaceConfig {
        rpc::ManagedHostServiceInterfaceConfig {
            service_id: "storage-service".to_string(),
            attachment_id: Some(rpc_common::Uuid {
                value: attachment_id.to_string(),
            }),
            interface_ordinal,
            slot_index,
            vpc_id: Some(vpc_id.parse().expect("service VPC ID")),
            link_prefix: link_prefix.to_string(),
            mac_address: mac_address.to_string(),
            internal_uuid: Some(rpc_common::Uuid {
                value: internal_uuid.to_string(),
            }),
            ..Default::default()
        }
    }

    /// Provides two address families sharing one occupied slot plus one empty fixed slot because
    /// DHCP replacement must retain topology independently from reservation occupancy.
    fn service_network_config() -> rpc::ManagedHostNetworkConfigResponse {
        let mut config = netconf(VpcVirtualizationType::Fnn, 32, 24, false, None, true, false);
        config.service_vpc_slot_inventory = Some(rpc::ServiceVpcSlotInventory {
            slots: vec![
                // Slot zero remains empty so fixed-listener retention is observable.
                rpc::ServiceVpcSlot {
                    slot_index: 0,
                    hbn_interface_name: "iface_svc_0".to_string(),
                },
                // Slot one carries both family bindings to exercise shared-listener replacement.
                rpc::ServiceVpcSlot {
                    slot_index: 1,
                    hbn_interface_name: "iface_svc_1".to_string(),
                },
            ],
        });
        config.service_interfaces = vec![
            // The IPv4 endpoint supplies the service reservation that DHCPv4 can answer.
            service_interface(
                "11111111-1111-1111-1111-111111111111",
                0,
                1,
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "192.0.2.10/31",
                "02:00:00:00:00:02", // Higher MAC first to expose sorting by MAC.
                "22222222-2222-2222-2222-222222222222",
            ),
            // The IPv6 endpoint supplies independent family data for the replacement checks.
            service_interface(
                "33333333-3333-3333-3333-333333333333",
                1,
                1,
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "2001:db8::10/127",
                "02:00:00:00:00:01", // Lower MAC second to expose sorting by MAC.
                "44444444-4444-4444-4444-444444444444",
            ),
        ];
        config
    }

    /// Verifies DHCP preparation preserves both tenant FQDNs and combines translated host
    /// listeners, fixed service listeners, and both address families' reservations from one
    /// accepted response so service DHCP coexists with host DHCP.
    #[test]
    fn service_dhcp_projection_combines_host_reservations_and_fixed_listeners() -> eyre::Result<()>
    {
        // The supplied HBN interface name and `d_` translation exercise listener selection; the
        // remaining fields provide complete host and dual-family service data for one replacement.
        let mut config = service_network_config();
        // Use a name unrelated to slot one so DHCP must translate the supplied interface name.
        config
            .service_vpc_slot_inventory
            .as_mut()
            .expect("fixed slot inventory")
            .slots[1]
            .hbn_interface_name = "storage_link".to_string();
        let translation = InterfaceTranslationMode::Prepend("d_".to_string());

        // Prepare the exact model submitted through UpdateAndReloadConfig.
        let prepared = prepare_dhcp_grpc_update(
            &config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            Some(&translation),
        )?
        .expect("combined topology must update rather than stop DHCP");

        // Check both tenant FQDNs and the listener union because translating host interface names
        // must preserve `InterfaceInfo` fields.
        let host_config = prepared.host_config.expect("host DHCP configuration");
        assert_eq!(
            host_config.host_ip_addresses["d_pf0vf0_if"].fqdn,
            config.tenant_interfaces[0].fqdn
        );
        assert_eq!(
            host_config.host_ip_addresses["d_pf0hpf_if"].fqdn,
            config.tenant_interfaces[1].fqdn
        );
        assert_eq!(
            prepared
                .interfaces
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "d_iface_svc_0",
                "d_pf0hpf_if",
                "d_pf0vf0_if",
                "d_storage_link",
            ]
        );

        // Empty slot zero remains explicit, while slot one retains both clients in MAC order.
        let groups = prepared
            .service_config
            .expect("service DHCP replacement")
            .into_interface_groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].receiving_interface, "d_iface_svc_0");
        assert!(groups[0].reservations.is_empty());
        assert_eq!(groups[1].receiving_interface, "d_storage_link");
        let [ipv6, ipv4] = groups[1].reservations.as_slice() else {
            panic!("expected two shared-slot reservations: {:?}", groups[1]);
        };
        assert_eq!(
            ipv6.attachment_id.to_string(),
            "33333333-3333-3333-3333-333333333333"
        );
        assert_eq!(ipv6.interface_ordinal, 1);
        assert_eq!(
            ipv6.internal_uuid.to_string(),
            "44444444-4444-4444-4444-444444444444"
        );
        assert_eq!(ipv6.mac_address.to_string(), "02:00:00:00:00:01");
        assert!(matches!(
            &ipv6.link,
            ServiceLink::Ipv6(link)
                if link.address == "2001:db8::11".parse::<Ipv6Addr>()?
                    && link.prefix == "2001:db8::10/127".parse()?
                    && link.hbn_peer == "2001:db8::10".parse::<Ipv6Addr>()?
        ));
        assert_eq!(
            ipv4.attachment_id.to_string(),
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(ipv4.interface_ordinal, 0);
        assert_eq!(
            ipv4.internal_uuid.to_string(),
            "22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(ipv4.mac_address.to_string(), "02:00:00:00:00:02");
        assert!(matches!(
            &ipv4.link,
            ServiceLink::Ipv4(link)
                if link.address == "192.0.2.11".parse::<Ipv4Addr>()?
                    && link.prefix == "192.0.2.10/31".parse()?
                    && link.gateway == "192.0.2.10".parse::<Ipv4Addr>()?
        ));
        Ok(())
    }

    /// Verifies input order does not affect fixed listeners or reservations, while removal and MAC
    /// changes remain visible to the DHCP server's service-configuration comparison so it can apply
    /// reservation changes without restarting for input order alone.
    #[test]
    fn service_dhcp_projection_normalizes_order_without_hiding_changes() -> eyre::Result<()> {
        // Reverse service interfaces and fixed slots without changing any field values.
        let original = service_network_config();
        let mut reordered = original.clone();
        reordered.service_interfaces.reverse();
        reordered
            .service_vpc_slot_inventory
            .as_mut()
            .expect("fixed slot inventory")
            .slots
            .reverse();

        // Convert both snapshots with the same interface translation.
        let translation = InterfaceTranslationMode::Prepend("d_".to_string());
        let original_projection = build_dhcp_service_config(&original, Some(&translation))?;
        let reordered_projection = build_dhcp_service_config(&reordered, Some(&translation))?;

        // Reordered inputs must produce equal service configurations and receiving-interface lists.
        assert_eq!(original_projection, reordered_projection);

        // Remove one service interface to check the converted configuration changes so the server
        // can retire its previous reservation.
        let mut removed = original.clone();
        removed.service_interfaces.pop();
        assert_ne!(
            build_dhcp_service_config(&removed, Some(&translation))?,
            original_projection
        );

        // Final detach must retain every fixed listener with an explicit empty reservation set.
        let mut detached = original.clone();
        detached.service_interfaces.clear();
        let (detached_config, detached_interfaces) =
            build_dhcp_service_config(&detached, Some(&translation))?;
        assert_eq!(detached_interfaces, ["d_iface_svc_0", "d_iface_svc_1"]);
        let detached_groups = detached_config
            .expect("fixed listeners after final detach")
            .into_interface_groups();
        let [slot_zero, slot_one] = detached_groups.as_slice() else {
            panic!("expected both fixed listeners after detach: {detached_groups:?}");
        };
        assert_eq!(slot_zero.receiving_interface, "d_iface_svc_0");
        assert!(slot_zero.reservations.is_empty());
        assert_eq!(slot_one.receiving_interface, "d_iface_svc_1");
        assert!(slot_one.reservations.is_empty());

        // A MAC-only change must remain observable even when host fields and explicit versions are unchanged.
        let mut changed_mac = original;
        changed_mac.service_interfaces[0].mac_address = "02:00:00:00:00:03".to_string();
        assert_ne!(
            build_dhcp_service_config(&changed_mac, Some(&translation))?,
            original_projection
        );
        Ok(())
    }

    /// Verifies endpoint conversion rejects missing inventory, unknown slot indices, and different
    /// VPC IDs within one slot, preventing DHCP state from diverging from HBN's local VRF
    /// assignment. Also rejects duplicate slot indices and unsupported IPv6 links before address
    /// arithmetic, so malformed Core responses return errors without collapsing slots or panicking.
    /// Empty HBN interface names must fail before translation can hide them.
    #[test]
    fn service_dhcp_projection_rejects_invalid_topology() {
        use carbide_test_support::Outcome::Fails;
        use carbide_test_support::scenarios;

        // Retain endpoints without inventory so conversion cannot silently discard their reservations.
        let mut missing_inventory = service_network_config();
        missing_inventory.service_vpc_slot_inventory = None;

        // Leave an empty supplied name that a nonempty translation prefix would otherwise conceal.
        let mut empty_interface_name = service_network_config();
        empty_interface_name
            .service_vpc_slot_inventory
            .as_mut()
            .expect("fixed slot inventory")
            .slots[0]
            .hbn_interface_name
            .clear();

        // An endpoint outside the complete inventory must not synthesize a listener name.
        let mut unknown_slot = service_network_config();
        unknown_slot.service_interfaces[0].slot_index = 7;

        // Two VPCs cannot share one HBN interface because the interface belongs to one VRF.
        let mut conflicting_vpc = service_network_config();
        conflicting_vpc.service_interfaces[1].vpc_id = Some(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
                .parse()
                .expect("conflicting VPC ID"),
        );

        // Keep listener names distinct and remove endpoints to isolate duplicate slot-index
        // validation.
        let mut duplicate_slot = service_network_config();
        duplicate_slot.service_interfaces.clear();
        duplicate_slot
            .service_vpc_slot_inventory
            .as_mut()
            .expect("fixed slot inventory")
            .slots[1]
            .slot_index = 0;

        // Set the low byte to its maximum so premature next-address arithmetic would overflow.
        let mut unsupported_ipv6_link = service_network_config();
        unsupported_ipv6_link.service_interfaces[1].link_prefix = "2001:db8::ff/128".to_string();

        let translation = InterfaceTranslationMode::Prepend("d_".to_string());
        scenarios!(run = |(config, expected_error)| {
            // Convert the malformed response before submission and require its specific rejection.
            build_dhcp_service_config(&config, Some(&translation))
                .map(|_| ())
                .map_err(|error| {
                    // Require the case-specific error so an unrelated validation failure cannot
                    // satisfy the test.
                    assert!(error.to_string().contains(expected_error), "{error:#}");
                })
        };
            "invalid service topology" {
                // Endpoints without inventory must fail instead of becoming an absent replacement.
                (missing_inventory, "service interfaces require a service VPC slot inventory") => Fails,
                // Validate the supplied name before translation turns it into a nonempty name.
                (empty_interface_name, "slot 0 has an empty HBN interface name") => Fails,
                // A slot index absent from the inventory must fail instead of inferring a listener.
                (unknown_slot, "references unknown slot 7") => Fails,
                // A shared HBN slot cannot accept endpoints assigned to different VPCs.
                (conflicting_vpc, "contains endpoints from VPCs") => Fails,
                // Inventories without endpoints still require unique indices despite distinct names.
                (duplicate_slot, "duplicate service VPC slot index 0") => Fails,
                // Unsupported links must fail before deriving an overflowing client address.
                (unsupported_ipv6_link, "must be a canonical /127") => Fails,
            }
        );
    }

    /// Verifies service-only responses retain fixed listeners without host DHCP and secondary-admin
    /// updates submit them through the control transport, while a legacy secondary-admin response
    /// without service inventory selects StopServer so stale listeners cannot remain active.
    /// Tenant-mode service-only responses must ignore a populated admin interface.
    #[tokio::test]
    async fn service_dhcp_projection_supports_service_only_and_secondary_admin_modes()
    -> eyre::Result<()> {
        use prost::Message;

        // Remove `tenant_interfaces` and `host_interface_id`, retaining the unselected admin
        // interface, managed-host loopback, `remote_id`, and every service slot and endpoint.
        let mut service_only = service_network_config();
        service_only.host_interface_id = None;
        service_only.tenant_interfaces.clear();
        let service_update = prepare_dhcp_grpc_update(
            &service_only,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
        )?
        .expect("service-only topology must update DHCP");
        let expected_service_config = build_dhcp_service_config(&service_only, None)?.0;

        // Service-only operation must omit host configuration while retaining service reservations,
        // fixed listeners, and the DHCPv4 configuration required by the IPv4 reservation.
        assert!(service_update.host_config.is_none());
        assert_eq!(service_update.service_config, expected_service_config);
        assert!(service_update.dhcp_config.ipv4()?.is_some());
        assert_eq!(
            service_update
                .interfaces
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["iface_svc_0", "iface_svc_1"]
        );

        // Configure secondary-admin mode while retaining both host and fixed service inputs.
        let mut secondary_admin = service_network_config();
        secondary_admin.use_admin_network = true;
        secondary_admin.is_primary_dpu = false;

        // Capture the update RPC so a secondary-admin stop cannot satisfy this observation.
        let (requests, mut received_requests) = tokio::sync::mpsc::unbounded_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Router::new().route(
            "/dhcp_server_control.DhcpServerControl/UpdateAndReloadConfig",
            post(move |body: axum::body::Bytes| {
                let requests = requests.clone();
                async move {
                    requests.send(body).expect("capture DHCP control request");
                    crate::tests::respond(())
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let root = tempfile::tempdir()?;

        // Reconcile through update_dhcp to exercise secondary-admin service dispatch.
        let submitted = update_dhcp(
            root.path(),
            &secondary_admin,
            true,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            Some(format!("http://{address}")),
            None,
        )
        .await;
        server.abort();
        let _ = server.await;
        assert!(submitted?, "secondary admin DPU must submit service DHCP");

        // Decode the request to distinguish omitted `host_config` from a present empty host
        // configuration.
        let submitted_request = received_requests
            .try_recv()
            .expect("secondary admin DPU must send UpdateAndReloadConfig");
        let secondary_update =
            crate::dhcp_server_grpc_client::UpdateAndReloadConfigRequest::decode(
                submitted_request.get(5..).expect("gRPC request frame"),
            )?;
        let expected_service_config = build_dhcp_service_config(&secondary_admin, None)?
            .0
            .map(Into::into);

        // Ordinary host DHCP must be suppressed without dropping either fixed service listener.
        assert!(secondary_update.host_config.is_none());
        assert_eq!(secondary_update.service_config, expected_service_config);
        assert_eq!(
            secondary_update
                .interfaces
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["iface_svc_0", "iface_svc_1"]
        );

        // Remove fixed listener inputs to reproduce a legacy secondary-admin Core response.
        let mut legacy_secondary_admin = secondary_admin;
        legacy_secondary_admin.service_interfaces.clear();
        legacy_secondary_admin.service_vpc_slot_inventory = None;
        let legacy_update = prepare_dhcp_grpc_update(
            &legacy_secondary_admin,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
        )?;

        // A legacy secondary-admin response must select StopServer so previously active listeners
        // cannot remain.
        assert!(legacy_update.is_none());
        Ok(())
    }

    /// Verifies a DPU whose managed-host loopback is IPv6 rejects IPv4 service reservations but
    /// retains IPv6-only reservations because IPv4 service replies require an IPv4 DPU loopback.
    /// Secondary-admin service replacements ignore `admin_interface.booturl`, while primary-admin
    /// host DHCP still requires a usable IPv6 PXE address when that field is absent.
    #[test]
    fn ipv6_primary_dpu_accepts_ipv6_but_rejects_ipv4_service_reservations() -> eyre::Result<()> {
        // Retain both service reservations while changing the managed-host loopback to IPv6.
        let mut config = service_network_config();
        config
            .managed_host_config
            .as_mut()
            .expect("managed-host configuration")
            .loopback_ip = "2001:db8::1".to_string();
        let mut service_addresses = test_service_addresses();
        // Supply IPv6 PXE discovery so boot URL validation cannot mask the service-family check.
        service_addresses.pxe_ips.push("2001:db8::80".parse()?);

        // Preparing the control replacement must fail before an unreachable reservation is sent.
        let error =
            prepare_dhcp_grpc_update(&config, &service_addresses, HBNDeviceNames::hbn_23(), None)
                .err()
                .expect("IPv4 service binding without DHCPv4 must be rejected");

        // Report the missing IPv4 DPU loopback rather than a generic listener or link error.
        assert!(
            error
                .to_string()
                .contains("IPv4 service reservations require an IPv4 DPU loopback")
        );

        // Remove only the IPv4 endpoint while preserving the IPv6 endpoint and fixed listeners.
        config.service_interfaces.remove(0);
        let prepared =
            prepare_dhcp_grpc_update(&config, &service_addresses, HBNDeviceNames::hbn_23(), None)?
                .expect("IPv6-only service bindings must remain active");

        // Preparing DHCP with an IPv6 managed-host loopback must retain the IPv6 reservation
        // without enabling DHCPv4.
        assert!(prepared.dhcp_config.ipv4()?.is_none());
        let expected_service_groups = prepared
            .service_config
            .expect("IPv6 service configuration")
            .into_interface_groups();
        let reservations = expected_service_groups
            .iter()
            .flat_map(|group| &group.reservations)
            .collect::<Vec<_>>();
        let [reservation] = reservations.as_slice() else {
            panic!("expected one IPv6 service reservation: {reservations:?}");
        };
        assert!(matches!(&reservation.link, ServiceLink::Ipv6(_)));

        // Select secondary-admin mode to suppress host DHCP while retaining the admin interface.
        config.use_admin_network = true;
        config.is_primary_dpu = false;
        // Omit `admin_interface.booturl` to model an admin interface that would need a generated
        // boot URL if active.
        config
            .admin_interface
            .as_mut()
            .expect("admin interface")
            .booturl = None;
        // Retain only IPv4 PXE discovery while the managed-host loopback is IPv6.
        service_addresses.pxe_ips.retain(IpAddr::is_ipv4);

        // Prepare the same IPv6 service reservations with host DHCP disabled.
        let secondary_update =
            prepare_dhcp_grpc_update(&config, &service_addresses, HBNDeviceNames::hbn_23(), None)?
                .expect("secondary-admin service DHCP must not require an admin boot URL");

        // Preserve both fixed listener names and the IPv6 service reservation while omitting host
        // DHCP and leaving DHCPv4 disabled.
        assert!(secondary_update.host_config.is_none());
        assert!(secondary_update.dhcp_config.ipv4()?.is_none());
        assert_eq!(
            secondary_update
                .service_config
                .expect("secondary-admin service configuration")
                .into_interface_groups(),
            expected_service_groups
        );
        assert_eq!(
            secondary_update
                .interfaces
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["iface_svc_0", "iface_svc_1"]
        );

        // Enable primary-admin host DHCP so the absent boot URL requires an IPv6 PXE address.
        config.is_primary_dpu = true;
        let error =
            prepare_dhcp_grpc_update(&config, &service_addresses, HBNDeviceNames::hbn_23(), None)
                .err()
                .expect("primary-admin host DHCP must require an IPv6 PXE address");

        // Require the missing IPv6 PXE address error for primary-admin host DHCP.
        assert!(error.to_string().contains("no address usable by this DPU"));
        Ok(())
    }

    /// Verifies a known zero-listener topology sends StopServer and retries rejection because an
    /// empty update request does not stop remembered listeners.
    #[tokio::test]
    async fn empty_known_dhcp_topology_retries_explicit_stop() -> eyre::Result<()> {
        // Clear service interfaces and supply an empty slot inventory to distinguish zero slots
        // from omission.
        let mut config = service_network_config();
        config.service_interfaces.clear();
        config.service_vpc_slot_inventory = Some(rpc::ServiceVpcSlotInventory { slots: vec![] });
        // Remove tenant interfaces and the host interface ID while retaining the unselected admin
        // interface. Omit managed-host configuration so StopServer must bypass loopback validation.
        config.managed_host_config = None;
        config.host_interface_id = None;
        config.tenant_interfaces.clear();

        // Reject the first StopServer call and accept the second without changing desired state.
        let attempts = Arc::new(AtomicUsize::new(0));
        let handler_attempts = attempts.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Router::new().route(
            "/dhcp_server_control.DhcpServerControl/StopServer",
            post(move || {
                let attempt = handler_attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt == 0 {
                        return (
                            [
                                ("content-type", "application/grpc"),
                                ("grpc-status", "14"),
                                ("grpc-message", "injected%20stop%20rejection"),
                            ],
                            "",
                        )
                            .into_response();
                    }
                    crate::tests::respond(()).into_response()
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let grpc_address = format!("http://{address}");
        let root = tempfile::tempdir()?;

        // The rejected stop must remain a retryable reconciliation error.
        let error = update_dhcp(
            root.path(),
            &config,
            true,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            Some(grpc_address.clone()),
            None,
        )
        .await
        .expect_err("rejected StopServer must fail reconciliation");
        assert!(format!("{error:#}").contains("injected stop rejection"));

        // The unchanged next iteration must send StopServer again and accept the recovery.
        let changed = update_dhcp(
            root.path(),
            &config,
            true,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            Some(grpc_address),
            None,
        )
        .await?;
        assert!(!changed);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        server.abort();
        let _ = server.await;
        Ok(())
    }

    /// Verifies the legacy file transport rejects populated service-slot inventory even without
    /// endpoints because its host-only files cannot represent service reservations. Accepting those
    /// files would falsely acknowledge service-reservation submission.
    #[tokio::test]
    async fn file_dhcp_transport_rejects_service_topology() -> eyre::Result<()> {
        // Remove endpoints while retaining both service VPC slots to exercise the pre-activation
        // response.
        let mut config = service_network_config();
        config.service_interfaces.clear();
        let root = tempfile::tempdir()?;

        // Invoke the legacy transport before any output files exist.
        let error = update_dhcp(
            root.path(),
            &config,
            true,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await
        .expect_err("file transport must reject fixed service topology");

        // Rejection must report the unsupported topology and precede every host-only file write.
        assert!(
            error
                .to_string()
                .contains("service VPC DHCP topology requires the gRPC control transport")
        );
        assert_eq!(fs::read_dir(root.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn ipv6_only_dhcp_uses_dpu_identity_without_placeholder_ipv4() -> eyre::Result<()> {
        let mut config = netconf(
            VpcVirtualizationType::EthernetVirtualizer,
            32,
            24,
            false,
            None,
            true,
            false,
        );
        let mut addresses = ServiceAddresses {
            pxe_ips: vec!["192.0.2.80".parse()?, "2001:db8::80".parse()?],
            ntpservers: vec![],
            nameservers: vec![],
        };
        config.managed_host_config.as_mut().unwrap().loopback_ip = "2001:db8::1".to_string();
        let dhcp = build_dhcp_server_config(&config, &addresses)?;
        assert_eq!(dhcp.carbide_dhcp_server, None);
        assert_eq!(dhcp.carbide_provisioning_server_ipv4, None);
        assert_eq!(
            dhcp.carbide_provisioning_server_ipv6,
            Some("2001:db8::80".parse()?),
        );
        assert_eq!(
            dhcp.server_identifier()?,
            DhcpV6ServerId::from_remote_id(&config.remote_id)?,
        );
        for (case, loopback, pxe_ips, expected_error) in [
            (
                "missing primary loopback",
                "",
                addresses.pxe_ips.clone(),
                "missing loopback IP",
            ),
            (
                "malformed primary loopback",
                "not-an-address",
                addresses.pxe_ips.clone(),
                "invalid primary loopback IP: not-an-address",
            ),
            (
                "no provisioning addresses",
                "2001:db8::1",
                vec![],
                "PXE/UEFI HTTP boot server has no address usable by this DPU; resolved addresses: []",
            ),
            (
                "no usable provisioning address",
                "2001:db8::1",
                vec!["192.0.2.80".parse()?],
                "PXE/UEFI HTTP boot server has no address usable by this DPU; resolved addresses: [192.0.2.80]",
            ),
        ] {
            config.managed_host_config.as_mut().unwrap().loopback_ip = loopback.to_string();
            addresses.pxe_ips = pxe_ips;
            let error = build_dhcp_server_config(&config, &addresses).expect_err(case);
            assert!(
                format!("{error:#}").contains(expected_error),
                "{case}: {error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn explicit_boot_urls_do_not_require_a_default_ipv6_provisioning_address() -> eyre::Result<()> {
        for (scenario, use_admin_network, tenant_boot_urls, expected_success) in [
            ("admin override", true, vec![], true),
            (
                "every tenant interface has an override",
                false,
                vec![
                    Some("http://[2001:db8::80]/boot.efi"),
                    Some("http://[2001:db8::81]/boot.efi"),
                ],
                true,
            ),
            (
                "one tenant still needs the default",
                false,
                vec![Some("http://[2001:db8::80]/boot.efi"), None],
                false,
            ),
            (
                "empty override disables boot URL generation",
                false,
                vec![Some("")],
                true,
            ),
            (
                "no interfaces need a generated boot URL",
                false,
                vec![],
                true,
            ),
        ] {
            let mut config = netconf(
                VpcVirtualizationType::EthernetVirtualizer,
                32,
                24,
                false,
                None,
                true,
                false,
            );
            config.managed_host_config.as_mut().unwrap().loopback_ip = "2001:db8::1".to_string();
            config.use_admin_network = use_admin_network;
            config.admin_interface.as_mut().unwrap().booturl =
                Some("http://[2001:db8::80]/boot.efi".to_string());
            let interface = config.tenant_interfaces[0].clone();
            config.tenant_interfaces = tenant_boot_urls
                .into_iter()
                .map(|booturl| rpc::FlatInterfaceConfig {
                    booturl: booturl.map(str::to_string),
                    ..interface.clone()
                })
                .collect();
            let result = build_dhcp_server_config(&config, &test_service_addresses());
            if expected_success {
                let dhcp = result.wrap_err(scenario)?;
                assert_eq!(dhcp.ipv4()?, None, "{scenario}");
                assert_eq!(dhcp.carbide_provisioning_server_ipv6, None, "{scenario}");
            } else {
                assert!(
                    format!("{:#}", result.expect_err(scenario))
                        .contains("no address usable by this DPU"),
                    "{scenario}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn persisted_dhcp_identity_does_not_require_readable_yaml() -> eyre::Result<()> {
        let directory = tempfile::tempdir()?;
        let live_config = FPath(directory.path().join("dhcp.yaml"));
        let identity_path = format!("{live_config}.duid");
        let saved = DhcpV6ServerId::try_from(vec![0, 2, 0, 0, 0x16, 0x47, 192, 0, 2, 1])?;
        fs::write(&live_config, "invalid: [")?;
        fs::write(&identity_path, saved.as_bytes())?;
        let mut prepared = PreparedDhcpServerConfig {
            dhcp: DhcpConfig {
                dhcpv6_server_id: Some(DhcpV6ServerId::from_remote_id("test-dpu")?),
                ..Default::default()
            },
            supervisor: String::new(),
            host: String::new(),
        };

        prepared.preserve_server_identifier(&live_config)?;

        assert_eq!(prepared.dhcp.server_identifier()?, saved);
        assert_eq!(fs::read_to_string(&live_config)?, "invalid: [");
        assert_eq!(fs::read(&identity_path)?, saved.as_bytes());

        // A corrupt sidecar must not fall back to otherwise valid YAML.
        fs::write(&live_config, serde_yaml::to_string(&prepared.dhcp)?)?;
        fs::write(&identity_path, b"invalid")?;
        let error = prepared
            .preserve_server_identifier(&live_config)
            .unwrap_err();
        assert!(format!("{error:#}").contains(&identity_path));
        assert!(
            error
                .downcast_ref::<carbide_rpc_utils::dhcp::DhcpDataError>()
                .is_some()
        );
        assert_eq!(fs::read(&identity_path)?, b"invalid");
        Ok(())
    }

    /// DHCP validation and reload failures must preserve configuration so a compatible binary can
    /// retry the update, including after a restart. An explicitly empty service inventory must
    /// still reload IPv4 files with a binary that lacks validation support.
    #[tokio::test]
    async fn ipv6_only_file_update_waits_for_compatible_binary() -> eyre::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        if std::env::var_os("DHCP_UPDATE_TEST_CHILD").is_none() {
            // Create `crictl` only after entering a process running this test
            // alone. Otherwise, an unrelated test can spawn while `crictl` is
            // open for writing and retain that descriptor until exec, making
            // our next execution fail with `ETXTBSY` after we close the file.
            let child_log = tempfile::NamedTempFile::new()?;
            let mut command = TokioCommand::new(std::env::current_exe()?);
            command
                .args([
                    "--exact",
                    "ethernet_virtualization::tests::ipv6_only_file_update_waits_for_compatible_binary",
                    "--nocapture",
                ])
                .env("DHCP_UPDATE_TEST_CHILD", "true")
                .stdout(child_log.as_file().try_clone()?)
                .stderr(child_log.as_file().try_clone()?)
                .kill_on_drop(true);

            // The argument check and nine stages each allow 30 seconds. Leave
            // another 30 seconds for setup and cleanup in the isolated child.
            // Keep output outside the timed future so a timeout retains the
            // child's diagnostics as well as reporting that it timed out.
            let status = timeout(Duration::from_secs(330), command.status()).await;
            let output = fs::read(child_log.path())
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|error| format!("could not read child output: {error}"));
            let status = status
                .wrap_err_with(|| format!("isolated DHCP update test timed out:\n{output}"))?
                .wrap_err_with(|| format!("isolated DHCP update test could not run:\n{output}"))?;

            assert!(
                status.success() && output.contains("1 passed; 0 failed"),
                "isolated DHCP update test failed ({status}):\n{output}",
            );
            return Ok(());
        }

        if let Some(root) = std::env::var_os("DHCP_UPDATE_TEST_ROOT") {
            let root = PathBuf::from(root);
            let stage = std::env::var("DHCP_UPDATE_TEST_STAGE")?;
            let mut config = netconf(
                VpcVirtualizationType::EthernetVirtualizer,
                32,
                24,
                false,
                None,
                true,
                false,
            );
            // Advertise zero slots in the successful IPv4 stage to exercise legacy file compatibility.
            if stage == "ipv4-old-binary" {
                config.service_vpc_slot_inventory =
                    Some(rpc::ServiceVpcSlotInventory { slots: vec![] });
            } else {
                config.managed_host_config.as_mut().unwrap().loopback_ip =
                    "2001:db8::1".to_string();
            }
            if stage.starts_with("stop-") {
                config.use_admin_network = true;
                config.is_primary_dpu = false;
                config.managed_host_config = None;
                config.remote_id.clear();
            }
            let addresses = ServiceAddresses {
                pxe_ips: vec!["192.0.2.80".parse()?, "2001:db8::80".parse()?],
                ntpservers: vec![],
                nameservers: vec![],
            };
            let result = update_dhcp(
                &root,
                &config,
                stage == "saved-before-reload",
                &addresses,
                HBNDeviceNames::pre_23(),
                None,
                None,
            )
            .await;
            match stage.as_str() {
                "old-binary" => assert!(
                    format!(
                        "{:#}",
                        result.expect_err("old binary should reject validation")
                    )
                    .contains("unexpected argument")
                ),
                "write-failure" => {
                    let error = format!(
                        "{:#}",
                        result.expect_err("blocked temporary file should fail the update")
                    );
                    assert!(error.contains("fs::write"));
                    assert!(!error.contains("restore DHCP files"));
                }
                "reload-failure" => assert!(
                    format!("{:#}", result.expect_err("reload should fail"))
                        .contains("injected reload failure")
                ),
                "stop-failure" => assert!(
                    format!("{:#}", result.expect_err("stop should fail"))
                        .contains("injected stop failure")
                ),
                "saved-before-reload" | "retry" | "ipv4-old-binary" | "stop-old-binary" => {
                    assert!(result?)
                }
                "unchanged" => assert!(!result?),
                _ => panic!("unexpected DHCP update stage: {stage}"),
            }
            return Ok(());
        }

        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let legacy = build_dhcp_server_config(
            &netconf(
                VpcVirtualizationType::EthernetVirtualizer,
                32,
                24,
                false,
                None,
                true,
                false,
            ),
            &test_service_addresses(),
        )?;
        let previous = [
            (dhcp::RELAY_PATH, "old relay".to_string()),
            (dhcp::RELAY_PATH_NVUE, "old NVUE relay".to_string()),
            (dhcp::SERVER_PATH, "old supervisor".to_string()),
            (dhcp::SERVER_CONFIG_PATH, serde_yaml::to_string(&legacy)?),
            (dhcp::SERVER_HOST_CONFIG_PATH, "old host config".to_string()),
        ];
        for (path, contents) in &previous {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, contents)?;
        }
        fs::copy(
            root.join(dhcp::SERVER_CONFIG_PATH),
            root.join("previous-dhcp.yaml"),
        )?;
        fs::create_dir(root.join("bin"))?;
        let crictl = root.join("bin/crictl");
        fs::write(
            &crictl,
            r#"#!/bin/sh
set -eu
root="$DHCP_UPDATE_TEST_ROOT"
if [ "$DHCP_UPDATE_TEST_STAGE" = saved-before-reload ]; then
    printf '%s\n' 'unexpected crictl call' >> "$root/commands"
    printf '%s\n' 'saving configuration must not call crictl' >&2
    exit 1
fi
if [ "$*" = 'ps --name=doca-hbn -o=json' ]; then
    printf '%s\n' '{"containers":[{"id":"test-hbn"}]}'
    exit 0
fi
[ "$1" = exec ] || exit 1
[ "$2" = test-hbn ] || exit 1
shift 2
case "$1" in
    /var/support/forge-dhcp/bin/forge-dhcp-server)
        [ "$#" = 7 ] || exit 1
        [ "$2" = --validate-config ] || exit 1
        [ "$4" = --host-config ] || exit 1
        [ "$6" = --dhcp-config ] || exit 1
        [ "$7" = /var/support/forge-dhcp/conf/dhcp.yaml ] || exit 1
        [ -f "$root$3" ] || exit 1
        [ -f "$root$5" ] || exit 1
        [ -f "$root$7" ] || exit 1
        printf '%s\n' validate >> "$root/commands"
        case "$DHCP_UPDATE_TEST_STAGE" in
            old-binary|write-failure|reload-failure)
                cmp -s "$root$7" "$root/previous-dhcp.yaml" || exit 1 ;;
        esac
        case "$DHCP_UPDATE_TEST_STAGE" in
            *old-binary)
                printf '%s\n' 'unexpected argument --validate-config' >&2
                exit 2 ;;
        esac
        cp "$root$3" "$root/validated-dhcp.yaml"
        cp "$root$5" "$root/validated-host.yaml" ;;
    bash)
        [ "$#" = 3 ] || exit 1
        [ "$2" = -c ] || exit 1
        [ "$3" = "$DHCP_UPDATE_EXPECTED_COMMAND" ] || exit 1
        [ -f "$root/var/support/forge-dhcp/conf/dhcp.PENDING" ] || exit 1
        case "$DHCP_UPDATE_TEST_STAGE" in
            stop-*) printf '%s\n' stop >> "$root/commands" ;;
            ipv4-old-binary) printf '%s\n' reload >> "$root/commands" ;;
            *)
                cmp -s "$root/validated-dhcp.yaml" "$root/var/support/forge-dhcp/conf/dhcp.yaml" || exit 1
                cmp -s "$root/validated-host.yaml" "$root/var/support/forge-dhcp/conf/host.yaml" || exit 1
                printf '%s\n' reload >> "$root/commands" ;;
        esac
        if [ "$DHCP_UPDATE_TEST_STAGE" = reload-failure ]; then
            printf '%s\n' 'injected reload failure' >&2
            exit 1
        elif [ "$DHCP_UPDATE_TEST_STAGE" = stop-failure ]; then
            printf '%s\n' 'injected stop failure' >&2
            exit 1
        fi ;;
    *) exit 1 ;;
esac
"#,
        )?;
        fs::set_permissions(&crictl, fs::Permissions::from_mode(0o755))?;
        let inherited_path =
            std::env::var_os("PATH").ok_or_else(|| eyre::eyre!("missing test PATH"))?;
        let path = std::env::join_paths(
            std::iter::once(root.join("bin")).chain(std::env::split_paths(&inherited_path)),
        )?;
        // An incorrect early argument must fail even when all later file
        // checks pass; shell `set -e` alone does not enforce an AND list.
        let mut invalid_command = TokioCommand::new(&crictl);
        invalid_command
            .args([
                "exec",
                "test-hbn",
                "/var/support/forge-dhcp/bin/forge-dhcp-server",
                "--wrong-validation-flag",
                "/previous-dhcp.yaml",
                "--host-config",
                "/previous-dhcp.yaml",
                "--dhcp-config",
                "/var/support/forge-dhcp/conf/dhcp.yaml",
            ])
            .env("DHCP_UPDATE_TEST_ROOT", root)
            .env("DHCP_UPDATE_TEST_STAGE", "check-arguments")
            .kill_on_drop(true);
        let output = timeout(Duration::from_secs(30), invalid_command.output()).await??;
        assert!(
            !output.status.success(),
            "fake accepted the wrong validation flag"
        );
        let pending_apply = FPath(root.join(dhcp::SERVER_CONFIG_PATH)).with_ext("PENDING");
        for stage in [
            "old-binary",
            "write-failure",
            "reload-failure",
            "saved-before-reload",
            "retry",
            "unchanged",
            "ipv4-old-binary",
            "stop-failure",
            "stop-old-binary",
        ] {
            fs::write(root.join("commands"), "")?;
            if stage == "saved-before-reload" {
                // Earlier failed applications deliberately retain the marker.
                // Remove it so this stage proves write-only mode creates one.
                fs::remove_file(&pending_apply)?;
                assert!(!pending_apply.exists());
            }
            let blocked_temp = FPath(root.join(dhcp::SERVER_HOST_CONFIG_PATH)).temp();
            if stage == "write-failure" {
                fs::create_dir(&blocked_temp)?;
            }
            if stage == "stop-failure" {
                fs::write(
                    root.join(format!("{}.duid", dhcp::SERVER_CONFIG_PATH)),
                    b"invalid identity",
                )?;
            }
            let before_stop = if stage.starts_with("stop-") {
                Some((
                    fs::read(root.join(dhcp::SERVER_CONFIG_PATH))?,
                    fs::read(root.join(dhcp::SERVER_HOST_CONFIG_PATH))?,
                    fs::read(root.join(dhcp::SERVER_PATH))?,
                ))
            } else {
                None
            };
            let mut command = TokioCommand::new(std::env::current_exe()?);
            command.args(["--exact", "ethernet_virtualization::tests::ipv6_only_file_update_waits_for_compatible_binary", "--nocapture"])
                .env("DHCP_UPDATE_TEST_ROOT", root)
                .env("DHCP_UPDATE_TEST_STAGE", stage)
                .env("DHCP_UPDATE_EXPECTED_COMMAND", if stage.starts_with("stop-") { dhcp::STOP_DHCP_SERVER } else { dhcp::RELOAD_DHCP_SERVER })
                .env("IGNORE_MGMT_VRF", "true")
                .env("PATH", &path)
                .kill_on_drop(true);
            let output = timeout(Duration::from_secs(30), command.output()).await??;
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
                "DHCP stage {stage} failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if matches!(stage, "old-binary" | "write-failure" | "reload-failure") {
                for (path, contents) in &previous {
                    assert_eq!(
                        &fs::read_to_string(root.join(path))?,
                        contents,
                        "{stage}: {path}"
                    );
                }
            }
            if stage == "write-failure" {
                fs::remove_dir(blocked_temp)?;
            }
            assert_eq!(
                pending_apply.exists(),
                matches!(
                    stage,
                    "write-failure" | "reload-failure" | "saved-before-reload" | "stop-failure"
                ),
                "{stage}"
            );
            let expected_commands = match stage {
                "old-binary" | "write-failure" => "validate\n",
                "reload-failure" | "retry" => "validate\nreload\n",
                "saved-before-reload" | "unchanged" => "",
                "ipv4-old-binary" => "reload\n",
                "stop-failure" | "stop-old-binary" => "stop\n",
                _ => unreachable!(),
            };
            assert_eq!(
                fs::read_to_string(root.join("commands"))?,
                expected_commands,
                "{stage}"
            );
            if let Some((dhcp_config, host_config, supervisor)) = before_stop {
                assert_eq!(fs::read(root.join(dhcp::SERVER_CONFIG_PATH))?, dhcp_config);
                assert_eq!(
                    fs::read(root.join(dhcp::SERVER_HOST_CONFIG_PATH))?,
                    host_config
                );
                assert_eq!(
                    fs::read(root.join(format!("{}.duid", dhcp::SERVER_CONFIG_PATH)))?,
                    b"invalid identity"
                );
                if stage == "stop-failure" {
                    assert_eq!(fs::read(root.join(dhcp::SERVER_PATH))?, supervisor);
                } else {
                    let supervisor = fs::read_to_string(root.join(dhcp::SERVER_PATH))?;
                    assert!(supervisor.contains("autostart = false"));
                    assert!(supervisor.contains("autorestart = false"));
                }
            }
            if stage == "saved-before-reload" {
                // The next process sees matching live files, but still owes a reload.
                assert!(pending_apply.exists());
                let saved: DhcpConfig = serde_yaml::from_str(&fs::read_to_string(
                    root.join(dhcp::SERVER_CONFIG_PATH),
                )?)?;
                assert_eq!(saved.ipv4()?, None);
                assert_eq!(saved.server_identifier()?, legacy.server_identifier()?);
                assert!(!root.join(dhcp::RELAY_PATH_NVUE).exists());
            } else if stage == "ipv4-old-binary" {
                let saved: DhcpConfig = serde_yaml::from_str(&fs::read_to_string(
                    root.join(dhcp::SERVER_CONFIG_PATH),
                )?)?;
                assert!(saved.ipv4()?.is_some());
            }
        }
        Ok(())
    }

    /// Provides matching canonical and deprecated dual-stack admin projections.
    ///
    /// Keeping both projections realistic makes mapping and reconciliation
    /// tests detect accidental reuse of the host `/128` as the segment CIDR.
    #[allow(deprecated)]
    fn dual_stack_admin_interface() -> rpc::FlatInterfaceConfig {
        rpc::FlatInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical.into(),
            vlan_id: 123,
            vni: 5555,
            vpc_vni: 7777,
            gateway: Some("10.217.4.65/26".to_string()),
            ip: Some("10.217.4.70".to_string()),
            interface_prefix: Some("10.217.4.70/32".to_string()),
            prefix: Some("10.217.4.64/26".to_string()),
            svi_ip: Some("10.217.4.66/26".to_string()),
            is_l2_segment: true,
            ipv6_interface_config: Some(rpc::FlatInterfaceIpv6Config {
                ip: "2001:db8:100::70".to_string(),
                interface_prefix: "2001:db8:100::70/128".to_string(),
                svi_ip: Some("2001:db8:100::66/64".to_string()),
            }),
            addresses: vec![
                rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V4.into(),
                    gateway: Some("10.217.4.65/26".to_string()),
                    ip: "10.217.4.70".to_string(),
                    interface_prefix: "10.217.4.70/32".to_string(),
                    prefix: "10.217.4.64/26".to_string(),
                    svi_ip: Some("10.217.4.66/26".to_string()),
                    ..Default::default()
                },
                rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V6.into(),
                    ip: "2001:db8:100::70".to_string(),
                    interface_prefix: "2001:db8:100::70/128".to_string(),
                    prefix: "2001:db8:100::/64".to_string(),
                    svi_ip: Some("2001:db8:100::66/64".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    /// Verifies managed-host loopbacks preserve optional IPv6 and reject invalid families.
    ///
    /// This keeps family validation at the NVUE rendering boundary.
    #[test]
    fn test_parse_managed_host_loopback_ips() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::scenarios;

        scenarios!(run = |config: rpc::ManagedHostNetworkConfig| {
            parse_managed_host_loopback_ips(&config).map_err(drop)
        };
            "valid wire values" {
                rpc::ManagedHostNetworkConfig {
                    loopback_ip: "10.0.0.1".to_string(),
                    loopback_ip_v6: None,
                    quarantine_state: None,
                } => Yields(("10.0.0.1".parse().unwrap(), None)),
                rpc::ManagedHostNetworkConfig {
                    loopback_ip: "10.0.0.1".to_string(),
                    loopback_ip_v6: Some("2001:db8::1".to_string()),
                    quarantine_state: None,
                } => Yields((
                    "10.0.0.1".parse().unwrap(),
                    Some("2001:db8::1".parse().unwrap()),
                )),
            }

            "invalid IPv6 wire value" {
                rpc::ManagedHostNetworkConfig {
                    loopback_ip: "10.0.0.1".to_string(),
                    loopback_ip_v6: Some("not-an-ipv6-address".to_string()),
                    quarantine_state: None,
                } => Fails,
                rpc::ManagedHostNetworkConfig {
                    loopback_ip: "10.0.0.1".to_string(),
                    loopback_ip_v6: Some("192.0.2.1".to_string()),
                    quarantine_state: None,
                } => Fails,
            }

            "invalid primary loopback" {
                rpc::ManagedHostNetworkConfig {
                    loopback_ip: String::new(),
                    loopback_ip_v6: None,
                    quarantine_state: None,
                } => Fails,
                rpc::ManagedHostNetworkConfig {
                    loopback_ip: "not-an-ip-address".to_string(),
                    loopback_ip_v6: None,
                    quarantine_state: None,
                } => Fails,
            }
        );
    }

    /// Presence, including an empty list, makes Core's resolved FNN policy
    /// authoritative. Only an older Core's absent field falls back to an
    /// aggregated legacy site-prefix list, while ETV retains the original list.
    #[test]
    fn site_isolation_prefixes_honor_resolved_fnn_policy_presence() {
        use carbide_test_support::Outcome::Yields;
        use carbide_test_support::scenarios;

        // Nested and adjacent legacy roots reveal whether fallback aggregation
        // is incorrectly applied to authoritative operator policy.
        let legacy = vec![
            "10.0.0.0/9".to_string(),
            "10.128.0.0/9".to_string(),
            "10.2.0.0/24".to_string(),
        ];
        let explicit = vec!["10.0.0.0/8".to_string(), "10.2.0.0/24".to_string()];

        // Keep the legacy source fixed and compare the selected policy before NVUE renders it.
        scenarios!(
            run = |(virtualization_type, null_routes): (_, Option<Vec<String>>)| {
                let config = rpc::ManagedHostNetworkConfigResponse {
                    site_fabric_prefixes: legacy.clone(),
                    site_fabric_null_routes: null_routes.map(|items| rpc_common::StringList { items }),
                    ..Default::default()
                };
                site_isolation_prefixes_for_rendering(virtualization_type, &config)
                    .map_err(|error| error.to_string())
            };
            "explicit FNN routes preserve boundaries" {
                // A child boundary is stronger policy than its parent and must survive.
                (VpcVirtualizationType::Fnn, Some(explicit.clone())) => Yields(explicit),
            }
            "explicit empty FNN routes disable fallback" {
                // Present-but-empty disables isolation routes instead of inheriting roots.
                (VpcVirtualizationType::Fnn, Some(vec![])) => Yields(vec![]),
            }
            "old Core FNN response uses legacy fallback" {
                // An absent field identifies old Core and requires the legacy fallback.
                (VpcVirtualizationType::Fnn, None) => Yields(vec!["10.0.0.0/8".to_string()]),
            }
            "ETV ignores the FNN-only field" {
                // ETV still consumes its legacy ACL list even if the new field is present.
                (
                    VpcVirtualizationType::EthernetVirtualizer,
                    Some(vec!["203.0.113.0/24".to_string()]),
                ) => Yields(legacy.clone()),
            }
        );
    }

    /// Verifies site NTP overrides replace service fallbacks per family independently.
    #[test]
    fn test_build_dhcp_ntp_servers() {
        use carbide_test_support::value_scenarios;

        let service_addrs = ServiceAddresses {
            pxe_ips: vec![],
            ntpservers: vec![
                IpAddr::from([192, 0, 2, 20]),
                "2001:db8::20".parse().unwrap(),
            ],
            nameservers: vec![],
        };

        value_scenarios!(run = |ntp_servers: Vec<String>| {
                let nc = rpc::ManagedHostNetworkConfigResponse {
                    ntp_servers,
                    ..Default::default()
                };
                build_dhcp_ntp_servers(&nc, &service_addrs)
            };
            "configured overrides" {
                // The supported site IPv4 value replaces only its service fallback.
                vec!["198.51.100.1".to_string()] => (
                    vec![Ipv4Addr::from([198, 51, 100, 1])],
                    vec!["2001:db8::20".parse::<Ipv6Addr>().unwrap()],
                ),
                // The site IPv6 value replaces only its DHCPv6 fallback.
                vec!["2001:db8::99".to_string()] => (
                    vec![Ipv4Addr::from([192, 0, 2, 20])],
                    vec!["2001:db8::99".parse::<Ipv6Addr>().unwrap()],
                ),
                // Both site families override their respective fallbacks.
                vec!["198.51.100.1".to_string(), "2001:db8::99".to_string()] => (
                    vec![Ipv4Addr::from([198, 51, 100, 1])],
                    vec!["2001:db8::99".parse::<Ipv6Addr>().unwrap()],
                ),
            }
            "empty configuration fallback" {
                // With no site values, both service-provided families survive.
                vec![] => (
                    vec![Ipv4Addr::from([192, 0, 2, 20])],
                    vec!["2001:db8::20".parse::<Ipv6Addr>().unwrap()],
                ),
            }
            "invalid-address fallback" {
                // An invalid site value retains both service-provided families.
                vec!["not-an-ip".to_string()] => (
                    vec![Ipv4Addr::from([192, 0, 2, 20])],
                    vec!["2001:db8::20".parse::<Ipv6Addr>().unwrap()],
                ),
            }
        );
    }

    #[test]
    fn test_hostname() -> Result<(), Box<dyn std::error::Error>> {
        let syscall_h = super::hostname()?;
        match std::env::var("HOSTNAME") {
            Ok(env_h) => assert_eq!(
                syscall_h.fqdn, env_h,
                "libc::gethostname output should match shell's $HOSTNAME"
            ),
            Err(_) => tracing::debug!("Env var $HOSTNAME missing, skipping test, not important"),
        }
        Ok(())
    }

    /// Adds one fixed service slot with complete selected-VPC state so renderer tests exercise the
    /// same response contract accepted by the periodic fetcher.
    #[allow(deprecated)]
    fn add_service_slot(
        config: &mut rpc::ManagedHostNetworkConfigResponse,
        link_prefixes: &[&str],
        overlapping_host_interface: Option<usize>,
        network_security_group: Option<rpc::FlatInterfaceNetworkSecurityGroupConfig>,
        leak_default_route_from_underlay: bool,
    ) {
        // Reuse shared host VPC state when requested; otherwise construct one service-only VPC.
        let (
            vpc_vni,
            vpc_prefixes,
            vpc_peer_prefixes,
            vpc_peer_vnis,
            mut routing_profile,
            loopback,
        ) = if let Some(index) = overlapping_host_interface {
            let host = &config.tenant_interfaces[index];
            (
                host.vpc_vni,
                host.vpc_prefixes.clone(),
                host.vpc_peer_prefixes.clone(),
                host.vpc_peer_vnis.clone(),
                host.vpc_routing_profile.clone().unwrap(),
                host.tenant_vrf_loopback_ip.clone(),
            )
        } else {
            (
                1_500_001,
                vec!["192.0.2.0/24".to_string(), "2001:db8:100::/48".to_string()],
                vec!["198.51.100.0/24".to_string()],
                vec![1_500_002],
                rpc::RoutingProfile::default(),
                Some("10.217.5.200".to_string()),
            )
        };
        routing_profile.leak_default_route_from_underlay = leak_default_route_from_underlay;

        // Install the complete fixed inventory before adding endpoints that reference its slot.
        config.network_virtualization_type = Some(rpc::VpcVirtualizationType::Fnn.into());
        config.site_global_vpc_vni = None;
        config.vpc_peer_vnis_authoritative = true;
        config.service_vpc_slot_inventory = Some(rpc::ServiceVpcSlotInventory {
            slots: vec![rpc::ServiceVpcSlot {
                slot_index: 0,
                hbn_interface_name: "iface_svc_0".to_string(),
            }],
        });

        // Give every requested link distinct stable identities while repeating shared VPC policy.
        config.service_interfaces = link_prefixes
            .iter()
            .enumerate()
            .map(|(index, link_prefix)| {
                let identity = index + 1;
                rpc::ManagedHostServiceInterfaceConfig {
                    service_id: format!("10000000-0000-0000-0000-{identity:012}"),
                    attachment_id: Some(::rpc::common::Uuid {
                        value: format!("20000000-0000-0000-0000-{identity:012}"),
                    }),
                    interface_ordinal: 0,
                    slot_index: 0, // Bind every endpoint to the fixed inventory slot above.
                    vpc_id: Some("30000000-0000-0000-0000-000000000001".parse().unwrap()),
                    vpc_vni,
                    vpc_prefixes: vpc_prefixes.clone(),
                    vpc_peer_prefixes: vpc_peer_prefixes.clone(),
                    vpc_peer_vnis: vpc_peer_vnis.clone(),
                    vpc_routing_profile: Some(routing_profile.clone()),
                    interface_routing_profile: None,
                    network_security_group: network_security_group.clone(),
                    mtu: Some(9_000), // Match the fixed DPF/CNI service MTU.
                    link_prefix: (*link_prefix).to_string(),
                    mac_address: format!("02:00:00:00:00:{identity:02x}"),
                    internal_uuid: Some(::rpc::common::Uuid {
                        value: format!("40000000-0000-0000-0000-{identity:012}"),
                    }),
                    tenant_vrf_loopback_ip: loopback.clone(),
                }
            })
            .collect();
    }

    /// Builds a VPC-sourced NSG so callers can attach its empty form to exercise default deny
    /// rather than the no-NSG fallback.
    fn service_network_security_group(
        id: &str,
        rules: Vec<rpc::ResolvedNetworkSecurityGroupRule>,
    ) -> rpc::FlatInterfaceNetworkSecurityGroupConfig {
        rpc::FlatInterfaceNetworkSecurityGroupConfig {
            id: id.to_string(),
            version: "V1-T1791792000000000".to_string(),
            source: rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
            rules,
            stateful_egress: false,
        }
    }

    /// Renders one complete response through the startup-file path so service assertions inspect
    /// the same YAML written for HBN rather than an intermediate Rust model.
    async fn render_startup_yaml(
        config: &rpc::ManagedHostNetworkConfigResponse,
    ) -> eyre::Result<String> {
        // Create only the HBN paths that the startup-file renderer writes.
        let tempdir = tempfile::tempdir()?;
        let hbn_root = tempdir.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // Render without posting to NVUE so assertions can inspect deterministic desired state.
        update_nvue(
            VpcVirtualizationType::Fnn,
            NvueUpdateFlavor::StartupFile {
                hbn_root,
                skip_post: true,
            },
            config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;

        // Read the exact file that a later live apply would submit.
        fs::read_to_string(hbn_root.join(nvue::PATH)).map_err(Into::into)
    }

    /// Collects one port's rendered ACL references so policy tests can compare wiring without
    /// depending on a generated NSG index.
    fn rendered_acl_names(
        documents: &serde_yaml::Value,
        interface: &str,
    ) -> std::collections::BTreeSet<String> {
        documents.as_sequence().expect("two YAML documents")[1]["set"]["interface"][interface]
            ["acl"]
            .as_mapping()
            .into_iter()
            .flatten()
            .filter_map(|(name, _)| name.as_str().map(str::to_string))
            .collect()
    }

    /// Proves an interrupted service-bearing NVUE apply remains unapplied and is retried from the
    /// saved desired YAML after an agent restart.
    #[tokio::test]
    async fn startup_file_retries_after_interrupted_apply() -> eyre::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        // Separate processes model a restart while sharing the saved YAML and
        // fake NVUE state. Only each child's PATH points at the fake `crictl`.
        if let Some(root) = std::env::var_os("NVUE_STARTUP_TEST_ROOT") {
            let root = PathBuf::from(root);
            let stage = std::env::var("NVUE_STARTUP_TEST_STAGE")?;
            let virtualization_type = VpcVirtualizationType::Fnn;
            let mut network_config = netconf(virtualization_type, 32, 24, false, None, true, false);
            add_service_slot(
                &mut network_config,
                &["192.0.2.10/31", "192.0.2.12/31"],
                None,
                None,
                false,
            );
            let update = async |skip_post| {
                super::update_nvue(
                    virtualization_type,
                    NvueUpdateFlavor::StartupFile {
                        hbn_root: &root,
                        skip_post,
                    },
                    &network_config,
                    &test_service_addresses(),
                    HBNDeviceNames::hbn_23(),
                    None,
                    None,
                )
                .await
            };

            match stage.as_str() {
                "save" => {
                    assert!(update(true).await?);
                    assert!(!update(true).await?);
                }
                "failure" => {
                    let error = update(false).await.expect_err("live apply should fail");
                    assert!(format!("{error:#}").contains("injected apply failure"));
                }
                "retry" => assert!(update(false).await?),
                "unchanged" => assert!(!update(false).await?),
                _ => panic!("unexpected StartupFile test stage: {stage}"),
            }
            return Ok(());
        }

        // Build the shared fake HBN filesystem and crictl shim used by every process stage.
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir_all(root.join("var/support"))?;
        fs::create_dir_all(root.join("etc/cumulus/acl/policy.d"))?;
        fs::create_dir(root.join("bin"))?;
        let crictl = root.join("bin/crictl");
        fs::write(
            &crictl,
            r#"#!/bin/sh
set -eu
root="$NVUE_STARTUP_TEST_ROOT"
if [ "$*" = 'ps --name=doca-hbn -o=json' ]; then
    printf '%s\n' '{"containers":[{"id":"test-hbn"}]}'
    exit 0
fi
[ "$1" = exec ] && [ "$2" = test-hbn ]
shift 2
printf '%s\n' "$*" >> "$root/commands"
case "$*" in
    'nv config replace /var/support/nvue_startup.yaml')
        cp "$root/var/support/nvue_startup.yaml" "$root/pending.yaml" ;;
    'nv config diff')
        if ! cmp -s "$root/pending.yaml" "$root/applied.yaml"; then
            printf '%s\n' 'configuration changed'
        fi ;;
    'nv config apply -y')
        if [ "$NVUE_STARTUP_TEST_STAGE" = failure ]; then
            printf '%s\n' 'injected apply failure' >&2
            exit 1
        fi
        cp "$root/pending.yaml" "$root/applied.yaml" ;;
    'nv config detach') rm "$root/pending.yaml" ;;
    'supervisorctl restart nl2doca') ;;
    *) printf 'unexpected command: %s\n' "$*" >&2; exit 1 ;;
esac
"#,
        )?;
        fs::set_permissions(&crictl, fs::Permissions::from_mode(0o755))?;

        // Stop after saving the desired file, while NVUE still has the old
        // configuration. This is the state left by an interrupted update.
        fs::write(root.join("applied.yaml"), "previous configuration")?;
        run_startup_file_attempt(root, "save").await?;
        assert!(!root.join("commands").exists(), "skip_reload ran a command");
        let desired = fs::read_to_string(root.join(nvue::PATH))?;
        assert!(
            desired.contains("iface_svc_0:")
                && desired.contains("192.0.2.10/31: {}")
                && desired.contains("192.0.2.12/31: {}"),
            "saved desired state must include the grouped service port"
        );

        // Fail the first live apply so the previous NVUE state and saved desired YAML remain
        // observable.
        run_startup_file_attempt(root, "failure").await?;
        assert_eq!(
            fs::read_to_string(root.join("applied.yaml"))?,
            "previous configuration"
        );
        assert_eq!(
            fs::read_to_string(FPath(root.join(nvue::PATH)).with_ext("error"))?,
            desired
        );
        let attempted_apply = "nv config replace /var/support/nvue_startup.yaml\nnv config diff\nnv config apply -y\n";
        assert_eq!(fs::read_to_string(root.join("commands"))?, attempted_apply);

        // Retry the saved desired YAML to confirm apply precedes the HBN restart.
        run_startup_file_attempt(root, "retry").await?;
        assert_eq!(fs::read_to_string(root.join("applied.yaml"))?, desired);
        let successful_retry =
            format!("{attempted_apply}{attempted_apply}supervisorctl restart nl2doca\n");
        assert_eq!(fs::read_to_string(root.join("commands"))?, successful_retry);

        // Reconcile the applied YAML again to confirm unchanged state detaches without a restart.
        run_startup_file_attempt(root, "unchanged").await?;
        assert_eq!(fs::read_to_string(root.join("applied.yaml"))?, desired);
        assert_eq!(
            fs::read_to_string(root.join("commands"))?,
            format!(
                "{successful_retry}nv config replace /var/support/nvue_startup.yaml\nnv config diff\nnv config detach\n"
            )
        );
        assert!(!root.join("pending.yaml").exists());
        Ok(())
    }

    /// Verifies grouped endpoint removal preserves the host port, shared VRF, BGP neighbor, and host
    /// loopback, accepts a later service loopback for that VRF, and removes that loopback when the
    /// service endpoints depart.
    /// Also verifies MTU omission so service ports follow existing host-port rendering.
    /// Equivalent conditional target orders on host and service ports must not cause a state mismatch.
    #[tokio::test]
    #[allow(deprecated)]
    async fn groups_service_endpoints_and_preserves_shared_vpc_state_on_removal() -> eyre::Result<()>
    {
        // Build one host VPC with three hidden endpoints sharing its fixed service slot.
        let mut config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        // Add the IPv6 link's containing VPC prefix and a host loopback so validation sees one
        // shared VPC and later removals can distinguish host and service loopbacks.
        config.tenant_interfaces[1]
            .vpc_prefixes
            .push("2001:db8:100::/48".to_string());
        config.tenant_interfaces[1].tenant_vrf_loopback_ip = Some("10.217.5.200".to_string());
        add_service_slot(
            &mut config,
            &["10.217.5.168/31", "10.217.5.170/31", "2001:db8:100::10/127"],
            Some(1),
            None,
            false,
        );

        // Give host and service ports the same conditional targets in different orders.
        let conditional_export = rpc::PrefixRouteTarget {
            prefix: "0.0.0.0/0".into(),
            match_type: rpc::ExportPrefixMatchType::Exact as i32,
            route_targets: vec![
                rpc_common::RouteTarget {
                    asn: 64512,
                    vni: Some(50000),
                    vni_self: false,
                },
                rpc_common::RouteTarget {
                    asn: 64512,
                    vni: Some(50001),
                    vni_self: false,
                },
            ],
            ..Default::default()
        };
        config.tenant_interfaces[1]
            .vpc_routing_profile
            .as_mut()
            .expect("host profile")
            .route_targets_on_exports_by_prefix = vec![conditional_export.clone()];
        for interface in &mut config.service_interfaces {
            let mut reordered = conditional_export.clone();
            reordered.route_targets.reverse();
            interface
                .vpc_routing_profile
                .as_mut()
                .expect("service profile")
                .route_targets_on_exports_by_prefix = vec![reordered];
        }

        // Render all endpoints and inspect their address set on the shared slot port.
        let output = render_startup_yaml(&config).await?;
        let documents: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let set = &documents.as_sequence().expect("two YAML documents")[1]["set"];
        // Equivalent target order must pass host/service validation and attach the shared VPC export map.
        assert_eq!(
            set["vrf"]["vpc_1025186"]["router"]["bgp"]["address-family"]["ipv4-unicast"]["route-export"]
                ["to-evpn"]["route-map"],
            "EXPORT_RT_vpc_1025186_V4"
        );
        let service_port = &set["interface"]["iface_svc_0"];
        let addresses = service_port["ip"]["address"]
            .as_mapping()
            .expect("service port has addresses");
        for address in ["10.217.5.168/31", "10.217.5.170/31", "2001:db8:100::10/127"] {
            assert!(
                addresses.contains_key(serde_yaml::Value::String(address.to_string())),
                "missing grouped service address {address}"
            );
        }
        assert!(service_port["link"].get("mtu").is_none());
        assert_eq!(service_port["ip"]["vrf"].as_str(), Some("vpc_1025186"));
        assert_eq!(
            set["vrf"]["vpc_1025186"]["loopback"]["ip"]["address"]["10.217.5.200/32"]
                .as_mapping()
                .map(|mapping| mapping.len()),
            Some(0)
        );

        // Remove only the first endpoint and prove its address leaves without disturbing its peers.
        config.service_interfaces.remove(0);
        let survivor_output = render_startup_yaml(&config).await?;
        let survivor_documents: serde_yaml::Value = serde_yaml::from_str(&survivor_output)?;
        let survivor_addresses = survivor_documents
            .as_sequence()
            .expect("two YAML documents")[1]["set"]["interface"]["iface_svc_0"]["ip"]["address"]
            .as_mapping()
            .expect("surviving service port has addresses");
        assert!(
            !survivor_addresses
                .contains_key(serde_yaml::Value::String("10.217.5.168/31".to_string()))
        );
        assert!(
            survivor_addresses
                .contains_key(serde_yaml::Value::String("10.217.5.170/31".to_string()))
        );
        assert!(survivor_addresses.contains_key(serde_yaml::Value::String(
            "2001:db8:100::10/127".to_string()
        )));

        // Remove the final endpoints and prove the host keeps its port, VRF, and routing neighbor.
        config.service_interfaces.clear();
        let host_only_output = render_startup_yaml(&config).await?;
        let host_only_documents: serde_yaml::Value = serde_yaml::from_str(&host_only_output)?;
        let host_only_set = &host_only_documents
            .as_sequence()
            .expect("two YAML documents")[1]["set"];
        assert!(host_only_set["interface"]["iface_svc_0"].is_null());
        assert!(!host_only_set["interface"]["pf0hpf_if"].is_null());
        assert!(!host_only_set["vrf"]["vpc_1025186"].is_null());
        assert!(
            !host_only_set["vrf"]["vpc_1025186"]["loopback"]["ip"]["address"]["10.217.5.200/32"]
                .is_null()
        );
        assert!(
            !host_only_set["vrf"]["vpc_1025186"]["router"]["bgp"]["neighbor"]["10.217.5.162"]
                .is_null()
        );

        // Remove the host loopback, then attach one service endpoint carrying the VPC loopback.
        config.tenant_interfaces[1].tenant_vrf_loopback_ip = None;
        add_service_slot(&mut config, &["10.217.5.168/31"], Some(1), None, false);
        config.service_interfaces[0].tenant_vrf_loopback_ip = Some("10.217.5.201".to_string());

        // The later service port must merge its loopback into the host-created VRF.
        let merged_output = render_startup_yaml(&config).await?;
        let merged_documents: serde_yaml::Value = serde_yaml::from_str(&merged_output)?;
        let merged_set = &merged_documents.as_sequence().expect("two YAML documents")[1]["set"];
        assert!(
            !merged_set["vrf"]["vpc_1025186"]["loopback"]["ip"]["address"]["10.217.5.201/32"]
                .is_null()
        );

        // Remove the endpoint that supplies the optional VPC loopback.
        config.service_interfaces.clear();
        let removed_output = render_startup_yaml(&config).await?;
        let removed_documents: serde_yaml::Value = serde_yaml::from_str(&removed_output)?;
        let removed_set = &removed_documents.as_sequence().expect("two YAML documents")[1]["set"];
        // The service-supplied loopback is absent while the host BGP neighbor remains.
        assert!(removed_set["vrf"]["vpc_1025186"]["loopback"].is_null());
        assert!(
            !removed_set["vrf"]["vpc_1025186"]["router"]["bgp"]["neighbor"]["10.217.5.162"]
                .is_null()
        );
        Ok(())
    }

    /// Verifies an IPv6 loopback supplied by either a host or service port renders once as a /128
    /// on their shared VRF, independently of the IPv4 service link family.
    #[tokio::test]
    #[allow(deprecated)]
    async fn renders_shared_vpc_ipv6_loopback_from_host_or_service() -> eyre::Result<()> {
        use carbide_test_support::Outcome::Yields;
        use carbide_test_support::{Case, check_cases_async};

        // Compare the complete address map so an extra loopback or an incorrect mask fails either case.
        let expected_loopbacks: serde_yaml::Value = serde_yaml::from_str("2001:db8::3/128: {}\n")?;
        check_cases_async(
            [
                // A service loopback must merge into the VRF already created by the host port.
                Case {
                    scenario: "service supplies loopback",
                    input: (None, Some("2001:db8::3")),
                    expect: Yields(expected_loopbacks.clone()),
                },
                // An omitted service loopback must preserve the host's canonical IPv6 value.
                Case {
                    scenario: "host supplies loopback",
                    input: (Some("2001:db8::3"), None),
                    expect: Yields(expected_loopbacks),
                },
            ],
            |(host_loopback, service_loopback)| async move {
                // Keep the service link IPv4 so its family cannot accidentally determine the loopback's mask.
                let mut config = netconf(
                    VpcVirtualizationType::Fnn,
                    32,
                    24,
                    false,
                    None,
                    false,
                    false,
                );
                let host = &mut config.tenant_interfaces[1];
                // IPv6 loopbacks live only in the family-neutral list, never in the legacy IPv4 field.
                host.tenant_vrf_loopback_ip = None;
                host.addresses = host_loopback
                    .map(|loopback| rpc::InterfaceAddressConfig {
                        address_family: rpc::AddressFamily::V6.into(),
                        tenant_vrf_loopback_ip: Some(loopback.to_string()),
                        ..Default::default()
                    })
                    .into_iter()
                    .collect();
                add_service_slot(&mut config, &["10.217.5.168/31"], Some(1), None, false);
                config.service_interfaces[0].tenant_vrf_loopback_ip =
                    service_loopback.map(str::to_owned);

                // Render the shared host/service VPC through the normal startup-file conversion.
                let output = render_startup_yaml(&config)
                    .await
                    .map_err(|error| format!("{error:#}"))?;
                let documents: serde_yaml::Value =
                    serde_yaml::from_str(&output).map_err(|error| error.to_string())?;

                // Compare this VRF's complete loopback map with the one /128 required by both cases.
                Ok::<_, String>(
                    documents.as_sequence().expect("two YAML documents")[1]["set"]["vrf"]
                        ["vpc_1025186"]["loopback"]["ip"]["address"]
                        .clone(),
                )
            },
        )
        .await;
        Ok(())
    }

    /// Verifies independent fixed slots render separate service VPCs, retain multiple IPv6 links
    /// on one port, and remove every service-only port and VRF after the final endpoint departs.
    #[tokio::test]
    async fn renders_independent_service_vpc_groups_and_cleans_them_up() -> eyre::Result<()> {
        // Build one IPv6 service VPC with two service links on its fixed slot.
        let mut config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        add_service_slot(
            &mut config,
            &["2001:db8:100::10/127", "2001:db8:100::12/127"],
            None,
            None,
            false,
        );

        // Add a second slot whose endpoint belongs to a different IPv4 VPC.
        config
            .service_vpc_slot_inventory
            .as_mut()
            .expect("service fixture has fixed inventory")
            .slots
            .push(rpc::ServiceVpcSlot {
                slot_index: 1,
                hbn_interface_name: "iface_svc_1".to_string(),
            });
        let mut second_group = config.service_interfaces[0].clone();
        second_group.service_id = "10000000-0000-0000-0000-000000000003".to_string();
        second_group.attachment_id = Some(::rpc::common::Uuid {
            value: "20000000-0000-0000-0000-000000000003".to_string(),
        });
        second_group.slot_index = 1;
        second_group.vpc_id = Some("30000000-0000-0000-0000-000000000002".parse().unwrap());
        second_group.vpc_vni = 1_500_003;
        second_group.vpc_prefixes = vec!["198.51.100.0/24".to_string()];
        second_group.vpc_peer_prefixes.clear();
        second_group.vpc_peer_vnis = vec![1_500_004];
        second_group.vpc_routing_profile = Some(rpc::RoutingProfile::default());
        second_group.link_prefix = "198.51.100.10/31".to_string();
        second_group.mac_address = "02:00:00:00:00:03".to_string();
        second_group.internal_uuid = Some(::rpc::common::Uuid {
            value: "40000000-0000-0000-0000-000000000003".to_string(),
        });
        second_group.tenant_vrf_loopback_ip = Some("10.217.5.201".to_string());
        config.service_interfaces.push(second_group);

        // Render both groups and inspect each port and VRF independently.
        let output = render_startup_yaml(&config).await?;
        let documents: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let set = &documents.as_sequence().expect("two YAML documents")[1]["set"];
        let address_keys = |value: &serde_yaml::Value| {
            value
                .as_mapping()
                .expect("service port addresses")
                .keys()
                .map(|address| address.as_str().expect("address string").to_string())
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            address_keys(&set["interface"]["iface_svc_0"]["ip"]["address"]),
            std::collections::BTreeSet::from([
                "2001:db8:100::10/127".to_string(),
                "2001:db8:100::12/127".to_string(),
            ])
        );
        assert_eq!(
            address_keys(&set["interface"]["iface_svc_1"]["ip"]["address"]),
            std::collections::BTreeSet::from(["198.51.100.10/31".to_string()])
        );
        assert_eq!(
            set["interface"]["iface_svc_0"]["ip"]["vrf"].as_str(),
            Some("vpc_1500001")
        );
        assert_eq!(
            set["interface"]["iface_svc_1"]["ip"]["vrf"].as_str(),
            Some("vpc_1500003")
        );
        assert!(
            set["vrf"]["vpc_1500001"]["evpn"]["vni"]["1500001"]
                .as_mapping()
                .is_some()
        );
        assert!(
            set["vrf"]["vpc_1500003"]["evpn"]["vni"]["1500003"]
                .as_mapping()
                .is_some()
        );

        // Remove every endpoint while retaining the fixed service-slot inventory.
        config.service_interfaces.clear();
        let empty_output = render_startup_yaml(&config).await?;
        let empty_documents: serde_yaml::Value = serde_yaml::from_str(&empty_output)?;
        let empty_set = &empty_documents.as_sequence().expect("two YAML documents")[1]["set"];

        // Neither unused slot nor either service-only VRF may remain in HBN desired state.
        for interface in ["iface_svc_0", "iface_svc_1"] {
            assert!(empty_set["interface"][interface].is_null());
        }
        for vrf in ["vpc_1500001", "vpc_1500003"] {
            assert!(empty_set["vrf"][vrf].is_null());
        }
        Ok(())
    }

    /// Verifies one NSG shared by instance and VPC attachments uses one ACL index because the NSG
    /// ID is the sole rendered policy identity.
    #[tokio::test]
    #[allow(deprecated)]
    async fn deduplicates_host_and_service_nsg_by_id() -> eyre::Result<()> {
        // Build one valid host/service overlap that references one NSG ID through both source types.
        let mut config = netconf(VpcVirtualizationType::Fnn, 32, 24, true, None, false, false);
        config.tenant_interfaces[1].tenant_vrf_loopback_ip = Some("10.217.5.200".to_string());
        let service_nsg = {
            let mut host_nsg = config.tenant_interfaces[1]
                .network_security_group
                .clone()
                .expect("host fixture has a security group");
            host_nsg.id = "shared-nsg".to_string();
            host_nsg.source = rpc::NetworkSecurityGroupSource::NsgSourceInstance.into();
            config.tenant_interfaces[1].network_security_group = Some(host_nsg.clone());

            let mut service_nsg = host_nsg;
            service_nsg.source = rpc::NetworkSecurityGroupSource::NsgSourceVpc.into();
            service_nsg
        };
        add_service_slot(
            &mut config,
            &["10.217.5.168/31"],
            Some(1),
            Some(service_nsg),
            false,
        );

        // Render the complete response through the startup-file path.
        let output = render_startup_yaml(&config).await?;
        let documents: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let host_acl = rendered_acl_names(&documents, "pf0hpf_if")
            .into_iter()
            .filter(|name| name.contains("security_group"))
            .collect::<std::collections::BTreeSet<_>>();
        let service_acl = rendered_acl_names(&documents, "iface_svc_0")
            .into_iter()
            .filter(|name| name.contains("security_group"))
            .collect::<std::collections::BTreeSet<_>>();

        // Collect every rendered NSG ACL so an unused duplicate definition set cannot survive.
        let globally_defined_nsg_acls =
            documents.as_sequence().expect("two YAML documents")[1]["set"]["acl"]
                .as_mapping()
                .expect("rendered ACL definitions")
                .keys()
                .filter_map(|name| name.as_str())
                .filter(|name| name.contains("security_group"))
                .map(str::to_string)
                .collect::<std::collections::BTreeSet<_>>();

        // Both ports reference the only four family/direction ACLs.
        assert_eq!(host_acl.len(), 4);
        assert_eq!(host_acl, service_acl);
        assert_eq!(host_acl, globally_defined_nsg_acls);
        Ok(())
    }

    /// Verifies hidden ports use only the selected VPC NSG rules, retain configured global deny
    /// prefixes without an NSG, preserve attached-empty default deny, inherit quarantine
    /// overrides, honor default-route imports from an external routing profile, and render
    /// service-only VPCs without tenant VRF loopbacks so HBN can realize service endpoints when
    /// `use_vpc_vrf_loopback` is false.
    /// Secondary Admin service VRFs without host or isolation routes omit `static` and retain
    /// BGP without emitting an explicit YAML null.
    #[tokio::test]
    #[allow(deprecated)]
    async fn renders_service_vpc_policy_without_host_nsg_fallback() -> eyre::Result<()> {
        // Build an accepted host/service VPC overlap with intentionally distinct effective NSGs.
        let mut distinct_policy =
            netconf(VpcVirtualizationType::Fnn, 32, 24, true, None, false, false);
        distinct_policy.tenant_interfaces[1]
            .network_security_group
            .as_mut()
            .expect("host fixture has a security group")
            .source = rpc::NetworkSecurityGroupSource::NsgSourceInstance.into();
        let host_nsg_id = distinct_policy.tenant_interfaces[1]
            .network_security_group
            .as_ref()
            .expect("host fixture has an instance security group")
            .id
            .clone();
        distinct_policy.tenant_interfaces[1].tenant_vrf_loopback_ip =
            Some("10.217.5.200".to_string());
        // Give the selected VPC NSG a unique permit rule so its referenced ACL cannot be
        // confused with the instance NSG or an unattached policy definition.
        let selected_vpc_rule = rpc::ResolvedNetworkSecurityGroupRule {
            src_prefixes: vec!["203.0.113.10/32".to_string()], // Identify the selected VPC rule.
            dst_prefixes: vec!["198.51.100.10/32".to_string()], // Distinguish it from the host rule.
            rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                id: Some("service-vpc-rule".to_string()),
                // Inspect this rule through the service ingress ACL.
                direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress.into(),
                ipv6: false, // Match the IPv4 prefixes above.
                protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoAny.into(),
                // Distinguish the selected VPC permit from the host deny policy.
                action: rpc::NetworkSecurityGroupRuleAction::NsgRuleActionPermit.into(),
                priority: 100,
                ..Default::default()
            }),
        };

        add_service_slot(
            &mut distinct_policy,
            &["10.217.5.168/31"],
            Some(1),
            Some(service_network_security_group(
                "service-vpc-nsg",
                vec![selected_vpc_rule],
            )),
            false,
        );

        // Distinct host and service NSGs remain separate per-port ACL references in the shared VRF.
        let distinct_output = render_startup_yaml(&distinct_policy).await?;
        let distinct_documents: serde_yaml::Value = serde_yaml::from_str(&distinct_output)?;
        let host_acl = rendered_acl_names(&distinct_documents, "pf0hpf_if")
            .into_iter()
            .filter(|name| name.contains("security_group"))
            .collect::<std::collections::BTreeSet<_>>();
        let service_acl = rendered_acl_names(&distinct_documents, "iface_svc_0")
            .into_iter()
            .filter(|name| name.contains("security_group"))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(!host_acl.is_empty() && !service_acl.is_empty());
        assert_ne!(
            host_acl, service_acl,
            "host NSG {host_nsg_id} leaked to the service port"
        );

        // The ingress ACL referenced by the service port must contain the selected VPC rule.
        let service_ingress_acl = service_acl
            .iter()
            .find(|name| name.ends_with("security_group_ipv4_host_ingress"))
            .expect("service port has an IPv4 ingress NSG ACL");
        let rendered_selected_vpc_rule = &distinct_documents
            .as_sequence()
            .expect("two YAML documents")[1]["set"]["acl"][service_ingress_acl.as_str()]["rule"]["2000"];
        assert!(
            rendered_selected_vpc_rule["action"]["permit"]
                .as_mapping()
                .is_some()
        );
        assert_eq!(
            rendered_selected_vpc_rule["match"]["ip"]["source-ip"].as_str(),
            Some("203.0.113.10/32")
        );
        assert_eq!(
            rendered_selected_vpc_rule["match"]["ip"]["dest-ip"].as_str(),
            Some("198.51.100.10/32")
        );

        // Remove only the selected VPC NSG from the service endpoints.
        for interface in &mut distinct_policy.service_interfaces {
            interface.network_security_group = None;
        }
        let no_service_nsg_output = render_startup_yaml(&distinct_policy).await?;
        let no_service_nsg_documents: serde_yaml::Value =
            serde_yaml::from_str(&no_service_nsg_output)?;
        // No service NSG reference remains after rendering, preventing fallback to the host NSG.
        assert!(
            rendered_acl_names(&no_service_nsg_documents, "iface_svc_0")
                .iter()
                .all(|name| !name.contains("security_group"))
        );

        // The service port retains exactly the configured global IPv4 deny policy without an NSG.
        let no_service_nsg_set = &no_service_nsg_documents
            .as_sequence()
            .expect("two YAML documents")[1]["set"];
        assert!(
            no_service_nsg_set["interface"]["iface_svc_0"]["acl"]
                ["p0000_deny_prefixes_ipv4"]["inbound"]
                .as_mapping()
                .is_some()
        );
        let deny_prefix_acl = &no_service_nsg_set["acl"]["p0000_deny_prefixes_ipv4"];
        let deny_prefix_rules = deny_prefix_acl["rule"]
            .as_mapping()
            .expect("deny-prefix ACL rules");
        assert_eq!(deny_prefix_rules.len(), 2);
        for (priority, prefix) in [("1001", "192.0.2.0/24"), ("1002", "198.51.100.0/24")] {
            let rule = &deny_prefix_acl["rule"][priority];
            assert!(rule["action"]["deny"].as_mapping().is_some());
            assert_eq!(rule["match"]["ip"]["dest-ip"].as_str(), Some(prefix));
        }

        // Attach a VPC NSG with no resolved rules to the service endpoints.
        for interface in &mut distinct_policy.service_interfaces {
            interface.network_security_group =
                Some(service_network_security_group("service-empty-nsg", vec![]));
        }
        let empty_nsg_output = render_startup_yaml(&distinct_policy).await?;
        let empty_nsg_documents: serde_yaml::Value = serde_yaml::from_str(&empty_nsg_output)?;
        let empty_nsg_acls = rendered_acl_names(&empty_nsg_documents, "iface_svc_0")
            .into_iter()
            .filter(|name| name.contains("security_group"))
            .collect::<std::collections::BTreeSet<_>>();
        // The attached empty NSG retains all four family/direction ACLs with default-deny rules.
        assert_eq!(empty_nsg_acls.len(), 4);
        for suffix in [
            "security_group_ipv4_host_egress",
            "security_group_ipv6_host_egress",
            "security_group_ipv4_host_ingress",
            "security_group_ipv6_host_ingress",
        ] {
            let acl_name = empty_nsg_acls
                .iter()
                .find(|name| name.ends_with(suffix))
                .unwrap_or_else(|| panic!("missing empty-NSG ACL {suffix}"));
            assert!(
                !empty_nsg_documents.as_sequence().expect("two YAML documents")[1]["set"]["acl"]
                    [acl_name.as_str()]["rule"]["65535"]["action"]["deny"]
                    .is_null()
            );
        }

        // Enter the secondary-admin path independently of quarantine.
        distinct_policy.use_admin_network = true;
        distinct_policy.is_primary_dpu = false;
        // Override the legacy site-fabric fallback with an explicitly empty resolved route list.
        distinct_policy.site_fabric_null_routes = Some(rpc_common::StringList { items: vec![] });

        // Secondary Admin responses suppress tenant host ports while retaining the service port.
        let secondary_admin_output = render_startup_yaml(&distinct_policy).await?;
        let secondary_admin_documents: serde_yaml::Value =
            serde_yaml::from_str(&secondary_admin_output)?;
        let secondary_admin_set = &secondary_admin_documents
            .as_sequence()
            .expect("two YAML documents")[1]["set"];
        assert!(secondary_admin_set["interface"]["pf0hpf_if"].is_null());
        assert!(!secondary_admin_set["interface"]["iface_svc_0"].is_null());

        // Check membership to distinguish omission from null, while retaining BGP in the same router map.
        let service_vrf_name = format!("vpc_{}", distinct_policy.service_interfaces[0].vpc_vni);
        let secondary_admin_router =
            &secondary_admin_set["vrf"][service_vrf_name.as_str()]["router"];
        assert!(
            !secondary_admin_router
                .as_mapping()
                .expect("service VRF router mapping")
                .contains_key(serde_yaml::Value::from("static"))
        );
        assert!(secondary_admin_router["bgp"].as_mapping().is_some());

        // Apply quarantine after proving the secondary-admin port-selection behavior.
        distinct_policy
            .managed_host_config
            .as_mut()
            .expect("fixture has managed-host config")
            .quarantine_state = Some(rpc::ManagedHostQuarantineState {
            mode: rpc::ManagedHostQuarantineMode::BlockAllTraffic.into(),
            reason: Some("service policy test".to_string()),
        });

        // Quarantine adds exactly the four global deny ACL references to the retained service port.
        let quarantined_output = render_startup_yaml(&distinct_policy).await?;
        let quarantined_documents: serde_yaml::Value = serde_yaml::from_str(&quarantined_output)?;
        let quarantine_acls = rendered_acl_names(&quarantined_documents, "iface_svc_0")
            .into_iter()
            .filter(|name| name.contains("security_policy_override"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            quarantine_acls,
            std::collections::BTreeSet::from([
                "p0004_security_policy_override_ipv4_host_egress".to_string(),
                "p0004_security_policy_override_ipv6_host_egress".to_string(),
                "p0004_security_policy_override_ipv4_host_ingress".to_string(),
                "p0004_security_policy_override_ipv6_host_ingress".to_string(),
            ])
        );

        // Model `use_vpc_vrf_loopback = false` by omitting the service-only external VPC's loopback;
        // the remaining fields provide its fixed slot, selected VPC policy, and default-route import.
        let mut external = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        add_service_slot(&mut external, &["192.0.2.10/31"], None, None, true);
        external.service_interfaces[0].tenant_vrf_loopback_ip = None;
        let external_profile = external.service_interfaces[0]
            .vpc_routing_profile
            .as_mut()
            .expect("service fixture has a routing profile");
        // Give this service-only VPC distinct import and export targets so its rendered policy
        // cannot be mistaken for the response-wide or host VPC targets.
        external_profile.route_target_imports = vec![rpc_common::RouteTarget {
            asn: 44_000,
            vni: Some(55_000),
            vni_self: false,
        }];
        external_profile.route_targets_on_exports = vec![rpc_common::RouteTarget {
            asn: 57_000,
            vni: Some(88_000),
            vni_self: false,
        }];

        // Render the response containing the service-only VPC through the HBN startup-file path.
        let external_output = render_startup_yaml(&external).await?;
        let external_documents: serde_yaml::Value = serde_yaml::from_str(&external_output)?;
        let set = &external_documents
            .as_sequence()
            .expect("two YAML documents")[1]["set"];
        let service_port = &set["interface"]["iface_svc_0"];
        let service_vrf = &set["vrf"]["vpc_1500001"];

        // The service port and its EVPN VNI remain present while the VRF omits only the optional
        // loopback.
        assert_eq!(service_port["ip"]["vrf"].as_str(), Some("vpc_1500001"));
        assert!(
            service_port["ip"]["address"]["192.0.2.10/31"]
                .as_mapping()
                .is_some()
        );
        assert!(!service_vrf["evpn"]["enable"].is_null());
        assert!(service_vrf["evpn"]["vni"]["1500001"].as_mapping().is_some());
        assert!(
            !service_vrf
                .as_mapping()
                .expect("service VPC VRF")
                .contains_key(serde_yaml::Value::String("loopback".to_string()))
        );

        // Inspect only this service VRF's default imports and route targets.
        let mapping_keys = |value: &serde_yaml::Value| {
            value
                .as_mapping()
                .expect("routing value is a mapping")
                .keys()
                .map(|key| key.as_str().expect("routing key").to_string())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let policy = &set["router"]["policy"];
        let ipv4 = &service_vrf["router"]["bgp"]["address-family"]["ipv4-unicast"];
        let ipv6 = &service_vrf["router"]["bgp"]["address-family"]["ipv6-unicast"];
        assert_eq!(
            ipv4["route-import"]["from-vrf"]["route-map"].as_str(),
            Some("FROM_DEFAULT_VRF_TO_vpc_1500001_MAP")
        );
        assert_eq!(
            ipv6["route-import"]["from-vrf"]["route-map"].as_str(),
            Some("FROM_DEFAULT_VRF_TO_vpc_1500001_MAP_V6")
        );
        assert_eq!(
            mapping_keys(&ipv4["route-import"]["from-vrf"]["list"]),
            std::collections::BTreeSet::from(["default".to_string()])
        );
        assert_eq!(
            mapping_keys(&ipv6["route-import"]["from-vrf"]["list"]),
            std::collections::BTreeSet::from(["default".to_string()])
        );
        assert_eq!(
            mapping_keys(
                &policy["prefix-list"]["FROM_DEFAULT_VRF_TO_vpc_1500001_PREFIX_LIST"]["rule"]["65534"]
                    ["match"]
            ),
            std::collections::BTreeSet::from(["0.0.0.0/0".to_string()])
        );
        assert_eq!(
            mapping_keys(
                &policy["prefix-list"]["FROM_DEFAULT_VRF_TO_vpc_1500001_PREFIX_LIST_IPV6"]["rule"]
                    ["65534"]["match"]
            ),
            std::collections::BTreeSet::from(["::/0".to_string()])
        );
        assert_eq!(
            policy["route-map"]["FROM_DEFAULT_VRF_TO_vpc_1500001_MAP"]["rule"]["10"]["match"]
                ["ip-prefix-list"]
                .as_str(),
            Some("FROM_DEFAULT_VRF_TO_vpc_1500001_PREFIX_LIST")
        );
        assert_eq!(
            policy["route-map"]["FROM_DEFAULT_VRF_TO_vpc_1500001_MAP_V6"]["rule"]["10"]
                ["match"]["ip-prefix-list"]
                .as_str(),
            Some("FROM_DEFAULT_VRF_TO_vpc_1500001_PREFIX_LIST_IPV6")
        );
        assert_eq!(
            mapping_keys(&service_vrf["router"]["bgp"]["route-export"]["to-evpn"]["route-target"]),
            std::collections::BTreeSet::from([
                "11414:1500001".to_string(),
                "11415:200".to_string(),
                "57000:88000".to_string(),
            ])
        );
        assert_eq!(
            mapping_keys(
                &service_vrf["router"]["bgp"]["route-import"]["from-evpn"]["route-target"]
            ),
            std::collections::BTreeSet::from([
                "11111:22222".to_string(),
                "11414:1500001".to_string(),
                "11414:1500002".to_string(),
                "44000:55000".to_string(),
                "auto".to_string(),
            ])
        );
        Ok(())
    }

    async fn run_startup_file_attempt(root: &Path, stage: &str) -> eyre::Result<()> {
        let inherited_path =
            std::env::var_os("PATH").ok_or_else(|| eyre::eyre!("missing test PATH"))?;
        let path = std::env::join_paths(
            std::iter::once(root.join("bin")).chain(std::env::split_paths(&inherited_path)),
        )?;
        let mut command = TokioCommand::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "ethernet_virtualization::tests::startup_file_retries_after_interrupted_apply",
                "--nocapture",
            ])
            .env("NVUE_STARTUP_TEST_ROOT", root)
            .env("NVUE_STARTUP_TEST_STAGE", stage)
            .env("IGNORE_MGMT_VRF", "true")
            .env("PATH", path)
            .kill_on_drop(true);
        // The fake commands only use local files; bound a stuck child to 30s.
        let output = timeout(Duration::from_secs(30), command.output()).await??;
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
            "StartupFile stage {stage} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_nvue() -> Result<(), Box<dyn std::error::Error>> {
        let virtualization_type = VpcVirtualizationType::EthernetVirtualizer;

        // Test without an NSG to make sure there are no changes for pre-FNN users
        // if they don't opt-in to a network security group.
        //
        // The `true` is `second_interface_l2`, which makes this the bridge case as well --
        // `nvue_startup.yaml.expected` already carries the `bridge:` blocks. Extend this
        // test rather than adding a separate bridge one; the last attempt at that passed
        // these exact arguments against a byte-identical copy of the golden, so a bridge
        // regression needed both files updated before anything would fail.
        let network_config = netconf(virtualization_type, 32, 24, false, None, true, false);

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check ACLs
        let expected = include_str!("../templates/tests/70-forge_nvue.rules.expected");
        compare_diffed(hbn_root.join(nvue::PATH_ACL), expected)?;

        // check startup.yaml
        let expected = include_str!("../templates/tests/nvue_startup.yaml.expected");
        compare_diffed(hbn_root.join(nvue::PATH), expected)?;

        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_nvue_quarantined() -> Result<(), Box<dyn std::error::Error>> {
        let virtualization_type = VpcVirtualizationType::EthernetVirtualizer;

        let network_config = {
            let mut cfg = netconf(virtualization_type, 32, 24, true, None, false, false);
            match cfg.managed_host_config.as_mut() {
                Some(c) => {
                    c.quarantine_state = Some(rpc::ManagedHostQuarantineState {
                        mode: rpc::ManagedHostQuarantineMode::BlockAllTraffic.into(),
                        reason: Some("test".to_string()),
                    })
                }
                None => panic!("missing managed_host_config"),
            }
            cfg
        };

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check ACLs
        let expected = include_str!("../templates/tests/70-forge_nvue.rules.expected");
        compare_diffed(hbn_root.join(nvue::PATH_ACL), expected)?;

        // check startup.yaml
        let expected = include_str!("../templates/tests/nvue_startup_quarantined.yaml.expected");
        compare_diffed(hbn_root.join(nvue::PATH), expected)?;

        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_fnn_quarantined() -> Result<(), Box<dyn std::error::Error>> {
        let virtualization_type = VpcVirtualizationType::Fnn;

        let network_config = {
            let mut cfg = netconf(virtualization_type, 32, 24, true, None, false, false);
            match cfg.managed_host_config.as_mut() {
                Some(c) => {
                    c.quarantine_state = Some(rpc::ManagedHostQuarantineState {
                        mode: rpc::ManagedHostQuarantineMode::BlockAllTraffic.into(),
                        reason: Some("test".to_string()),
                    })
                }
                None => panic!("missing managed_host_config"),
            }
            cfg
        };

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check ACLs
        let expected = include_str!("../templates/tests/70-forge_nvue.rules.expected");
        compare_diffed(hbn_root.join(nvue::PATH_ACL), expected)?;

        // check startup.yaml
        let expected =
            include_str!("../templates/tests/nvue_startup_quarantined_fnn.yaml.expected");
        compare_diffed(hbn_root.join(nvue::PATH), expected)?;

        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_fnn_with_leaks() -> Result<(), Box<dyn std::error::Error>> {
        let virtualization_type = VpcVirtualizationType::Fnn;

        //let network_config = netconf(virtualization_type, 32, 24, false, None, false, true);

        let mut network_config = netconf(virtualization_type, 32, 24, false, None, false, true);

        // Set an interface profile for a prefix that falls within the VPC profile's prefix.
        network_config.tenant_interfaces[0].interface_routing_profile =
            Some(rpc::FlatInterfaceRoutingProfile {
                allowed_anycast_prefixes: vec![rpc::PrefixFilterPolicyEntry {
                    prefix: "5.255.254.67/32".to_string(),
                }],
            });

        // Set an interface profile for a prefix that falls OUTSIDE the VPC profile's prefix.
        // This should trigger policy to empty out and block prefixes from the tenant.
        // Because a VPC profiles exists and has a prefix list, there is no fallback to AnycastSitePrefixes.
        network_config.tenant_interfaces[1].interface_routing_profile =
            Some(rpc::FlatInterfaceRoutingProfile {
                allowed_anycast_prefixes: vec![rpc::PrefixFilterPolicyEntry {
                    prefix: "67.67.67.6/7".to_string(),
                }],
            });

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check startup.yaml
        let expected = include_str!("../templates/tests/nvue_startup_fnn_with_leaks.yaml.expected");
        compare_diffed(hbn_root.join(nvue::PATH), expected)?;

        Ok(())
    }

    /// Verifies one segment prefix drives both the DPU address and RA PIO while
    /// the stateful tenant `/128` remains a separate host route.
    #[tokio::test]
    #[allow(deprecated)]
    async fn stateful_tenant_ipv6_renders_dpu_address_and_ra_from_segment_prefix()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut network_config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        let interface = &mut network_config.tenant_interfaces[1];
        interface.ipv6_interface_config = Some(rpc::FlatInterfaceIpv6Config {
            ip: "2001:db8::1".to_string(),
            interface_prefix: "2001:db8::1/128".to_string(),
            svi_ip: None,
        });
        interface.addresses.push(rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V6.into(),
            ip: "2001:db8::1".to_string(),
            interface_prefix: "2001:db8::1/128".to_string(),
            prefix: "2001:db8::/127".to_string(),
            ..Default::default()
        });

        // Render through the full Core-response-to-NVUE wiring boundary.
        let tempdir = tempfile::tempdir()?;
        let hbn_root = tempdir.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
        super::update_nvue(
            VpcVirtualizationType::Fnn,
            NvueUpdateFlavor::StartupFile {
                hbn_root,
                skip_post: true,
            },
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        let output = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let docs: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let set = &docs.as_sequence().expect("two YAML documents")[1]["set"];
        let interface = &set["interface"]["pf0hpf_if"];
        let ip = &interface["ip"];

        // The DPU derives ::0/127 from the prefix; ::1/128 remains the tenant route.
        assert!(!ip["address"]["2001:db8::/127"].is_null());
        assert!(ip["address"]["2001:db8::1/128"].is_null());
        assert!(
            set["interface"]["pf0hpf_if"]["acl"]["admin_ipv6_host_to_overlay_flood_prevention"]
                .is_null()
        );
        assert!(set["acl"]["admin_ipv6_host_to_overlay_flood_prevention"].is_null());
        assert!(set["acl"]["admin_ipv6_overlay_to_host_flood_prevention"].is_null());
        let frr_snippet = docs.as_sequence().expect("two YAML documents")[1]["set"]["system"]
            ["config"]["snippet"]["frr.conf"]
            .as_str()
            .expect("stateful tenant should render an FRR snippet");
        assert!(
            frr_snippet.contains("interface pf0hpf_if vrf vpc_1025186")
                && frr_snippet.contains("ipv6 nd prefix 2001:db8::/127 no-autoconfig")
                && frr_snippet
                    .lines()
                    .any(|line| line.trim() == "ipv6 nd managed-config-flag")
        );
        assert!(
            !frr_snippet
                .lines()
                .any(|line| line.trim() == "no ipv6 nd managed-config-flag")
        );
        Ok(())
    }

    /// Proves tenant L2 keeps its IPv6 SVI/VRR state without enabling RA at
    /// the full response-through-`update_nvue` wiring boundary.
    /// Agent-generated tenant RA belongs only on routed interfaces; advertising on a shared
    /// L2 segment could make hosts select a DPU as an unintended IPv6 default router.
    #[tokio::test]
    #[allow(deprecated)]
    async fn slaac_tenant_l2_renders_vrr_without_router_advertisement()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut network_config =
            netconf(VpcVirtualizationType::Fnn, 32, 24, false, None, true, false);
        let interface = &mut network_config.tenant_interfaces[1];
        interface.ipv6_interface_config = Some(rpc::FlatInterfaceIpv6Config {
            ip: String::new(),
            interface_prefix: "2001:db8:185::/64".to_string(),
            svi_ip: Some("2001:db8:185::2/64".to_string()),
        });
        interface.addresses.push(rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V6.into(),
            interface_prefix: "2001:db8:185::/64".to_string(),
            prefix: "2001:db8:185::/64".to_string(),
            svi_ip: Some("2001:db8:185::2/64".to_string()),
            ..Default::default()
        });

        let tempdir = tempfile::tempdir()?;
        let hbn_root = tempdir.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
        super::update_nvue(
            VpcVirtualizationType::Fnn,
            NvueUpdateFlavor::StartupFile {
                hbn_root,
                skip_post: true,
            },
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;

        let output = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let docs: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let set = &docs.as_sequence().expect("two YAML documents")[1]["set"];
        assert!(set["system"]["config"]["snippet"].is_null());
        assert!(!set["interface"]["vlan185"]["ip"]["address"]["2001:db8:185::2/64"].is_null());
        assert!(
            !set["interface"]["vlan185"]["ip"]["vrr"]["address"]["2001:db8:185::/64"].is_null()
        );
        assert!(set["acl"]["admin_ipv6_host_to_overlay_flood_prevention"].is_null());
        assert!(set["acl"]["admin_ipv6_overlay_to_host_flood_prevention"].is_null());
        Ok(())
    }

    /// An IPv6-only admin response still uses an IPv4 DPU underlay. Its SVI, RA, and host
    /// route must render without an empty IPv4 static route or BGP network entry.
    /// A loopback added to its canonical IPv6 entry must render as a /128 on the Admin VRF.
    #[tokio::test]
    #[allow(deprecated)]
    async fn stateful_ipv6_only_admin_renders_without_ipv4()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut network_config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        network_config.use_admin_network = true;
        network_config.network_virtualization_type = Some(rpc::VpcVirtualizationType::Fnn.into());
        network_config.admin_interface = Some(rpc::FlatInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical.into(),
            vlan_id: 123,
            vni: 5555,
            vpc_vni: 7777,
            is_l2_segment: true,
            ipv6_interface_config: Some(rpc::FlatInterfaceIpv6Config {
                ip: "2001:db8:9::3".to_string(),
                interface_prefix: "2001:db8:9::3/128".to_string(),
                svi_ip: Some("2001:db8:9::2/64".to_string()),
            }),
            addresses: vec![rpc::InterfaceAddressConfig {
                address_family: rpc::AddressFamily::V6.into(),
                ip: "2001:db8:9::3".to_string(),
                interface_prefix: "2001:db8:9::3/128".to_string(),
                prefix: "2001:db8:9::/64".to_string(),
                svi_ip: Some("2001:db8:9::2/64".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let tempdir = tempfile::tempdir()?;
        let hbn_root = tempdir.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        assert!(
            super::update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &network_config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                None,
                None,
            )
            .await?
        );
        let output = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let docs: serde_yaml::Value = serde_yaml::from_str(&output)?;
        // Compare the full NVUE configuration without pinning insignificant template whitespace.
        let expected =
            include_str!("../templates/tests/nvue_startup_fnn_ipv6_only_admin.yaml.expected");
        assert_eq!(docs, serde_yaml::from_str::<serde_yaml::Value>(expected)?);
        let set = &docs.as_sequence().expect("two YAML documents")[1]["set"];
        assert_eq!(
            set["nve"]["vxlan"]["source"]["address"].as_str(),
            Some("10.217.5.39")
        );
        let vlan = &set["interface"]["vlan123"];
        assert_eq!(
            vlan["ip"]["address"],
            serde_yaml::from_str::<serde_yaml::Value>("2001:db8:9::2/64: {}\n")?
        );
        assert_eq!(
            vlan["ip"]["vrr"]["address"],
            serde_yaml::from_str::<serde_yaml::Value>("2001:db8:9::/64: {}\n")?
        );

        let admin_router = &set["vrf"]["vpc_7777"]["router"];
        assert_eq!(
            admin_router["static"]["2001:db8:9::3/128"],
            serde_yaml::from_str::<serde_yaml::Value>(
                "address-family: ipv6-unicast\nvia:\n  vlan123:\n    type: interface\n"
            )?
        );
        assert!(admin_router["bgp"]["address-family"]["ipv4-unicast"]["network"].is_null());
        assert_eq!(
            admin_router["bgp"]["address-family"]["ipv6-unicast"]["network"],
            serde_yaml::from_str::<serde_yaml::Value>("2001:db8:9::3/128: {}\n")?
        );

        // DHCPv6 assigns the host /128; RA advertises its containing segment on the SVI.
        let snippet = set["system"]["config"]["snippet"]["frr.conf"]
            .as_str()
            .expect("IPv6-only admin must render RA");
        assert!(snippet.contains("interface vlan123 vrf vpc_7777"));
        assert!(snippet.contains("ipv6 nd prefix 2001:db8:9::/64 no-autoconfig"));
        assert!(
            snippet
                .lines()
                .any(|line| line.trim() == "ipv6 nd managed-config-flag")
        );
        assert!(
            snippet
                .lines()
                .any(|line| line.trim() == "ipv6 nd other-config-flag")
        );
        assert!(snippet.contains("ipv6 nd rdnss 2001:db8::53 1800"));
        assert!(snippet.contains("ipv6 nd rdnss 2001:db8::54 1800"));

        // DHCP keeps its IPv4 transport configuration without requiring an IPv4 host binding.
        let prepared = prepare_dhcp_server_config(
            &network_config,
            &test_service_addresses(),
            &HBNDeviceNames::hbn_23(),
        )?;
        assert_eq!(
            prepared.dhcp.carbide_dhcp_server,
            Some("10.217.5.39".parse()?)
        );
        assert_eq!(
            prepared.dhcp.carbide_provisioning_server_ipv4,
            Some("192.0.2.10".parse()?)
        );
        let host_config: HostConfig = serde_yaml::from_str(&prepared.host)?;
        assert_eq!(
            host_config
                .host_ip_addresses
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["vlan123"]
        );
        let admin_host = &host_config.host_ip_addresses["vlan123"];
        assert_eq!(
            (
                admin_host.address,
                admin_host.gateway,
                admin_host.prefix.as_deref()
            ),
            (None, None, None)
        );
        let admin_ipv6 = admin_host
            .ipv6
            .as_ref()
            .expect("admin DHCP IPv6 host binding");
        assert_eq!(admin_ipv6.address, Some("2001:db8:9::3".parse()?));
        assert_eq!(admin_ipv6.prefix, "2001:db8:9::3/128");

        // Add a loopback only to the canonical IPv6 entry after checking the retained Admin golden.
        network_config
            .admin_interface
            .as_mut()
            .expect("Admin interface")
            .addresses[0]
            .tenant_vrf_loopback_ip = Some("2001:db8:9::4".to_string());
        let with_loopback = render_startup_yaml(&network_config).await?;
        let documents: serde_yaml::Value = serde_yaml::from_str(&with_loopback)?;

        // The primary Admin conversion must retain that address with an IPv6 host prefix.
        assert!(
            documents.as_sequence().expect("two YAML documents")[1]["set"]["vrf"]
                ["vpc_7777"]["loopback"]["ip"]["address"]["2001:db8:9::4/128"]
                .as_mapping()
                .is_some()
        );
        Ok(())
    }

    /// Verifies routed admin renders existing gateway prefixes, PF DHCP/RA, and host routes
    /// without a host-facing bridge or SVI, so enabling it preserves the client addressing contract.
    /// Secondary admin service ports must retain link-prefix leaks because they use separate addressing.
    #[tokio::test]
    async fn routed_admin_preserves_subnet_addressing() -> eyre::Result<()> {
        // Retain matching canonical and legacy addresses while selecting routed admin and host leaks.
        let mut config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        config.use_admin_network = true;
        config.network_virtualization_type = Some(rpc::VpcVirtualizationType::Fnn.into());
        let mut admin_interface = dual_stack_admin_interface();
        admin_interface.is_l2_segment = false; // Route the existing /26 and /64 on the PF.
        admin_interface.vpc_routing_profile = Some(rpc::RoutingProfile {
            leak_tenant_host_routes_to_underlay: true, // Preserve the admin host-export policy.
            ..Default::default()
        });
        config.admin_interface = Some(admin_interface);

        // Render the complete response through the same startup-file path used by DPU OS agents.
        let output = render_startup_yaml(&config).await?;
        let documents: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let set = &documents.as_sequence().expect("two YAML documents")[1]["set"];
        let host_port = &set["interface"]["pf0hpf_if"];

        // Keep the existing gateway prefixes on the PF and remove the admin L2 attachment.
        assert!(set.get("bridge").is_none());
        assert!(set["interface"].get("vlan123").is_none());
        assert!(host_port.get("bridge").is_none());
        assert!(host_port["ip"].get("vrr").is_none());
        assert_eq!(host_port["ip"]["vrf"].as_str(), Some("vpc_7777"));
        assert_eq!(
            host_port["ip"]["address"],
            serde_yaml::from_str::<serde_yaml::Value>(
                "10.217.4.65/26: {}\n2001:db8:100::/64: {}\n"
            )?
        );
        assert!(set["vrf"]["vpc_7777"]["evpn"]["vni"]["7777"].is_mapping());

        // Originate each host route through its local PF while preserving both EVPN exports.
        let router = &set["vrf"]["vpc_7777"]["router"];
        for (prefix, family) in [
            // IPv4 must advertise the individual host rather than rely on the shared /26.
            ("10.217.4.70/32", "ipv4-unicast"),
            // IPv6 must retain a host /128 independently of the /64 advertised by RA.
            ("2001:db8:100::70/128", "ipv6-unicast"),
        ] {
            assert_eq!(
                router["static"][prefix],
                serde_yaml::from_str::<serde_yaml::Value>(&format!(
                    "address-family: {family}\nvia:\n  pf0hpf_if:\n    type: interface\n"
                ))?
            );
            assert_eq!(
                router["bgp"]["address-family"][family]["network"],
                serde_yaml::from_str::<serde_yaml::Value>(&format!("{prefix}: {{}}\n"))?
            );
            assert_eq!(
                router["bgp"]["address-family"][family]["route-export"]["to-evpn"]["enable"]
                    .as_str(),
                Some("on")
            );
        }

        // Preserve the existing per-host IPv6 underlay leak instead of exporting the shared /64.
        let prefix_lists = &set["router"]["policy"]["prefix-list"];
        assert_eq!(
            prefix_lists["ALLOW_TO_UNDERLAY_PREFIX_LIST_IPV6"]["rule"]["65002"]["match"],
            serde_yaml::from_str::<serde_yaml::Value>("2001:db8:100::70/128: {}\n")?
        );

        // Emit stateful RA on the PF using the retained admin /64 rather than a /127 linknet.
        let snippet = set["system"]["config"]["snippet"]["frr.conf"]
            .as_str()
            .expect("routed admin RA");
        assert!(snippet.contains("interface pf0hpf_if vrf vpc_7777"));
        assert!(!snippet.contains("interface vlan123 vrf vpc_7777"));
        assert!(snippet.contains("ipv6 nd prefix 2001:db8:100::/64 no-autoconfig"));
        assert!(
            snippet
                .lines()
                .any(|line| line.trim() == "ipv6 nd managed-config-flag")
        );

        // Match the file-based DHCP listener and reservation to the PF without changing client masks.
        let prepared = prepare_dhcp_server_config(
            &config,
            &test_service_addresses(),
            &HBNDeviceNames::hbn_23(),
        )?;
        assert!(prepared.supervisor.contains("--interfaces pf0hpf_if "));
        assert!(!prepared.supervisor.contains("--interfaces vlan123 "));
        let host_config: HostConfig = serde_yaml::from_str(&prepared.host)?;
        assert_eq!(host_config.host_ip_addresses.len(), 1);
        let host_binding = host_config
            .host_ip_addresses
            .get("pf0hpf_if")
            .expect("admin PF binding");
        assert_eq!(
            (
                host_binding.address,
                host_binding.gateway,
                host_binding.prefix.as_deref()
            ),
            (
                Some("10.217.4.70".parse()?),
                Some("10.217.4.65".parse()?),
                Some("10.217.4.64/26")
            )
        );
        let ipv6_binding = host_binding
            .ipv6
            .as_ref()
            .expect("stateful admin IPv6 binding");
        assert_eq!(ipv6_binding.address, Some("2001:db8:100::70".parse()?));
        assert_eq!(ipv6_binding.prefix, "2001:db8:100::70/128");

        // Preserve the DPF DHCP control path's host PF listener for the same routed response.
        let control_update = prepare_dhcp_grpc_update(
            &config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
        )?
        .expect("primary admin DHCP update");
        assert_eq!(control_update.interfaces, vec!["pf0hpf_if".to_string()]);

        // Retain a service on a secondary Admin DPU, where the host PF is not rendered.
        config.is_primary_dpu = false;
        add_service_slot(&mut config, &["2001:db8:100:1::10/127"], None, None, false);
        config.service_interfaces[0]
            .vpc_routing_profile
            .as_mut()
            .expect("service routing profile")
            .leak_tenant_host_routes_to_underlay = true;

        // Render the secondary response and verify its service keeps the routed link-prefix leak.
        let secondary_output = render_startup_yaml(&config).await?;
        let secondary_documents: serde_yaml::Value = serde_yaml::from_str(&secondary_output)?;
        let secondary_set = &secondary_documents
            .as_sequence()
            .expect("two YAML documents")[1]["set"];
        assert!(secondary_set["interface"].get("pf0hpf_if").is_none());
        assert_eq!(
            secondary_set["router"]["policy"]["prefix-list"]["ALLOW_TO_UNDERLAY_PREFIX_LIST_IPV6"]
                ["rule"]["65002"]["match"],
            serde_yaml::from_str::<serde_yaml::Value>("2001:db8:100:1::10/127: {}\n")?
        );
        Ok(())
    }

    /// Verifies a realistic dual-stack admin response drives the existing SVI,
    /// explicit RA target, resolver changes, and complete IPv6 withdrawal.
    /// Host routes must be originated and withdrawn with that state so routed reachability follows it.
    /// The local FMDS opt-in must also reach the rendered attachment and be
    /// removable without changing the retained IPv4 peer.
    #[tokio::test]
    async fn stateful_admin_ipv6_reconciles_svi_ra_and_rdnss()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut network_config = netconf(
            VpcVirtualizationType::Fnn,
            32,
            24,
            false,
            None,
            false,
            false,
        );
        network_config.use_admin_network = true;
        network_config.network_virtualization_type = Some(rpc::VpcVirtualizationType::Fnn.into());
        network_config.admin_interface = Some(dual_stack_admin_interface());

        let tempdir = tempfile::tempdir()?;
        let hbn_root = tempdir.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // Resolve the operator's existing address list before rendering HBN.
        let agent_config_path = hbn_root.join("agent.toml");
        fs::write(
            &agent_config_path,
            "[fmds-armos-networking.config]\naddresses = [\"169.254.169.254/30\", \"fd00:76b9::254/127\"]",
        )?;
        let agent_config =
            carbide_host_support::agent_config::AgentConfig::load_from(&agent_config_path)?;
        // Render the populated response through the complete update path.
        assert!(
            super::update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &network_config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                None,
                agent_config.fmds_armos_networking.ipv6_gateway()?,
            )
            .await?
        );
        let output = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let docs: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let set = &docs.as_sequence().expect("two YAML documents")[1]["set"];
        let vlan = &set["interface"]["vlan123"];
        assert!(set["interface"]["pf0dpu1_if"]["ip"]["address"]["fd00:76b9::255/127"].is_mapping());
        assert!(set["interface"]["pf0dpu1_if"]["ip"]["address"]["169.254.169.253/30"].is_mapping());
        let snippet = set["system"]["config"]["snippet"]["frr.conf"]
            .as_str()
            .expect("admin RA should render an FRR snippet");

        // The segment configures VRR and PIO while the host /128 is not
        // reinterpreted as the segment CIDR.
        assert!(!vlan["ip"]["address"]["2001:db8:100::66/64"].is_null());
        assert!(!vlan["ip"]["vrr"]["address"]["2001:db8:100::/64"].is_null());
        assert!(vlan["ip"]["vrr"]["address"]["2001:db8:100::70/128"].is_null());

        // Both host routes must exist locally before BGP can originate them into EVPN.
        let admin_router = &set["vrf"]["vpc_7777"]["router"];
        for (prefix, family) in [
            // Adding IPv6 must preserve the existing IPv4 route and origination.
            ("10.217.4.70/32", "ipv4-unicast"),
            // IPv6 originates the individual host, not the shared admin segment.
            ("2001:db8:100::70/128", "ipv6-unicast"),
        ] {
            assert_eq!(
                admin_router["static"][prefix],
                serde_yaml::from_str::<serde_yaml::Value>(&format!(
                    "address-family: {family}\nvia:\n  vlan123:\n    type: interface\n"
                ))?
            );
            assert_eq!(
                admin_router["bgp"]["address-family"][family]["network"],
                serde_yaml::from_str::<serde_yaml::Value>(&format!("{prefix}: {{}}\n"))?
            );
        }

        // EVPN origination must not bypass the disabled underlay export policy.
        assert!(
            set["router"]["policy"]["prefix-list"]["ALLOW_TO_UNDERLAY_PREFIX_LIST_IPV6"]["rule"]
                ["65002"]
                .is_null()
        );

        // Stateful RA targets the VLAN SVI, sets M=1/O=1/A=0, and advertises
        // only the IPv6 service resolvers with finite lifetimes.
        assert!(snippet.contains("interface vlan123 vrf vpc_7777"));
        assert!(!snippet.contains("interface pf0hpf_if vrf vpc_7777"));
        assert!(snippet.contains("ipv6 nd prefix 2001:db8:100::/64 no-autoconfig"));
        assert!(snippet.contains("ipv6 nd ra-lifetime 1800"));
        assert!(
            snippet
                .lines()
                .any(|line| line.trim() == "ipv6 nd managed-config-flag")
        );
        assert!(
            !snippet
                .lines()
                .any(|line| line.trim() == "no ipv6 nd managed-config-flag")
        );
        assert!(
            snippet
                .lines()
                .any(|line| line.trim() == "ipv6 nd other-config-flag")
        );
        assert!(
            !snippet
                .lines()
                .any(|line| line.trim() == "no ipv6 nd other-config-flag")
        );
        assert_eq!(
            snippet
                .lines()
                .filter(|line| line.contains("ipv6 nd rdnss"))
                .collect::<Vec<_>>(),
            [
                " ipv6 nd rdnss 2001:db8::53 1800",
                " ipv6 nd rdnss 2001:db8::54 1800",
            ]
        );

        // The existing IPv4 and two directional IPv6 containment policies are
        // attached at the host-facing admin boundary.
        let interface_acl = &set["interface"]["pf0hpf_if"]["acl"];
        assert_eq!(
            interface_acl["dhcp_flood_prevention"],
            serde_yaml::from_str::<serde_yaml::Value>("inbound: {}\n")?
        );
        assert_eq!(
            interface_acl["admin_ipv6_host_to_overlay_flood_prevention"],
            serde_yaml::from_str::<serde_yaml::Value>("inbound: {}\n")?
        );
        assert_eq!(
            interface_acl["admin_ipv6_overlay_to_host_flood_prevention"],
            serde_yaml::from_str::<serde_yaml::Value>("outbound: {}\n")?
        );

        // Replacing VLAN, VRF, prefix, host, SVI, and resolver inputs must
        // produce only the replacement desired state.
        let admin_interface = network_config
            .admin_interface
            .as_mut()
            .expect("fixture should contain an admin interface");
        admin_interface.vlan_id = 124;
        admin_interface.vni = 6666;
        admin_interface.vpc_vni = 8888;
        admin_interface.vpc_routing_profile = Some(rpc::RoutingProfile {
            leak_tenant_host_routes_to_underlay: true, // Export the host /128.
            ..Default::default()
        });
        // Leave the deprecated sidecar stale: every admin IPv6 render input
        // must come from the authoritative address entry below.
        let canonical = admin_interface
            .addresses
            .iter_mut()
            .find(|address| address.address_family == i32::from(rpc::AddressFamily::V6))
            .expect("fixture should contain canonical IPv6");
        canonical.ip = "2001:db8:200::80".to_string();
        canonical.interface_prefix = "2001:db8:200::80/128".to_string();
        canonical.prefix = "2001:db8:200::/64".to_string();
        canonical.svi_ip = Some("2001:db8:200::66/64".to_string());
        let replacement_services = ServiceAddresses {
            pxe_ips: vec![],
            ntpservers: vec![],
            nameservers: vec!["2001:db8:ffff::53".parse().unwrap()],
        };
        assert!(
            super::update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &network_config,
                &replacement_services,
                HBNDeviceNames::hbn_23(),
                None,
                None,
            )
            .await?
        );
        let replacement = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let replacement_docs: serde_yaml::Value = serde_yaml::from_str(&replacement)?;
        let replacement_set =
            &replacement_docs.as_sequence().expect("two YAML documents")[1]["set"];
        // Omitted local opt-in removes only the IPv6 FMDS peer on the next render.
        assert!(
            replacement_set["interface"]["pf0dpu1_if"]["ip"]["address"]["fd00:76b9::255/127"]
                .is_null()
        );
        assert!(
            replacement_set["interface"]["pf0dpu1_if"]["ip"]["address"]["169.254.169.253/30"]
                .is_mapping()
        );
        let replacement_snippet = replacement_set["system"]["config"]["snippet"]["frr.conf"]
            .as_str()
            .expect("replacement admin RA should render an FRR snippet");
        assert!(replacement_snippet.contains("interface vlan124 vrf vpc_8888"));
        assert!(replacement_snippet.contains("ipv6 nd prefix 2001:db8:200::/64 no-autoconfig"));
        assert!(replacement_snippet.contains("ipv6 nd rdnss 2001:db8:ffff::53 1800"));
        assert!(
            !replacement_set["interface"]["vlan124"]["ip"]["address"]["2001:db8:200::66/64"]
                .is_null()
        );
        assert!(
            !replacement_set["interface"]["vlan124"]["ip"]["vrr"]["address"]["2001:db8:200::/64"]
                .is_null()
        );
        // Route installation, origination, and permitted export must follow the new host and VLAN.
        let replacement_router = &replacement_set["vrf"]["vpc_8888"]["router"];
        assert_eq!(
            replacement_router["static"]["2001:db8:200::80/128"],
            serde_yaml::from_str::<serde_yaml::Value>(
                "address-family: ipv6-unicast\nvia:\n  vlan124:\n    type: interface\n"
            )?
        );
        assert_eq!(
            replacement_router["bgp"]["address-family"]["ipv6-unicast"]["network"],
            serde_yaml::from_str::<serde_yaml::Value>("2001:db8:200::80/128: {}\n")?
        );
        assert_eq!(
            replacement_set["router"]["policy"]["prefix-list"]["ALLOW_TO_UNDERLAY_PREFIX_LIST_IPV6"]
                ["rule"]["65002"]["match"],
            serde_yaml::from_str::<serde_yaml::Value>("2001:db8:200::80/128: {}\n")?
        );
        let replacement_neighbors =
            &replacement_set["vrf"]["vpc_8888"]["router"]["bgp"]["neighbor"];
        assert!(replacement_neighbors["2001:db8:100::70"].is_null());
        assert_eq!(
            replacement_neighbors["2001:db8:200::80"]["peer-group"].as_str(),
            Some("tenant")
        );
        assert_eq!(
            replacement_neighbors["2001:db8:200::80"]["passive-mode"].as_str(),
            Some("on")
        );
        for stale in [
            "interface vlan123 vrf vpc_7777",
            "2001:db8:100::/64",
            "2001:db8:100::70",
            "2001:db8::53",
            "2001:db8::54",
        ] {
            assert!(
                !replacement.contains(stale),
                "stale admin RA input: {stale}"
            );
        }

        // Removing IPv6 resolver membership from the replacement state keeps
        // RA/SVI desired state while withdrawing the complete RDNSS list.
        let ipv4_only_services = ServiceAddresses {
            pxe_ips: vec![],
            ntpservers: vec![],
            nameservers: vec!["192.0.2.53".parse().unwrap()],
        };
        assert!(
            super::update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &network_config,
                &ipv4_only_services,
                HBNDeviceNames::hbn_23(),
                None,
                None,
            )
            .await?
        );
        let without_rdnss = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let without_rdnss_docs: serde_yaml::Value = serde_yaml::from_str(&without_rdnss)?;
        let without_rdnss_set = &without_rdnss_docs
            .as_sequence()
            .expect("two YAML documents")[1]["set"];
        let without_rdnss_snippet = without_rdnss_set["system"]["config"]["snippet"]["frr.conf"]
            .as_str()
            .expect("admin RA should remain without RDNSS");
        assert!(without_rdnss_snippet.contains("ipv6 nd prefix 2001:db8:200::/64 no-autoconfig"));
        assert!(!without_rdnss_snippet.contains("ipv6 nd rdnss"));
        assert!(
            !without_rdnss_set["interface"]["vlan124"]["ip"]["address"]["2001:db8:200::66/64"]
                .is_null()
        );
        assert!(
            !without_rdnss_set["interface"]["vlan124"]["ip"]["vrr"]["address"]["2001:db8:200::/64"]
                .is_null()
        );

        // Removing the authoritative IPv6 projection withdraws the complete
        // RA/RDNSS and SVI state even while the deprecated sidecar remains.
        let admin_interface = network_config
            .admin_interface
            .as_mut()
            .expect("fixture should contain an admin interface");
        admin_interface
            .addresses
            .retain(|address| address.address_family != i32::from(rpc::AddressFamily::V6));
        assert!(
            super::update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &network_config,
                &ipv4_only_services,
                HBNDeviceNames::hbn_23(),
                None,
                None,
            )
            .await?
        );
        let output = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let docs: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let set = &docs.as_sequence().expect("two YAML documents")[1]["set"];
        assert!(set["system"]["config"]["snippet"].is_null());
        assert!(set["interface"]["vlan124"]["ip"]["address"]["2001:db8:200::66/64"].is_null());
        assert!(set["interface"]["vlan124"]["ip"]["vrr"]["address"]["2001:db8:200::/64"].is_null());

        // Withdrawal removes IPv6 routing and export while preserving IPv4 on the new VLAN.
        let admin_router = &set["vrf"]["vpc_8888"]["router"];
        assert!(admin_router["static"]["2001:db8:200::80/128"].is_null());
        assert!(admin_router["bgp"]["address-family"]["ipv6-unicast"]["network"].is_null());
        assert!(
            set["router"]["policy"]["prefix-list"]["ALLOW_TO_UNDERLAY_PREFIX_LIST_IPV6"]["rule"]
                ["65002"]
                .is_null()
        );
        assert_eq!(
            admin_router["static"]["10.217.4.70/32"],
            serde_yaml::from_str::<serde_yaml::Value>(
                "address-family: ipv4-unicast\nvia:\n  vlan124:\n    type: interface\n"
            )?
        );
        assert_eq!(
            admin_router["bgp"]["address-family"]["ipv4-unicast"]["network"],
            serde_yaml::from_str::<serde_yaml::Value>("10.217.4.70/32: {}\n")?
        );
        assert!(
            set["interface"]["pf0hpf_if"]["acl"]["admin_ipv6_host_to_overlay_flood_prevention"]
                .is_null()
        );
        assert!(
            set["interface"]["pf0hpf_if"]["acl"]["admin_ipv6_overlay_to_host_flood_prevention"]
                .is_null()
        );
        assert!(set["acl"]["admin_ipv6_host_to_overlay_flood_prevention"].is_null());
        assert!(set["acl"]["admin_ipv6_overlay_to_host_flood_prevention"].is_null());
        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_fnn_with_missing_vpcs() -> Result<(), Box<dyn std::error::Error>> {
        let virtualization_type = VpcVirtualizationType::Fnn;

        let mut network_config = netconf(virtualization_type, 32, 24, false, None, false, false);

        // Empty out the interfaces so we complain if we don't see VPCs.
        network_config.tenant_interfaces = vec![];

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        assert!(
            super::update_nvue(
                virtualization_type,
                update_flavor,
                &network_config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                None,
                None,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("BUG: network config provided without interfaces")
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_nvue_with_nsg() -> Result<(), Box<dyn std::error::Error>> {
        // Test WITH an NSG
        let virtualization_type = VpcVirtualizationType::EthernetVirtualizer;

        let network_config = netconf(virtualization_type, 32, 24, true, None, false, false);

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // let has_changes = super::update_nvue(
        //     virtualization_type,
        //     hbn_root,
        //     &network_config,
        //     true,
        //     HBNDeviceNames::hbn_23(),
        // )
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check ACLs
        let expected = include_str!("../templates/tests/70-forge_nvue.rules.expected");
        compare_diffed(hbn_root.join(nvue::PATH_ACL), expected)?;

        // check startup.yaml
        let expected = include_str!("../templates/tests/nvue_startup_with_nsg.yaml.expected");
        compare_diffed(hbn_root.join(nvue::PATH), expected)?;

        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_nvue_with_empty_nsg_default_deny()
    -> Result<(), Box<dyn std::error::Error>> {
        let virtualization_type = VpcVirtualizationType::EthernetVirtualizer;
        let mut network_config = netconf(virtualization_type, 32, 24, true, None, false, false);

        // Empty out all NSG rules.  This should result in config that
        // just has a single default deny.
        for iface in network_config.tenant_interfaces.iter_mut() {
            if let Some(nsg) = iface.network_security_group.as_mut() {
                nsg.rules = vec![];
            }
        }

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // let has_changes = super::update_nvue(
        //     virtualization_type,
        //     hbn_root,
        //     &network_config,
        //     true,
        //     HBNDeviceNames::hbn_23(),
        // )
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check ACLs
        let expected = include_str!("../templates/tests/70-forge_nvue.rules.expected");
        compare_diffed(hbn_root.join(nvue::PATH_ACL), expected)?;

        // check startup.yaml.
        let expected = include_str!(
            "../templates/tests/nvue_startup_with_empty_nsg_default_deny.yaml.expected"
        );
        compare_diffed(hbn_root.join(nvue::PATH), expected)?;

        const ERR_FILE: &str = "/tmp/test_nvue_startup.yaml";
        let startup_yaml = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let _: Vec<serde_yaml::Value> = serde_yaml::from_str(&startup_yaml)
            .inspect_err(|_| {
                let mut f = fs::File::create(ERR_FILE).unwrap();
                f.write_all(startup_yaml.as_bytes()).unwrap();
            })
            .wrap_err(format!("YAML parser error. output written to {ERR_FILE}"))?;

        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_nvue_fnn_classic_with_nsg() -> Result<(), Box<dyn std::error::Error>>
    {
        let virtualization_type = VpcVirtualizationType::Fnn;
        let network_config = netconf(virtualization_type, 32, 24, true, None, false, false);

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // let has_changes = super::update_nvue(
        //     virtualization_type,
        //     hbn_root,
        //     &network_config,
        //     true,
        //     HBNDeviceNames::hbn_23(),
        // )
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check ACLs
        let expected = include_str!("../templates/tests/70-forge_nvue.rules.expected");
        compare_diffed(hbn_root.join(nvue::PATH_ACL), expected)?;

        // check startup.yaml.
        // TODO: This should be fixed when new template is merged.
        //
        // let expected = include_str!("../templates/tests/nvue_startup_fnn_classic.yaml.expected");
        // compare_diffed(hbn_root.join(nvue::PATH), expected)?;
        // Until then... let's at least confirm valid YAML...
        const ERR_FILE: &str = "/tmp/test_nvue_startup.yaml";
        let startup_yaml = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let _: Vec<serde_yaml::Value> = serde_yaml::from_str(&startup_yaml)
            .inspect_err(|_| {
                let mut f = fs::File::create(ERR_FILE).unwrap();
                f.write_all(startup_yaml.as_bytes()).unwrap();
            })
            .wrap_err(format!("YAML parser error. output written to {ERR_FILE}"))?;

        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_nvue_fnn_classic_with_empty_nsg_default_deny()
    -> Result<(), Box<dyn std::error::Error>> {
        let virtualization_type = VpcVirtualizationType::Fnn;
        let mut network_config =
            netconf(virtualization_type, 32, 24, true, Some(3109), false, false);

        // Empty out all NSG rules.  This should result in config that
        // just has a single default deny.
        for iface in network_config.tenant_interfaces.iter_mut() {
            if let Some(nsg) = iface.network_security_group.as_mut() {
                nsg.rules = vec![];
            }
        }

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // let has_changes = super::update_nvue(
        //     virtualization_type,
        //     hbn_root,
        //     &network_config,
        //     true,
        //     HBNDeviceNames::hbn_23(),
        // )
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check ACLs
        let expected = include_str!("../templates/tests/70-forge_nvue.rules.expected");
        compare_diffed(hbn_root.join(nvue::PATH_ACL), expected)?;

        // check startup.yaml.
        let expected = include_str!(
            "../templates/tests/nvue_startup_fnn_classic_with_empty_nsg_default_deny.yaml.expected"
        );
        compare_diffed(hbn_root.join(nvue::PATH), expected)?;

        const ERR_FILE: &str = "/tmp/test_nvue_startup.yaml";
        let startup_yaml = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let _: Vec<serde_yaml::Value> = serde_yaml::from_str(&startup_yaml)
            .inspect_err(|_| {
                let mut f = fs::File::create(ERR_FILE).unwrap();
                f.write_all(startup_yaml.as_bytes()).unwrap();
            })
            .wrap_err(format!("YAML parser error. output written to {ERR_FILE}"))?;

        Ok(())
    }

    #[tokio::test]
    async fn test_with_tenant_nvue_fnn_classic() -> Result<(), Box<dyn std::error::Error>> {
        let virtualization_type = VpcVirtualizationType::Fnn;
        let network_config = netconf(virtualization_type, 32, 24, false, None, false, false);

        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("var/support"))?;
        fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;

        // let has_changes = super::update_nvue(
        //     virtualization_type,
        //     hbn_root,
        //     &network_config,
        //     true,
        //     HBNDeviceNames::hbn_23(),
        // )
        let update_flavor = NvueUpdateFlavor::StartupFile {
            hbn_root,
            skip_post: true,
        };

        let has_changes = super::update_nvue(
            virtualization_type,
            update_flavor,
            &network_config,
            &test_service_addresses(),
            HBNDeviceNames::hbn_23(),
            None,
            None,
        )
        .await?;
        assert!(
            has_changes,
            "update_nvue should have written the file, there should be changes"
        );

        // check ACLs
        let expected = include_str!("../templates/tests/70-forge_nvue.rules.expected");
        compare_diffed(hbn_root.join(nvue::PATH_ACL), expected)?;

        // check startup.yaml.
        let expected = include_str!("../templates/tests/nvue_startup_fnn_classic.yaml.expected");
        compare_diffed(hbn_root.join(nvue::PATH), expected)?;
        const ERR_FILE: &str = "/tmp/test_nvue_startup.yaml";
        let startup_yaml = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let _: Vec<serde_yaml::Value> = serde_yaml::from_str(&startup_yaml)
            .inspect_err(|_| {
                let mut f = fs::File::create(ERR_FILE).unwrap();
                f.write_all(startup_yaml.as_bytes()).unwrap();
            })
            .wrap_err(format!("YAML parser error. output written to {ERR_FILE}"))?;

        Ok(())
    }

    /// Verifies only Core responses marked authoritative can activate peer VNI
    /// imports, so an older Core cannot bypass the existing-peering policy.
    #[tokio::test]
    async fn peer_vnis_require_an_authoritative_core_marker()
    -> Result<(), Box<dyn std::error::Error>> {
        for (scenario, authoritative) in [
            // Old Core supplies peer VNIs without proving its policy filtered them.
            ("legacy Core response", false),
            // New Core explicitly certifies the list for route-target imports.
            ("authoritative Core response", true),
        ] {
            // Use a populated peer list so absence of an import proves filtering.
            let mut network_config = netconf(
                VpcVirtualizationType::Fnn,
                32,
                24,
                false,
                None,
                false,
                false,
            );
            assert!(
                network_config.tenant_interfaces[0]
                    .vpc_peer_vnis
                    .contains(&1_025_187)
            );
            network_config.vpc_peer_vnis_authoritative = authoritative;

            // Render through the real agent update path into a temporary HBN tree.
            let td = tempfile::tempdir()?;
            let hbn_root = td.path();
            fs::create_dir_all(hbn_root.join("var/support"))?;
            fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
            super::update_nvue(
                VpcVirtualizationType::Fnn,
                NvueUpdateFlavor::StartupFile {
                    hbn_root,
                    skip_post: true,
                },
                &network_config,
                &test_service_addresses(),
                HBNDeviceNames::hbn_23(),
                None,
                None,
            )
            .await?;

            // The route target must appear exactly when Core authorized the list.
            let startup_yaml = fs::read_to_string(hbn_root.join(nvue::PATH))?;
            assert_eq!(
                startup_yaml.contains("11414:1025187: {}"),
                authoritative,
                "{scenario}"
            );
        }
        Ok(())
    }

    // Builds the deprecated compatibility shape consumed by these renderer tests.
    #[allow(deprecated)]
    fn netconf(
        virtualization_type: VpcVirtualizationType,
        interface_prefix_length: u8,
        network_prefix_length: u8,
        include_network_security_group: bool,
        site_global_vpc_vni: Option<u32>,
        second_interface_l2: bool,
        include_network_host_route_and_default_leaking: bool,
    ) -> rpc::ManagedHostNetworkConfigResponse {
        // The config we received from API server
        // Admin won't be used
        let admin_interface_prefix: IpNetwork = "10.217.5.123/32".parse().unwrap();
        let admin_interface = rpc::FlatInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical.into(),
            virtual_function_id: None,
            vlan_id: 1,
            vni: 1001,
            vpc_vni: 1002,
            gateway: Some("10.217.5.123/28".to_string()),
            ip: Some("10.217.5.123".to_string()),
            interface_prefix: Some(admin_interface_prefix.to_string()),
            vpc_prefixes: vec![],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            prefix: Some("10.217.5.123/28".to_string()),
            fqdn: "myhost.forge".to_string(),
            booturl: Some("test".to_string()),
            svi_ip: None,
            tenant_vrf_loopback_ip: Some("10.217.5.124".to_string()),
            is_l2_segment: true,
            network_security_group: None,
            internal_uuid: None,
            mtu: None,
            ipv6_interface_config: None,
            vpc_routing_profile: None,
            interface_routing_profile: None,
            addresses: vec![],
        };
        assert_eq!(admin_interface.svi_ip, None);

        let interface_prefix_1: IpNetwork = format!("10.217.5.170/{interface_prefix_length}")
            .parse()
            .unwrap();
        let interface_prefix_2: IpNetwork = format!("10.217.5.162/{interface_prefix_length}")
            .parse()
            .unwrap();

        let svi_ip1: IpAddr = IpAddr::from_str("10.217.5.172").unwrap();
        let svi_ip2: IpAddr = IpAddr::from_str("10.217.5.164").unwrap();

        let vpc_peer_vnis = match virtualization_type {
            VpcVirtualizationType::EthernetVirtualizer => {
                vec![]
            }
            _ => {
                vec![1025186, 1025187]
            }
        };
        let tenant_interfaces = vec![
            rpc::FlatInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Virtual.into(),
                virtual_function_id: Some(0),
                vlan_id: 196,
                vni: 1025196,
                vpc_vni: 1025197,
                gateway: Some("10.217.5.169/29".to_string()),
                ip: Some("10.217.5.170".to_string()),
                interface_prefix: Some(interface_prefix_1.to_string()),
                vpc_prefixes: vec!["10.217.5.160/30".to_string(), "10.217.5.168/29".to_string()],
                vpc_peer_prefixes: vec!["10.217.6.176/29".to_string()],
                vpc_peer_vnis,
                prefix: Some("10.217.5.169/29".to_string()),
                fqdn: "myhost.forge.1".to_string(),
                booturl: None,
                svi_ip: get_svi_ip(
                    &Some(svi_ip1),
                    virtualization_type,
                    true,
                    network_prefix_length,
                )
                .unwrap()
                .map(|ip| ip.to_string()),
                tenant_vrf_loopback_ip: None,
                is_l2_segment: true,
                network_security_group: None,
                internal_uuid: None,
                mtu: None,
                ipv6_interface_config: None,
                vpc_routing_profile: Some(rpc::RoutingProfile {
                    route_targets_on_exports_by_prefix: vec![],
                    leak_default_route_from_underlay:
                        include_network_host_route_and_default_leaking,
                    leak_tenant_host_routes_to_underlay:
                        include_network_host_route_and_default_leaking,
                    tenant_leak_communities_accepted:
                        include_network_host_route_and_default_leaking,
                    accepted_leaks_from_underlay: if include_network_host_route_and_default_leaking
                    {
                        vec![rpc::PrefixFilterPolicyEntry {
                            prefix: "10.255.0.0/24".to_string(),
                        }]
                    } else {
                        vec![]
                    },
                    allowed_anycast_prefixes: vec![rpc::PrefixFilterPolicyEntry {
                        prefix: "5.255.254.0/24".to_string(),
                    }],
                    route_target_imports: vec![rpc_common::RouteTarget {
                        asn: 44444,
                        vni: Some(55555),
                        vni_self: false,
                    }],
                    route_targets_on_exports: vec![rpc_common::RouteTarget {
                        asn: 77415,
                        vni: Some(800),
                        vni_self: false,
                    }],
                }),
                interface_routing_profile: None,
                addresses: vec![],
            },
            rpc::FlatInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical.into(),
                virtual_function_id: None,
                vlan_id: 185,
                vni: 1025185,
                vpc_vni: 1025186,
                gateway: Some("10.217.5.161/30".to_string()),
                ip: Some("10.217.5.162".to_string()),
                interface_prefix: Some(interface_prefix_2.to_string()),
                vpc_prefixes: vec!["10.217.5.160/30".to_string(), "10.217.5.168/29".to_string()],
                vpc_peer_prefixes: vec!["10.217.6.176/29".to_string()],
                vpc_peer_vnis: vec![],
                prefix: Some("10.217.5.162/30".to_string()),
                fqdn: "myhost.forge.2".to_string(),
                booturl: None,
                svi_ip: get_svi_ip(
                    &Some(svi_ip2),
                    virtualization_type,
                    second_interface_l2,
                    network_prefix_length,
                )
                .unwrap()
                .map(|ip| ip.to_string()),
                tenant_vrf_loopback_ip: None,
                is_l2_segment: second_interface_l2,
                network_security_group: if !include_network_security_group {
                    None
                } else {
                    Some(rpc::FlatInterfaceNetworkSecurityGroupConfig {
                    id: "5b931164-d9c6-11ef-8292-232e57575621".to_string(),
                    version: "V1-1".to_string(),
                    source: rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
                    stateful_egress: true,
                    rules: vec![rpc::ResolvedNetworkSecurityGroupRule {
                        src_prefixes: vec!["0.0.0.0/0".to_string()],
                        dst_prefixes: vec!["1.0.0.0/0".to_string()],
                        rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                            id: Some("anything".to_string()),
                            direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                                .into(),
                            ipv6: false,
                            src_port_start: Some(80),
                            src_port_end: Some(81),
                            dst_port_start: Some(80),
                            dst_port_end: Some(81),
                            protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                            action: rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                            priority: 9001,
                            source_net: Some(
                                rpc::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                                    "0.0.0.0/0".to_string(),
                                ),
                            ),
                            destination_net: Some(
                                rpc::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                                    "0.0.0.0/0".to_string(),
                                ),
                            ),
                        }),
                    },
                    rpc::ResolvedNetworkSecurityGroupRule {
                        src_prefixes: vec!["0.0.0.0/0".to_string()],
                        dst_prefixes: vec!["1.0.0.0/0".to_string()],
                        rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                            id: Some("anything".to_string()),
                            direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                                .into(),
                            ipv6: false,
                            src_port_start: Some(80),
                            src_port_end: Some(81),
                            dst_port_start: Some(80),
                            dst_port_end: Some(81),
                            protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                            action: rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                            priority: 9001,
                            source_net: Some(
                                rpc::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                                    "1.0.0.0/0".to_string(),
                                ),
                            ),
                            destination_net: Some(
                                rpc::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                                    "1.0.0.0/0".to_string(),
                                ),
                            ),
                        }),
                    },
                    rpc::ResolvedNetworkSecurityGroupRule {
                        src_prefixes: vec!["0.0.0.0/0".to_string()],
                        dst_prefixes: vec!["1.0.0.0/0".to_string()],
                        rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                            id: Some("anything".to_string()),
                            direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                                .into(),
                            ipv6: false,
                            src_port_start: None,
                            src_port_end: None,
                            dst_port_start: Some(8080),
                            dst_port_end: Some(8080),
                            protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                            action: rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                            priority: 9001,
                            source_net: Some(
                                rpc::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                                    "1.0.0.0/0".to_string(),
                                ),
                            ),
                            destination_net: Some(
                                rpc::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                                    "1.0.0.0/0".to_string(),
                                ),
                            ),
                        }),
                    },
                    rpc::ResolvedNetworkSecurityGroupRule {
                        src_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:8888/128".to_string()],
                        dst_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:9999/128".to_string()],
                        rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                            id: Some("anything".to_string()),
                            direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                                .into(),
                            ipv6: true,
                            src_port_start: Some(80),
                            src_port_end: Some(81),
                            dst_port_start: Some(80),
                            dst_port_end: Some(81),
                            protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                            action: rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                            priority: 9001,
                            source_net: Some(
                                rpc::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                                    "2001:db8:3333:4444:5555:6666:7777:8888/128".to_string(),
                                ),
                            ),
                            destination_net: Some(
                                rpc::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                                    "2001:db8:3333:4444:5555:6666:7777:9999/128".to_string(),
                                ),
                            ),
                        }),
                    },
                    rpc::ResolvedNetworkSecurityGroupRule {
                        src_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:8888/128".to_string()],
                        dst_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:9999/128".to_string()],
                        rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                            id: Some("anything".to_string()),
                            direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                                .into(),
                            ipv6: true,
                            src_port_start: Some(80),
                            src_port_end: Some(81),
                            dst_port_start: Some(80),
                            dst_port_end: Some(81),
                            protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                            action: rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                            priority: 9001,
                            source_net: Some(
                                rpc::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                                    "2001:db8:3333:4444:5555:6666:7777:8888/128".to_string(),
                                ),
                            ),
                            destination_net: Some(
                                rpc::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                                    "2001:db8:3333:4444:5555:6666:7777:9999/128".to_string(),
                                ),
                            ),
                        }),
                    }],
                })
                },
                internal_uuid: None,
                mtu: None,
                ipv6_interface_config: None,
                vpc_routing_profile: Some(rpc::RoutingProfile {
                    route_targets_on_exports_by_prefix: vec![],
                    leak_default_route_from_underlay:
                        include_network_host_route_and_default_leaking,
                    leak_tenant_host_routes_to_underlay:
                        include_network_host_route_and_default_leaking,
                    tenant_leak_communities_accepted:
                        include_network_host_route_and_default_leaking,
                    accepted_leaks_from_underlay: if include_network_host_route_and_default_leaking
                    {
                        vec![rpc::PrefixFilterPolicyEntry {
                            prefix: "10.255.0.0/24".to_string(),
                        }]
                    } else {
                        vec![]
                    },
                    allowed_anycast_prefixes: vec![rpc::PrefixFilterPolicyEntry {
                        prefix: "5.255.254.0/24".to_string(),
                    }],
                    route_target_imports: vec![rpc_common::RouteTarget {
                        asn: 44444,
                        vni: Some(55555),
                        vni_self: false,
                    }],
                    route_targets_on_exports: vec![rpc_common::RouteTarget {
                        asn: 77415,
                        vni: Some(800),
                        vni_self: false,
                    }],
                }),
                interface_routing_profile: None,
                addresses: vec![],
            },
        ];

        let svi_is_some = virtualization_type == VpcVirtualizationType::Fnn;
        assert_eq!(
            tenant_interfaces[0].svi_ip.is_some(),
            svi_is_some && tenant_interfaces[0].is_l2_segment,
            "got svi_ip: {:?}",
            tenant_interfaces[0].svi_ip
        );
        assert_eq!(
            tenant_interfaces[1].svi_ip.is_some(),
            svi_is_some && tenant_interfaces[1].is_l2_segment,
            "got svi_ip 1: {:?}",
            tenant_interfaces[1].svi_ip
        );

        let netconf = rpc::ManagedHostNetworkConfig {
            loopback_ip: "10.217.5.39".to_string(),
            loopback_ip_v6: None,
            quarantine_state: None,
        };
        rpc::ManagedHostNetworkConfigResponse {
            service_interfaces: vec![],
            service_vpc_slot_inventory: None,
            asn: 4259912557,
            datacenter_asn: 11414,
            site_global_vpc_vni,
            bgp_leaf_session_password: None,
            anycast_site_prefixes: vec!["5.255.255.0/24".to_string()],
            tenant_host_asn: Some(65100),
            common_internal_route_target: Some(rpc_common::RouteTarget {
                asn: 11415,
                vni: Some(200),
                vni_self: false,
            }),
            additional_route_target_imports: vec![rpc_common::RouteTarget {
                asn: 11111,
                vni: Some(22222),
                vni_self: false,
            }],

            // This should be ignored because we've defined the routing profile on the "flat interface" config.
            routing_profile: Some(rpc::RoutingProfile {
                route_targets_on_exports_by_prefix: vec![],
                leak_default_route_from_underlay: include_network_host_route_and_default_leaking,
                leak_tenant_host_routes_to_underlay: include_network_host_route_and_default_leaking,
                tenant_leak_communities_accepted: include_network_host_route_and_default_leaking,
                accepted_leaks_from_underlay: if include_network_host_route_and_default_leaking {
                    vec![rpc::PrefixFilterPolicyEntry {
                        prefix: "111.255.0.0/24".to_string(),
                    }]
                } else {
                    vec![]
                },
                allowed_anycast_prefixes: vec![rpc::PrefixFilterPolicyEntry {
                    prefix: "5.255.254.0/24".to_string(),
                }],
                route_target_imports: vec![rpc_common::RouteTarget {
                    asn: 34444,
                    vni: Some(85555),
                    vni_self: false,
                }],
                route_targets_on_exports: vec![rpc_common::RouteTarget {
                    asn: 67415,
                    vni: Some(8000),
                    vni_self: false,
                }],
            }),

            network_security_policy_overrides: vec![rpc::ResolvedNetworkSecurityGroupRule {
                src_prefixes: vec!["7.7.7.0/24".to_string()],
                dst_prefixes: vec!["7.7.7.0/24".to_string()],
                rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                    id: Some("anything".to_string()),
                    direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                        .into(),
                    ipv6: false,
                    src_port_start: Some(80),
                    src_port_end: Some(81),
                    dst_port_start: Some(80),
                    dst_port_end: Some(81),
                    protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                    action: rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                    priority: 0,
                    source_net: Some(
                        rpc::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                            "0.0.0.0/0".to_string(),
                        ),
                    ),
                    destination_net: Some(
                        rpc::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                            "0.0.0.0/0".to_string(),
                        ),
                    ),
                }),
            }],

            // yes it's in there twice I dunno either
            dhcp_servers: vec!["10.217.5.197".to_string(), "10.217.5.197".to_string()],
            ntp_servers: vec![],
            dhcpv6_server_preference: Some(255),
            vni_device: "vxlan48".to_string(),

            managed_host_config: Some(netconf),
            managed_host_config_version: "V1-T1666644937952267".to_string(),

            use_admin_network: false,
            admin_interface: Some(admin_interface),

            tenant_interfaces,
            instance_network_config_version: "V1-T1666644937952999".to_string(),

            instance_id: Some(
                uuid::Uuid::try_from("60cef902-9779-4666-8362-c9bb4b37184f")
                    .unwrap()
                    .into(),
            ),
            remote_id: "test".to_string(),

            // For NetworkMonitor
            dpu_network_pinger_type: None,

            // For ETV:
            network_virtualization_type: None,
            vpc_vni: None,
            route_servers: vec!["172.43.0.1".to_string(), "172.43.0.2".to_string()],
            deny_prefixes: vec!["192.0.2.0/24".into(), "198.51.100.0/24".into()],
            site_fabric_prefixes: vec!["10.217.0.0/16".into()],
            site_fabric_null_routes: None,
            vpc_peer_vnis_authoritative: true,
            deprecated_deny_prefixes: vec![],
            enable_dhcp: true,
            vpc_isolation_behavior: rpc::VpcIsolationBehaviorType::VpcIsolationMutual.into(),
            host_interface_id: Some("60cef902-9779-4666-8362-c9bb4b37185f".to_string()),
            is_primary_dpu: true,
            min_dpu_functioning_links: None,
            internet_l3_vni: Some(1337),
            stateful_acls_enabled: true,
            instance: None,
            dpu_extension_services: vec![],
            astra_config: None,
            use_admin_network_changed: None,
            accepted_lldp_snapshot_fingerprint: None,
        }
    }

    #[tokio::test]
    async fn test_reset() -> Result<(), Box<dyn std::error::Error>> {
        let td = tempfile::tempdir()?;
        let hbn_root = td.path();
        fs::create_dir_all(hbn_root.join("etc/supervisor/conf.d"))?;
        fs::create_dir_all(hbn_root.join("var/support/forge-dhcp/conf"))?;

        // Create NVUE config to verify it gets cleaned up
        let nvue_dir = hbn_root.join(crate::nvue::PATH);
        fs::create_dir_all(nvue_dir.parent().unwrap())?;
        fs::write(&nvue_dir, "test nvue config")?;
        assert!(nvue_dir.exists());

        super::reset(hbn_root, true).await;

        // NVUE config should be removed
        assert!(!nvue_dir.exists());

        // check dhcp server
        let dhcp_path = hbn_root.join("etc/supervisor/conf.d/default-forge-dhcp-server.conf");
        let dhcp_contents = super::read_limited(&dhcp_path, MAX_DHCP_ACL_SIZE)
            .wrap_err(format!("failed reading {dhcp_path:?}"))?;
        assert_eq!(dhcp_contents, crate::dhcp::TMPL_EMPTY);
        Ok(())
    }

    #[test]
    fn test_parse_fdb() -> Result<(), Box<dyn std::error::Error>> {
        let json = include_str!("hbn_bridge_fdb.json");
        let out = super::parse_fdb(json)?;
        let twenty_one = out.get(&21).unwrap();
        assert_eq!(twenty_one.len(), 2); // interface both sides
        if !twenty_one.iter().any(|f| f.mac == "7e:f6:b2:b2:f0:97") {
            panic!("Expected MAC not found in vlan 21's parsed fdb");
        }
        // "permanent" were filtered out already
        assert!(!twenty_one.iter().any(|f| f.state == "permanent"));
        Ok(())
    }

    #[test]
    fn test_parse_ip_show() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"[{"ifindex":26,"ifname":"pf0vf0_if_r","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":9216,"qdisc":"mq","master":"ovs-system","operstate":"UP","group":"default","txqlen":1000,"link_type":"ether","address":"4e:1f:bd:97:23:3e","broadcast":"ff:ff:ff:ff:ff:ff","altnames":["enp3s0f0npf0sf131072"],"addr_info":[{"family":"inet6","local":"fe80::4c1f:bdff:fe97:233e","prefixlen":64,"scope":"link","valid_life_time":4294967295,"preferred_life_time":4294967295}]}]"#;
        let out: Vec<super::IpShow> = serde_json::from_str(json)?;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].address, "4e:1f:bd:97:23:3e");
        Ok(())
    }

    /// ETV must render valid YAML without FNN policy objects, so the shared
    /// fixture cannot silently select the other virtualization template.
    #[test]
    fn test_nvue_is_yaml_etv() -> Result<(), Box<dyn std::error::Error>> {
        test_nvue_is_yaml_inner(false)
    }

    /// FNN must parse its own template, including its underlay export policy;
    /// parsing ETV output would not protect FNN template syntax.
    #[test]
    fn test_nvue_is_yaml_fnnv() -> Result<(), Box<dyn std::error::Error>> {
        test_nvue_is_yaml_inner(true)
    }

    /// Render the requested virtualization mode and check a distinguishing
    /// policy node, so YAML validity is proved for the selected template.
    fn test_nvue_is_yaml_inner(is_fnn: bool) -> Result<(), Box<dyn std::error::Error>> {
        let vpc_virtualization_type = if is_fnn {
            VpcVirtualizationType::Fnn
        } else {
            VpcVirtualizationType::EthernetVirtualizer
        };

        let network_security_groups = vec![nvue::NetworkSecurityGroup {
            id: "7777f270-dd02-11ef-80d2-9f8689fc7df7".to_string(),
            stateful_egress: true,
            rules: vec![nvue::NetworkSecurityGroupRule {
                id: "6313f270-dd02-11ef-80d2-9f8689fc7df7".to_string(),
                ingress: true,
                ipv6: true,
                priority: 1001,
                can_match_any_protocol: false,
                can_be_stateful: true,
                protocol: "TCP".to_string(),
                src_prefixes: vec!["2.2.2.2/24".to_string()],
                dst_prefixes: vec!["3.3.3.3/24".to_string()],
                src_port_start: Some(5),
                src_port_end: Some(50),
                dst_port_start: Some(8),
                dst_port_end: Some(80),
                action: "PERMIT".to_string(),
            }],
        }];

        let networks = vec![nvue::PortConfig {
            network_security_group_id: Some(network_security_groups[0].id.clone()),
            interface_name: HBNDeviceNames::hbn_23().reps[0].to_string(),
            is_phy: true,
            host_ip: "10.217.4.70".to_string(),
            host_route: "10.217.4.70/32".to_string(),
            host_ipv6: None,
            host_ipv6_route: None,
            vlan: 123u16,
            vni: Some(5555),
            l3_vni: Some(7777),
            gateway_cidrs: vec!["10.217.4.65/26".to_string()],
            svi_ip: if is_fnn {
                Some("10.217.4.66/26".to_string())
            } else {
                None
            },
            tenant_vrf_loopback_ip: if is_fnn {
                Some("10.217.4.67".to_string())
            } else {
                None
            },
            vpc_prefixes: vec!["10.217.4.168/29".to_string()],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            routing_profile: None,
            interface_routing_profile: None,
            is_l2_segment: true,
            is_service: false,
            ipv6_port_config: None,
        }];
        let hostname = super::hostname().wrap_err("gethostname error")?;
        let vpc_vni = 7777;
        let conf = nvue::NvueConfig {
            bgp_leaf_session_password: None,
            is_fnn,
            vpc_virtualization_type,
            use_admin_network: true,
            tenancy_enabled: true,
            site_global_vpc_vni: None,
            loopback_ip: "10.217.5.39".parse().unwrap(),
            loopback_ip_v6: None,
            asn: 65535,
            datacenter_asn: 11414,
            anycast_site_prefixes: vec!["5.255.255.0/24".to_string()],
            tenant_host_asn: Some(65100),
            common_internal_route_target: Some(nvue::RouteTargetConfig {
                asn: 11415,
                vni: 200,
            }),
            additional_route_target_imports: vec![nvue::RouteTargetConfig {
                asn: 44444,
                vni: 55555,
            }],
            dpu_hostname: hostname.hostname,
            dpu_search_domain: hostname.search_domain,
            hbn_version: None,
            uplinks: HBNDeviceNames::hbn_23()
                .uplinks
                .into_iter()
                .map(String::from)
                .collect(),
            dhcp_servers: vec!["10.217.5.197".parse().unwrap()],
            route_servers: vec!["172.43.0.1".parse().unwrap(), "172.43.0.2".parse().unwrap()],
            deny_prefixes: vec![],
            use_vpc_isolation: false,
            site_fabric_prefixes: vec!["10.217.4.128/26".to_string()],
            stateful_acls_enabled: true,
            ct_port_configs: networks,
            ct_vrf_name: format!("vpc_{vpc_vni}"),
            ct_access_vlans: vec![nvue::VlanConfig {
                vlan_id: 123,
                network: "10.217.4.70/32".to_string(),
                ip: "10.217.4.70".to_string(),
                ipv6_vlan_config: None,
            }],
            // Global and per-VPC targets each contribute a separate rendered
            // entry, so use distinct values in this YAML syntax fixture.
            ct_routing_profile: Some(nvue::RoutingProfile {
                route_targets_on_exports_by_prefix: vec![],
                tenant_leak_communities_accepted: false,
                leak_default_route_from_underlay: false,
                leak_tenant_host_routes_to_underlay: false,
                accepted_leaks_from_underlay: vec![],
                allowed_anycast_prefixes: vec!["5.255.254.0/24".to_string()],
                route_target_imports: vec![nvue::RouteTargetConfig {
                    asn: 44444,
                    vni: 55556,
                }],
                route_targets_on_exports: vec![nvue::RouteTargetConfig {
                    asn: 11415,
                    vni: 201,
                }],
            }),

            network_security_policy_override_rules: vec![nvue::NetworkSecurityGroupRule {
                id: "5553f270-dd02-11ef-80d2-9f8689fc7df7".to_string(),
                ingress: true,
                ipv6: false,
                priority: 1,
                can_match_any_protocol: true,
                can_be_stateful: true,
                protocol: "ANY".to_string(),
                src_prefixes: vec!["7.7.7.0/24".to_string()],
                dst_prefixes: vec!["6.6.6.0/24".to_string()],
                src_port_start: Some(5),
                src_port_end: Some(50),
                dst_port_start: Some(8),
                dst_port_end: Some(80),
                action: "DENY".to_string(),
            }],

            ct_l3_vni: Some(vpc_vni),
            ct_vrf_loopback: "FNN".to_string(),
            l3_domains: vec![],
            network_security_groups,
            is_dpu_os: true,
            fmds_gateway_vlan: None,
            fmds_ipv6_gateway: None,
        };
        let startup_yaml = nvue::build(conf)?;

        let yaml_obj: Vec<serde_yaml::Value> = serde_yaml::from_str(&startup_yaml)
            .wrap_err_with(|| format!("failed to parse rendered NVUE YAML:\n{startup_yaml}"))?;
        assert_eq!(yaml_obj.len(), 2); // 'header' and 'set'
        assert_eq!(
            yaml_obj[1]["set"]["router"]["policy"]["prefix-list"]["ALLOW_TO_UNDERLAY_PREFIX_LIST"]
                .is_mapping(),
            is_fnn,
            "the requested virtualization mode must select its own template"
        );
        Ok(())
    }

    fn compare_diffed<P: AsRef<Path>>(
        p1: P,
        expected: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let left_contents = fs::read_to_string(p1.as_ref())?;
        let left_contents = left_contents.as_str();
        let right_contents = expected;
        let r = crate::util::compare_lines(left_contents, right_contents, None);
        eprint!("Diff output:\n{}", r.report());
        assert!(r.is_identical());
        Ok(())
    }

    /// Verifies generic service-address partitioning preserves order within each family.
    #[test]
    fn split_addresses_by_family_partitions_by_family() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |input: Vec<IpAddr>| -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
                split_addresses_by_family(&input)
            };
            "splits nameservers by family" {
                // Mixed input preserves the original order within each family.
                vec![
                    IpAddr::from([10, 0, 0, 1]),
                    "2001:db8::1".parse::<IpAddr>().unwrap(),
                    IpAddr::from([10, 0, 0, 2]),
                ] => (
                    vec![Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 2)],
                    vec!["2001:db8::1".parse::<Ipv6Addr>().unwrap()],
                ),
                // IPv4-only input leaves the IPv6 result empty.
                vec![IpAddr::from([10, 0, 0, 1])] => (vec![Ipv4Addr::new(10, 0, 0, 1)], vec![]),
                // IPv6-only input leaves the IPv4 result empty.
                vec!["2001:db8::1".parse::<IpAddr>().unwrap()]
                    => (vec![], vec!["2001:db8::1".parse::<Ipv6Addr>().unwrap()]),
                // Empty input produces two empty family lists.
                vec![] => (vec![], vec![]),
            }
        );
    }

    /// Verifies the Core-to-agent response preserves Preference presence and
    /// rejects values outside the shared DHCP model before either delivery path.
    #[test]
    fn dhcpv6_server_preference_validates_network_response() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::scenarios;

        scenarios!(run = |preference| {
                dhcpv6_server_preference(&rpc::ManagedHostNetworkConfigResponse {
                    dhcpv6_server_preference: preference,
                    ..Default::default()
                })
                .map_err(drop)
            };
            "legacy omission" {
                // An older Core keeps the option absent instead of acquiring NICo's new default.
                None => Yields(None),
            }
            "configured values" {
                // Explicit zero remains distinguishable from legacy omission.
                Some(0) => Yields(Some(0)),
                // The one-octet maximum survives the widened protobuf field.
                Some(255) => Yields(Some(255)),
            }
            "invalid widened value" {
                // A corrupt or future value cannot be silently truncated on delivery.
                Some(256) => Fails,
            }
        );
    }

    /// Verifies tenant RA consumes the family-neutral V6 entry, preserves the
    /// explicit addressing mode, and never broadens an allocated prefix.
    #[test]
    fn tenant_ipv6_router_advertisement_requires_mode_specific_prefix() {
        use carbide_test_support::value_scenarios;

        let v4 = rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V4.into(),
            ip: "192.0.2.10".to_string(),
            interface_prefix: "192.0.2.10/32".to_string(),
            prefix: "192.0.2.0/24".to_string(),
            ..Default::default()
        };
        let resolvers = vec![
            "2001:db8::53".parse().unwrap(),
            "2001:db8::54".parse().unwrap(),
        ];

        value_scenarios!(run = |addresses| {
                let interface = rpc::FlatInterfaceConfig {
                    addresses,
                    ..Default::default()
                };
                tenant_ipv6_router_advertisement(&interface, &resolvers).map(|advertisement| {
                    (
                        advertisement.mode,
                        advertisement.prefix,
                        advertisement.rdnss_servers,
                    )
                })
            };
            "stateful allocated /127" {
                // A tenant /128 at the second endpoint selects stateful RA for its /127.
                vec![v4, rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V6.into(),
                    ip: "2001:db8::1".to_string(),
                    interface_prefix: "2001:db8::1/128".to_string(),
                    prefix: "2001:db8::/127".to_string(),
                    ..Default::default()
                }] => Some((
                    nvue::Ipv6RouterAdvertisementMode::Stateful,
                    "2001:db8::/127".to_string(),
                    resolvers.clone(),
                )),
            }
            "explicit SLAAC prefix" {
                // Prefix-only host configuration selects SLAAC without manufacturing an address.
                vec![rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V6.into(),
                    ip: String::new(),
                    interface_prefix: "2001:db8:1::/64".to_string(),
                    prefix: "2001:db8:1::/64".to_string(),
                    ..Default::default()
                }] => Some((
                    nvue::Ipv6RouterAdvertisementMode::Slaac,
                    "2001:db8:1::/64".to_string(),
                    resolvers.clone(),
                )),
            }
            "invalid or ambiguous mode inputs" {
                // Routing-only data does not establish either tenant address mode.
                vec![rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V6.into(),
                    prefix: "2001:db8:2::/64".to_string(),
                    ..Default::default()
                }] => None,
                // The first /127 address belongs to the DPU interface, not the tenant binding.
                vec![rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V6.into(),
                    ip: "2001:db8:3::".to_string(),
                    interface_prefix: "2001:db8:3::/128".to_string(),
                    prefix: "2001:db8:3::/127".to_string(),
                    ..Default::default()
                }] => None,
                // A concrete address does not turn a SLAAC-sized prefix into stateful allocation.
                vec![rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V6.into(),
                    ip: "2001:db8:4::1".to_string(),
                    interface_prefix: "2001:db8:4::/64".to_string(),
                    prefix: "2001:db8:4::/64".to_string(),
                    ..Default::default()
                }] => None,
            }
        );
    }

    /// Verifies authoritative prefix normalization, legacy fallback, and the
    /// malformed-data precedence needed during mixed-version upgrades.
    #[test]
    fn ipv6_segment_prefix_falls_back_only_when_authoritative_data_is_absent() {
        use carbide_test_support::value_scenarios;

        let mut canonical = dual_stack_admin_interface();
        canonical
            .addresses
            .iter_mut()
            .find(|address| address.address_family == i32::from(rpc::AddressFamily::V6))
            .expect("fixture should contain canonical IPv6")
            .prefix = "2001:db8:100::70/64".to_string();
        let mut legacy = canonical.clone();
        legacy
            .addresses
            .retain(|address| address.address_family != i32::from(rpc::AddressFamily::V6));
        let mut empty_authoritative = canonical.clone();
        empty_authoritative
            .addresses
            .iter_mut()
            .find(|address| address.address_family == i32::from(rpc::AddressFamily::V6))
            .expect("fixture should contain canonical IPv6")
            .prefix = String::new();
        let legacy_prefix = "2001:db8:ffff::/64";

        value_scenarios!(run = |interface: rpc::FlatInterfaceConfig| {
                ipv6_segment_prefix(&interface, legacy_prefix)
            };
            "authoritative segment" {
                // Host bits in an authoritative prefix are normalized before rendering.
                canonical => Some("2001:db8:100::/64".to_string()),
            }
            "legacy response" {
                // Only an absent canonical V6 entry permits the sidecar fallback.
                legacy => Some(legacy_prefix.to_string()),
            }
            "empty authoritative segment" {
                // Present but empty new data must not be reinterpreted as legacy data.
                empty_authoritative => None,
            }
        );
    }

    /// Verifies admin RA requires FNN and a contained host `/128`, so the
    /// containing segment is advertised without requiring a tenant `/127`.
    /// Primary-DPU/admin-mode gating remains at its sole production call site.
    #[test]
    #[allow(deprecated)]
    fn admin_ipv6_router_advertisement_requires_valid_admin_host_projection() {
        use carbide_test_support::value_scenarios;

        let valid = dual_stack_admin_interface();
        // Retain complete canonical IPv6 while omitting its deprecated sidecar.
        let mut canonical_only = valid.clone();
        canonical_only.ipv6_interface_config = None;
        // Replace the individual host route with the shared segment to exercise validation.
        let mut invalid_host_route = valid.clone();
        invalid_host_route
            .addresses
            .iter_mut()
            .find(|address| address.address_family == i32::from(rpc::AddressFamily::V6))
            .expect("fixture should contain canonical IPv6")
            .interface_prefix = "2001:db8:100::/64".to_string();
        // Keep the host address while replacing its containing segment with an unrelated prefix.
        let mut outside_segment = valid.clone();
        outside_segment
            .addresses
            .iter_mut()
            .find(|address| address.address_family == i32::from(rpc::AddressFamily::V6))
            .expect("fixture should contain canonical IPv6")
            .prefix = "2001:db8:200::/64".to_string();
        let resolvers = vec!["2001:db8::53".parse().unwrap()];

        value_scenarios!(run = |(virtualization_type, interface)| {
                let address = canonical_ipv6_address(&interface)
                    .expect("test interface should contain canonical IPv6");
                admin_ipv6_router_advertisement(
                    virtualization_type,
                    address,
                    &resolvers,
                )
                .map(|advertisement| (
                    advertisement.prefix,
                    advertisement.mode,
                    advertisement.rdnss_servers,
                ))
            };
            "valid FNN admin SVI" {
                // Canonical host and prefix data alone enable stateful RA on the VLAN.
                (VpcVirtualizationType::Fnn, canonical_only) => Some((
                    "2001:db8:100::/64".to_string(),
                    nvue::Ipv6RouterAdvertisementMode::Stateful,
                    resolvers.clone(),
                )),
            }
            "non-FNN admin interface" {
                // ETV has no supported admin VPC RA path.
                (VpcVirtualizationType::EthernetVirtualizer, valid) => None,
            }
            "non-host interface prefix" {
                // A /64 cannot stand in for the allocated host route.
                (VpcVirtualizationType::Fnn, invalid_host_route) => None,
            }
            "host outside segment" {
                // An unrelated segment must never be advertised for this host.
                (VpcVirtualizationType::Fnn, outside_segment) => None,
            }
        );
    }

    fn validate_dhcp_config(received: DhcpConfig, expected: DhcpConfig) {
        assert_eq!(received.lease_time_secs, expected.lease_time_secs);
        assert_eq!(received.renewal_time_secs, expected.renewal_time_secs);
        assert_eq!(received.rebinding_time_secs, expected.rebinding_time_secs);
        assert_eq!(received.carbide_nameservers, expected.carbide_nameservers);
        assert_eq!(received.carbide_api_url, expected.carbide_api_url);
        assert_eq!(received.carbide_ntpservers, expected.carbide_ntpservers);
        assert_eq!(
            received.carbide_provisioning_server_ipv4,
            expected.carbide_provisioning_server_ipv4
        );
        assert_eq!(
            received.carbide_provisioning_server_ipv6,
            expected.carbide_provisioning_server_ipv6
        );
        assert_eq!(received.carbide_dhcp_server, expected.carbide_dhcp_server);
        assert_eq!(received.dhcpv6_server_id, expected.dhcpv6_server_id);
        assert_eq!(
            received.carbide_nameservers_v6,
            expected.carbide_nameservers_v6
        );
        assert_eq!(
            received.carbide_ntpservers_v6,
            expected.carbide_ntpservers_v6
        );
        assert_eq!(
            received.carbide_dhcp_server_v6,
            expected.carbide_dhcp_server_v6
        );
        assert_eq!(
            received.dhcpv6_preferred_lifetime_secs,
            expected.dhcpv6_preferred_lifetime_secs
        );
        assert_eq!(
            received.dhcpv6_valid_lifetime_secs,
            expected.dhcpv6_valid_lifetime_secs
        );
        assert_eq!(
            received.dhcpv6_server_preference,
            expected.dhcpv6_server_preference
        );
    }

    fn validate_host_config(received: HostConfig, expected: HostConfig) {
        assert_eq!(received.host_interface_id, expected.host_interface_id);

        let mut vlans_received = received.host_ip_addresses.keys().collect::<Vec<&String>>();
        let mut vlans_expected = expected.host_ip_addresses.keys().collect::<Vec<&String>>();

        vlans_expected.sort();
        vlans_received.sort();

        assert_eq!(vlans_received, vlans_expected);

        for vlan in vlans_received {
            let ip_config_received = received.host_ip_addresses.get(vlan).unwrap();
            let ip_config_expected = expected.host_ip_addresses.get(vlan).unwrap();

            assert_eq!(ip_config_received.fqdn, ip_config_expected.fqdn);
            assert_eq!(ip_config_received.booturl, ip_config_expected.booturl);
            assert_eq!(ip_config_received.gateway, ip_config_expected.gateway);
            assert_eq!(ip_config_received.address, ip_config_expected.address);
            assert_eq!(ip_config_received.prefix, ip_config_expected.prefix);
            assert_eq!(ip_config_received.ipv6, ip_config_expected.ipv6);
        }
    }

    /// Verifies the deprecated file-backed DHCP compatibility input renders
    /// dual-stack options and host state.
    #[test]
    #[allow(deprecated)]
    fn test_with_tenant_dhcp_server() -> Result<(), Box<dyn std::error::Error>> {
        // Model the API-provided admin interface with both address families.
        let admin_interface_prefix: IpNetwork = "10.217.5.123/32".parse().unwrap();
        let admin_interface = rpc::FlatInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical.into(),
            virtual_function_id: None,
            vlan_id: 1,
            vni: 1001,
            vpc_vni: 1002,
            gateway: Some("10.217.5.123".to_string()),
            ip: Some("10.217.5.123".to_string()),
            interface_prefix: Some(admin_interface_prefix.to_string()),
            vpc_prefixes: vec![],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            prefix: Some("10.217.5.123".to_string()),
            fqdn: "myhost.forge".to_string(),
            booturl: Some("test".to_string()),
            svi_ip: None,
            tenant_vrf_loopback_ip: Some("10.213.2.1".to_string()),
            is_l2_segment: true,
            network_security_group: None,
            internal_uuid: None,
            mtu: None,
            ipv6_interface_config: None,
            vpc_routing_profile: None,
            interface_routing_profile: None,
            addresses: vec![
                rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V4.into(),
                    gateway: Some("10.217.5.123".to_string()),
                    ip: "10.217.5.123".to_string(),
                    interface_prefix: admin_interface_prefix.to_string(),
                    prefix: "10.217.5.123".to_string(),
                    tenant_vrf_loopback_ip: Some("10.213.2.1".to_string()),
                    ..Default::default()
                },
                rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V6.into(),
                    ip: "2001:db8::123".to_string(),
                    interface_prefix: "2001:db8::123/128".to_string(),
                    prefix: "2001:db8::/64".to_string(),
                    ..Default::default()
                },
            ],
        };

        let mut admin_interface_with_mtu = admin_interface.clone();
        admin_interface_with_mtu.mtu = Some(1500);

        assert_eq!(admin_interface.svi_ip, None);

        let interface_prefix_1: IpNetwork = "10.217.5.170/32".parse().unwrap();
        let interface_prefix_2: IpNetwork = "10.217.5.162/32".parse().unwrap();
        let svi_ip: IpAddr = IpAddr::from_str("10.217.5.2").unwrap();

        let tenant_interfaces = vec![
            rpc::FlatInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Virtual.into(),
                virtual_function_id: Some(0),
                vlan_id: 196,
                vni: 1025196,
                vpc_vni: 1025197,
                gateway: Some("10.217.5.169".to_string()),
                ip: Some("10.217.5.170".to_string()),
                interface_prefix: Some(interface_prefix_1.to_string()),
                vpc_prefixes: vec!["10.217.5.160/30".to_string(), "10.217.5.168/29".to_string()],
                vpc_peer_prefixes: vec!["10.217.6.176/29".to_string()],
                vpc_peer_vnis: vec![],
                prefix: Some("10.217.5.169/29".to_string()),
                fqdn: "myhost.forge.1".to_string(),
                booturl: None,
                svi_ip: get_svi_ip(&Some(svi_ip), VpcVirtualizationType::Fnn, true, 24)
                    .unwrap()
                    .map(|x| x.to_string()),
                tenant_vrf_loopback_ip: Some("10.213.2.1".to_string()),
                is_l2_segment: true,
                network_security_group: None,
                internal_uuid: None,
                mtu: None,
                ipv6_interface_config: None,
                vpc_routing_profile: None,
                interface_routing_profile: None,
                addresses: vec![],
            },
            rpc::FlatInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical.into(),
                virtual_function_id: None,
                vlan_id: 185,
                vni: 1025185,
                vpc_vni: 1025186,
                gateway: Some("10.217.5.161".to_string()),
                ip: Some("10.217.5.162".to_string()),
                interface_prefix: Some(interface_prefix_2.to_string()),
                vpc_prefixes: vec!["10.217.5.160/30".to_string(), "10.217.5.168/29".to_string()],
                vpc_peer_prefixes: vec!["10.217.6.176/29".to_string()],
                vpc_peer_vnis: vec![],
                prefix: Some("10.217.5.162/30".to_string()),
                fqdn: "myhost.forge.2".to_string(),
                booturl: None,
                svi_ip: get_svi_ip(&Some(svi_ip), VpcVirtualizationType::Fnn, false, 24)
                    .unwrap()
                    .map(|x| x.to_string()),
                tenant_vrf_loopback_ip: Some("10.213.2.1".to_string()),
                is_l2_segment: true,
                network_security_group: None,
                internal_uuid: None,
                mtu: None,
                ipv6_interface_config: None,
                vpc_routing_profile: None,
                interface_routing_profile: None,
                addresses: vec![rpc::InterfaceAddressConfig {
                    address_family: rpc::AddressFamily::V6.into(),
                    ip: "2001:db8:185::1".to_string(),
                    interface_prefix: "2001:db8:185::1/128".to_string(),
                    prefix: "2001:db8:185::/127".to_string(),
                    ..Default::default()
                }],
            },
        ];

        assert_eq!(
            tenant_interfaces[0].svi_ip,
            Some("10.217.5.2/24".to_string())
        );
        assert_eq!(tenant_interfaces[1].svi_ip, None);

        let netconf = rpc::ManagedHostNetworkConfig {
            loopback_ip: "10.217.5.39".to_string(),
            loopback_ip_v6: None,
            quarantine_state: None,
        };

        let dhcp_config = DhcpConfig {
            carbide_nameservers: vec![Ipv4Addr::from([10, 1, 1, 1])],
            carbide_ntpservers: vec![
                Ipv4Addr::from([127, 0, 0, 1]),
                Ipv4Addr::from([127, 0, 0, 2]),
                Ipv4Addr::from([127, 0, 0, 3]),
            ],
            carbide_nameservers_v6: vec!["2001:db8::53".parse().unwrap()],
            carbide_ntpservers_v6: vec!["2001:db8::123".parse().unwrap()],
            carbide_provisioning_server_ipv4: Some(Ipv4Addr::from([10, 0, 0, 1])),
            carbide_provisioning_server_ipv6: Some("2001:db8::80".parse().unwrap()),
            lease_time_secs: 604800,
            renewal_time_secs: 3600,
            rebinding_time_secs: 432000,
            carbide_api_url: None,
            carbide_dhcp_server: Some(Ipv4Addr::from([10, 217, 5, 39])),
            dhcpv6_preferred_lifetime_secs: dhcp::DHCPV6_PREFERRED_LIFETIME_SECS,
            dhcpv6_valid_lifetime_secs: dhcp::DHCPV6_VALID_LIFETIME_SECS,
            dhcpv6_server_preference: Some(0),
            ..Default::default()
        };

        let mut network_config = rpc::ManagedHostNetworkConfigResponse {
            service_interfaces: vec![],
            service_vpc_slot_inventory: None,
            bgp_leaf_session_password: None,
            site_global_vpc_vni: None,
            asn: 4259912557,
            datacenter_asn: 11414,
            common_internal_route_target: Some(rpc_common::RouteTarget {
                asn: 11415,
                vni: Some(200),
                vni_self: false,
            }),
            additional_route_target_imports: vec![rpc_common::RouteTarget {
                asn: 11111,
                vni: Some(22222),
                vni_self: false,
            }],

            anycast_site_prefixes: vec!["5.255.255.0/24".to_string()],
            tenant_host_asn: Some(65100),
            routing_profile: Some(rpc::RoutingProfile {
                route_targets_on_exports_by_prefix: vec![],
                tenant_leak_communities_accepted: false,
                leak_default_route_from_underlay: false,
                leak_tenant_host_routes_to_underlay: false,
                accepted_leaks_from_underlay: vec![],
                allowed_anycast_prefixes: vec![rpc::PrefixFilterPolicyEntry {
                    prefix: "5.255.254.0/24".to_string(),
                }],
                route_target_imports: vec![rpc_common::RouteTarget {
                    asn: 44444,
                    vni: Some(55555),
                    vni_self: false,
                }],
                route_targets_on_exports: vec![rpc_common::RouteTarget {
                    asn: 77415,
                    vni: Some(800),
                    vni_self: false,
                }],
            }),

            // yes it's in there twice I dunno either
            dhcp_servers: vec!["10.217.5.197".to_string(), "10.217.5.197".to_string()],
            ntp_servers: vec![],
            dhcpv6_server_preference: Some(0),
            vni_device: "vxlan48".to_string(),

            managed_host_config: Some(netconf),
            managed_host_config_version: "V1-T1666644937952267".to_string(),

            use_admin_network: true,
            admin_interface: Some(admin_interface),

            tenant_interfaces,
            instance_network_config_version: "V1-T1666644937952999".to_string(),

            network_security_policy_overrides: vec![],
            instance_id: Some(
                uuid::Uuid::try_from("60cef902-9779-4666-8362-c9bb4b37184f")
                    .wrap_err("uuid::try_from")?
                    .into(),
            ),
            remote_id: "test".to_string(),

            dpu_network_pinger_type: None,

            network_virtualization_type: None,
            vpc_vni: None,
            route_servers: vec!["172.43.0.1".to_string(), "172.43.0.2".to_string()],
            deny_prefixes: vec!["192.0.2.0/24".into(), "198.51.100.0/24".into()],
            site_fabric_prefixes: vec!["10.217.0.0/16".into()],
            site_fabric_null_routes: None,
            vpc_peer_vnis_authoritative: true,
            vpc_isolation_behavior: rpc::VpcIsolationBehaviorType::VpcIsolationMutual.into(),
            deprecated_deny_prefixes: vec![],
            enable_dhcp: true,
            host_interface_id: Some("60cef902-9779-4666-8362-c9bb4b37185f".to_string()),
            min_dpu_functioning_links: None,
            is_primary_dpu: true,
            internet_l3_vni: Some(1337),
            stateful_acls_enabled: true,
            instance: None,
            dpu_extension_services: vec![],
            astra_config: None,
            use_admin_network_changed: None,
            accepted_lldp_snapshot_fingerprint: None,
        };

        // Include sibling backup files in the fixture's cleanup on every exit.
        let directory = tempfile::tempdir()?;
        let f = tempfile::NamedTempFile::new_in(directory.path())?;
        let fp = FPath(f.path().to_owned());

        let g = tempfile::NamedTempFile::new_in(directory.path())?;
        let gp = FPath(PathBuf::from(g.path()));

        let h = tempfile::NamedTempFile::new_in(directory.path())?;
        let hp = FPath(PathBuf::from(h.path()));

        let i = tempfile::NamedTempFile::new_in(directory.path())?;
        let ip = FPath(PathBuf::from(i.path()));

        let service_addrs = ServiceAddresses {
            pxe_ips: vec![
                "2001:db8::80".parse().unwrap(),
                IpAddr::from([10, 0, 0, 1]),
                "2001:db8::81".parse().unwrap(),
            ],
            ntpservers: vec![
                IpAddr::from([127, 0, 0, 1]),
                IpAddr::from([127, 0, 0, 2]),
                IpAddr::from([127, 0, 0, 3]),
                "2001:db8::123".parse().unwrap(),
            ],
            nameservers: vec![IpAddr::from([10, 1, 1, 1]), "2001:db8::53".parse().unwrap()],
        };

        let mut host_config_str =
            dhcp::build_server_host_config(network_config.clone(), &HBNDeviceNames::pre_23())?;
        assert!(!host_config_str.contains("mtu"));
        assert!(host_config_str.contains("ipv6:"));
        assert!(host_config_str.contains("2001:db8::123"));

        let mut network_config2 = network_config.clone();
        network_config2.admin_interface = Some(admin_interface_with_mtu);

        host_config_str =
            dhcp::build_server_host_config(network_config2.clone(), &HBNDeviceNames::pre_23())?;
        assert!(host_config_str.contains("mtu: 1500"));
        match write_dhcp_test_files(
            &fp,
            &super::DhcpServerPaths {
                server: gp.clone(),
                config: hp.clone(),
                host_config: ip.clone(),
            },
            &network_config,
            &service_addrs,
            &HBNDeviceNames::pre_23(),
        ) {
            Err(err) => {
                panic!("write_dhcp_test_files error: {err}");
            }
            Ok(false) => {
                panic!("write_dhcp_test_files says the config didn't change, that's wrong");
            }
            Ok(true) => {
                // success
            }
        }
        let dhcp_contents = super::read_limited(g.path(), MAX_DHCP_ACL_SIZE)?;
        assert!(dhcp_contents.contains("vlan1"));

        let dhcp_config_received: DhcpConfig =
            serde_yaml::from_str(&super::read_limited(h.path(), MAX_DHCP_ACL_SIZE)?)?;
        validate_dhcp_config(dhcp_config_received, dhcp_config);

        let dhcp_host_config: HostConfig =
            serde_yaml::from_str(&super::read_limited(i.path(), MAX_DHCP_ACL_SIZE)?)?;
        assert_eq!(
            dhcp_host_config
                .host_ip_addresses
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["vlan1"]
        );
        let admin_ipv6 = dhcp_host_config.host_ip_addresses["vlan1"]
            .ipv6
            .as_ref()
            .expect("admin DHCP host entry should contain IPv6");
        assert_eq!(admin_ipv6.address, Some("2001:db8::123".parse().unwrap()));
        assert_eq!(admin_ipv6.prefix, "2001:db8::123/128");
        validate_host_config(
            dhcp_host_config,
            HostConfig::try_from(network_config.clone(), "pf0hpf_sf", "pf0vf", "_sf", true)?,
        );

        // tenant host config.
        network_config.use_admin_network = false;

        host_config_str =
            dhcp::build_server_host_config(network_config.clone(), &HBNDeviceNames::pre_23())?;
        assert!(!host_config_str.contains("mtu"));

        network_config2 = network_config.clone();
        network_config2.tenant_interfaces[0].mtu = Some(1500);
        network_config2.tenant_interfaces[1].mtu = Some(1500);
        host_config_str =
            dhcp::build_server_host_config(network_config2, &HBNDeviceNames::pre_23())?;
        assert!(host_config_str.contains("mtu: 1500"));

        let service_addrs = ServiceAddresses {
            pxe_ips: vec![IpAddr::from([10, 0, 0, 1])],
            ntpservers: vec![],
            nameservers: vec![IpAddr::from([10, 1, 1, 1]), "2001:db8::53".parse().unwrap()],
        };
        match write_dhcp_test_files(
            &fp,
            &super::DhcpServerPaths {
                server: gp,
                config: hp,
                host_config: ip,
            },
            &network_config,
            &service_addrs,
            &HBNDeviceNames::pre_23(),
        ) {
            Err(err) => {
                panic!("write_dhcp_test_files error: {err}");
            }
            Ok(false) => {
                panic!("write_dhcp_test_files says the config didn't change, that's wrong");
            }
            Ok(true) => {
                // success
            }
        }
        let dhcp_config = DhcpConfig {
            carbide_nameservers: vec![Ipv4Addr::from([10, 1, 1, 1])],
            carbide_ntpservers: vec![],
            carbide_provisioning_server_ipv4: Some(Ipv4Addr::from([10, 0, 0, 1])),
            lease_time_secs: 604800,
            renewal_time_secs: 3600,
            rebinding_time_secs: 432000,
            carbide_api_url: None,
            carbide_dhcp_server: Some(Ipv4Addr::from([10, 217, 5, 39])),
            dhcpv6_preferred_lifetime_secs: dhcp::DHCPV6_PREFERRED_LIFETIME_SECS,
            dhcpv6_valid_lifetime_secs: dhcp::DHCPV6_VALID_LIFETIME_SECS,
            carbide_nameservers_v6: vec!["2001:db8::53".parse().unwrap()],
            dhcpv6_server_preference: Some(0),
            ..Default::default()
        };
        let dhcp_contents = super::read_limited(g.path(), MAX_DHCP_ACL_SIZE)?;
        assert!(dhcp_contents.contains("vlan196"));
        assert!(dhcp_contents.contains("vlan185"));

        let dhcp_config_received: DhcpConfig =
            serde_yaml::from_str(&super::read_limited(h.path(), MAX_DHCP_ACL_SIZE)?)?;
        validate_dhcp_config(dhcp_config_received, dhcp_config);

        let dhcp_host_config: HostConfig =
            serde_yaml::from_str(&super::read_limited(i.path(), MAX_DHCP_ACL_SIZE)?)?;
        assert!(
            dhcp_host_config
                .host_ip_addresses
                .values()
                .any(|interface| {
                    interface.ipv6.as_ref().is_some_and(|ipv6| {
                        ipv6.address == Some("2001:db8:185::1".parse().unwrap())
                            && ipv6.prefix == "2001:db8:185::1/128"
                    })
                })
        );
        validate_host_config(
            dhcp_host_config,
            HostConfig::try_from(network_config, "pf0hpf_sf", "pf0vf", "_sf", true)?,
        );

        Ok(())
    }

    fn write_dhcp_test_files(
        relay: &FPath,
        server: &super::DhcpServerPaths,
        config: &rpc::ManagedHostNetworkConfigResponse,
        addresses: &ServiceAddresses,
        devices: &HBNDeviceNames,
    ) -> eyre::Result<bool> {
        let prepared = super::prepare_dhcp_server_config(config, addresses, devices)?;
        let files = prepared.files(relay, server, &FPath(relay.with_ext("NVUE")))?;
        let changed = files.iter().any(|file| file.previous != file.next);
        for file in files {
            file.write(file.next.as_deref())?;
        }
        Ok(changed)
    }

    #[test]
    fn test_dhcp_server_config_errors_without_ipv4_pxe() -> Result<(), Box<dyn std::error::Error>> {
        let netconf = rpc::ManagedHostNetworkConfig {
            loopback_ip: "10.217.5.39".to_string(),
            loopback_ip_v6: None,
            quarantine_state: None,
        };
        let network_config = rpc::ManagedHostNetworkConfigResponse {
            service_interfaces: vec![],
            service_vpc_slot_inventory: None,
            bgp_leaf_session_password: None,
            site_global_vpc_vni: None,
            asn: 4259912557,
            datacenter_asn: 11414,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            anycast_site_prefixes: vec![],
            tenant_host_asn: None,
            routing_profile: None,
            dhcp_servers: vec![],
            ntp_servers: vec![],
            dhcpv6_server_preference: Some(255),
            vni_device: "vxlan48".to_string(),
            managed_host_config: Some(netconf),
            managed_host_config_version: "V1-T1".to_string(),
            use_admin_network: false,
            admin_interface: None,
            tenant_interfaces: vec![],
            instance_network_config_version: "V1-T1".to_string(),
            network_security_policy_overrides: vec![],
            instance_id: None,
            remote_id: "test".to_string(),
            dpu_network_pinger_type: None,
            network_virtualization_type: None,
            vpc_vni: None,
            route_servers: vec![],
            deny_prefixes: vec![],
            site_fabric_prefixes: vec![],
            site_fabric_null_routes: None,
            vpc_peer_vnis_authoritative: false,
            vpc_isolation_behavior: rpc::VpcIsolationBehaviorType::VpcIsolationMutual.into(),
            deprecated_deny_prefixes: vec![],
            enable_dhcp: true,
            host_interface_id: None,
            min_dpu_functioning_links: None,
            is_primary_dpu: true,
            internet_l3_vni: None,
            stateful_acls_enabled: false,
            instance: None,
            dpu_extension_services: vec![],
            astra_config: None,
            use_admin_network_changed: None,
            accepted_lldp_snapshot_fingerprint: None,
        };

        let directory = tempfile::tempdir()?;
        let f = tempfile::NamedTempFile::new_in(directory.path())?;
        let fp = FPath(PathBuf::from(f.path()));

        let g = tempfile::NamedTempFile::new_in(directory.path())?;
        let gp = FPath(PathBuf::from(g.path()));

        let h = tempfile::NamedTempFile::new_in(directory.path())?;
        let hp = FPath(PathBuf::from(h.path()));

        let i = tempfile::NamedTempFile::new_in(directory.path())?;
        let ip = FPath(PathBuf::from(i.path()));

        let service_addrs = ServiceAddresses {
            pxe_ips: vec!["fd00::1".parse().unwrap()],
            ntpservers: vec![],
            nameservers: vec![IpAddr::from([10, 1, 1, 1])],
        };

        let result = write_dhcp_test_files(
            &fp,
            &super::DhcpServerPaths {
                server: gp,
                config: hp,
                host_config: ip,
            },
            &network_config,
            &service_addrs,
            &HBNDeviceNames::pre_23(),
        );

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert_eq!(
            err_msg,
            "DHCPv4 server config requires an IPv4 PXE/UEFI HTTP boot address, but none found in [fd00::1]"
        );

        Ok(())
    }

    #[test]
    fn test_cmd_return_val() {
        // Primary dpu admin network
        assert_eq!(needed_interface_state(true, true), InterfaceState::Up);

        // Primary dpu tenant network
        assert_eq!(needed_interface_state(true, false), InterfaceState::Up);

        // Primary dpu admin network
        assert_eq!(needed_interface_state(true, true), InterfaceState::Up);

        // Secondary dpu admin network
        assert_eq!(needed_interface_state(false, true), InterfaceState::Down);

        // Secondary dpu tenant network
        assert_eq!(needed_interface_state(false, false), InterfaceState::Up);
    }

    #[test]
    fn test_interface_translation() {
        let translation = InterfaceTranslationMode::Prepend("pre_".into());
        let interface_name = "i0";
        let translated_interface_name = translation.translate(interface_name);
        assert_eq!(translated_interface_name.as_str(), "pre_i0");
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn ipv6_status_preserves_slaac_prefix_without_host_address() {
        for (
            scenario,
            use_admin_network,
            ip,
            interface_prefix,
            expected_addresses,
            expected_prefixes,
        ) in [
            (
                "tenant interface with a concrete IPv6 host address",
                false,
                "2001:db8::1",
                "2001:db8::/127",
                vec!["2001:db8::1"],
                vec!["2001:db8::/127"],
            ),
            (
                "tenant SLAAC prefix without a host address",
                false,
                "",
                "2001:db8::/64",
                vec![],
                vec!["2001:db8::/64"],
            ),
            (
                "admin SLAAC prefix without a host address",
                true,
                "",
                "2001:db8::/64",
                vec![],
                vec!["2001:db8::/64"],
            ),
        ] {
            let iface = rpc::FlatInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical.into(),
                vlan_id: 100,
                ipv6_interface_config: Some(rpc::FlatInterfaceIpv6Config {
                    ip: ip.to_string(),
                    interface_prefix: interface_prefix.to_string(),
                    svi_ip: None,
                }),
                ..Default::default()
            };
            let network_config = rpc::ManagedHostNetworkConfigResponse {
                service_interfaces: vec![],
                service_vpc_slot_inventory: None,
                use_admin_network,
                admin_interface: use_admin_network.then_some(iface.clone()),
                tenant_interfaces: (!use_admin_network)
                    .then_some(vec![iface])
                    .unwrap_or_default(),
                ..Default::default()
            };

            assert!(tenant_peers(&network_config).is_empty(), "{scenario}");

            let observations =
                interfaces(&network_config, "02:00:00:00:00:01".parse().unwrap(), None)
                    .await
                    .unwrap();

            assert_eq!(observations.len(), 1, "{scenario}");
            assert_eq!(observations[0].addresses, expected_addresses, "{scenario}");
            assert_eq!(observations[0].prefixes, expected_prefixes, "{scenario}");
            assert!(observations[0].gateways.is_empty(), "{scenario}");
        }
    }
}
