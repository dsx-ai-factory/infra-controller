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
use carbide_rpc_utils::dhcp::{DhcpConfig, DhcpV6ServerId};
use eyre::WrapErr;
use mac_address::MacAddress;
use nvue_client::client::{NvueClient, NvueClientError};
use nvue_client::config::{NvueConfig, NvueConfigWithHeader};
use serde::Deserialize;
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

use crate::nvue::NetworkSecurityGroupRule;
use crate::{HBNDeviceNames, acl_rules, dhcp, hbn, nvue};

/// None of the files we deal with should be bigger than this
const MAX_EXPECTED_SIZE: u64 = 1048576; // 1 MiB

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

/// Resolve site-configured DHCPv4 NTP and DNS-discovered DHCPv6 NTP options.
fn build_dhcp_ntp_servers(
    nc: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
) -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
    // Start with the NTP servers from the service addresses, which is read from carbide-ntp.forge.
    let (mut ntpservers_v4, ntpservers_v6) = split_addresses_by_family(&service_addrs.ntpservers);

    // The site configuration contract is IPv4-only, so it replaces option 42
    // without suppressing the DNS-derived DHCPv6 option 56 fallback.
    if !nc.ntp_servers.is_empty() {
        let site_v4 = nc
            .ntp_servers
            .iter()
            .filter_map(|server| match Ipv4Addr::from_str(server) {
                Ok(address) => Some(address),
                Err(error) => {
                    tracing::debug!(
                        ntp_server = %server,
                        error = %error,
                        "Invalid IPv4 NTP server from ManagedHostNetworkConfigResponse, ignoring"
                    );
                    None
                }
            })
            .collect::<Vec<_>>();
        if !site_v4.is_empty() {
            ntpservers_v4 = site_v4;
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
    // a revision was applied, Ok(None) if the config was unchanged, and
    // otherwise passes through errors from the inner client.
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
            let revision_id = self.nvue_client.push_config(config).await?;
            self.last_applied_hash.replace(new_hash);
            Ok(revision_id)
        }
    }
}

