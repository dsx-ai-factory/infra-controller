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
use std::io::Read;
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
use carbide_network::virtualization::{VpcVirtualizationType, build_dual_stack_list};
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

/// Stores addresses of dependent services that the DHCP module announces.
/// Note that these can apply to both IPv4 and IPv6; pxe_ips is actually
/// UEFI HTTP boot in this case, and NTP is still NTP. We should be able
/// to leverage this struct even in DHCPv6 land (whereas other things don't
/// really carry through to DHCPv6).
pub(super) struct ServiceAddresses {
    pub(super) pxe_ips: Vec<IpAddr>,
    pub(super) ntpservers: Vec<IpAddr>,
    pub(super) nameservers: Vec<IpAddr>,
}

/// Split a dual-stack nameserver list into its IPv4 and IPv6 members, so the
/// gRPC and file-write DHCP-config paths derive both families the same way.
fn split_nameservers_by_family(nameservers: &[IpAddr]) -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
    nameservers
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

fn build_dhcp_ntp_servers(
    nc: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
) -> Vec<Ipv4Addr> {
    // Start with the NTP servers from the service addresses, which is read from carbide-ntp.forge.
    let mut ntp_servers = service_addrs
        .ntpservers
        .iter()
        .filter_map(|x| match x {
            IpAddr::V4(x) => Some(*x),
            _ => None,
        })
        .collect::<Vec<Ipv4Addr>>();

    // If the site has configured NTP servers, use them instead.
    if !nc.ntp_servers.is_empty() {
        let site_ntp_servers: Vec<Ipv4Addr> = nc.ntp_servers
        .iter()
        .filter_map(|s| match IpAddr::from_str(s) {
            Ok(IpAddr::V4(ip)) => Some(ip),
            Ok(IpAddr::V6(_)) => {
                tracing::debug!(
                    ntp_server = %s,
                    "IPv6 NTP server from ManagedHostNetworkConfigResponse is ignored for DHCPv4 config"
                );
                None
            }
            Err(e) => {
                tracing::debug!(
                    ntp_server = %s,
                    error = %e,
                    "Invalid NTP server IP from ManagedHostNetworkConfigResponse, ignoring"
                );
                None
            }
        })
        .collect();

        if !site_ntp_servers.is_empty() {
            ntp_servers = site_ntp_servers;
        }
    }

    ntp_servers
}