/// Converts an RPC routing profile into the NVUE renderer model.
impl From<&rpc::RoutingProfile> for nvue::RoutingProfile {
    fn from(profile: &rpc::RoutingProfile) -> Self {
        // Preserve the API-provided routing policy without applying template concerns here.
        nvue::RoutingProfile {
            leak_default_route_from_underlay: profile.leak_default_route_from_underlay,
            leak_tenant_host_routes_to_underlay: profile.leak_tenant_host_routes_to_underlay,
            tenant_leak_communities_accepted: profile.tenant_leak_communities_accepted,
            route_target_imports: profile
                .route_target_imports
                .iter()
                .map(|rt| nvue::RouteTargetConfig {
                    asn: rt.asn,
                    vni: rt.vni,
                })
                .collect(),
            route_targets_on_exports: profile
                .route_targets_on_exports
                .iter()
                .map(|rt| nvue::RouteTargetConfig {
                    asn: rt.asn,
                    vni: rt.vni,
                })
                .collect(),
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
        }
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

/// Builds stateful RA inputs for the existing primary-DPU admin VLAN SVI.
///
/// The canonical address entry keeps the host `/128` distinct from the
/// containing segment so the host route cannot become the advertised prefix.
fn admin_ipv6_router_advertisement(
    virtualization_type: VpcVirtualizationType,
    interface: &FlatInterfaceConfig,
    address: &rpc::InterfaceAddressConfig,
    rdnss_servers: &[Ipv6Addr],
) -> Option<nvue::Ipv6RouterAdvertisementConfig> {
    // The caller supplies admin ports only on the primary DPU. Keep the
    // render-side guard for the FNN L2 SVI that can actually emit RA.
    if virtualization_type != VpcVirtualizationType::Fnn || !interface.is_l2_segment {
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

/// Update the NVUE network config, returning whether NVUE applied a change.
/// With `StartupFile` and `skip_post`, only save the desired file and return
/// whether that file was replaced. Errors from saving or applying the desired
/// configuration are returned to the caller.
// The fetcher projects `addresses` into these compatibility fields before rendering.
#[allow(deprecated)]
pub(super) async fn update_nvue(
    vpc_virtualization_type: VpcVirtualizationType,
    update_flavor: NvueUpdateFlavor<'_>,
    nc: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
    hbn_device_names: HBNDeviceNames,
    supplemental_config: Option<&str>,
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

    let (has_stateful_nsg, network_security_groups) =
        build_network_security_group_rules(&nc.tenant_interfaces)?;

    // If we aren't on the admin network _or_ if we are the primary DPU
    // then we should be enabled for tenancy (i.e. VRFs and related config)
    let tenancy_enabled = !nc.use_admin_network || nc.is_primary_dpu;

    let physical_name = hbn_device_names.reps[0].to_string();
    let (_, rdnss_servers) = split_addresses_by_family(&service_addrs.nameservers);
    let networks = if nc.use_admin_network {
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
                gateway_cidr: admin_interface.gateway.clone().unwrap_or_default(),
                ipv6_port_config: admin_ipv6.map(|ipv6| nvue::Ipv6PortConfig {
                    gateway_cidr: normalized_ipv6_segment_prefix(ipv6)
                        .map(|prefix| prefix.to_string())
                        .unwrap_or_default(),
                    svi_ip: ipv6.svi_ip.clone(),
                    router_advertisement: admin_ipv6_router_advertisement(
                        vpc_virtualization_type,
                        admin_interface,
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
                tenant_vrf_loopback_ip: admin_interface.tenant_vrf_loopback_ip.clone(),
                network_security_group_id: None, // NSGs are not applied on the admin network.
                routing_profile: admin_interface
                    .vpc_routing_profile
                    .as_ref()
                    .map(nvue::RoutingProfile::from),
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
                gateway_cidr: net.gateway.clone().unwrap_or_default(),
                ipv6_port_config: net.ipv6_interface_config.as_ref().map(|v6| {
                    // New configs derive the DPU address from Core's one
                    // authoritative prefix. Only configs without a V6 entry
                    // retain the legacy sidecar mapping during upgrades.
                    let gateway_cidr =
                        ipv6_segment_prefix(net, &v6.interface_prefix).unwrap_or_default();
                    nvue::Ipv6PortConfig {
                        gateway_cidr,
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
                tenant_vrf_loopback_ip: net.tenant_vrf_loopback_ip.clone(),
                network_security_group_id: net
                    .network_security_group
                    .as_ref()
                    .map(|n| n.id.clone()),
                routing_profile: net
                    .vpc_routing_profile
                    .as_ref()
                    .map(nvue::RoutingProfile::from),
                interface_routing_profile: net
                    .interface_routing_profile
                    .as_ref()
                    .map(nvue::InterfaceRoutingProfile::from),
                is_l2_segment: net.is_l2_segment,
            });
        }
        ifs
    };

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
        common_internal_route_target: nc.common_internal_route_target.map(|rt| {
            nvue::RouteTargetConfig {
                asn: rt.asn,
                vni: rt.vni,
            }
        }),
        additional_route_target_imports: nc
            .additional_route_target_imports
            .iter()
            .map(|rt| nvue::RouteTargetConfig {
                asn: rt.asn,
                vni: rt.vni,
            })
            .collect(),
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
        ct_routing_profile: nc.routing_profile.as_ref().map(nvue::RoutingProfile::from),
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
            // Cleanup non-NVUE ACL files
            // We can remove this once az01 is upgraded
            cleanup_old_acls(hbn_root);

            // Write the extra ACL config
            let path_acl = FPath(hbn_root.join(nvue::PATH_ACL));
            path_acl.cleanup();
            let mut rules = NVUED_BLOCK_RULE.to_string();
            rules.push_str(acl_rules::ARP_SUPPRESSION_RULE);
            match write(rules, &path_acl, "NVUE ACL", false) {
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
            let path = FPath(hbn_root.join(nvue::PATH));
            path.cleanup();
            // If switching to the admin network, we want to just force the write.
            // We've seen a past incident where a tenant managed to create a config
            // that exceeded MAX_EXPECTED_SIZE.  Because of the diff check failing, it
            // also prevented a successful termination because the NVUE config couldn't
            // be switched to the admin network.
            let file_changed = write(
                next_contents,
                &path,
                "NVUE",
                nc.use_admin_network
                    && path.0.exists()
                    && path.0.metadata()?.len() > MAX_EXPECTED_SIZE,
            )
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

fn build_network_security_group_rules(
    interfaces: &[FlatInterfaceConfig],
) -> eyre::Result<(bool, Vec<nvue::NetworkSecurityGroup>)> {
    let mut network_security_groups = HashMap::<String, nvue::NetworkSecurityGroup>::new();
    let mut has_stateful = false;
    for iface in interfaces {
        if let Some(ref nsg) = iface.network_security_group {
            let rules = nsg
                .rules
                .iter()
                .map(NetworkSecurityGroupRule::try_from)
                .collect::<Result<Vec<NetworkSecurityGroupRule>, _>>()?;

            has_stateful |= nsg.stateful_egress;

            network_security_groups
                .entry(nsg.id.clone())
                .or_insert_with(|| nvue::NetworkSecurityGroup {
                    id: nsg.id.clone(),
                    rules,
                    stateful_egress: nsg.stateful_egress,
                });
        }
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

    if pxe_ip_v4.is_none() && pxe_ip_v6.is_none() {
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

/// Send DHCP and host configuration through `UpdateAndReloadConfig`.
///
/// Matching configuration and interfaces do not restart a running server, so
/// this is safe to call on every tick. Returns `Ok(true)` after the control
/// update succeeds. With no interfaces, the server stages the configuration
/// without applying it; success does not guarantee every listener has bound.
async fn update_dhcp_via_grpc(
    grpc_addr: &str,
    network_config: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
    hbn_device_names: HBNDeviceNames,
    interface_translation_mode: Option<&InterfaceTranslationMode>,
) -> eyre::Result<bool> {
    let dhcp_config = build_dhcp_server_config(network_config, service_addrs)?;
    let mut host_config = carbide_rpc_utils::dhcp::HostConfig::try_from(
        network_config.clone(),
        hbn_device_names.reps[0],
        hbn_device_names.virt_rep_begin,
        hbn_device_names.sf_id,
        false,
    )?;

    // Update the interface names if translation is needed.
    if let Some(translation_mode) = interface_translation_mode {
        host_config.host_ip_addresses = host_config
            .host_ip_addresses
            .into_iter()
            .map(|(name, info)| (translation_mode.translate(&name), info))
            .collect();
    }

    let interfaces: Vec<String> = host_config.host_ip_addresses.keys().cloned().collect();

    crate::dhcp_server_grpc_client::update_and_reload(
        grpc_addr,
        dhcp_config,
        Some(host_config),
        interfaces,
    )
    .await
    .wrap_err_with(|| format!("update_dhcp_via_grpc({grpc_addr})"))?;
    Ok(true)
}

/// Updates DHCP server configuration using either the gRPC or file-write path.
///
/// When `dhcp_grpc_server` is `Some`, delegates to [`update_dhcp_via_grpc`]
/// which pushes YAML configs to the dhcp-server control service directly.
///
/// When `dhcp_grpc_server` is `None`, validates IPv6-only configuration with
/// the installed server before replacing files and restarting supervisord.
/// Write-only updates (`skip_post`) defer that check until application. Stopping
/// a secondary admin DHCP server preserves DHCP configuration and identity
/// without preparing a replacement. Failed writes or reloads attempt to
/// restore the previous files and report any restoration failure.
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
        if stop_server {
            return stop_dhcp_via_grpc(addr).await;
        }

        let needed_state = needed_interface_state(
            network_config.is_primary_dpu,
            network_config.use_admin_network,
        );

        if needed_state == InterfaceState::Up {
            return update_dhcp_via_grpc(
                addr,
                network_config,
                service_addrs,
                hbn_device_names,
                interface_translation_mode,
            )
            .await;
        }

        return Ok(false);
    }

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
    match write(dhcp::blank(), &dhcp_relay_path, "DHCP relay", false) {
        Ok(true) => post_actions.push(PostAction {
            cmd: dhcp::RELOAD_CMD,
        }),
        Ok(false) => {}
        Err(err) => errs.push(format!("Write blank DHCP relay: {err:#}")),
    }
    let dhcp_server_path = FPath(hbn_root.join(dhcp::SERVER_PATH));
    match write(dhcp::blank(), &dhcp_server_path, "DHCP server", false) {
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
        let vlan_intf = nc
            .admin_interface
            .as_ref()
            .map(|x| format!("vlan{}", x.vlan_id))
            .ok_or_else(|| eyre::eyre!("admin interface missing on admin network"))?;
        vec![vlan_intf]
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
        let previous = match read_limited(live_config) {
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
        let previous = match read_limited(path) {
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
                write(contents.to_owned(), &self.path, "DHCP configuration", false)?;
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
    force: bool,
) -> eyre::Result<bool> {
    let path_tmp = path.temp();
    fs::write(&path_tmp, next_contents.clone())
        .wrap_err_with(|| format!("fs::write {}", path_tmp.display()))?;

    if !force {
        let path_tmp_size = path_tmp.metadata()?.len();
        if path_tmp_size > MAX_EXPECTED_SIZE {
            return Err(eyre::eyre!(
                "new content for '{}' would exceed MAX_EXPECTED_SIZE: {} > {}",
                path_tmp.display(),
                path_tmp_size,
                MAX_EXPECTED_SIZE
            ));
        }
    }

    let has_changed = if !force && path.0.exists() {
        let current = read_limited(path).wrap_err_with(|| format!("read_limited {path}"))?;
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

// std::fs::read_to_string but limited to 4k bytes for safety
fn read_limited<P: AsRef<Path>>(path: P) -> io::Result<String> {
    let f = File::open(path)?;
    let l = f.metadata()?.len();
    if l > MAX_EXPECTED_SIZE {
        return Err(io::Error::other(
            // ErrorKind::FileTooLarge but it's nightly only
            format!("{l} > {MAX_EXPECTED_SIZE} bytes"),
        ));
    }
    // in case it changes as we read
    let mut f_limit = f.take(MAX_EXPECTED_SIZE);
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

    use ::rpc::{common as rpc_common, forge as rpc};
    use carbide_network::virtualization::{VpcVirtualizationType, get_svi_ip};
    use carbide_rpc_utils::dhcp::{DhcpConfig, HostConfig};
    use eyre::WrapErr;
    use ipnetwork::IpNetwork;

    use super::*;
    use crate::ethernet_virtualization::{
        InterfaceState, ServiceAddresses, needed_interface_state,
    };
    use crate::{HBNDeviceNames, dhcp, nvue};

    /// Supplies stable service discovery results to NVUE tests so RDNSS and
    /// DHCPv6 can be compared against one agent-local source.
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

    #[tokio::test]
    async fn ipv6_only_file_update_waits_for_compatible_binary() -> eyre::Result<()> {
        use std::os::unix::fs::PermissionsExt;

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
            if stage != "ipv4-old-binary" {
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

    /// Verifies IPv4 site NTP overrides do not suppress DNS-derived DHCPv6 NTP.
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
            }
            "empty configuration fallback" {
                // With no site values, both service-provided families survive.
                vec![] => (
                    vec![Ipv4Addr::from([192, 0, 2, 20])],
                    vec!["2001:db8::20".parse::<Ipv6Addr>().unwrap()],
                ),
            }
            "invalid-address fallback" {
                // An invalid site IPv4 value retains both service-provided families.
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

    #[tokio::test]
    async fn startup_file_retries_after_interrupted_apply() -> eyre::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        // Separate processes model a restart while sharing the saved YAML and
        // fake NVUE state. Only each child's PATH points at the fake `crictl`.
        if let Some(root) = std::env::var_os("NVUE_STARTUP_TEST_ROOT") {
            let root = PathBuf::from(root);
            let stage = std::env::var("NVUE_STARTUP_TEST_STAGE")?;
            let virtualization_type = VpcVirtualizationType::EthernetVirtualizer;
            let network_config = netconf(virtualization_type, 32, 24, false, None, true, false);
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

        run_startup_file_attempt(root, "retry").await?;
        assert_eq!(fs::read_to_string(root.join("applied.yaml"))?, desired);
        let successful_retry =
            format!("{attempted_apply}{attempted_apply}supervisorctl restart nl2doca\n");
        assert_eq!(fs::read_to_string(root.join("commands"))?, successful_retry);

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

    /// Verifies a realistic dual-stack admin response drives the existing SVI,
    /// explicit RA target, resolver changes, and complete IPv6 withdrawal.
    /// Host routes must be originated and withdrawn with that state so routed reachability follows it.
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
            )
            .await?
        );
        let output = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let docs: serde_yaml::Value = serde_yaml::from_str(&output)?;
        let set = &docs.as_sequence().expect("two YAML documents")[1]["set"];
        let vlan = &set["interface"]["vlan123"];
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
            )
            .await?
        );
        let replacement = fs::read_to_string(hbn_root.join(nvue::PATH))?;
        let replacement_docs: serde_yaml::Value = serde_yaml::from_str(&replacement)?;
        let replacement_set =
            &replacement_docs.as_sequence().expect("two YAML documents")[1]["set"];
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
                        vni: 55555,
                    }],
                    route_targets_on_exports: vec![rpc_common::RouteTarget {
                        asn: 77415,
                        vni: 800,
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
                        vni: 55555,
                    }],
                    route_targets_on_exports: vec![rpc_common::RouteTarget {
                        asn: 77415,
                        vni: 800,
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
                vni: 200,
            }),
            additional_route_target_imports: vec![rpc_common::RouteTarget {
                asn: 11111,
                vni: 22222,
            }],

            // This should be ignored because we've defined the routing profile on the "flat interface" config.
            routing_profile: Some(rpc::RoutingProfile {
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
                    vni: 85555,
                }],
                route_targets_on_exports: vec![rpc_common::RouteTarget {
                    asn: 67415,
                    vni: 8000,
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
        let dhcp_contents =
            super::read_limited(&dhcp_path).wrap_err(format!("failed reading {dhcp_path:?}"))?;
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

    #[test]
    fn test_nvue_is_yaml_etv() -> Result<(), Box<dyn std::error::Error>> {
        test_nvue_is_yaml_inner(false)
    }

    #[test]
    fn test_nvue_is_yaml_fnnv() -> Result<(), Box<dyn std::error::Error>> {
        test_nvue_is_yaml_inner(true)
    }

    fn test_nvue_is_yaml_inner(is_fnn: bool) -> Result<(), Box<dyn std::error::Error>> {
        let vpc_virtualization_type = VpcVirtualizationType::EthernetVirtualizer;

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
            gateway_cidr: "10.217.4.65/26".to_string(),
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
            ct_routing_profile: Some(nvue::RoutingProfile {
                tenant_leak_communities_accepted: false,
                leak_default_route_from_underlay: false,
                leak_tenant_host_routes_to_underlay: false,
                accepted_leaks_from_underlay: vec![],
                allowed_anycast_prefixes: vec!["5.255.254.0/24".to_string()],
                route_target_imports: vec![nvue::RouteTargetConfig {
                    asn: 44444,
                    vni: 55555,
                }],
                route_targets_on_exports: vec![nvue::RouteTargetConfig {
                    asn: 11415,
                    vni: 200,
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
        };
        let startup_yaml = nvue::build(conf)?;

        const ERR_FILE: &str = "/tmp/test_nvue_startup.yaml";
        let yaml_obj: Vec<serde_yaml::Value> = serde_yaml::from_str(&startup_yaml)
            .inspect_err(|_| {
                let mut f = fs::File::create(ERR_FILE).unwrap();
                f.write_all(startup_yaml.as_bytes()).unwrap();
            })
            .wrap_err(format!("YAML parser error. output written to {ERR_FILE}"))?;
        assert_eq!(yaml_obj.len(), 2); // 'header' and 'set'
        Ok(())
    }

    fn compare_diffed<P: AsRef<Path>>(
        p1: P,
        expected: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let left_contents = super::read_limited(p1.as_ref())?;
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

    /// Verifies admin RA accepts only an FNN VLAN with a contained host `/128`.
    /// Primary-DPU/admin-mode gating remains at its sole production call site.
    #[test]
    #[allow(deprecated)]
    fn admin_ipv6_router_advertisement_requires_valid_admin_host_projection() {
        use carbide_test_support::value_scenarios;

        let valid = dual_stack_admin_interface();
        let mut canonical_only = valid.clone();
        canonical_only.ipv6_interface_config = None;
        let mut invalid_host_route = valid.clone();
        invalid_host_route
            .addresses
            .iter_mut()
            .find(|address| address.address_family == i32::from(rpc::AddressFamily::V6))
            .expect("fixture should contain canonical IPv6")
            .interface_prefix = "2001:db8:100::/64".to_string();
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
                    &interface,
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
                // ETV has no supported admin VPC SVI RA path.
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
                vni: 200,
            }),
            additional_route_target_imports: vec![rpc_common::RouteTarget {
                asn: 11111,
                vni: 22222,
            }],

            anycast_site_prefixes: vec!["5.255.255.0/24".to_string()],
            tenant_host_asn: Some(65100),
            routing_profile: Some(rpc::RoutingProfile {
                tenant_leak_communities_accepted: false,
                leak_default_route_from_underlay: false,
                leak_tenant_host_routes_to_underlay: false,
                accepted_leaks_from_underlay: vec![],
                allowed_anycast_prefixes: vec![rpc::PrefixFilterPolicyEntry {
                    prefix: "5.255.254.0/24".to_string(),
                }],
                route_target_imports: vec![rpc_common::RouteTarget {
                    asn: 44444,
                    vni: 55555,
                }],
                route_targets_on_exports: vec![rpc_common::RouteTarget {
                    asn: 77415,
                    vni: 800,
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
        let dhcp_contents = super::read_limited(g.path())?;
        assert!(dhcp_contents.contains("vlan1"));

        let dhcp_config_received: DhcpConfig =
            serde_yaml::from_str(&super::read_limited(h.path())?)?;
        validate_dhcp_config(dhcp_config_received, dhcp_config);

        let dhcp_host_config: HostConfig = serde_yaml::from_str(&super::read_limited(i.path())?)?;
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
        let dhcp_contents = super::read_limited(g.path())?;
        assert!(dhcp_contents.contains("vlan196"));
        assert!(dhcp_contents.contains("vlan185"));

        let dhcp_config_received: DhcpConfig =
            serde_yaml::from_str(&super::read_limited(h.path())?)?;
        validate_dhcp_config(dhcp_config_received, dhcp_config);

        let dhcp_host_config: HostConfig = serde_yaml::from_str(&super::read_limited(i.path())?)?;
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

    #[test]
    fn test_stop_server_matches_needed_state_down() {
        // `update_dhcp` short-circuits to `stop_dhcp_via_grpc` when
        // `use_admin_network && !is_primary_dpu`. That condition must remain
        // identical to "needed_interface_state == Down". If either condition
        // changes independently, the new `if needed_state == Up { ... } else
        // { Ok(false) }` branch in update_dhcp becomes reachable and the
        // invariant in this test pins the divergence.
        for &is_primary in &[true, false] {
            for &use_admin in &[true, false] {
                let stop_server = use_admin && !is_primary;
                let needed_down =
                    needed_interface_state(is_primary, use_admin) == InterfaceState::Down;
                assert_eq!(
                    stop_server, needed_down,
                    "stop_server flag must match needed_state==Down (is_primary_dpu={is_primary}, use_admin_network={use_admin})"
                );
            }
        }
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