/// How we tell HBN to notice the new file we wrote
#[derive(Debug)]
struct PostAction {
    cmd: &'static str,
    path: FPath,
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

/// Update the NVUE network config. Returns Ok(true) if the configuration changed, and
/// Ok(false) if not.
// The fetcher projects `addresses` into these compatibility fields before rendering.
#[allow(deprecated)]
pub(super) async fn update_nvue(
    vpc_virtualization_type: VpcVirtualizationType,
    update_flavor: NvueUpdateFlavor<'_>,
    nc: &rpc::ManagedHostNetworkConfigResponse,
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
        vec![nvue::VlanConfig {
            vlan_id: admin_interface.vlan_id,
            network: admin_interface.interface_prefix.clone().unwrap_or_default(),
            ip: admin_interface.ip.clone().unwrap_or_default(),
            ipv6_vlan_config: admin_interface.ipv6_interface_config.as_ref().map(|v6| {
                nvue::Ipv6VlanConfig {
                    network: v6.interface_prefix.clone(),
                    ip: v6.ip.clone(),
                }
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
    let networks = if nc.use_admin_network {
        if nc.is_primary_dpu {
            let admin_interface = nc
                .admin_interface
                .as_ref()
                .ok_or_else(|| eyre::eyre!("missing admin_interface"))?;
            vec![nvue::PortConfig {
                interface_name: physical_name,
                is_phy: true,
                host_ip: admin_interface.ip.clone().unwrap_or_default(),
                host_route: admin_interface.interface_prefix.clone().unwrap_or_default(),
                host_ipv6: admin_interface
                    .ipv6_interface_config
                    .as_ref()
                    .map(|v6| v6.ip.clone()),
                host_ipv6_route: admin_interface
                    .ipv6_interface_config
                    .as_ref()
                    .map(|v6| v6.interface_prefix.clone()),
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
                ipv6_port_config: admin_interface.ipv6_interface_config.as_ref().map(|v6| {
                    nvue::Ipv6PortConfig {
                        gateway_cidr: v6.interface_prefix.clone(),
                        svi_ip: v6.svi_ip.clone(),
                    }
                }),
                vpc_prefixes: admin_interface.vpc_prefixes.clone(),
                vpc_peer_prefixes: admin_interface.vpc_peer_prefixes.clone(),
                vpc_peer_vnis: admin_interface.vpc_peer_vnis.clone(),
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

            // For stateful FNN interfaces with IPv6, the address configured on
            // the DPU is the network address of the /127 linknet (the ::0 end).
            // The ::1 end is the host. SLAAC instead carries the selected /64
            // without a concrete host address.
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
                    nvue::Ipv6PortConfig {
                        gateway_cidr: v6.interface_prefix.clone(),
                        svi_ip: v6.svi_ip.clone(),
                    }
                }),
                vpc_prefixes: net.vpc_prefixes.clone(),
                vpc_peer_prefixes: net.vpc_peer_prefixes.clone(),
                vpc_peer_vnis: net.vpc_peer_vnis.clone(),
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
        site_fabric_prefixes: nc.site_fabric_prefixes.clone(),
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
            if !write(
                next_contents,
                &path,
                "NVUE",
                nc.use_admin_network
                    && path.0.exists()
                    && path.0.metadata()?.len() > MAX_EXPECTED_SIZE,
            )
            .wrap_err(format!("NVUE config at {path}"))?
            {
                // config didn't change OR we are switching to the admin network.
                return Ok(false);
            };

            if !skip_post {
                // Apply only when NVUE reports semantic diff.
                return nvue::apply(hbn_root, &path).await;
            }
            Ok(true)
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

async fn do_post(
    skip_post: bool,
    post_actions: Vec<PostAction>,
    mut errs: Vec<String>,
) -> eyre::Result<bool> {
    let has_changes = !post_actions.is_empty();
    if !skip_post {
        for post in post_actions {
            match hbn::run_in_container_shell(post.cmd).await {
                Ok(_) => {
                    let path_bak = post.path.backup();
                    if path_bak.exists()
                        && let Err(err) = fs::remove_file(&path_bak)
                    {
                        errs.push(format!(
                            "remove .BAK on success {}: {err:#}",
                            path_bak.display()
                        ));
                    }
                }
                Err(err) => {
                    errs.push(format!("running reload cmd '{}': {err:#}", post.cmd));

                    // If reload failed we won't be using the new config. Move it out of the way..
                    let path_tmp = post.path.temp();
                    if let Err(err) = fs::rename(&post.path, &path_tmp) {
                        errs.push(format!(
                            "rename {} to {} on error: {err:#}",
                            post.path,
                            path_tmp.display()
                        ));
                    }
                    // .. and copy the old one back.
                    // This also ensures that we retry writing the config on subsequent runs.
                    let path_bak = post.path.backup();
                    if path_bak.exists()
                        && let Err(err) = fs::rename(&path_bak, &post.path)
                    {
                        errs.push(format!(
                            "rename {} to {}, reverting on error: {err:#}",
                            path_bak.display(),
                            post.path
                        ));
                    }
                }
            }
        }
    }

    let err_message = errs.join(", ");
    if !err_message.is_empty() {
        eyre::bail!(err_message);
    }
    Ok(has_changes)
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

/// Sends the current DHCP server config to the dhcp-server process via gRPC.
///
/// Builds YAML representations of [`DhcpConfig`] and [`HostConfig`] from the
/// supplied network config and service addresses, then calls `UpdateConfig`
/// followed by `ReloadConfig` on the remote control service.  The server only
/// restarts when the content has actually changed (see server-side diffing in
/// `grpc_server.rs`), so this is safe to call on every agent tick.
///
/// Returns `Ok(true)` on success (matching the convention of the file-write
/// path) so callers can treat both paths uniformly.
async fn update_dhcp_via_grpc(
    grpc_addr: &str,
    network_config: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
    hbn_device_names: HBNDeviceNames,
    interface_translation_mode: Option<&InterfaceTranslationMode>,
) -> eyre::Result<bool> {
    let Some(mh_nc) = &network_config.managed_host_config else {
        eyre::bail!("loopback IP is missing. can't write dhcp-server config");
    };
    let loopback_ip: Ipv4Addr = mh_nc.loopback_ip.parse()?;

    let (nameservers_v4, nameservers_v6) = split_nameservers_by_family(&service_addrs.nameservers);

    let ntpservers_v4 = build_dhcp_ntp_servers(network_config, service_addrs);

    let pxe_ip_v4 = service_addrs
        .pxe_ips
        .iter()
        .find_map(|x| match x {
            IpAddr::V4(x) => Some(*x),
            _ => None,
        })
        .ok_or_else(|| {
            eyre::eyre!(
                "DHCPv4 server config requires an IPv4 PXE/UEFI HTTP boot address, but none found in {:?}",
                service_addrs.pxe_ips
            )
        })?;

    let dhcp_config = carbide_rpc_utils::dhcp::DhcpConfig::from_forge_dhcp_config(
        pxe_ip_v4,
        ntpservers_v4,
        nameservers_v4,
        nameservers_v6,
        loopback_ip,
    )?;
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
/// When `dhcp_grpc_server` is `None`, falls back to the original behaviour:
/// writes config files into the HBN container and triggers a supervisord
/// restart via [`do_post`] if any file changed.
///
/// Returns `Ok(true)` if a reload was triggered, `Ok(false)` if configs were
/// already up-to-date.
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
    let mut has_cleaned_dhcp_relay_config = path_dhcp_relay.cleanup();
    has_cleaned_dhcp_relay_config = has_cleaned_dhcp_relay_config || path_dhcp_relay_nvue.cleanup();

    // Delete NVUE relay config in case we used that previously
    let _ = fs::remove_file(path_dhcp_relay_nvue);

    // Start DHCP Server in HBN.
    let post_action = match write_dhcp_v4_server_config(
        &path_dhcp_relay,
        &paths_dhcp_server,
        network_config,
        service_addrs,
        &hbn_device_names,
    ) {
        Ok(true) if stop_server => PostAction {
            path: paths_dhcp_server.server,
            cmd: dhcp::STOP_DHCP_SERVER,
        },
        Ok(true) => PostAction {
            path: paths_dhcp_server.server,
            cmd: dhcp::RELOAD_DHCP_SERVER,
        },
        Ok(false) => {
            // If we deleted an old relay config we need to reload to stop the relay running
            if has_cleaned_dhcp_relay_config {
                PostAction {
                    path: paths_dhcp_server.server,
                    cmd: dhcp::RELOAD_DHCP_SERVER,
                }
            } else {
                return Ok(false);
            }
        }
        Err(err) => eyre::bail!("write dhcp server config file: {err:#}"),
    };

    do_post(skip_post, vec![post_action], vec![]).await
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
            path: dhcp_relay_path,
            cmd: dhcp::RELOAD_CMD,
        }),
        Ok(false) => {}
        Err(err) => errs.push(format!("Write blank DHCP relay: {err:#}")),
    }
    let dhcp_server_path = FPath(hbn_root.join(dhcp::SERVER_PATH));
    match write(dhcp::blank(), &dhcp_server_path, "DHCP server", false) {
        Ok(true) => post_actions.push(PostAction {
            path: dhcp_server_path,
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

// In case DHCP server has to be configured in HBN,
// 1. stop dhcp-relay
// 2. Copy dhcp_config file
// 3. Copy host_config file
// 4. Reload supervisord
//
// This is currently scoped to IPv4 only, and there are
// a few IPv4-specific checks for things like NTP servers,
// UEFI HTTP/PXE IP, and nameservers below.
fn write_dhcp_v4_server_config(
    dhcp_relay_path: &FPath,
    dhcp_server_path: &DhcpServerPaths,
    nc: &rpc::ManagedHostNetworkConfigResponse,
    service_addrs: &ServiceAddresses,
    hbn_device_names: &HBNDeviceNames,
) -> eyre::Result<bool> {
    match write(dhcp::blank(), dhcp_relay_path, "blank DHCP relay", false) {
        Ok(true) => {
            dhcp_relay_path.del("BAK");
        }
        Ok(false) => {}
        Err(err) => tracing::warn!(
            %dhcp_relay_path,
            error = format!("{err:#}"),
            "Write blank DHCP relay"
        ),
    }

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

    let Some(mh_nc) = &nc.managed_host_config else {
        return Err(eyre::eyre!(
            "loopback IP is missing. can't write dhcp-server config"
        ));
    };

    let loopback_ip = mh_nc.loopback_ip.parse()?;

    // Split the dual-stack nameservers by family: the IPv4 set drives the
    // DHCPv4 options written here, while the IPv6 set is held in the config for
    // the eventual DHCPv6 / RA consumer (inert in this path for now).
    let (nameservers_v4, nameservers_v6) = split_nameservers_by_family(&service_addrs.nameservers);

    let ntpservers_v4 = build_dhcp_ntp_servers(nc, service_addrs);

    let pxe_ip_v4 = service_addrs
        .pxe_ips
        .iter()
        .find_map(|x| match x {
            IpAddr::V4(x) => Some(*x),
            _ => None,
        })
        .ok_or_else(|| {
            eyre::eyre!("DHCPv4 server config requires an IPv4 PXE/UEFI HTTP boot address, but none found in {:?}", service_addrs.pxe_ips)
        })?;

    let mut has_changes = false;

    let next_contents = dhcp::build_server_supervisord_config(dhcp::DhcpServerSupervisordConfig {
        interfaces,
        autostart: (!nc.use_admin_network || nc.is_primary_dpu),
    })?;
    match write(
        next_contents,
        &dhcp_server_path.server,
        "DHCP server",
        false,
    ) {
        Ok(true) => {
            has_changes = true;
            dhcp_server_path.server.del("BAK");
        }
        Ok(false) => {}
        Err(err) => tracing::error!(
            dhcp_server_path = %dhcp_server_path.server,
            error = format!("{err:#}"),
            "Write DHCP server"
        ),
    }

    let next_contents = dhcp::build_server_config(
        pxe_ip_v4,
        ntpservers_v4,
        nameservers_v4,
        nameservers_v6,
        loopback_ip,
    )?;
    match write(
        next_contents,
        &dhcp_server_path.config,
        "DHCP server config",
        false,
    ) {
        Ok(true) => {
            has_changes = true;
            dhcp_server_path.config.del("BAK");
        }
        Ok(false) => {}
        Err(err) => tracing::error!(
            dhcp_server_config_path = %dhcp_server_path.config,
            error = format!("{err:#}"),
            "Write DHCP server config"
        ),
    }

    let next_contents = dhcp::build_server_host_config(nc.clone(), hbn_device_names)?;
    match write(
        next_contents,
        &dhcp_server_path.host_config,
        "DHCP server host config",
        false,
    ) {
        Ok(true) => {
            has_changes = true;
            dhcp_server_path.host_config.del("BAK");
        }
        Ok(false) => {}
        Err(err) => tracing::error!(
            dhcp_server_host_config_path = %dhcp_server_path.host_config,
            error = format!("{err:#}"),
            "Write DHCP server host config"
        ),
    }

    Ok(has_changes)
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
    /// `.BAK` is the backup so that we can rollback if the reload command fails.
    /// It should either be removed (success) or renamed back to the main file (failure).
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

    #[test]
    fn test_build_dhcp_ntp_servers() {
        let service_addrs = ServiceAddresses {
            pxe_ips: vec![],
            ntpservers: vec![IpAddr::from([192, 0, 2, 20])],
            nameservers: vec![],
        };
        let nc = rpc::ManagedHostNetworkConfigResponse {
            ntp_servers: vec!["198.51.100.1".to_string(), "198.51.100.2".to_string()],
            ..Default::default()
        };

        let out = build_dhcp_ntp_servers(&nc, &service_addrs);
        assert_eq!(
            out,
            vec![
                Ipv4Addr::from([198, 51, 100, 1]),
                Ipv4Addr::from([198, 51, 100, 2])
            ]
        );
    }

    #[test]
    fn test_build_dhcp_ntp_servers_fallback() {
        let service_addrs = ServiceAddresses {
            pxe_ips: vec![],
            ntpservers: vec![IpAddr::from([192, 0, 2, 20])],
            nameservers: vec![],
        };

        let empty_nc = rpc::ManagedHostNetworkConfigResponse::default();

        assert_eq!(
            build_dhcp_ntp_servers(&empty_nc, &service_addrs),
            vec![Ipv4Addr::from([192, 0, 2, 20])]
        );

        let invalid_nc = rpc::ManagedHostNetworkConfigResponse {
            ntp_servers: vec!["not-an-ip".to_string(), "2001:db8::1".to_string()],
            ..Default::default()
        };

        assert_eq!(
            build_dhcp_ntp_servers(&invalid_nc, &service_addrs),
            vec![Ipv4Addr::from([192, 0, 2, 20])]
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

    #[test]
    fn split_nameservers_by_family_partitions_by_family() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |input: Vec<IpAddr>| -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
                split_nameservers_by_family(&input)
            };
            "splits nameservers by family" {
                vec![
                    IpAddr::from([10, 0, 0, 1]),
                    "2001:db8::1".parse::<IpAddr>().unwrap(),
                    IpAddr::from([10, 0, 0, 2]),
                ] => (
                    vec![Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 2)],
                    vec!["2001:db8::1".parse::<Ipv6Addr>().unwrap()],
                ),
                vec![IpAddr::from([10, 0, 0, 1])] => (vec![Ipv4Addr::new(10, 0, 0, 1)], vec![]),
                vec!["2001:db8::1".parse::<IpAddr>().unwrap()]
                    => (vec![], vec!["2001:db8::1".parse::<Ipv6Addr>().unwrap()]),
                vec![] => (vec![], vec![]),
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
        assert_eq!(received.carbide_dhcp_server, expected.carbide_dhcp_server);
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
        }
    }

    // Exercises the DHCP renderer's deprecated compatibility input.
    #[test]
    #[allow(deprecated)]
    fn test_with_tenant_dhcp_server() -> Result<(), Box<dyn std::error::Error>> {
        // The config we received from API server
        // Admin won't be used
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
            addresses: vec![],
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
                addresses: vec![],
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
            carbide_provisioning_server_ipv4: Ipv4Addr::from([10, 0, 0, 1]),
            lease_time_secs: 604800,
            renewal_time_secs: 3600,
            rebinding_time_secs: 432000,
            carbide_api_url: None,
            carbide_dhcp_server: Ipv4Addr::from([10, 217, 5, 39]),
            ..Default::default()
        };

        let mut network_config = rpc::ManagedHostNetworkConfigResponse {
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

        let f = tempfile::NamedTempFile::new()?;
        let fp = FPath(f.path().to_owned());

        let g = tempfile::NamedTempFile::new()?;
        let gp = FPath(PathBuf::from(g.path()));

        let h = tempfile::NamedTempFile::new()?;
        let hp = FPath(PathBuf::from(h.path()));

        let i = tempfile::NamedTempFile::new()?;
        let ip = FPath(PathBuf::from(i.path()));

        let service_addrs = ServiceAddresses {
            pxe_ips: vec![IpAddr::from([10, 0, 0, 1])],
            ntpservers: vec![
                IpAddr::from([127, 0, 0, 1]),
                IpAddr::from([127, 0, 0, 2]),
                IpAddr::from([127, 0, 0, 3]),
            ],
            nameservers: vec![IpAddr::from([10, 1, 1, 1])],
        };

        let mut host_config_str =
            dhcp::build_server_host_config(network_config.clone(), &HBNDeviceNames::pre_23())?;
        assert!(!host_config_str.contains("mtu"));

        let mut network_config2 = network_config.clone();
        network_config2.admin_interface = Some(admin_interface_with_mtu);

        host_config_str =
            dhcp::build_server_host_config(network_config2.clone(), &HBNDeviceNames::pre_23())?;
        assert!(host_config_str.contains("mtu: 1500"));
        match super::write_dhcp_v4_server_config(
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
                panic!("write_dhcp_server error: {err}");
            }
            Ok(false) => {
                panic!("write_dhcp_server says the config didn't change, that's wrong");
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
            nameservers: vec![IpAddr::from([10, 1, 1, 1])],
        };
        match super::write_dhcp_v4_server_config(
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
                panic!("write_dhcp_server error: {err}");
            }
            Ok(false) => {
                panic!("write_dhcp_server says the config didn't change, that's wrong");
            }
            Ok(true) => {
                // success
            }
        }
        let dhcp_config = DhcpConfig {
            carbide_nameservers: vec![Ipv4Addr::from([10, 1, 1, 1])],
            carbide_ntpservers: vec![],
            carbide_provisioning_server_ipv4: Ipv4Addr::from([10, 0, 0, 1]),
            lease_time_secs: 604800,
            renewal_time_secs: 3600,
            rebinding_time_secs: 432000,
            carbide_api_url: None,
            carbide_dhcp_server: Ipv4Addr::from([10, 217, 5, 39]),
            ..Default::default()
        };
        let dhcp_contents = super::read_limited(g.path())?;
        assert!(dhcp_contents.contains("vlan196"));
        assert!(dhcp_contents.contains("vlan185"));

        let dhcp_config_received: DhcpConfig =
            serde_yaml::from_str(&super::read_limited(h.path())?)?;
        validate_dhcp_config(dhcp_config_received, dhcp_config);

        let dhcp_host_config: HostConfig = serde_yaml::from_str(&super::read_limited(i.path())?)?;
        validate_host_config(
            dhcp_host_config,
            HostConfig::try_from(network_config, "pf0hpf_sf", "pf0vf", "_sf", true)?,
        );

        Ok(())
    }

    // test_dhcp_server_config_errors_without_ipv4_pxe is more or less
    // a copypasta of other testing above, and its purpose in life is
    // to make sure we get the [expected] error when passing a list of
    // IPs to the DHCPv4 config builder, and no IPv4 addresses exist
    // to build config against. This should really only happen in an
    // IPv6-only environment, which would be really impressive.
    #[test]
    fn test_dhcp_server_config_errors_without_ipv4_pxe() -> Result<(), Box<dyn std::error::Error>> {
        let netconf = rpc::ManagedHostNetworkConfig {
            loopback_ip: "10.217.5.39".to_string(),
            loopback_ip_v6: None,
            quarantine_state: None,
        };
        let network_config = rpc::ManagedHostNetworkConfigResponse {
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

        let f = tempfile::NamedTempFile::new()?;
        let fp = FPath(PathBuf::from(f.path()));

        let g = tempfile::NamedTempFile::new()?;
        let gp = FPath(PathBuf::from(g.path()));

        let h = tempfile::NamedTempFile::new()?;
        let hp = FPath(PathBuf::from(h.path()));

        let i = tempfile::NamedTempFile::new()?;
        let ip = FPath(PathBuf::from(i.path()));

        let service_addrs = ServiceAddresses {
            pxe_ips: vec!["fd00::1".parse().unwrap()],
            ntpservers: vec![],
            nameservers: vec![IpAddr::from([10, 1, 1, 1])],
        };

        let result = super::write_dhcp_v4_server_config(
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
        assert!(
            err_msg.contains("IPv4 PXE/UEFI HTTP boot address"),
            "Expected error about missing IPv4, got: {err_msg}"
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
