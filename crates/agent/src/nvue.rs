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

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv6Addr};
use std::path::Path;

use ::rpc::forge as rpc;
use carbide_network::ip::prefix::IpNet;
use carbide_network::sanitized_mac;
use carbide_network::virtualization::VpcVirtualizationType;
use eyre::WrapErr;
use gtmpl_derive::Gtmpl;
use mac_address::MacAddress;
use serde::Deserialize;

pub const PATH: &str = "var/support/nvue_startup.yaml";
pub const SAVE_PATH: &str = "etc/nvue.d/startup.yaml";
pub const PATH_ACL: &str = "etc/cumulus/acl/policy.d/70-forge_nvue.rules";

const TMPL_ETV_WITH_NVUE: &str = include_str!("../templates/nvue_startup_etv.conf");
const TMPL_FNN: &str = include_str!("../templates/nvue_startup_fnn.conf");

/// Returns the NVUE template for the given virtualization type.
pub fn template_for(vtype: VpcVirtualizationType) -> eyre::Result<&'static str> {
    match vtype {
        VpcVirtualizationType::EthernetVirtualizer
        | VpcVirtualizationType::EthernetVirtualizerWithNvue => Ok(TMPL_ETV_WITH_NVUE),
        VpcVirtualizationType::Fnn => Ok(TMPL_FNN),
        // Flat VPCs attach instances via a plain NIC (the host's
        // primary fabric interface is not a DPU), so there's no NVUE
        // template to render for them -- this function is the DPU
        // agent's template selector, and Flat instances don't run
        // through a DPU agent at all.
        //
        // (NICo today doesn't model mixed-mode hosts that have a NIC
        // primary plus secondary DPUs used for VFs; if that ever
        // becomes a target, the dispatch here would need rethinking.)
        VpcVirtualizationType::Flat => {
            Err(eyre::eyre!("flat VPC virtualization type not supported",))
        }
    }
}

/// This value is added to the priority value specified
/// by users for their NSG rules.
const NETWORK_SECURITY_GROUP_RULE_PRIORITY_START: u32 = 2000;

const VPC_ISOLATION_RULE_INDEX_START: usize = 10;
const SITE_FABRIC_RULE_INDEX_START: usize = 1000;

/// Deduplicated ACL prefixes and the first index available after their rules.
struct PreparedAclPrefixes {
    prefixes: Vec<Prefix>,
    next_index: usize,
}

/// Builds a stable CIDR union and resolves duplicate indices within one ACL namespace.
fn deduplicate_acl_prefixes(
    prefixes: impl IntoIterator<Item = Prefix>,
) -> eyre::Result<PreparedAclPrefixes> {
    let mut seen_prefixes = HashSet::new();

    // ACL rule order is observable, so preserve the first occurrence of each prefix.
    let mut unique_prefixes = prefixes
        .into_iter()
        .filter(|prefix| seen_prefixes.insert(prefix.Prefix.clone()))
        .collect::<Vec<_>>();
    let mut next_index = unique_prefixes
        .iter()
        .map(|prefix| {
            prefix
                .Index
                .parse::<usize>()
                .wrap_err_with(|| format!("invalid ACL rule index {}", prefix.Index))
        })
        .collect::<eyre::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or(VPC_ISOLATION_RULE_INDEX_START - 1)
        .checked_add(1)
        .ok_or_else(|| eyre::eyre!("VPC isolation rule index overflow"))?;
    let mut used_indices = HashSet::new();

    for prefix in &mut unique_prefixes {
        // Preserve established indices; move only a distinct prefix that reuses one.
        if !used_indices.insert(prefix.Index.clone()) {
            prefix.Index = next_index.to_string();
            used_indices.insert(prefix.Index.clone());
            next_index = next_index
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("VPC isolation rule index overflow"))?;
        }
    }

    Ok(PreparedAclPrefixes {
        prefixes: unique_prefixes,
        next_index,
    })
}

/// Reassigns one ACL prefix list to a collision-free contiguous index range.
fn reindex_acl_prefixes(prefixes: &[Prefix], start_index: usize) -> eyre::Result<Vec<Prefix>> {
    prefixes
        .iter()
        .enumerate()
        .map(|(offset, prefix)| {
            let index = start_index
                .checked_add(offset)
                .ok_or_else(|| eyre::eyre!("ACL rule index overflow"))?;
            Ok(Prefix {
                Index: index.to_string(),
                Prefix: prefix.Prefix.clone(),
            })
        })
        .collect()
}

/// This limits the number of rules we'll allow into the set for
/// nvue.  We do not expect to ever hit this as the rules should
/// have been limited before they reached the DPU.  It's purpose here
/// is defense-in-depth.
///
/// We have something similar on the controller side, though the limit
/// there is likely to stay in the hundreds.  The limit here will
/// likely always be far larger because we're only concerned with
/// protecting the DPU from getting a rule set that would expand
/// into something big enough to exhaust its physical resources.
/// We want a limit small enough to protect us but big enough that we
/// don't have to remember to keep bumping this up as we decide nvue
/// can handle more rules.
///
/// *_NOTE: This is a limit per unique NSG.  Multiple interfaces could
/// each have a unique NSG associated, either directly or via different VPC
/// associations per interface._*
const NETWORK_SECURITY_GROUP_RULE_COUNT_MAX: usize = 10000;

/// `split_prefixes_by_family` splits a list of CIDR prefix strings
/// into IPv4 and IPv6 buckets. Each bucket gets sequential indices
/// starting at `start_index`. Unparseable prefixes are warned and
/// dropped (because NVUE would fail on invalid addresses anyway).
///
/// `filter` accepts a set of prefixes that can be compared to `prefixes`, and any prefix not
/// found in `filter` will be removed.
/// `start_index` should usually be set to 1 to avoid nvue config complaints when
/// using the output as part of policy generation.
fn split_prefixes_by_family(
    prefixes: &[String],
    filter: Option<&[IpNet]>,
    start_index: usize,
) -> (Vec<Prefix>, Vec<Prefix>) {
    let valid: Vec<_> = if let Some(filter) = filter {
        prefixes
            .iter()
            .filter_map(|s| match s.parse::<IpNet>() {
                Ok(net) => {
                    if filter.iter().any(|f| prefix_contains(f, &net)) {
                        Some((s.clone(), net))
                    } else {
                        tracing::warn!(
                            prefix = %net,
                            "excluding prefix outside filter list");
                        None
                    }
                }
                Err(e) => {
                    tracing::warn!(prefix = %s, error = %e, "dropping unparseable prefix");
                    None
                }
            })
            .collect()
    } else {
        prefixes
            .iter()
            .filter_map(|s| match s.parse::<IpNet>() {
                Ok(net) => Some((s.clone(), net)),
                Err(e) => {
                    tracing::warn!(prefix = %s, error = %e, "dropping unparseable prefix");
                    None
                }
            })
            .collect()
    };

    let (v4, v6): (Vec<_>, Vec<_>) = valid
        .into_iter()
        .partition(|(_, net)| matches!(net, IpNet::V4(_)));

    // NOTE: The implicit assumption here is that v4 and v6 prefixes will always be listed separately (in policies, etc),
    // and so they can share the same index starting point.
    // This is probably true, but might be good to not work under that assumption.
    let make_prefixes = |items: Vec<(String, IpNet)>| -> Vec<Prefix> {
        items
            .into_iter()
            .enumerate()
            .map(|(idx, (s, _))| Prefix {
                Index: format!("{}", idx + start_index),
                Prefix: s,
            })
            .collect()
    };

    (make_prefixes(v4), make_prefixes(v6))
}

/// Returns whether `candidate` is equal to or narrower than `allowed`.
fn prefix_contains(allowed: &IpNet, candidate: &IpNet) -> bool {
    match (allowed, candidate) {
        (IpNet::V4(allowed), IpNet::V4(candidate)) => {
            allowed.prefix_len() <= candidate.prefix_len() && allowed.contains(&candidate.network())
        }
        (IpNet::V6(allowed), IpNet::V6(candidate)) => {
            allowed.prefix_len() <= candidate.prefix_len() && allowed.contains(&candidate.network())
        }
        _ => false,
    }
}

/// Parses a list of strings to a Vec of IpNet, dropping
/// and warning about parse failures.
fn parse_prefixes(prefixes: &[String]) -> Vec<IpNet> {
    prefixes
        .iter()
        .filter_map(|prefix| match prefix.parse::<IpNet>() {
            Ok(prefix) => Some(prefix),
            Err(e) => {
                tracing::warn!(
                    prefix = %prefix,
                    error = %e,
                    "dropping unparseable VPC anycast prefix"
                );
                None
            }
        })
        .collect()
}

pub fn build(conf: NvueConfig) -> eyre::Result<String> {
    let template = template_for(conf.vpc_virtualization_type)?;
    let is_etv = matches!(
        conf.vpc_virtualization_type,
        VpcVirtualizationType::EthernetVirtualizer
            | VpcVirtualizationType::EthernetVirtualizerWithNvue
    );
    let is_dpu_os = conf.is_dpu_os;
    let fmds_gateway_vlan = conf.fmds_gateway_vlan;
    // HostInterfaces is the legacy template shape. The same host-facing values
    // are mirrored into TmplConfigPort so templates can move off HostInterfaces.
    let host_interfaces: Vec<TmplHostInterfaces> = conf
        .ct_access_vlans
        .into_iter()
        .map(|vl| TmplHostInterfaces {
            ID: vl.vlan_id,
            HostIP: vl.ip,
            HostIPv6: vl.ipv6_vlan_config.as_ref().map(|v6| v6.ip.clone()),
            HostRoute: vl.network,
        })
        .collect();

    let deprecated_routing_profile = conf.ct_routing_profile.clone();
    let deprecated_tenant_routing_profile = deprecated_routing_profile
        .as_ref()
        .map(TmplRoutingProfile::from);

    // There are two assumptions about pre-FNN...
    // 1 - ManagedHostNetworkConfigResponse only has rules inherited either from VPC or Instance,
    //     and VPC-spanning per-interface isn't possible.
    // 2 - There is only one DPU.
    //
    // Both of these are valid assumptions right now because L3 EVPN and multi-DPU are only possible with FNN,
    // We probably shouldn't warn anymore, but we probably should drop info for now.

    if conf.network_security_groups.len() > 1 {
        tracing::info!(
            "Found more than one interface with network security group applied in ManagedHostNetworkConfigResponse, so rules will be merged when FNN is not in use.",
        );
    }

    let mut nsg_id_index_map = HashMap::<String, u16>::new();
    let mut network_security_groups =
        Vec::<TmplNetworkSecurityGroup>::with_capacity(conf.network_security_groups.len());

    // Pre-FNN will still expect a merged/flattened version.
    let mut merged_ingress_ipv4_nsg_rules = vec![];
    let mut merged_ingress_ipv6_nsg_rules = vec![];
    let mut merged_egress_ipv4_nsg_rules = vec![];
    let mut merged_egress_ipv6_nsg_rules = vec![];

    let has_network_security_group = !conf.network_security_groups.is_empty();

    for (i, nsg) in conf.network_security_groups.into_iter().enumerate() {
        let idx = i.try_into().wrap_err(format!(
            "number of unique network security groups exceeds {} limit",
            u16::MAX
        ))?;

        nsg_id_index_map.insert(nsg.id, idx);

        let (ingress_ipv4_rules, egress_ipv4_rules, ingress_ipv6_rules, egress_ipv6_rules) =
            prepare_network_security_group_rules(nsg.rules)?;

        merged_ingress_ipv4_nsg_rules.extend_from_slice(&ingress_ipv4_rules);
        merged_ingress_ipv6_nsg_rules.extend_from_slice(&ingress_ipv6_rules);
        merged_egress_ipv4_nsg_rules.extend_from_slice(&egress_ipv4_rules);
        merged_egress_ipv6_nsg_rules.extend_from_slice(&egress_ipv6_rules);

        network_security_groups.push(TmplNetworkSecurityGroup {
            Index: idx,
            StatefulEgress: nsg.stateful_egress,
            IngressNetworkSecurityGroupRulesIpv4: ingress_ipv4_rules,
            IngressNetworkSecurityGroupRulesIpv6: ingress_ipv6_rules,
            EgressNetworkSecurityGroupRulesIpv4: egress_ipv4_rules,
            EgressNetworkSecurityGroupRulesIpv6: egress_ipv6_rules,
        });
    }

    let mut port_configs = Vec::with_capacity(conf.ct_port_configs.len());
    let mut vpc_configs = HashMap::<u32, TmplVpc>::new();

    // For non-FNN (ETV), tenant-wide VPC peer prefixes and VNIs come from
    // the first port config that has them. Extract these before the loop.
    let (vpc_peer_prefixes, vpc_peer_prefixes_ipv6) = conf
        .ct_port_configs
        .iter()
        .find(|p| !p.vpc_peer_prefixes.is_empty())
        .map(|p| split_prefixes_by_family(&p.vpc_peer_prefixes, None, 1))
        .unwrap_or_default();
    let vpc_peer_vnis: Vec<TmplVni> = conf
        .ct_port_configs
        .iter()
        .find(|p| !p.vpc_peer_vnis.is_empty())
        .map(|p| {
            p.vpc_peer_vnis
                .iter()
                .map(|vni| TmplVni { Vni: *vni })
                .collect()
        })
        .unwrap_or_default();

    if conf
        .ct_port_configs
        .iter()
        .filter(|p| !p.vpc_peer_prefixes.is_empty())
        .count()
        > 1
    {
        tracing::info!(
            "Found more than one tenant interface, so VPC peering details of only the first found will be used when FNN is not in use."
        );
    }

    let mut has_any_vpc_vrf_loopback = false;
    let mut fmds_gateway_matched = false;
    let mut vpc_anycast_prefix_cache = HashMap::<u32, Vec<IpNet>>::new();

    for (base_i, network) in conf.ct_port_configs.into_iter().enumerate() {
        let l3_vni = network.l3_vni.unwrap_or_default();
        // Prefer the per-port VPC routing profile, falling back to the deprecated tenant-level profile.
        let vpc_routing_profile = network
            .routing_profile
            .as_ref()
            .or(deprecated_routing_profile.as_ref());

        // This check is already done in the primary caller, but it's cheap and it matters here,
        // so just being extra defensive.
        if conf.vpc_virtualization_type == VpcVirtualizationType::Fnn
            && vpc_routing_profile.is_none()
        {
            return Err(eyre::eyre!(
                "BUG: FNN config provided without routing-profile"
            ));
        }

        // Cache the allowed_anycast_prefixes for the first VPC seen for this l3_vni
        // so that we don't keep reparsing in the event that multiple interfaces are in the same VPC.
        // l3_vni to VPC is 1:1, and the same l3_vni shouldn't see a different profile.
        // This only matters for FNN, and we've just checked that a routing profile exists
        // if this is an FNN config.
        // Here's what we expect:
        // - If a user wants to apply per-interface prefix filters, there must be a VPC-level
        //   prefix list specified to compare for validity.
        // - If the list in the VPC profile is empty, then we're going to end up intentionally
        //   filtering out any per-interface anycast prefixes.
        // - The template then, for now, provides a fallback that allows anything in site_anycast_prefixes,
        //   which will soon be ignored for FNN.
        // - Once that fallback is removed, an empty VPC profile prefix list filters out all interface-level prefixes.
        // This continues the pattern of explicit opt-in for for anycast prefix filtering by VPC profile
        // until we finally remove the site_anycast_prefix support for FNN.
        vpc_anycast_prefix_cache.entry(l3_vni).or_insert_with(|| {
            parse_prefixes(
                vpc_routing_profile
                    .map(|p| p.allowed_anycast_prefixes.as_slice())
                    .unwrap_or_default(),
            )
        });

        let routing_profile = vpc_routing_profile.map(TmplRoutingProfile::from);
        let interface_profile = network.interface_routing_profile.as_ref();
        let interface_routing_profile = {
            match (vpc_routing_profile, interface_profile) {
                // If there's an interface-specific profile, make sure it's a subset of what's
                // on the VPC.
                (Some(_), Some(interface_profile)) => {
                    let filter = vpc_anycast_prefix_cache
                        .get(&l3_vni)
                        .map(|v| v.as_slice())
                        .unwrap_or_default();

                    let (v4, v6) = split_prefixes_by_family(
                        &interface_profile.allowed_anycast_prefixes,
                        Some(filter),
                        1,
                    );

                    Some(TmplInterfaceRoutingProfile {
                        HasAllowedAnycastPrefixesIpv4: !v4.is_empty(),
                        HasAllowedAnycastPrefixesIpv6: !v6.is_empty(),
                        AllowedAnycastPrefixesIpv4: v4,
                        AllowedAnycastPrefixesIpv6: v6,
                    })
                }
                // If there's no interface-specific profile, just inherit from the VPC.
                // Let this happen in the template for now.
                (Some(_), None) => None,

                // If there's no VPC routing profile, then lock it down by default.
                // For FNN, there should always be a routing profile.
                // For the legacy ETV, it won't be referencing interface routing profiles.
                (None, _) => None,
            }
        };
        let svi_mac = vni_to_svi_mac(network.vni.unwrap_or(0))?.to_string();

        let (vpc_ipv4, vpc_ipv6) =
            split_prefixes_by_family(&network.vpc_prefixes, None, (base_i + 1) * 10);

        let port = TmplConfigPort {
            InterfaceName: network.interface_name.clone(),
            Index: format!("{}", (base_i + 1) * 10),
            VlanID: network.vlan,
            HostIP: network.host_ip.clone(),
            HostIPv6: network
                .host_ipv6
                .clone()
                .filter(|address| !address.is_empty()),
            HostRoute: network.host_route.clone(),
            HostIPv6Route: network.host_ipv6_route.clone(),
            // HasRoutingProfile means this port has an interface override; otherwise templates inherit from the VPC profile.
            HasRoutingProfile: interface_routing_profile.is_some(),
            RoutingProfile: interface_routing_profile.unwrap_or_default(),
            IsPhy: network.is_phy,
            L2VNI: network.vni.map(|x| x.to_string()).unwrap_or("".to_string()),
            IPs: std::iter::once(network.gateway_cidr.clone())
                .filter(|address| !address.is_empty())
                .collect(),
            IPsIpv6: network
                .ipv6_port_config
                .as_ref()
                .map(|v6| v6.gateway_cidr.clone())
                .into_iter()
                .collect(),
            SviIPs: network.svi_ip.iter().cloned().collect(),
            SviIPsIpv6: network
                .ipv6_port_config
                .as_ref()
                .and_then(|v6| v6.svi_ip.clone())
                .into_iter()
                .collect(),
            SviMAC: svi_mac,
            VrfName: format!("vpc_{l3_vni}"),
            HasVpcPeerPrefixes: !network.vpc_peer_prefixes.is_empty(),
            HasVpcPeerPrefixesIpv6: network
                .vpc_peer_prefixes
                .iter()
                .any(|p| matches!(p.parse::<IpNet>(), Ok(IpNet::V6(_)))),
            HasVpcPrefixes: !vpc_ipv4.is_empty(),
            // Only used directly by ETV template now.
            // FNN template uses PortPrefixes, which is built from this.
            VpcPrefixes: vpc_ipv4,
            HasVpcPrefixesIpv6: !vpc_ipv6.is_empty(),
            VpcPrefixesIpv6: vpc_ipv6,
            IsL2Segment: network.is_l2_segment,
            StorageTarget: false, // XXX (Classic, L3)
            HasNetworkSecurityGroup: network.network_security_group_id.is_some(),
            NetworkSecurityGroupIndex: network
                .network_security_group_id
                .as_ref()
                .map(|nid| {
                    nsg_id_index_map
                    .get(nid)
                    .copied()
                    .ok_or_else(|| {
                    eyre::eyre!(
                        "BUG: PortConfig references network security group ID that does not exist",
                    )
                    })
                })
                .transpose()?,
            HasFmdsGateway: !is_dpu_os && (fmds_gateway_vlan == Some(network.vlan)),
        };
        if port.HasFmdsGateway {
            fmds_gateway_matched = true;
        }
        let port_has_neighbor = !port.HostIP.is_empty() || port.HostIPv6.is_some();

        let (vpc_peer_ipv4, vpc_peer_ipv6) =
            split_prefixes_by_family(&network.vpc_peer_prefixes, None, 1);

        has_any_vpc_vrf_loopback =
            has_any_vpc_vrf_loopback || network.tenant_vrf_loopback_ip.is_some();

        vpc_configs
            .entry(l3_vni)
            .and_modify(|v| {
                if let (Some(existing_profile), Some(next_profile)) =
                    (v.RoutingProfile.as_ref(), routing_profile.as_ref())
                    && existing_profile != next_profile
                {
                    tracing::warn!(
                        l3_vni,
                        "found different routing profiles for the same VPC L3 VNI; using the first profile"
                    );
                }
                if v.RoutingProfile.is_none() {
                    v.RoutingProfile = routing_profile.clone();
                }
                v.PortPrefixes.extend_from_slice(&port.VpcPrefixes);
                v.PortPrefixesIpv6.extend_from_slice(&port.VpcPrefixesIpv6);
                v.HasNeighbors |= port_has_neighbor;
                v.PortConfigs.push(port.clone());
            })
            .or_insert_with(|| TmplVpc {
                VrfName: port.VrfName.clone(),
                L3VNI: l3_vni,
                HasVrfLoopback: network.tenant_vrf_loopback_ip.is_some(),
                VrfLoopback: network.tenant_vrf_loopback_ip.unwrap_or_default(),
                HasNeighbors: port_has_neighbor,
                PortConfigs: vec![port.clone()],
                HasVpcPeerPrefixes: !vpc_peer_ipv4.is_empty(),
                VpcPeerPrefixes: vpc_peer_ipv4,
                HasVpcPeerPrefixesIpv6: !vpc_peer_ipv6.is_empty(),
                VpcPeerPrefixesIpv6: vpc_peer_ipv6,
                HasVpcPeerVnis: !network.vpc_peer_vnis.is_empty(),
                VpcPeerVnis: network
                    .vpc_peer_vnis
                    .iter()
                    .map(|vni| TmplVni { Vni: *vni })
                    .collect(),
                RoutingProfile: routing_profile.clone(),
                PortPrefixes: port.VpcPrefixes.clone(),
                PortPrefixesIpv6: port.VpcPrefixesIpv6.clone(),
                SiteFabricPrefixes: vec![],
                SiteFabricPrefixesIpv6: vec![],
            });

        port_configs.push(port);
    }

    if let Some(vlan) = fmds_gateway_vlan
        && !fmds_gateway_matched
    {
        tracing::error!(
            vlan,
            "fmds_gateway_vlan not found in any port config; FMDS gateway address will not be configured"
        );
    }

    let include_bridge = !port_configs.is_empty() && port_configs.iter().all(|b| b.IsL2Segment);

    let (
        ingress_ipv4_override_rules,
        egress_ipv4_override_rules,
        ingress_ipv6_override_rules,
        egress_ipv6_override_rules,
    ) = prepare_network_security_group_rules(conf.network_security_policy_override_rules)?;

    let (site_fabric_ipv4, site_fabric_ipv6) = split_prefixes_by_family(
        &conf.site_fabric_prefixes,
        None,
        SITE_FABRIC_RULE_INDEX_START,
    );

    let mut vpcs = vpc_configs.into_values().collect::<Vec<TmplVpc>>();
    vpcs.sort_by_key(|a| a.L3VNI);
    for vpc in &mut vpcs {
        let PreparedAclPrefixes {
            prefixes: ipv4_prefixes,
            next_index: ipv4_next_index,
        } = deduplicate_acl_prefixes(std::mem::take(&mut vpc.PortPrefixes))?;
        let PreparedAclPrefixes {
            prefixes: ipv6_prefixes,
            next_index: ipv6_next_index,
        } = deduplicate_acl_prefixes(std::mem::take(&mut vpc.PortPrefixesIpv6))?;
        vpc.SiteFabricPrefixes = reindex_acl_prefixes(
            &site_fabric_ipv4,
            ipv4_next_index.max(SITE_FABRIC_RULE_INDEX_START),
        )?;
        vpc.SiteFabricPrefixesIpv6 = reindex_acl_prefixes(
            &site_fabric_ipv6,
            ipv6_next_index.max(SITE_FABRIC_RULE_INDEX_START),
        )?;
        vpc.PortPrefixes = ipv4_prefixes;
        vpc.PortPrefixesIpv6 = ipv6_prefixes;
    }

    let (vpc_isolation_prefixes, etv_isolation_next_index) = if is_etv {
        let PreparedAclPrefixes {
            prefixes,
            next_index,
        } = deduplicate_acl_prefixes(
            port_configs
                .iter()
                .flat_map(|port| port.VpcPrefixes.iter().cloned()),
        )?;
        (prefixes, next_index)
    } else {
        (vec![], VPC_ISOLATION_RULE_INDEX_START)
    };
    let site_fabric_ipv4 = if is_etv {
        reindex_acl_prefixes(
            &site_fabric_ipv4,
            etv_isolation_next_index.max(SITE_FABRIC_RULE_INDEX_START),
        )?
    } else {
        site_fabric_ipv4
    };

    // Preserve the established deny-rule floor unless ETV permits occupy it.
    let legacy_deny_prefix_rule_index_start = SITE_FABRIC_RULE_INDEX_START
        .checked_add(conf.site_fabric_prefixes.len())
        .ok_or_else(|| eyre::eyre!("deny-prefix rule index overflow"))?;
    let deny_prefix_rule_index_start = if is_etv && !has_network_security_group {
        legacy_deny_prefix_rule_index_start.max(etv_isolation_next_index)
    } else {
        legacy_deny_prefix_rule_index_start
    };

    let mut tenant_host_routes_to_underlay = Vec::new();
    let mut tenant_host_routes_to_underlay_ipv6 = Vec::new();
    for vpc in vpcs.iter().filter(|vpc| {
        vpc.RoutingProfile
            .as_ref()
            .is_some_and(|profile| profile.LeakTenantHostRoutesToUnderlay)
    }) {
        for port in &vpc.PortConfigs {
            // IPv4 already leaks the configured gateway CIDR from `IPs`; changing
            // that would alter existing prefix-list matches. IPv6 deliberately
            // uses `HostIPv6Route`, the interface prefix the underlay needs.
            tenant_host_routes_to_underlay.extend_from_slice(&port.IPs);
            tenant_host_routes_to_underlay_ipv6.extend(
                port.HostIPv6Route
                    .iter()
                    .filter(|route| !route.is_empty())
                    .cloned(),
            );
        }
    }

    let (anycast_ipv4, anycast_ipv6) =
        split_prefixes_by_family(&conf.anycast_site_prefixes, None, 1000);
    let (deny_ipv4, deny_ipv6) =
        split_prefixes_by_family(&conf.deny_prefixes, None, deny_prefix_rule_index_start);

    let params = TmplNvue {
        HasBgpLeafSessionPassword: conf.bgp_leaf_session_password.is_some(),
        BgpLeafSessionPassword: conf.bgp_leaf_session_password.unwrap_or_default(),
        UseAdminNetwork: conf.use_admin_network,
        LoopbackIP: conf.loopback_ip.to_string(),
        HasLoopbackIpv6: conf.loopback_ip_v6.is_some(),
        LoopbackIpv6: conf
            .loopback_ip_v6
            .map(|ip| ip.to_string())
            .unwrap_or_default(),
        HasSiteGlobalVpcVni: conf.site_global_vpc_vni.is_some(),
        SiteGlobalVpcVni: conf.site_global_vpc_vni.unwrap_or_default(),
        HasAnyVpcTenantHostLeakToUnderlay: !tenant_host_routes_to_underlay.is_empty(),
        TenantHostRoutesToUnderlay: tenant_host_routes_to_underlay,
        HasAnyVpcTenantHostLeakToUnderlayIpv6: !tenant_host_routes_to_underlay_ipv6.is_empty(),
        TenantHostRoutesToUnderlayIpv6: tenant_host_routes_to_underlay_ipv6,
        HasAnyVpcVrfLoopback: has_any_vpc_vrf_loopback,
        ASN: conf.asn,
        DatacenterASN: conf.datacenter_asn,
        UseCommonInternalTenantRouteTarget: conf.common_internal_route_target.is_some(),
        CommonInternalRouteTarget: conf.common_internal_route_target.map(|rt| {
            TmplRouteTargetConfig {
                ASN: rt.asn,
                VNI: rt.vni,
            }
        }),
        AdditionalRouteTargetImports: conf
            .additional_route_target_imports
            .iter()
            .map(|rt| TmplRouteTargetConfig {
                ASN: rt.asn,
                VNI: rt.vni,
            })
            .collect(),
        DPUHostname: conf.dpu_hostname,
        SearchDomain: conf.dpu_search_domain,
        Uplinks: conf.uplinks.clone(),
        RouteServers: conf.route_servers.iter().map(|ip| ip.to_string()).collect(),
        DHCPServers: conf.dhcp_servers.iter().map(|ip| ip.to_string()).collect(),
        AnycastSitePrefixes: anycast_ipv4,
        AnycastSitePrefixesIpv6: anycast_ipv6,
        HasSiteFabricPrefixes: !site_fabric_ipv4.is_empty(),
        SiteFabricPrefixes: site_fabric_ipv4,
        HasSiteFabricPrefixesIpv6: !site_fabric_ipv6.is_empty(),
        SiteFabricPrefixesIpv6: site_fabric_ipv6,
        HasDenyPrefixes: !deny_ipv4.is_empty(),
        DenyPrefixes: deny_ipv4,
        HasDenyPrefixesIpv6: !deny_ipv6.is_empty(),
        DenyPrefixesIpv6: deny_ipv6,
        StatefulAclsEnabled: conf.stateful_acls_enabled,
        UseVpcIsolation: conf.use_vpc_isolation,
        HasIpv4IngressSecurityPolicyOverrideRules: !ingress_ipv4_override_rules.is_empty(),
        HasIpv4EgressSecurityPolicyOverrideRules: !egress_ipv4_override_rules.is_empty(),
        HasIpv6IngressSecurityPolicyOverrideRules: !ingress_ipv6_override_rules.is_empty(),
        HasIpv6EgressSecurityPolicyOverrideRules: !egress_ipv6_override_rules.is_empty(),
        Ipv4IngressNetworkSecurityPolicyOverrideRules: ingress_ipv4_override_rules,
        Ipv4EgressNetworkSecurityPolicyOverrideRules: egress_ipv4_override_rules,
        Ipv6IngressNetworkSecurityPolicyOverrideRules: ingress_ipv6_override_rules,
        Ipv6EgressNetworkSecurityPolicyOverrideRules: egress_ipv6_override_rules,
        HbnVersion: conf.hbn_version,
        Tenant: TmplComputeTenant {
            RoutingProfile: deprecated_tenant_routing_profile,
            Vpcs: vpcs,
            VpcIsolationPrefixes: vpc_isolation_prefixes,
            VrfName: conf.ct_vrf_name,
            L3VNI: conf.ct_l3_vni.unwrap_or_default().to_string(),
            PortConfigs: port_configs,
            HasHostASN: conf.tenant_host_asn.is_some(),
            HostASN: conf.tenant_host_asn.unwrap_or_default(),
            // Keep legacy tenant-wide HostInterfaces for pre-FNN templates; new templates should use PortConfigs.
            HostInterfaces: host_interfaces,
            NetworkSecurityGroups: network_security_groups,
            HasNetworkSecurityGroup: has_network_security_group,
            HasIpv4IngressSecurityGroupRules: !merged_ingress_ipv4_nsg_rules.is_empty(),
            HasIpv4EgressSecurityGroupRules: !merged_egress_ipv4_nsg_rules.is_empty(),
            HasIpv6IngressSecurityGroupRules: !merged_ingress_ipv6_nsg_rules.is_empty(),
            HasIpv6EgressSecurityGroupRules: !merged_egress_ipv6_nsg_rules.is_empty(),
            IngressNetworkSecurityGroupRulesIpv4: merged_ingress_ipv4_nsg_rules,
            EgressNetworkSecurityGroupRulesIpv4: merged_egress_ipv4_nsg_rules,
            IngressNetworkSecurityGroupRulesIpv6: merged_ingress_ipv6_nsg_rules,
            EgressNetworkSecurityGroupRulesIpv6: merged_egress_ipv6_nsg_rules,
            HasVpcPeerPrefixes: !vpc_peer_prefixes.is_empty(),
            VpcPeerPrefixes: vpc_peer_prefixes,
            HasVpcPeerPrefixesIpv6: !vpc_peer_prefixes_ipv6.is_empty(),
            VpcPeerPrefixesIpv6: vpc_peer_prefixes_ipv6,
            HasVpcPeerVnis: !vpc_peer_vnis.is_empty(),
            VpcPeerVnis: vpc_peer_vnis,
        },
        // XXX: Unused placeholders for later.
        IsStorageClient: false,                   // XXX (Classic, L3)
        StorageDpuIP: "127.9.9.9".to_string(),    // XXX (Classic, L3)
        l3vnistorageVLAN: "vlan1337".to_string(), // XXX (Classic, L3)
        StorageL3VNI: 0,                          // XXX (Classic, L3)
        StorageLoopback: "127.8.8.8".to_string(), // XXX (Classic, L3)
        DPUstorageprefix: "127.7.7.7/32".to_string(),
        IncludeBridge: include_bridge,
        IsDpuOs: is_dpu_os,
    };

    gtmpl::template(template, params).map_err(|e| {
        println!("ERR filling template: {e}",);
        e.into()
    })
}

/// Prepares a set of network security groups rules for template use.
/// In the process, it expands the rules and evaluates whether they
/// exceed predefined limits.
///
/// * `rules` - A list of network security group rules
#[allow(clippy::type_complexity)]
fn prepare_network_security_group_rules(
    rules: Vec<NetworkSecurityGroupRule>,
) -> Result<
    (
        Vec<TmplNetworkSecurityGroupRule>,
        Vec<TmplNetworkSecurityGroupRule>,
        Vec<TmplNetworkSecurityGroupRule>,
        Vec<TmplNetworkSecurityGroupRule>,
    ),
    eyre::Error,
> {
    let mut ingress_ipv4_rules: Vec<&NetworkSecurityGroupRule> = vec![];
    let mut egress_ipv4_rules: Vec<&NetworkSecurityGroupRule> = vec![];
    let mut ingress_ipv6_rules: Vec<&NetworkSecurityGroupRule> = vec![];
    let mut egress_ipv6_rules: Vec<&NetworkSecurityGroupRule> = vec![];

    let mut total_rule_count: usize = 0;

    for rule in rules.iter() {
        // Calculate and accumulate what the number of rules
        // would be after expansion so we can cut things off
        // and err if we got a bad payload that could risk
        // the DPU itself.
        total_rule_count = match total_rule_count.overflowing_add(
            rule.src_prefixes
                .len()
                .saturating_mul(rule.dst_prefixes.len())
                .saturating_mul(
                    (rule
                        .src_port_end
                        .unwrap_or_default()
                        .saturating_sub(rule.src_port_start.unwrap_or_default())
                        + 1) as usize,
                )
                .saturating_mul(
                    (rule
                        .dst_port_end
                        .unwrap_or_default()
                        .saturating_sub(rule.dst_port_start.unwrap_or_default())
                        + 1) as usize,
                ),
        ) {
            (_, true) => {
                return Err(eyre::eyre!(
                    "supplied network security group rule count exceeds limit of {}",
                    NETWORK_SECURITY_GROUP_RULE_COUNT_MAX
                ));
            }
            (v, false) => v,
        };

        if total_rule_count > NETWORK_SECURITY_GROUP_RULE_COUNT_MAX {
            return Err(eyre::eyre!(
                "supplied network security group rule count exceeds limit of {}",
                NETWORK_SECURITY_GROUP_RULE_COUNT_MAX
            ));
        }

        match (rule.ingress, rule.ipv6) {
            (true, false) => ingress_ipv4_rules.push(rule),
            (false, false) => egress_ipv4_rules.push(rule),
            (true, true) => ingress_ipv6_rules.push(rule),
            (false, true) => egress_ipv6_rules.push(rule),
        }
    }

    // Order the rules by priority
    ingress_ipv4_rules.sort_by_key(|nsg| nsg.priority);
    egress_ipv4_rules.sort_by_key(|nsg| nsg.priority);
    ingress_ipv6_rules.sort_by_key(|nsg| nsg.priority);
    egress_ipv6_rules.sort_by_key(|nsg| nsg.priority);

    Ok((
        expand_network_security_group_rules(ingress_ipv4_rules),
        expand_network_security_group_rules(egress_ipv4_rules),
        expand_network_security_group_rules(ingress_ipv6_rules),
        expand_network_security_group_rules(egress_ipv6_rules),
    ))
}

/// Expands a set of network security group rules.
/// Source and destination port ranges and prefix lists will
/// be expanded to a set of individual rules.
/// A new vector of template-ready expanded network security
/// groups will be returned.
///
/// * `nsgs` - A list of references to network security groups to expand.
fn expand_network_security_group_rules(
    rules: Vec<&NetworkSecurityGroupRule>,
) -> Vec<TmplNetworkSecurityGroupRule> {
    let mut tmpl_rules: Vec<TmplNetworkSecurityGroupRule> = vec![];

    // NVUE config keys rules on priority, meaning no two rules can
    // have the same priority in a given list.
    // NOTE: This implicitly gives us at least one rule limit:
    // If no two rules in a list can have the same priority, and the
    // priority value max in nvue is limited to an unsigned 16-bit value,
    // that u16 max priority number becomes the max number of rules in a
    // given list.

    for rule in rules {
        for src_prefix in &rule.src_prefixes {
            for dst_prefix in &rule.dst_prefixes {
                if let (Some(src_start), Some(src_end)) = (rule.src_port_start, rule.src_port_end) {
                    if let (Some(dst_start), Some(dst_end)) =
                        (rule.dst_port_start, rule.dst_port_end)
                    {
                        for si in src_start..=src_end {
                            for di in dst_start..=dst_end {
                                tmpl_rules.push(TmplNetworkSecurityGroupRule {
                                    Id: rule.id.clone(),
                                    HasSrcPort: true,
                                    SrcPort: si,
                                    HasDstPort: true,
                                    DstPort: di,
                                    CanMatchAnyProtocol: rule.can_match_any_protocol,
                                    CanBeStateful: rule.can_be_stateful,
                                    Protocol: rule.protocol.clone(),
                                    Action: rule.action.clone(),
                                    SrcPrefix: src_prefix.clone(),
                                    DstPrefix: dst_prefix.clone(),
                                    OriginalPriority: rule.priority,
                                    Priority: tmpl_rules.len() as u32
                                        + NETWORK_SECURITY_GROUP_RULE_PRIORITY_START,
                                });
                            }
                        }
                    } else {
                        for si in src_start..=src_end {
                            tmpl_rules.push(TmplNetworkSecurityGroupRule {
                                Id: rule.id.clone(),
                                HasSrcPort: true,
                                SrcPort: si,
                                HasDstPort: false,
                                DstPort: 0,
                                CanMatchAnyProtocol: rule.can_match_any_protocol,
                                CanBeStateful: rule.can_be_stateful,
                                Protocol: rule.protocol.clone(),
                                Action: rule.action.clone(),
                                SrcPrefix: src_prefix.clone(),
                                DstPrefix: dst_prefix.clone(),
                                OriginalPriority: rule.priority,
                                Priority: tmpl_rules.len() as u32
                                    + NETWORK_SECURITY_GROUP_RULE_PRIORITY_START,
                            });
                        }
                    }
                } else if let (Some(dst_start), Some(dst_end)) =
                    (rule.dst_port_start, rule.dst_port_end)
                {
                    for di in dst_start..=dst_end {
                        tmpl_rules.push(TmplNetworkSecurityGroupRule {
                            Id: rule.id.clone(),
                            HasSrcPort: false,
                            SrcPort: 0,
                            HasDstPort: true,
                            DstPort: di,
                            CanMatchAnyProtocol: rule.can_match_any_protocol,
                            CanBeStateful: rule.can_be_stateful,
                            Protocol: rule.protocol.clone(),
                            Action: rule.action.clone(),
                            SrcPrefix: src_prefix.clone(),
                            DstPrefix: dst_prefix.clone(),
                            OriginalPriority: rule.priority,
                            Priority: tmpl_rules.len() as u32
                                + NETWORK_SECURITY_GROUP_RULE_PRIORITY_START,
                        });
                    }
                } else {
                    tmpl_rules.push(TmplNetworkSecurityGroupRule {
                        Id: rule.id.clone(),
                        HasSrcPort: false,
                        SrcPort: 0,
                        HasDstPort: false,
                        DstPort: 0,
                        CanMatchAnyProtocol: rule.can_match_any_protocol,
                        CanBeStateful: rule.can_be_stateful,
                        Protocol: rule.protocol.clone(),
                        Action: rule.action.clone(),
                        SrcPrefix: src_prefix.clone(),
                        DstPrefix: dst_prefix.clone(),
                        OriginalPriority: rule.priority,
                        Priority: tmpl_rules.len() as u32
                            + NETWORK_SECURITY_GROUP_RULE_PRIORITY_START,
                    });
                }
            }
        }
    }

    tmpl_rules
}

// Add a hack to completely overwrite the cl-platform check. New hardware has decided to change a
// value in sys_vendor, and this causes the cl-platform script to fail and not detect the vendor
// which causes nvued to fail as well.
pub async fn hack_platform_config_for_nvue() -> eyre::Result<()> {
    let container_id = super::hbn::get_hbn_container_id().await?;

    let stdout = super::hbn::run_in_container(&container_id, &["platform-detect"], true).await?;

    // the bug in new hardware causes the previous command to emit nothing, so if it is not emitting
    // anything, assume the hack needs to be applied.
    if stdout.is_empty() {
        let stdout = super::hbn::run_in_container(
            &container_id,
            &[
                "bash",
                "-c",
                "echo echo -n mlnx,bluefield > /usr/lib/cumulus/cl-platform", // yes, thats two echo on purpose
            ],
            true,
        )
        .await?;
        if !stdout.is_empty() {
            tracing::info!("config hack to replace platform: {stdout}");
        }
    }

    Ok(())
}

// Apply the config at `config_path`.
//
// Returns true if we performed `nv config apply`, false when pending config matched
// applied config and was detached without applying.
pub async fn apply(hbn_root: &Path, config_path: &super::FPath) -> eyre::Result<bool> {
    match run_apply(hbn_root, &config_path.0).await {
        Ok(applied) => {
            config_path.del("BAK");
            Ok(applied)
        }
        Err(err) => {
            tracing::error!(
                error = %err,
                error_chain = format!("{err:#}").replace('\n', "; "),
                "NVUE post command failed"
            );

            // If the config apply failed, we won't be using it, so move it out
            // of the way to an .error file for others to enjoy (while attempting
            // to remove any previous .error file in the process).
            let path_error = config_path.with_ext("error");
            if path_error.exists()
                && let Err(e) = fs::remove_file(path_error.clone())
            {
                tracing::warn!(
                    error_file_path = %path_error.display(),
                    error = %e,
                    "Failed to remove previous error file"
                );
            }

            if let Err(err) = fs::rename(config_path, &path_error) {
                eyre::bail!(
                    "rename {config_path} to {} on error: {err:#}",
                    path_error.display()
                );
            }
            // .. and copy the old one back.
            // This also ensures that we retry writing the config on subsequent runs.
            let path_bak = config_path.backup();
            if path_bak.exists()
                && let Err(err) = fs::rename(&path_bak, config_path)
            {
                eyre::bail!(
                    "rename {} to {config_path}, reverting on error: {err:#}",
                    path_bak.display(),
                );
            }

            Err(err)
        }
    }
}

// Ask NVUE to use the config at `path`
async fn run_apply(hbn_root: &Path, path: &Path) -> eyre::Result<bool> {
    let mut in_container_path = path
        .strip_prefix(hbn_root)
        .wrap_err("stripping hbn_root prefix from path to make in-container path")?
        .to_path_buf();
    // If hbn_root ends with "/", the stripped path will have it removed from start. Add back.
    if !in_container_path.has_root() {
        in_container_path = Path::new("/").join(in_container_path);
    }
    let container_id = super::hbn::get_hbn_container_id().await?;

    // Set this config as the pending one. This is where we'd get yaml parse errors and
    // other validation errors. Stores the pending config internally somewhere.
    let stdout = super::hbn::run_in_container(
        &container_id,
        &[
            "nv",
            "config",
            "replace",
            &in_container_path.to_string_lossy(),
        ],
        true,
    )
    .await?;
    if !stdout.is_empty() {
        tracing::info!("nv config replace: {stdout}");
    }

    // Compare pending to applied config at NVUE layer.
    // This avoids no-op apply cycles when textual YAML ordering changes but
    // semantic config does not.
    //
    // BUG: It seems like under some conditions (which I don't really
    // understand) this diff can return some output, but then the subsequent
    // `nv config apply` will warn about "config apply executed with no config
    // diff". This can result in needless PostConfigCheckWait health alerts, so
    // we're working around this by avoiding even calling this code if we see
    // unchanged config versions in the main loop. -drew
    let stdout =
        super::hbn::run_in_container(&container_id, &["nv", "config", "diff"], true).await?;
    if stdout.is_empty() {
        let stdout =
            super::hbn::run_in_container(&container_id, &["nv", "config", "detach"], true).await?;
        if !stdout.is_empty() {
            tracing::info!("nv config detach: {stdout}");
        }
        return Ok(false);
    }
    let config_diff_stdout = stdout;

    // Apply the pending config.
    //
    // - Writes:
    //   . /etc/frr/frr.conf
    //   . /etc/network/interfaces
    //   . /etc/frr/daemons
    //   . /etc/supervisor/conf.d/isc-dhcp-relay-default
    //   . and others (acls, nginx, ...)
    // - Restarts necessary services.
    // - Log is in /var/lib/hbn/var/lib/nvue/config/apply_log.txt
    // Once this returns networking should be ready to use.
    let stdout =
        super::hbn::run_in_container(&container_id, &["nv", "config", "apply", "-y"], true).await?;
    if !stdout.is_empty() {
        tracing::info!("nv config apply: {stdout}");
        // We're logging this just to see what was in there, in case it can help
        // explain the "config apply executed with no config diff" message.
        tracing::info!("nv config diff: {config_diff_stdout}");
    }

    // Restart nl2doca
    // This is a workaround for a bug in versions of HBN 3.2.0 and older that
    // will sometimes lead to loss of connectivity when switch over to an L3 evpn overlay.
    let stdout = super::hbn::run_in_container(
        &container_id,
        &["supervisorctl", "restart", "nl2doca"],
        false,
    )
    .await?;
    if !stdout.is_empty() {
        tracing::info!("nl2doca restart: {stdout}");
    }

    Ok(true)
}

/// vni_to_svimac takes an VNI (which is a 24 bit integer whose range
/// is 0-16777215), pads it with zeroes (so its 12 characters long), and
/// then turns it into a MAC address for the purpose of having a consistent
/// SVI MAC address value for all DPUs in a given VPC.
///
/// e.g, an L2VNI of 1637817 would result in an SviMAC of 00:00:01:63:78:17
/// for all DPUs in the VPC.
fn vni_to_svi_mac(vni: u32) -> eyre::Result<MacAddress> {
    sanitized_mac(&format!("{vni:012}"))
}

#[derive(Clone, Deserialize, Debug, PartialEq)]
pub struct RouteTargetConfig {
    pub asn: u32,
    pub vni: u32,
}

/// Converts an agent routing profile into the template-ready routing profile.
impl From<&RoutingProfile> for TmplRoutingProfile {
    fn from(profile: &RoutingProfile) -> Self {
        // Split mixed-family prefix lists before passing them to the template.
        let (v4leaks, v6leaks) =
            split_prefixes_by_family(&profile.accepted_leaks_from_underlay, None, 1);
        let (v4allowed_anycast, v6allowed_anycast) =
            split_prefixes_by_family(&profile.allowed_anycast_prefixes, None, 1);
        let has_leaks_from_underlay_ipv4 =
            profile.leak_default_route_from_underlay || !v4leaks.is_empty();
        let has_leaks_from_underlay_ipv6 =
            profile.leak_default_route_from_underlay || !v6leaks.is_empty();

        TmplRoutingProfile {
            TenantLeakCommunitiesAccepted: profile.tenant_leak_communities_accepted,
            LeakDefaultRouteFromUnderlay: profile.leak_default_route_from_underlay,
            LeakTenantHostRoutesToUnderlay: profile.leak_tenant_host_routes_to_underlay,
            HasLeaksFromUnderlayIpv4: has_leaks_from_underlay_ipv4,
            HasLeaksFromUnderlayIpv6: has_leaks_from_underlay_ipv6,
            AcceptedLeaksFromUnderlayIpv4: v4leaks,
            AcceptedLeaksFromUnderlayIpv6: v6leaks,
            HasAllowedAnycastPrefixesIpv4: !v4allowed_anycast.is_empty(),
            HasAllowedAnycastPrefixesIpv6: !v6allowed_anycast.is_empty(),
            AllowedAnycastPrefixesIpv4: v4allowed_anycast,
            AllowedAnycastPrefixesIpv6: v6allowed_anycast,
            RouteTargetImports: profile
                .route_target_imports
                .iter()
                .map(|rt| TmplRouteTargetConfig {
                    ASN: rt.asn,
                    VNI: rt.vni,
                })
                .collect(),
            RouteTargetsOnExports: profile
                .route_targets_on_exports
                .iter()
                .map(|rt| TmplRouteTargetConfig {
                    ASN: rt.asn,
                    VNI: rt.vni,
                })
                .collect(),
        }
    }
}

// What we need to configure NVUE
pub struct NvueConfig {
    pub is_fnn: bool,
    pub is_dpu_os: bool,
    /// In containerized mode, the vlan ID of the pf0hpf-facing SVI that should
    /// carry 169.254.169.253/30 as the FMDS gateway address.
    pub fmds_gateway_vlan: Option<u16>,
    pub vpc_virtualization_type: VpcVirtualizationType,
    pub use_admin_network: bool,
    pub tenancy_enabled: bool,
    pub loopback_ip: IpAddr,
    pub loopback_ip_v6: Option<Ipv6Addr>,
    pub asn: u32,
    pub datacenter_asn: u32,
    pub site_global_vpc_vni: Option<u32>,
    pub common_internal_route_target: Option<RouteTargetConfig>,
    pub additional_route_target_imports: Vec<RouteTargetConfig>,
    pub bgp_leaf_session_password: Option<String>,

    pub dpu_hostname: String,
    pub dpu_search_domain: String,
    pub hbn_version: Option<String>,
    pub uplinks: Vec<String>,
    pub route_servers: Vec<IpAddr>,
    pub dhcp_servers: Vec<IpAddr>,
    pub l3_domains: Vec<L3Domain>,
    pub deny_prefixes: Vec<String>,
    pub site_fabric_prefixes: Vec<String>,
    pub anycast_site_prefixes: Vec<String>,
    pub tenant_host_asn: Option<u32>,
    pub use_vpc_isolation: bool,
    pub stateful_acls_enabled: bool,

    pub network_security_groups: Vec<NetworkSecurityGroup>,

    pub network_security_policy_override_rules: Vec<NetworkSecurityGroupRule>,

    // Currently we have a single tenant, hence the single ct_ prefix.
    // Later this will be Vec<ComputeTenant>.

    // ct_vrf_name is the VRF name. This value needs to be 15 characters
    // or less, somehow derived from the VPC, and is the same for all
    // DPUs in a VPC. To achieve this, we currently take the L3VNI of the
    // VPC, and assign this as "vrf_<l3vni>". This ensures we keep the
    // character count below 15, and by using the L3VNI, we're able to
    // directly correlate that back to the VPC.
    pub ct_vrf_name: String,
    pub ct_l3_vni: Option<u32>,
    pub ct_vrf_loopback: String,
    pub ct_port_configs: Vec<PortConfig>,
    pub ct_access_vlans: Vec<VlanConfig>,
    pub ct_routing_profile: Option<RoutingProfile>,
}

#[derive(Clone, Deserialize, Debug, PartialEq)]
pub struct RoutingProfile {
    pub leak_default_route_from_underlay: bool,
    pub leak_tenant_host_routes_to_underlay: bool,
    pub route_target_imports: Vec<RouteTargetConfig>,
    pub route_targets_on_exports: Vec<RouteTargetConfig>,
    pub tenant_leak_communities_accepted: bool,
    pub accepted_leaks_from_underlay: Vec<String>,
    #[serde(default)]
    pub allowed_anycast_prefixes: Vec<String>,
}

#[derive(Clone, Deserialize, Debug, PartialEq)]
pub struct InterfaceRoutingProfile {
    pub allowed_anycast_prefixes: Vec<String>,
}

#[derive(Clone, Deserialize, Debug)]
pub struct NetworkSecurityGroup {
    pub id: String,
    pub stateful_egress: bool,
    pub rules: Vec<NetworkSecurityGroupRule>,
}

#[derive(Clone, Deserialize, Debug)]
pub struct NetworkSecurityGroupRule {
    pub id: String,
    pub ingress: bool,
    pub ipv6: bool,
    pub priority: u32,
    pub src_port_start: Option<u32>,
    pub src_port_end: Option<u32>,
    pub dst_port_start: Option<u32>,
    pub dst_port_end: Option<u32>,
    pub can_match_any_protocol: bool,
    pub can_be_stateful: bool,
    pub protocol: String,
    pub action: String,
    pub src_prefixes: Vec<String>,
    pub dst_prefixes: Vec<String>,
}

impl TryFrom<&rpc::ResolvedNetworkSecurityGroupRule> for NetworkSecurityGroupRule {
    type Error = eyre::Error;

    fn try_from(
        resolved_rule: &rpc::ResolvedNetworkSecurityGroupRule,
    ) -> Result<Self, Self::Error> {
        let Some(ref rule) = resolved_rule.rule else {
            return Err(eyre::eyre!("BUG: attempting to convert empty NSG rule"));
        };

        Ok(NetworkSecurityGroupRule {
            id: rule.id.clone().unwrap_or_default(),
            ingress: rule.direction()
                == rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress,
            can_match_any_protocol: rule.protocol()
                == rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoAny,
            // We'll only automatically handle stateful tracking for egress rules
            // that specify TCP/UDP, a dst port, and NO src port because it becomes
            // extremely difficult for users to get rule combinations for common
            // use-cases if the stateful option isn't narrowly implemented.
            // ICMP is _technically_ documented as valid for stateful tracking,
            // but it's not throroughly tested/validated, and the user would have
            // no way to achieve non-stateful ICMP if stateful is enabled for the
            // NSG, so it's being left out.
            can_be_stateful: rule.direction()
                == rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                && matches!(
                    rule.protocol(),
                    rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp
                        | rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoUdp
                )
                && rule.dst_port_start.is_some()
                && rule.src_port_start.is_none(),
            ipv6: rule.ipv6,
            priority: rule.priority,
            src_port_start: rule.src_port_start,
            src_port_end: rule.src_port_end,
            dst_port_start: rule.dst_port_start,
            dst_port_end: rule.dst_port_end,
            protocol: rpc::NetworkSecurityGroupRuleProtocol::to_string_from_enum_i32(
                rule.protocol,
            )?
            .to_lowercase(),
            action: rpc::NetworkSecurityGroupRuleAction::to_string_from_enum_i32(rule.action)?
                .to_lowercase(),
            src_prefixes: resolved_rule.src_prefixes.clone(),
            dst_prefixes: resolved_rule.dst_prefixes.clone(),
        })
    }
}

pub struct VlanConfig {
    pub vlan_id: u32,
    pub network: String,
    pub ip: String,
    pub ipv6_vlan_config: Option<Ipv6VlanConfig>,
}

#[derive(Clone, Deserialize, Debug)]
pub struct Ipv6VlanConfig {
    pub network: String,
    pub ip: String,
}

#[derive(Deserialize, Debug)]
pub struct L3Domain {
    pub l3_domain_name: String,
    pub services: Vec<String>,
}

/// IPv6 configuration for a port.
#[derive(Clone, Deserialize, Debug)]
pub struct Ipv6PortConfig {
    /// IPv6 value configured on the DPU in CIDR notation (for example,
    /// "2001:db8::0/127"). Stateful FNN uses the ::0 end of a /127 linknet
    /// (RFC 6164). SLAAC carries the selected /64 without a concrete host
    /// address.
    pub gateway_cidr: String,
    /// SVI IP for L2 segments -- the DPU's gateway address on the VLAN.
    pub svi_ip: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct PortConfig {
    pub interface_name: String,
    pub vlan: u16,
    pub host_ip: String,
    pub host_route: String,
    pub host_ipv6: Option<String>,
    pub host_ipv6_route: Option<String>,
    pub vni: Option<u32>, // In FNN, admin network has both an l2vni and an l3vni
    pub l3_vni: Option<u32>,
    pub gateway_cidr: String,
    /// Optional IPv6 configuration for interfaces that include IPv6.
    pub ipv6_port_config: Option<Ipv6PortConfig>,
    pub vpc_prefixes: Vec<String>,
    pub vpc_peer_prefixes: Vec<String>,
    pub vpc_peer_vnis: Vec<u32>,
    /// SVI IP for L2 segments -- the DPU's gateway address on the VLAN (IPv4).
    pub svi_ip: Option<String>,
    pub tenant_vrf_loopback_ip: Option<String>,
    pub is_l2_segment: bool,
    pub is_phy: bool,
    pub network_security_group_id: Option<String>,
    #[serde(default)]
    pub routing_profile: Option<RoutingProfile>,
    pub interface_routing_profile: Option<InterfaceRoutingProfile>,
}

//
// Go template objects, hence allow(non_snake_case)
//

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplNvue {
    UseAdminNetwork: bool, // akak service network
    HasSiteGlobalVpcVni: bool,
    SiteGlobalVpcVni: u32,
    LoopbackIP: String,
    HasLoopbackIpv6: bool,
    LoopbackIpv6: String,
    /// Does any opted-in VPC have an IPv4 host route to leak to the underlay?
    HasAnyVpcTenantHostLeakToUnderlay: bool,
    /// IPv4 host routes prepared for the underlay prefix list.
    TenantHostRoutesToUnderlay: Vec<String>,

    /// Does any opted-in VPC have an IPv6 host route to leak to the underlay?
    HasAnyVpcTenantHostLeakToUnderlayIpv6: bool,
    /// IPv6 host routes prepared for the underlay prefix list.
    TenantHostRoutesToUnderlayIpv6: Vec<String>,

    /// Does any VPC have a VRF loopback?
    HasAnyVpcVrfLoopback: bool,

    ASN: u32,
    DatacenterASN: u32,
    UseCommonInternalTenantRouteTarget: bool,
    CommonInternalRouteTarget: Option<TmplRouteTargetConfig>,
    AdditionalRouteTargetImports: Vec<TmplRouteTargetConfig>,

    DPUHostname: String,  // The first part of the FQDN
    SearchDomain: String, // The rest of the FQDN
    Uplinks: Vec<String>,
    RouteServers: Vec<String>,

    /// Format: IPv4 address of (per tenant) dhcp server
    DHCPServers: Vec<String>, // Previously 'Servers'

    /// Format: CIDR of the infastructure prefixes to block. Origin is carbide-api config file.
    DenyPrefixes: Vec<Prefix>,
    DenyPrefixesIpv6: Vec<Prefix>,

    HasDenyPrefixes: bool,
    HasDenyPrefixesIpv6: bool,

    /// Format: CIDR of the site prefixes for tenant use.  If VPC isolation is applied,
    /// and there is no network security group applied overriding the behavior,
    /// these will be blocked as well.
    SiteFabricPrefixes: Vec<Prefix>,
    SiteFabricPrefixesIpv6: Vec<Prefix>,

    HasSiteFabricPrefixes: bool,
    HasSiteFabricPrefixesIpv6: bool,

    /// Format: CIDR of the site prefixes that tenants are allowed to
    /// from the host to the DPU.
    AnycastSitePrefixes: Vec<Prefix>,
    AnycastSitePrefixesIpv6: Vec<Prefix>,

    // Whether VPC-isolation should be applied.
    UseVpcIsolation: bool,

    HbnVersion: Option<String>,

    /// Whether stateful ACLs are possible and we
    /// should perform any extra config to prepare
    /// for them.
    StatefulAclsEnabled: bool,

    /// Whether there are global policies that should be evaluated
    /// after deny prefixes but before any tenant-defined rules.
    HasIpv4IngressSecurityPolicyOverrideRules: bool,
    HasIpv4EgressSecurityPolicyOverrideRules: bool,
    HasIpv6IngressSecurityPolicyOverrideRules: bool,
    HasIpv6EgressSecurityPolicyOverrideRules: bool,
    Ipv4IngressNetworkSecurityPolicyOverrideRules: Vec<TmplNetworkSecurityGroupRule>,
    Ipv4EgressNetworkSecurityPolicyOverrideRules: Vec<TmplNetworkSecurityGroupRule>,
    Ipv6IngressNetworkSecurityPolicyOverrideRules: Vec<TmplNetworkSecurityGroupRule>,
    Ipv6EgressNetworkSecurityPolicyOverrideRules: Vec<TmplNetworkSecurityGroupRule>,

    /// For when we have more than one tenant
    Tenant: TmplComputeTenant,

    // XXX: These are unused placeholders for later.
    // StorageDpuIP is an interface that should exist on
    // client nodes that are NOT storage targets, so in the
    // case where StorageTarget is false, we would expect
    // there to be a StorageDpuIP.
    IsStorageClient: bool,    // XXX (Classic, L3)
    StorageDpuIP: String,     // XXX (Classic, L3)
    l3vnistorageVLAN: String, // XXX (Classic, L3)
    StorageL3VNI: u32,        // XXX (Classic, L3)
    StorageLoopback: String,  // XXX (Classic, L3)
    DPUstorageprefix: String, // XXX (Classic, L3)
    IncludeBridge: bool,
    IsDpuOs: bool,

    HasBgpLeafSessionPassword: bool,
    /// A password to use for the BGP session with the
    /// leaf TOR.
    BgpLeafSessionPassword: String,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug, PartialEq)]
struct TmplRouteTargetConfig {
    ASN: u32,
    VNI: u32,
}

/// Template-ready representation of a network security group rule.
/// Direction (ingress/egress), ipv (4/6), and priority
/// ordering will be grouped and ordered in advance.
/// Priority is still included mostly as a convenience,
/// but we'll also pad the value to a minimum of 100
/// so that there's room for low-priority "system rules"
/// to be inserted if needed.
#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplNetworkSecurityGroupRule {
    Id: String,
    HasSrcPort: bool,
    SrcPort: u32,
    HasDstPort: bool,
    DstPort: u32,
    CanMatchAnyProtocol: bool,
    CanBeStateful: bool,
    Protocol: String,
    Action: String,
    SrcPrefix: String,
    DstPrefix: String,
    Priority: u32,
    OriginalPriority: u32,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug, PartialEq)]
struct TmplRoutingProfile {
    LeakTenantHostRoutesToUnderlay: bool,
    LeakDefaultRouteFromUnderlay: bool,
    RouteTargetImports: Vec<TmplRouteTargetConfig>,
    RouteTargetsOnExports: Vec<TmplRouteTargetConfig>,
    TenantLeakCommunitiesAccepted: bool,
    /// Whether there are _any_ leaks from the default
    /// VRF into the tenant VRF being used.
    HasLeaksFromUnderlayIpv4: bool,
    HasLeaksFromUnderlayIpv6: bool,
    AcceptedLeaksFromUnderlayIpv4: Vec<Prefix>,
    AcceptedLeaksFromUnderlayIpv6: Vec<Prefix>,
    HasAllowedAnycastPrefixesIpv4: bool,
    HasAllowedAnycastPrefixesIpv6: bool,
    AllowedAnycastPrefixesIpv4: Vec<Prefix>,
    AllowedAnycastPrefixesIpv6: Vec<Prefix>,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug, PartialEq, Default)]
struct TmplInterfaceRoutingProfile {
    HasAllowedAnycastPrefixesIpv4: bool,
    HasAllowedAnycastPrefixesIpv6: bool,
    AllowedAnycastPrefixesIpv4: Vec<Prefix>,
    AllowedAnycastPrefixesIpv6: Vec<Prefix>,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplComputeTenant {
    Vpcs: Vec<TmplVpc>,
    PortConfigs: Vec<TmplConfigPort>,
    /// Deduplicated IPv4 prefixes used only by ETV's tenant-wide VPC isolation policies.
    VpcIsolationPrefixes: Vec<Prefix>,
    NetworkSecurityGroups: Vec<TmplNetworkSecurityGroup>,

    // TODO:  Everything thing from here down should remain for pre-FNN purposes,
    //        but they now live in the Vecs above to support multiple VPCs per DPU.
    //
    /// Tenant name/id with a max of 15 chars, because it's also used for the interface name.
    /// Linux is limited to 15 chars for interface names.
    VrfName: String,

    /// L3VNI VPC-specifc VNI, which is globally unique. GNI allocates us
    /// a pool of VNIs to assign as we see fit, so we carve out blocks
    /// per-site, and then manage them via the VPC_VNI (vpc-vni) resource
    /// pool.
    // TODO(chet): Does this need to be a string?
    L3VNI: String,

    HasHostASN: bool,
    /// An ASN allocated for tenants to use
    /// when they peer with the DPU.
    /// If configured, the DPU will expect the host
    /// to peer with this ASN.  If left unset
    /// remote-as external will be used, allowing
    /// any ASN.
    HostASN: u32,

    HostInterfaces: Vec<TmplHostInterfaces>,

    HasVpcPeerPrefixes: bool,
    VpcPeerPrefixes: Vec<Prefix>,
    HasVpcPeerPrefixesIpv6: bool,
    VpcPeerPrefixesIpv6: Vec<Prefix>,

    HasVpcPeerVnis: bool,
    VpcPeerVnis: Vec<TmplVni>,

    RoutingProfile: Option<TmplRoutingProfile>,

    HasNetworkSecurityGroup: bool,
    IngressNetworkSecurityGroupRulesIpv4: Vec<TmplNetworkSecurityGroupRule>,
    IngressNetworkSecurityGroupRulesIpv6: Vec<TmplNetworkSecurityGroupRule>,
    EgressNetworkSecurityGroupRulesIpv4: Vec<TmplNetworkSecurityGroupRule>,
    EgressNetworkSecurityGroupRulesIpv6: Vec<TmplNetworkSecurityGroupRule>,
    HasIpv4IngressSecurityGroupRules: bool,
    HasIpv4EgressSecurityGroupRules: bool,
    HasIpv6IngressSecurityGroupRules: bool,
    HasIpv6EgressSecurityGroupRules: bool,
    // /////////////
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplNetworkSecurityGroup {
    Index: u16,
    StatefulEgress: bool,
    IngressNetworkSecurityGroupRulesIpv4: Vec<TmplNetworkSecurityGroupRule>,
    IngressNetworkSecurityGroupRulesIpv6: Vec<TmplNetworkSecurityGroupRule>,
    EgressNetworkSecurityGroupRulesIpv4: Vec<TmplNetworkSecurityGroupRule>,
    EgressNetworkSecurityGroupRulesIpv6: Vec<TmplNetworkSecurityGroupRule>,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplVpc {
    /// Tenant name/id with a max of 15 chars, because it's also used for the interface name.
    /// Linux is limited to 15 chars for interface names.
    VrfName: String,

    /// VPC-specifc VNI, which must be unique within a given site but
    /// not necessarily globally unique.
    L3VNI: u32,

    // VrfLoopback is the tenant loopback IP assigned to each DPU.
    // It was originally expected to be allocated from an interface-specific
    // /30 (as the first IP in the allocation), but it's actually allocated
    // from a dedicated resource-pool, handed out as un-related /32s, and
    // interfaces in FNN get /31s.
    /// The tenant loopback IP assigned to each DPU.
    HasVrfLoopback: bool,
    VrfLoopback: String,

    HasNeighbors: bool,
    PortConfigs: Vec<TmplConfigPort>,

    HasVpcPeerPrefixes: bool,
    VpcPeerPrefixes: Vec<Prefix>,
    HasVpcPeerPrefixesIpv6: bool,
    VpcPeerPrefixesIpv6: Vec<Prefix>,

    // The relationship between interface:VPC is 1:1 but VPC:interface is 1:M.
    // So, a single VPC could have multiple, per-port, VpcPrefixes.  We can
    // accumulate these and pass them into the template for ease-of-use.
    /// The list of prefixes for all ports/interfaces that belong to this VPC.
    PortPrefixes: Vec<Prefix>,
    PortPrefixesIpv6: Vec<Prefix>,
    /// Site-fabric deny prefixes indexed within this FNN VPC's IPv4 isolation ACL.
    SiteFabricPrefixes: Vec<Prefix>,
    /// Site-fabric deny prefixes indexed within this FNN VPC's IPv6 isolation ACL.
    SiteFabricPrefixesIpv6: Vec<Prefix>,

    HasVpcPeerVnis: bool,
    VpcPeerVnis: Vec<TmplVni>,

    RoutingProfile: Option<TmplRoutingProfile>,
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Gtmpl)]
struct TmplHostInterfaces {
    ID: u32,
    HostIP: String,
    /// IPv6 host address, when configured.
    HostIPv6: Option<String>,

    // HostRoute in the context of FNN-L3 is the /31 prefix allocation.
    // This used to be populated as the HostIP + "/32", but then with
    // the advent of interface prefix allocations (where ETV is just a /32,
    // and FNN-L3 is a /31), HostRoute became the allocation (which was
    // a drop-in replacement for ETV/Classic environments).
    HostRoute: String,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplConfigPort {
    InterfaceName: String,
    Index: String,
    VlanID: u16,
    HostIP: String,
    HostIPv6: Option<String>,
    HostRoute: String,
    HostIPv6Route: Option<String>,
    HasRoutingProfile: bool,
    RoutingProfile: TmplInterfaceRoutingProfile,

    /// Format: 24bit integer (usable range: 4096 to 16777215).
    /// Empty string if no tenant
    L2VNI: String, // Previously called VNIDevice
    /// DPU-side IPv4 addresses with their prefix lengths.
    IPs: Vec<String>,
    /// DPU-side IPv6 addresses with their prefix lengths.
    IPsIpv6: Vec<String>,

    /// IPv4 SVI addresses for symmetrical EVPN. These are plain addresses
    /// without prefix lengths, typically the second usable address in the prefix.
    SviIPs: Vec<String>,
    /// IPv6 SVI addresses for symmetrical EVPN, without prefix lengths.
    SviIPsIpv6: Vec<String>,

    /// VRR, the distributed gateway, needs a manually defined MAC address. This can be overlapping
    /// on the different VTEPs, but it is very convenient to be unique on the same VTEP.
    ///
    /// In other words, this is the same value for all DPUs in a given VPC.
    ///
    /// TO MAKE THIS THE SAME FOR A GIVEN VPC, we take the L2VNI (which is a 24bit integer),
    /// pad it with zeroes so its 12 characters long, and then shove some colons in there.
    ///
    /// For example, for a VPC with an L2VNI of 1683714, the SviMAC would
    /// be configured as 00:00:01:68:37:14.
    ///
    /// Format: 48bit mac address in standard hex notation, e.g: 00:00:00:00:00:10
    SviMAC: String,

    /// The name of the VRF this interface belongs to.
    VrfName: String,

    HasVpcPrefixes: bool,

    /// Tenant VPCs we should allow them to access
    VpcPrefixes: Vec<Prefix>,

    // XXX: all of these added so the L3 template can build, need
    // to really actually wire them up.
    StorageTarget: bool, // XXX (Classic, L3)

    // does this segment support L2?
    IsL2Segment: bool,

    IsPhy: bool,

    HasVpcPeerPrefixes: bool,
    HasVpcPrefixesIpv6: bool,
    VpcPrefixesIpv6: Vec<Prefix>,
    HasVpcPeerPrefixesIpv6: bool,

    HasNetworkSecurityGroup: bool,
    NetworkSecurityGroupIndex: Option<u16>,
    HasFmdsGateway: bool,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug, PartialEq)]
struct Prefix {
    Index: String,
    Prefix: String,
}

#[allow(non_snake_case)]
#[derive(Clone, Gtmpl, Debug)]
struct TmplVni {
    Vni: u32,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use carbide_test_support::value_scenarios;

    use super::*;

    /// One `split_prefixes_by_family` scenario: the input prefixes, an optional
    /// VPC allow-list `filter` (as prefix strings, parsed inside the run), the
    /// `start_index` each family bucket counts from, and the `(ipv4, ipv6)`
    /// `Prefix` buckets the split should yield.
    struct SplitRow {
        prefixes: Vec<&'static str>,
        filter: Option<Vec<&'static str>>,
        start_index: usize,
    }

    /// Builds a `Prefix` from its index and CIDR string -- the shape
    /// `split_prefixes_by_family` produces -- so expected buckets read as a list
    /// of `(index, prefix)` pairs.
    fn prefix(index: &str, cidr: &str) -> Prefix {
        Prefix {
            Index: index.to_string(),
            Prefix: cidr.to_string(),
        }
    }

    /// Verifies a distinct prefix that reuses an occupied key is relocated after the existing
    /// range, because deduplication alone cannot make that ACL mapping valid.
    #[test]
    fn test_deduplicate_acl_prefixes_relocates_distinct_index_collision() {
        // Keep two established indices and collide a third distinct prefix with the latter.
        let prepared = deduplicate_acl_prefixes(vec![
            prefix("10", "10.0.0.0/24"),
            prefix("20", "10.0.1.0/24"),
            prefix("20", "10.0.2.0/24"),
        ])
        .expect("prefix preparation should succeed");

        // Only the conflicting rule moves, and the next free index advances with it.
        assert_eq!(
            prepared.prefixes,
            vec![
                prefix("10", "10.0.0.0/24"),
                prefix("20", "10.0.1.0/24"),
                prefix("21", "10.0.2.0/24"),
            ]
        );
        assert_eq!(prepared.next_index, 22);
    }

    #[test]
    fn test_split_prefixes_by_family() {
        value_scenarios!(run = |row: SplitRow| {
            let prefixes: Vec<String> = row.prefixes.iter().map(|s| s.to_string()).collect();
            let filter = row
                .filter
                .map(|f| parse_prefixes(&f.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            split_prefixes_by_family(&prefixes, filter.as_deref(), row.start_index)
        };
            "mixed families share the start index" {
                SplitRow {
                    prefixes: vec!["10.0.0.0/8", "2001:db8::/32", "192.168.0.0/16", "fd00::/8"],
                    filter: None,
                    start_index: 1000,
                } => (
                    vec![prefix("1000", "10.0.0.0/8"), prefix("1001", "192.168.0.0/16")],
                    vec![prefix("1000", "2001:db8::/32"), prefix("1001", "fd00::/8")],
                ),
            }

            "ipv4 only" {
                SplitRow {
                    prefixes: vec!["10.0.0.0/8", "172.16.0.0/12"],
                    filter: None,
                    start_index: 1,
                } => (
                    vec![prefix("1", "10.0.0.0/8"), prefix("2", "172.16.0.0/12")],
                    vec![],
                ),
            }

            "ipv6 only" {
                SplitRow {
                    prefixes: vec!["2001:db8::/32", "fd00::/8"],
                    filter: None,
                    start_index: 1,
                } => (
                    vec![],
                    vec![prefix("1", "2001:db8::/32"), prefix("2", "fd00::/8")],
                ),
            }

            "empty input" {
                SplitRow {
                    prefixes: vec![],
                    filter: None,
                    start_index: 1000,
                } => (vec![], vec![]),
            }

            "unparseable prefixes are dropped" {
                SplitRow {
                    prefixes: vec!["not-a-cidr", "10.0.0.0/8"],
                    filter: None,
                    start_index: 1,
                } => (vec![prefix("1", "10.0.0.0/8")], vec![]),
            }

            "ipv4-mapped ipv6 sorts into the v6 bucket" {
                // IPv4-mapped IPv6 addresses (::ffff:x.x.x.x) parse as V6.
                SplitRow {
                    prefixes: vec!["::ffff:192.0.2.33/128", "10.0.0.0/8", "2001:db8::/32"],
                    filter: None,
                    start_index: 1,
                } => (
                    vec![prefix("1", "10.0.0.0/8")],
                    vec![prefix("1", "::ffff:192.0.2.33/128"), prefix("2", "2001:db8::/32")],
                ),
            }

            "filter keeps only prefixes contained by the allow-list" {
                // One contained and one excluded prefix per family; family-specific
                // indexes are preserved.
                SplitRow {
                    prefixes: vec![
                        "192.0.2.64/26",
                        "198.51.100.0/24",
                        "2001:db8:1::/64",
                        "2001:db9::/48",
                    ],
                    filter: Some(vec!["192.0.2.0/24", "2001:db8::/32"]),
                    start_index: 1,
                } => (
                    vec![prefix("1", "192.0.2.64/26")],
                    vec![prefix("1", "2001:db8:1::/64")],
                ),
            }
        );
    }

    /// Helper to build a minimal NvueConfig for template rendering tests.
    /// Uses EthernetVirtualizer (ETV) by default.
    fn minimal_nvue_config() -> NvueConfig {
        NvueConfig {
            bgp_leaf_session_password: None,
            is_fnn: false,
            is_dpu_os: true,
            fmds_gateway_vlan: None,
            vpc_virtualization_type: VpcVirtualizationType::EthernetVirtualizer,
            use_admin_network: false,
            tenancy_enabled: true,
            loopback_ip: "10.0.0.1".parse().unwrap(),
            loopback_ip_v6: None,
            asn: 65000,
            datacenter_asn: 11414,
            site_global_vpc_vni: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            dpu_hostname: "test-dpu".to_string(),
            dpu_search_domain: "test.local".to_string(),
            hbn_version: None,
            uplinks: vec!["p0_if".to_string()],
            route_servers: vec![],
            dhcp_servers: vec![],
            l3_domains: vec![],
            deny_prefixes: vec![],
            site_fabric_prefixes: vec![],
            anycast_site_prefixes: vec![],
            tenant_host_asn: Some(65100),
            use_vpc_isolation: false,
            stateful_acls_enabled: false,
            network_security_groups: vec![],
            network_security_policy_override_rules: vec![],
            ct_vrf_name: "vpc_100".to_string(),
            ct_l3_vni: Some(100),
            ct_vrf_loopback: "10.0.0.2".to_string(),
            ct_port_configs: vec![],
            ct_access_vlans: vec![],
            ct_routing_profile: None,
        }
    }

    struct LoopbackRenderRow {
        virtualization_type: VpcVirtualizationType,
        loopback_ip_v6: Option<Ipv6Addr>,
    }

    fn address_set(addresses: &[&str]) -> BTreeSet<String> {
        addresses
            .iter()
            .map(|address| (*address).to_string())
            .collect()
    }

    fn yaml_mapping_keys(value: &serde_yaml::Value) -> BTreeSet<String> {
        match value {
            serde_yaml::Value::Null => BTreeSet::new(),
            serde_yaml::Value::Mapping(mapping) => mapping
                .keys()
                .map(|key| {
                    key.as_str()
                        .expect("mapping key should be a string")
                        .to_string()
                })
                .collect(),
            value => panic!("expected a YAML mapping or null, got {value:?}"),
        }
    }

    #[test]
    fn test_build_renders_ipv6_loopback_only_for_fnn() {
        value_scenarios!(run = |row: LoopbackRenderRow| {
            let mut conf = minimal_nvue_config();
            conf.is_fnn = row.virtualization_type == VpcVirtualizationType::Fnn;
            conf.vpc_virtualization_type = row.virtualization_type;
            conf.loopback_ip_v6 = row.loopback_ip_v6;

            let rendered = build(conf).unwrap();
            let rendered_yaml: serde_yaml::Value = serde_yaml::from_str(&rendered).unwrap();
            let loopback_addresses = rendered_yaml.as_sequence().unwrap()[1]["set"]["interface"]
                ["lo"]["ip"]["address"]
                .as_mapping()
                .unwrap()
                .keys()
                .map(|address| address.as_str().unwrap().to_string())
                .collect();
            let ipv6_underlay_prefixes = rendered_yaml.as_sequence().unwrap()[1]["set"]["router"]
                ["policy"]["prefix-list"]["ALLOW_TO_UNDERLAY_PREFIX_LIST_IPV6"]["rule"]["65000"]
                ["match"]
                .as_mapping()
                .into_iter()
                .flat_map(|mapping| mapping.keys())
                .map(|address| address.as_str().unwrap().to_string())
                .collect();

            assert!(
                rendered.contains("address: 10.0.0.1"),
                "the IPv4 loopback must remain the VXLAN VTEP"
            );
            assert!(
                rendered.contains("router-id: 10.0.0.1"),
                "the IPv4 loopback must remain the BGP router ID"
            );

            (loopback_addresses, ipv6_underlay_prefixes)
        };
            "FNN loopback rendering" {
                LoopbackRenderRow {
                    virtualization_type: VpcVirtualizationType::Fnn,
                    loopback_ip_v6: Some("2001:db8::1".parse().unwrap()),
                } => (
                    address_set(&["10.0.0.1/32", "2001:db8::1/128"]),
                    address_set(&["2001:db8::1/128"]),
                ),
                LoopbackRenderRow {
                    virtualization_type: VpcVirtualizationType::Fnn,
                    loopback_ip_v6: None,
                } => (address_set(&["10.0.0.1/32"]), address_set(&[])),
            }

            "ETV ignores the IPv6 loopback" {
                LoopbackRenderRow {
                    virtualization_type: VpcVirtualizationType::EthernetVirtualizer,
                    loopback_ip_v6: Some("2001:db8::1".parse().unwrap()),
                } => (address_set(&["10.0.0.1/32"]), address_set(&[])),
                LoopbackRenderRow {
                    virtualization_type: VpcVirtualizationType::EthernetVirtualizerWithNvue,
                    loopback_ip_v6: Some("2001:db8::1".parse().unwrap()),
                } => (address_set(&["10.0.0.1/32"]), address_set(&[])),
            }
        );
    }

    #[test]
    fn test_template_for_etv() {
        assert!(template_for(VpcVirtualizationType::EthernetVirtualizer).is_ok());
    }

    #[test]
    fn test_template_for_etv_with_nvue() {
        // EthernetVirtualizerWithNvue is the same as EthernetVirtualizer
        // now, and while we shouldn't see it coming through anymore, lets
        // still make sure we support it.
        assert!(template_for(VpcVirtualizationType::EthernetVirtualizerWithNvue).is_ok());
    }

    #[test]
    fn test_template_for_fnn() {
        assert!(template_for(VpcVirtualizationType::Fnn).is_ok());
    }

    /// Helper to compare build() output against a golden file, using the same
    /// diff-based comparison as the ethernet_virtualization tests.
    fn assert_build_matches_golden(conf: NvueConfig, golden_file: &str) {
        let output = build(conf).expect("build should succeed");
        let expected = golden_file;
        let r = crate::util::compare_lines(&output, expected, None);
        if !r.is_identical() {
            eprintln!("Golden file diff:\n{}", r.report());
            panic!("build output does not match golden file");
        }
    }

    #[test]
    fn test_build_accepts_ethernet_virtualizer() {
        let mut conf = minimal_nvue_config();
        conf.vpc_virtualization_type = VpcVirtualizationType::EthernetVirtualizer;
        assert!(build(conf).is_ok());
    }

    #[test]
    fn test_build_etv_no_ipv6_acls_even_with_ipv6_data() {
        // ETV templates do not support IPv6 ACLs. Even when IPv6 prefix data
        // is provided, the ETV template should not emit any IPv6 ACL blocks.
        let mut conf = minimal_nvue_config();
        conf.use_vpc_isolation = true;
        conf.deny_prefixes = vec!["192.0.2.0/24".into(), "2001:db8:dead::/48".into()];
        conf.site_fabric_prefixes = vec!["10.0.0.0/16".into(), "fd00::/48".into()];
        conf.ct_port_configs = vec![PortConfig {
            interface_name: "pf0vf0_if".into(),
            vlan: 100,
            host_ip: "10.0.1.2".into(),
            host_route: "10.0.1.0/24".into(),
            host_ipv6: None,
            host_ipv6_route: None,
            vni: Some(1000),
            l3_vni: Some(100),
            gateway_cidr: "10.0.1.1/24".into(),
            vpc_prefixes: vec!["10.0.1.0/24".into(), "2001:db8:1::/48".into()],
            vpc_peer_prefixes: vec!["10.0.2.0/24".into(), "2001:db8:2::/48".into()],
            vpc_peer_vnis: vec![],
            svi_ip: None,
            tenant_vrf_loopback_ip: None,
            is_l2_segment: true,
            is_phy: false,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
            ipv6_port_config: None,
        }];
        conf.ct_access_vlans = vec![VlanConfig {
            vlan_id: 100,
            network: "10.0.1.0/24".into(),
            ip: "10.0.1.2".into(),
            ipv6_vlan_config: None,
        }];
        assert_build_matches_golden(
            conf,
            include_str!("../templates/tests/nvue_build_etv_ipv6_data.yaml.expected"),
        );
    }

    #[test]
    fn test_build_fnn_ipv6_acls() {
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.use_vpc_isolation = true;
        conf.deny_prefixes = vec!["192.0.2.0/24".into(), "2001:db8:bad::/48".into()];
        conf.site_fabric_prefixes = vec!["10.0.0.0/16".into(), "fd00:abcd::/32".into()];
        conf.ct_routing_profile = Some(RoutingProfile {
            tenant_leak_communities_accepted: false,
            leak_default_route_from_underlay: false,
            leak_tenant_host_routes_to_underlay: false,
            route_target_imports: vec![],
            route_targets_on_exports: vec![],
            accepted_leaks_from_underlay: vec![],
            allowed_anycast_prefixes: vec![],
        });
        conf.ct_port_configs = vec![PortConfig {
            interface_name: "pf0vf0_if".into(),
            vlan: 100,
            host_ip: "10.0.1.2".into(),
            host_route: "10.0.1.0/24".into(),
            host_ipv6: None,
            host_ipv6_route: None,
            vni: Some(1000),
            l3_vni: Some(100),
            gateway_cidr: "10.0.1.1/24".into(),
            vpc_prefixes: vec!["10.0.1.0/24".into(), "2001:db8:1::/48".into()],
            vpc_peer_prefixes: vec!["10.0.2.0/24".into(), "2001:db8:2::/48".into()],
            vpc_peer_vnis: vec![],
            svi_ip: Some("10.0.1.254".into()),
            tenant_vrf_loopback_ip: Some("10.0.0.2".into()),
            is_l2_segment: false,
            is_phy: false,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
            ipv6_port_config: None,
        }];
        conf.ct_access_vlans = vec![VlanConfig {
            vlan_id: 100,
            network: "10.0.1.0/24".into(),
            ip: "10.0.1.2".into(),
            ipv6_vlan_config: None,
        }];
        assert_build_matches_golden(
            conf,
            include_str!("../templates/tests/nvue_build_fnn_ipv6_acls.yaml.expected"),
        );
    }

    #[test]
    fn test_build_ipv4_only_no_ipv6_acls() {
        let mut conf = minimal_nvue_config();
        conf.use_vpc_isolation = true;
        conf.deny_prefixes = vec!["192.0.2.0/24".into()];
        conf.site_fabric_prefixes = vec!["10.0.0.0/16".into()];
        conf.ct_port_configs = vec![PortConfig {
            interface_name: "pf0vf0_if".into(),
            vlan: 100,
            host_ip: "10.0.1.2".into(),
            host_route: "10.0.1.0/24".into(),
            host_ipv6: None,
            host_ipv6_route: None,
            vni: Some(1000),
            l3_vni: Some(100),
            gateway_cidr: "10.0.1.1/24".into(),
            vpc_prefixes: vec!["10.0.1.0/24".into()],
            vpc_peer_prefixes: vec!["10.0.2.0/24".into()],
            vpc_peer_vnis: vec![],
            svi_ip: None,
            tenant_vrf_loopback_ip: None,
            is_l2_segment: true,
            is_phy: false,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
            ipv6_port_config: None,
        }];
        conf.ct_access_vlans = vec![VlanConfig {
            vlan_id: 100,
            network: "10.0.1.0/24".into(),
            ip: "10.0.1.2".into(),
            ipv6_vlan_config: None,
        }];
        assert_build_matches_golden(
            conf,
            include_str!("../templates/tests/nvue_build_etv_ipv4_only.yaml.expected"),
        );
    }

    #[test]
    fn test_build_fnn_ipv6_only_vpc_prefixes() {
        // When vpc_prefixes contains only IPv6 entries, HasVpcPrefixes (IPv4)
        // should be false, and HasVpcPrefixesIpv6 should be true.
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.use_vpc_isolation = true;
        conf.deny_prefixes = vec!["192.0.2.0/24".into()];
        conf.site_fabric_prefixes = vec!["10.0.0.0/16".into(), "fd00::/48".into()];
        conf.ct_routing_profile = Some(RoutingProfile {
            tenant_leak_communities_accepted: false,
            leak_default_route_from_underlay: false,
            leak_tenant_host_routes_to_underlay: false,

            route_target_imports: vec![],
            route_targets_on_exports: vec![],
            accepted_leaks_from_underlay: vec![],
            allowed_anycast_prefixes: vec![],
        });
        conf.ct_port_configs = vec![PortConfig {
            interface_name: "pf0vf0_if".into(),
            vlan: 100,
            host_ip: "10.0.1.2".into(),
            host_route: "10.0.1.0/24".into(),
            host_ipv6: None,
            host_ipv6_route: None,
            vni: Some(1000),
            l3_vni: Some(100),
            gateway_cidr: "10.0.1.1/24".into(),
            vpc_prefixes: vec!["2001:db8:1::/48".into(), "2001:db8:2::/48".into()],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            svi_ip: Some("10.0.1.254".into()),
            tenant_vrf_loopback_ip: Some("10.0.0.2".into()),
            is_l2_segment: false,
            is_phy: false,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
            ipv6_port_config: None,
        }];
        conf.ct_access_vlans = vec![VlanConfig {
            vlan_id: 100,
            network: "10.0.1.0/24".into(),
            ip: "10.0.1.2".into(),
            ipv6_vlan_config: None,
        }];
        assert_build_matches_golden(
            conf,
            include_str!("../templates/tests/nvue_build_fnn_ipv6_only_vpc.yaml.expected"),
        );
    }

    #[test]
    fn test_build_deny_prefix_index_offset() {
        // When site_fabric_prefixes has entries, deny prefix indices should
        // start after them (offset by site_fabric_prefixes.len()).
        let mut conf = minimal_nvue_config();
        conf.deny_prefixes = vec!["192.0.2.0/24".into(), "2001:db8:bad::/48".into()];
        conf.site_fabric_prefixes = vec![
            "10.0.0.0/16".into(),
            "172.16.0.0/12".into(),
            "fd00::/48".into(),
        ];
        conf.ct_port_configs = vec![PortConfig {
            interface_name: "pf0vf0_if".into(),
            vlan: 100,
            host_ip: "10.0.1.2".into(),
            host_route: "10.0.1.0/24".into(),
            host_ipv6: None,
            host_ipv6_route: None,
            vni: Some(1000),
            l3_vni: Some(100),
            gateway_cidr: "10.0.1.1/24".into(),
            vpc_prefixes: vec!["10.0.1.0/24".into()],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            svi_ip: None,
            tenant_vrf_loopback_ip: None,
            is_l2_segment: true,
            is_phy: false,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
            ipv6_port_config: None,
        }];
        conf.ct_access_vlans = vec![VlanConfig {
            vlan_id: 100,
            network: "10.0.1.0/24".into(),
            ip: "10.0.1.2".into(),
            ipv6_vlan_config: None,
        }];
        assert_build_matches_golden(
            conf,
            include_str!("../templates/tests/nvue_build_etv_deny_prefix_offset.yaml.expected"),
        );
    }

    #[test]
    fn test_build_fnn_multi_port_ipv6_accumulation() {
        // When multiple ports belong to the same VPC (same l3_vni),
        // PortPrefixesIpv6 should accumulate from all ports.
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.use_vpc_isolation = true;
        conf.site_fabric_prefixes = vec!["10.0.0.0/16".into(), "fd00::/32".into()];
        conf.ct_routing_profile = Some(RoutingProfile {
            tenant_leak_communities_accepted: false,
            leak_default_route_from_underlay: false,
            leak_tenant_host_routes_to_underlay: false,
            route_target_imports: vec![],
            route_targets_on_exports: vec![],
            accepted_leaks_from_underlay: vec![],
            allowed_anycast_prefixes: vec![],
        });
        conf.ct_port_configs = vec![
            PortConfig {
                interface_name: "pf0vf0_if".into(),
                vlan: 100,
                host_ip: "10.0.1.2".into(),
                host_route: "10.0.1.0/24".into(),
                host_ipv6: None,
                host_ipv6_route: None,
                vni: Some(1000),
                l3_vni: Some(200),
                gateway_cidr: "10.0.1.1/24".into(),
                vpc_prefixes: vec!["10.0.1.0/24".into(), "2001:db8:1::/48".into()],
                vpc_peer_prefixes: vec![],
                vpc_peer_vnis: vec![],
                svi_ip: Some("10.0.1.254".into()),
                tenant_vrf_loopback_ip: Some("10.0.0.2".into()),
                is_l2_segment: false,
                is_phy: false,
                network_security_group_id: None,
                routing_profile: None,
                interface_routing_profile: None,
                ipv6_port_config: None,
            },
            PortConfig {
                interface_name: "pf0hpf_if".into(),
                vlan: 101,
                host_ip: "10.0.2.2".into(),
                host_route: "10.0.2.0/24".into(),
                host_ipv6: None,
                host_ipv6_route: None,
                vni: Some(1001),
                l3_vni: Some(200),
                gateway_cidr: "10.0.2.1/24".into(),
                vpc_prefixes: vec!["10.0.2.0/24".into(), "2001:db8:2::/48".into()],
                vpc_peer_prefixes: vec![],
                vpc_peer_vnis: vec![],
                svi_ip: Some("10.0.2.254".into()),
                tenant_vrf_loopback_ip: Some("10.0.0.2".into()),
                is_l2_segment: false,
                is_phy: false,
                network_security_group_id: None,
                routing_profile: None,
                interface_routing_profile: None,
                ipv6_port_config: None,
            },
        ];
        conf.ct_access_vlans = vec![
            VlanConfig {
                vlan_id: 100,
                network: "10.0.1.0/24".into(),
                ip: "10.0.1.2".into(),
                ipv6_vlan_config: None,
            },
            VlanConfig {
                vlan_id: 101,
                network: "10.0.2.0/24".into(),
                ip: "10.0.2.2".into(),
                ipv6_vlan_config: None,
            },
        ];
        assert_build_matches_golden(
            conf,
            include_str!("../templates/tests/nvue_build_fnn_multi_port_ipv6.yaml.expected"),
        );
    }

    /// Builds the dual-stack FNN fixture shared by the address and route-leak tests.
    fn dual_stack_fnn_config() -> NvueConfig {
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.use_vpc_isolation = true;
        conf.site_fabric_prefixes = vec!["10.0.0.0/16".into(), "fd00::/32".into()];
        conf.ct_routing_profile = Some(RoutingProfile {
            leak_default_route_from_underlay: false,
            leak_tenant_host_routes_to_underlay: false,
            tenant_leak_communities_accepted: false,
            route_target_imports: vec![],
            route_targets_on_exports: vec![],
            accepted_leaks_from_underlay: vec![],
            allowed_anycast_prefixes: vec![],
        });
        conf.ct_port_configs = vec![PortConfig {
            interface_name: "pf0vf0_if".into(),
            vlan: 100,
            host_ip: "10.0.1.1".into(),
            host_route: "10.0.1.0/31".into(),
            host_ipv6: Some("2001:db8::1".into()),
            host_ipv6_route: Some("2001:db8::0/127".into()),
            vni: Some(1000),
            l3_vni: Some(100),
            gateway_cidr: "10.0.1.0/31".into(),
            ipv6_port_config: Some(Ipv6PortConfig {
                gateway_cidr: "2001:db8::0/127".into(),
                svi_ip: None,
            }),
            vpc_prefixes: vec!["10.0.1.0/24".into(), "2001:db8::/48".into()],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            svi_ip: Some("10.0.1.254".into()),
            tenant_vrf_loopback_ip: Some("10.0.0.2".into()),
            is_l2_segment: false,
            is_phy: false,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
        }];
        conf.ct_access_vlans = vec![VlanConfig {
            vlan_id: 100,
            network: "10.0.1.0/31".into(),
            ip: "10.0.1.1".into(),
            ipv6_vlan_config: Some(Ipv6VlanConfig {
                network: "2001:db8::0/127".into(),
                ip: "2001:db8::1".into(),
            }),
        }];
        conf
    }

    /// Verifies repeated VPC-wide prefixes from multiple ports produce one permit rule per
    /// CIDR, because duplicate YAML rule keys prevent NVUE from applying the entire config.
    #[test]
    fn test_build_deduplicates_shared_vpc_isolation_prefixes() {
        // Model Core's repeated VPC-wide list at the old 10-rule collision boundary.
        let mut conf = dual_stack_fnn_config();
        conf.site_fabric_prefixes = vec!["192.0.2.0/24".into(), "fd00::/32".into()];
        let shared_ipv4_prefixes = (0..11)
            .map(|index| format!("10.{index}.0.0/16"))
            .collect::<Vec<_>>();
        let shared_ipv6_prefixes = (0..11)
            .map(|index| format!("2001:db8:{index:x}::/48"))
            .collect::<Vec<_>>();
        let shared_prefixes = shared_ipv4_prefixes
            .iter()
            .chain(&shared_ipv6_prefixes)
            .cloned()
            .collect::<Vec<_>>();
        conf.ct_port_configs[0].vpc_prefixes = shared_prefixes.clone();
        conf.ct_port_configs.push(PortConfig {
            interface_name: "pf0vf1_if".into(),
            vlan: 101,
            host_ip: "172.16.0.1".into(),
            host_route: "172.16.0.0/31".into(),
            host_ipv6: None,
            host_ipv6_route: None,
            vni: Some(1001),
            l3_vni: Some(100),
            gateway_cidr: "172.16.0.0/31".into(),
            ipv6_port_config: None,
            vpc_prefixes: shared_prefixes,
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            svi_ip: None,
            tenant_vrf_loopback_ip: Some("10.0.0.2".into()),
            is_l2_segment: false,
            is_phy: false,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
        });

        // Parsing exercises the duplicate-key rejection boundary from the report.
        let output = build(conf).expect("build should succeed");
        let documents: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("output should have unique YAML keys");
        let acl = &documents.as_sequence().unwrap()[1]["set"]["acl"];

        // Both families preserve established indices and render each unique permit once.
        let assert_isolation_rules = |policy: &str, expected: &[String]| {
            let rules = &acl[policy]["rule"];
            assert_eq!(
                rules["20"]["match"]["ip"]["dest-ip"].as_str(),
                Some(expected[10].as_str())
            );
            let rules = rules
                .as_mapping()
                .expect("isolation rules should be a mapping");
            let permit_prefixes = rules
                .values()
                .filter(|rule| !rule["action"]["permit"].is_null())
                .map(|rule| {
                    rule["match"]["ip"]["dest-ip"]
                        .as_str()
                        .expect("permit rule should have a destination prefix")
                        .to_string()
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(
                permit_prefixes,
                expected.iter().cloned().collect::<BTreeSet<_>>()
            );
            assert_eq!(rules.len(), expected.len() + 1);
        };
        assert_isolation_rules("p0010_vpc_100_isolation_ipv4", &shared_ipv4_prefixes);
        assert_isolation_rules("p0010_vpc_100_isolation_ipv6", &shared_ipv6_prefixes);
    }

    /// Verifies FNN site-fabric indices move only for an occupied rule key in the same VPC and
    /// address family, because each per-VPC policy is a separate YAML mapping.
    #[test]
    fn test_build_scopes_fnn_site_fabric_indices_to_each_acl() {
        // Fill one VPC's IPv6 permit range through index 1000 and keep a second VPC small.
        let mut conf = dual_stack_fnn_config();
        conf.site_fabric_prefixes = vec!["192.0.2.0/24".into(), "fd00::/32".into()];
        conf.ct_port_configs[0].vpc_prefixes = (0..991)
            .map(|index| format!("2001:db8:{index:x}::/64"))
            .collect();
        conf.ct_port_configs.push(PortConfig {
            interface_name: "pf0vf1_if".into(),
            vlan: 101,
            host_ip: "172.16.0.1".into(),
            host_route: "172.16.0.0/31".into(),
            host_ipv6: None,
            host_ipv6_route: None,
            vni: Some(1001),
            l3_vni: Some(200),
            gateway_cidr: "172.16.0.0/31".into(),
            ipv6_port_config: None,
            vpc_prefixes: vec!["10.20.0.0/16".into(), "2001:db9::/48".into()],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            svi_ip: None,
            tenant_vrf_loopback_ip: Some("10.0.0.3".into()),
            is_l2_segment: false,
            is_phy: false,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
        });

        // Render the independent VPC/family ACL mappings.
        let output = build(conf).expect("build should succeed");
        let documents: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("output should have unique YAML keys");
        let acl = &documents.as_sequence().unwrap()[1]["set"]["acl"];

        // Only the high-index VPC's IPv6 site rule moves beyond the occupied key.
        let assert_site_rule = |policy: &str, index: &str, expected: &str| {
            let rule = &acl[policy]["rule"][index];
            assert!(!rule["action"]["deny"].is_null());
            assert_eq!(rule["match"]["ip"]["dest-ip"].as_str(), Some(expected));
        };
        assert_site_rule("p0010_vpc_100_isolation_ipv4", "1000", "192.0.2.0/24");
        assert_site_rule("p0010_vpc_100_isolation_ipv6", "1001", "fd00::/32");
        assert_site_rule("p0010_vpc_200_isolation_ipv4", "1000", "192.0.2.0/24");
        assert_site_rule("p0010_vpc_200_isolation_ipv6", "1000", "fd00::/32");
    }

    /// Verifies ETV advances site and deny rules only beyond permits in their respective
    /// tenant-wide ACL mappings, because those policies do not share their rule keys.
    #[test]
    fn test_build_scopes_etv_site_and_deny_indices_to_each_acl() {
        // Occupy ETV permit indices through 1001 in both tenant-wide policies.
        let mut conf = dual_stack_fnn_config();
        conf.is_fnn = false;
        conf.vpc_virtualization_type = VpcVirtualizationType::EthernetVirtualizer;
        conf.ct_routing_profile = None;
        conf.site_fabric_prefixes = vec!["192.0.2.0/24".into()];
        conf.deny_prefixes = vec!["198.51.100.0/24".into()];
        conf.ct_port_configs[0].vpc_prefixes = (0..992)
            .map(|index| format!("10.{}.{}.0/24", index / 256, index % 256))
            .collect();

        // Render and inspect the two independent ETV ACL mappings.
        let output = build(conf).expect("build should succeed");
        let documents: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("output should have unique YAML keys");
        let acl = &documents.as_sequence().unwrap()[1]["set"]["acl"];

        // Both policies may independently use 1002 after their common permit range.
        assert_eq!(
            acl["p0010_vpc_isolation_ipv4"]["rule"]["1002"]["match"]["ip"]["dest-ip"].as_str(),
            Some("192.0.2.0/24")
        );
        assert_eq!(
            acl["p0000_deny_prefixes_ipv4"]["rule"]["1002"]["match"]["ip"]["dest-ip"].as_str(),
            Some("198.51.100.0/24")
        );
    }

    #[test]
    fn test_build_fnn_dual_stack_interface() {
        let mut conf = dual_stack_fnn_config();
        conf.ct_routing_profile
            .as_mut()
            .expect("fixture should have a routing profile")
            .leak_tenant_host_routes_to_underlay = true;
        assert_build_matches_golden(
            conf,
            include_str!("../templates/tests/nvue_build_fnn_dual_stack.yaml.expected"),
        );
    }

    #[test]
    fn test_build_fnn_ipv6_only_interface() {
        let mut conf = dual_stack_fnn_config();
        let port = conf
            .ct_port_configs
            .first_mut()
            .expect("fixture should have a port");
        port.host_ip.clear();
        port.host_route.clear();
        port.gateway_cidr.clear();
        port.host_ipv6 = Some(String::new());
        port.host_ipv6_route = None;
        port.svi_ip = None;
        port.ipv6_port_config
            .as_mut()
            .expect("fixture should have an IPv6 port config")
            .gateway_cidr = "2001:db8::/64".into();
        port.vpc_prefixes
            .retain(|prefix| matches!(prefix.parse::<IpNet>(), Ok(IpNet::V6(_))));

        let vlan = conf
            .ct_access_vlans
            .first_mut()
            .expect("fixture should have an access VLAN");
        vlan.ip.clear();
        vlan.network.clear();
        vlan.ipv6_vlan_config
            .as_mut()
            .expect("fixture should have an IPv6 VLAN config")
            .ip
            .clear();

        let output = build(conf).expect("build should succeed");
        let docs: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("output should be valid YAML");
        let set = &docs.as_sequence().unwrap()[1]["set"];

        assert_eq!(
            yaml_mapping_keys(&set["interface"]["pf0vf0_if"]["ip"]["address"]),
            address_set(&["2001:db8::/64"]),
        );
        let bgp = &set["vrf"]["vpc_100"]["router"]["bgp"];
        assert!(
            bgp.get("neighbor").is_none(),
            "interface with no host address must omit the empty neighbor map"
        );
        assert!(
            !has_null_leaf(&docs),
            "interface with no host address rendered a null config leaf:\n\n{output}"
        );
    }

    #[test]
    fn test_build_fnn_dual_stack_l2_interface() {
        let mut conf = dual_stack_fnn_config();
        let port = conf
            .ct_port_configs
            .first_mut()
            .expect("fixture should have a port");
        port.is_l2_segment = true;
        port.gateway_cidr = "10.0.1.1/24".into();
        port.svi_ip = Some("10.0.1.2".into());
        let ipv6 = port
            .ipv6_port_config
            .as_mut()
            .expect("fixture should have IPv6 port config");
        ipv6.gateway_cidr = "2001:db8::1/64".into();
        ipv6.svi_ip = Some("2001:db8::2".into());

        let output = build(conf).expect("build should succeed");
        let docs: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("output should be valid YAML");
        let vlan = &docs.as_sequence().unwrap()[1]["set"]["interface"]["vlan100"];

        assert_eq!(
            yaml_mapping_keys(&vlan["ip"]["address"]),
            address_set(&["10.0.1.2", "2001:db8::2"]),
        );
        assert_eq!(
            yaml_mapping_keys(&vlan["ip"]["vrr"]["address"]),
            address_set(&["10.0.1.1/24", "2001:db8::1/64"]),
        );
    }

    #[test]
    fn test_build_etv_dual_stack_interface() {
        let mut conf = dual_stack_fnn_config();
        conf.is_fnn = false;
        conf.vpc_virtualization_type = VpcVirtualizationType::EthernetVirtualizer;
        conf.ct_routing_profile = None;

        let output = build(conf).expect("build should succeed");
        let docs: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("output should be valid YAML");
        let addresses =
            &docs.as_sequence().unwrap()[1]["set"]["interface"]["vlan100"]["ip"]["address"];

        assert_eq!(
            yaml_mapping_keys(addresses),
            address_set(&["10.0.1.0/31", "2001:db8::0/127"]),
        );
    }

    struct FnnUnderlayLeakRow {
        leak_tenant_host_routes_to_underlay: bool,
        include_ipv6: bool,
    }

    #[derive(Debug, PartialEq)]
    struct FnnUnderlayLeakResult {
        interface_addresses: BTreeSet<String>,
        has_ipv4_leak_rule: bool,
        ipv4_leak_prefixes: BTreeSet<String>,
        has_ipv6_leak_rule: bool,
        ipv6_leak_prefixes: BTreeSet<String>,
    }

    #[test]
    fn test_build_fnn_underlay_leaks_follow_profile_and_address_family() {
        value_scenarios!(run = |row: FnnUnderlayLeakRow| {
            let mut conf = dual_stack_fnn_config();
            conf.ct_routing_profile
                .as_mut()
                .expect("fixture should have a routing profile")
                .leak_tenant_host_routes_to_underlay =
                row.leak_tenant_host_routes_to_underlay;
            if !row.include_ipv6 {
                let port = conf
                    .ct_port_configs
                    .first_mut()
                    .expect("fixture should have a port");
                port.host_ipv6 = None;
                port.host_ipv6_route = None;
                port.ipv6_port_config = None;
                conf.ct_access_vlans
                    .first_mut()
                    .expect("fixture should have an access VLAN")
                    .ipv6_vlan_config = None;
            }

            let output = build(conf).expect("build should succeed");
            let docs: serde_yaml::Value =
                serde_yaml::from_str(&output).expect("output should be valid YAML");
            let set = &docs.as_sequence().unwrap()[1]["set"];
            let prefix_lists = &set["router"]["policy"]["prefix-list"];
            let ipv4_leak_rule =
                &prefix_lists["ALLOW_TO_UNDERLAY_PREFIX_LIST"]["rule"]["65002"];
            let ipv6_prefix_list = &prefix_lists["ALLOW_TO_UNDERLAY_PREFIX_LIST_IPV6"];
            let ipv6_leak_rule = &ipv6_prefix_list["rule"]["65002"];
            assert_eq!(
                ipv6_prefix_list["rule"]["65535"]["action"].as_str(),
                Some("deny"),
                "IPv6 underlay prefix list should retain its default deny",
            );
            assert!(
                ipv6_prefix_list["rule"]["65535"]["match"]["any"]
                    .as_mapping()
                    .is_some(),
                "IPv6 default deny should match any route",
            );

            FnnUnderlayLeakResult {
                interface_addresses: yaml_mapping_keys(
                    &set["interface"]["pf0vf0_if"]["ip"]["address"],
                ),
                has_ipv4_leak_rule: !ipv4_leak_rule.is_null(),
                ipv4_leak_prefixes: yaml_mapping_keys(&ipv4_leak_rule["match"]),
                has_ipv6_leak_rule: !ipv6_leak_rule.is_null(),
                ipv6_leak_prefixes: yaml_mapping_keys(&ipv6_leak_rule["match"]),
            }
        };
            "the routing profile and interface family select leak rules" {
                FnnUnderlayLeakRow {
                    leak_tenant_host_routes_to_underlay: false,
                    include_ipv6: true,
                } => FnnUnderlayLeakResult {
                    interface_addresses: address_set(&["10.0.1.0/31", "2001:db8::0/127"]),
                    has_ipv4_leak_rule: false,
                    ipv4_leak_prefixes: address_set(&[]),
                    has_ipv6_leak_rule: false,
                    ipv6_leak_prefixes: address_set(&[]),
                },
                FnnUnderlayLeakRow {
                    leak_tenant_host_routes_to_underlay: true,
                    include_ipv6: true,
                } => FnnUnderlayLeakResult {
                    interface_addresses: address_set(&["10.0.1.0/31", "2001:db8::0/127"]),
                    has_ipv4_leak_rule: true,
                    ipv4_leak_prefixes: address_set(&["10.0.1.0/31"]),
                    has_ipv6_leak_rule: true,
                    ipv6_leak_prefixes: address_set(&["2001:db8::0/127"]),
                },
                FnnUnderlayLeakRow {
                    leak_tenant_host_routes_to_underlay: true,
                    include_ipv6: false,
                } => FnnUnderlayLeakResult {
                    interface_addresses: address_set(&["10.0.1.0/31"]),
                    has_ipv4_leak_rule: true,
                    ipv4_leak_prefixes: address_set(&["10.0.1.0/31"]),
                    has_ipv6_leak_rule: false,
                    ipv6_leak_prefixes: address_set(&[]),
                },
            }
        );
    }

    /// `serde_yaml::Value::Null` indicates a YAML key with no value (i.e. a
    /// bare `key:` followed by nothing). In cases like this, YAML parsing fails,
    /// and NVUE subsequently rejects with something like `Error: 'set' operation
    /// values must not be 'null'`.
    /// This helper exists so we can check for empty leaf renderings in general for
    /// any tests that would like to check for such a situation.
    fn has_null_leaf(v: &serde_yaml::Value) -> bool {
        match v {
            serde_yaml::Value::Null => true,
            serde_yaml::Value::Mapping(m) => m.values().any(has_null_leaf),
            serde_yaml::Value::Sequence(s) => s.iter().any(has_null_leaf),
            _ => false,
        }
    }

    /// When a DPU has no tenant VPCs assigned, the rendered FNN YAML must contain
    /// no null config leaves (like a `list:` with no entries, or `from-vrf:` with
    /// no subsequent config, etc).
    #[test]
    fn test_build_fnn_with_no_vpcs_emits_no_yaml_nulls() {
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        // For this one, we'll intentionally leave ct_port_configs / ct_access_vlans empty
        // to ensure $tenant.Vpcs is empty in the template, and that route-import
        // blocks that would otherwise have an empty VPC VRF list are excluded.
        assert!(conf.ct_port_configs.is_empty());

        let output = build(conf).expect("build should succeed with empty ct_port_configs");
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("rendered YAML must parse");

        assert!(
            !has_null_leaf(&parsed),
            "rendered YAML contains a null leaf:\n\n{output}"
        );
    }

    fn phy_port_config(vlan: u16) -> PortConfig {
        PortConfig {
            interface_name: "pf0hpf_if".into(),
            vlan,
            host_ip: "10.0.1.2".into(),
            host_route: "10.0.1.0/24".into(),
            host_ipv6: None,
            host_ipv6_route: None,
            vni: Some(1000),
            l3_vni: Some(100),
            gateway_cidr: "10.0.1.1/24".into(),
            vpc_prefixes: vec![],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            svi_ip: Some("10.0.1.2".into()),
            tenant_vrf_loopback_ip: None,
            is_l2_segment: true,
            is_phy: true,
            network_security_group_id: None,
            routing_profile: None,
            interface_routing_profile: None,
            ipv6_port_config: None,
        }
    }

    // In container mode with fmds_gateway_vlan matching the phy port vlan, the
    // FMDS gateway address should appear in the rendered output.
    #[test]
    fn test_fmds_gateway_etv_container_mode_emits_address() {
        let mut conf = minimal_nvue_config();
        conf.is_dpu_os = false;
        conf.fmds_gateway_vlan = Some(274);
        conf.ct_port_configs = vec![phy_port_config(274)];
        let output = build(conf).expect("build should succeed");
        assert!(
            output.contains("169.254.169.253/30"),
            "expected FMDS gateway address in output:\n{output}"
        );
    }

    // In DPU-OS mode the FMDS gateway address must NOT appear on the vlan SVI
    // (it lives on pf0dpu1_if instead, managed by the startup template).
    #[test]
    fn test_fmds_gateway_etv_dpu_os_mode_suppressed() {
        let mut conf = minimal_nvue_config();
        conf.is_dpu_os = true;
        // Even if fmds_gateway_vlan is accidentally set, DPU-OS mode must ignore it.
        conf.fmds_gateway_vlan = Some(274);
        conf.ct_port_configs = vec![phy_port_config(274)];
        let output = build(conf).expect("build should succeed");
        // The ETV template uses pf0dpu1_if for the gateway in DPU-OS mode, not the vlan SVI.
        assert!(
            !output.contains("169.254.169.253/30") || output.contains("pf0dpu1_if"),
            "DPU-OS mode should not put 169.254.169.253/30 on the vlan SVI"
        );
    }

    // fmds_gateway_vlan pointing at a vlan that does not exist in ct_port_configs
    // should log an error but still return a valid (if incomplete) config.
    #[test]
    fn test_fmds_gateway_vlan_not_in_port_configs_still_builds() {
        let mut conf = minimal_nvue_config();
        conf.is_dpu_os = false;
        conf.fmds_gateway_vlan = Some(999); // no port has vlan 999
        conf.ct_port_configs = vec![phy_port_config(274)];
        // Should succeed (just logs an error, does not fail the build).
        let output = build(conf).expect("build should succeed even when vlan is missing");
        assert!(
            !output.contains("169.254.169.253/30"),
            "mismatched vlan should not produce FMDS address in output"
        );
    }

    // fmds_gateway_vlan=None should never emit the FMDS gateway address.
    #[test]
    fn test_fmds_gateway_none_emits_no_address() {
        let mut conf = minimal_nvue_config();
        conf.is_dpu_os = false;
        conf.fmds_gateway_vlan = None;
        conf.ct_port_configs = vec![phy_port_config(274)];
        let output = build(conf).expect("build should succeed");
        assert!(
            !output.contains("169.254.169.253/30"),
            "no fmds_gateway_vlan should not produce FMDS address in output"
        );
    }

    fn minimal_fnn_routing_profile() -> RoutingProfile {
        RoutingProfile {
            tenant_leak_communities_accepted: false,
            leak_default_route_from_underlay: false,
            leak_tenant_host_routes_to_underlay: false,
            route_target_imports: vec![],
            route_targets_on_exports: vec![],
            accepted_leaks_from_underlay: vec![],
            allowed_anycast_prefixes: vec![],
        }
    }

    // Same checks for FNN template.
    #[test]
    fn test_fmds_gateway_fnn_container_mode_emits_address() {
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.is_dpu_os = false;
        conf.fmds_gateway_vlan = Some(274);
        conf.ct_routing_profile = Some(minimal_fnn_routing_profile());
        conf.ct_port_configs = vec![phy_port_config(274)];
        let output = build(conf).expect("build should succeed");
        assert!(
            output.contains("169.254.169.253/30"),
            "expected FMDS gateway address in FNN output:\n{output}"
        );
    }

    #[test]
    fn test_fmds_gateway_fnn_dpu_os_mode_suppressed() {
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.is_dpu_os = true;
        conf.fmds_gateway_vlan = Some(274);
        conf.ct_routing_profile = Some(minimal_fnn_routing_profile());
        conf.ct_port_configs = vec![phy_port_config(274)];
        let output = build(conf).expect("build should succeed");
        assert!(
            !output.contains("169.254.169.253/30") || output.contains("pf0dpu1_if"),
            "DPU-OS mode should not put 169.254.169.253/30 on the vlan SVI in FNN"
        );
    }

    fn l3_phy_port_config(vlan: u16) -> PortConfig {
        PortConfig {
            is_l2_segment: false,
            svi_ip: None,
            ..phy_port_config(vlan)
        }
    }

    // FNN L3 (is_l2_segment=false): in container mode the FMDS gateway address
    // must appear on the L3 interface's ip block, not an SVI.
    #[test]
    fn test_fmds_gateway_fnn_l3_container_mode_emits_address() {
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.is_dpu_os = false;
        conf.fmds_gateway_vlan = Some(274);
        conf.ct_routing_profile = Some(minimal_fnn_routing_profile());
        conf.ct_port_configs = vec![l3_phy_port_config(274)];
        let output = build(conf).expect("build should succeed");
        assert!(
            output.contains("169.254.169.253/30"),
            "expected FMDS gateway address on FNN L3 interface:\n{output}"
        );
    }

    // FNN L3 in DPU-OS mode: 169.254.169.253/30 must NOT appear on pf0hpf_if
    // (the startup template puts it on pf0dpu1_if instead). The naive
    // `!contains(addr) || contains(pf0dpu1_if)` check is vacuously true because
    // the FNN template always renders pf0dpu1_if in DPU-OS mode, so we parse the
    // YAML and assert at the per-interface level.
    #[test]
    fn test_fmds_gateway_fnn_l3_dpu_os_mode_suppressed() {
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.is_dpu_os = true;
        conf.fmds_gateway_vlan = Some(274);
        conf.ct_routing_profile = Some(minimal_fnn_routing_profile());
        conf.ct_port_configs = vec![l3_phy_port_config(274)];
        let output = build(conf).expect("build should succeed");
        let docs: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("output should be valid YAML");
        let interfaces = &docs.as_sequence().unwrap()[1]["set"]["interface"];
        // The L3 interface must not carry the FMDS address.
        assert!(
            interfaces["pf0hpf_if"]["ip"]["address"]["169.254.169.253/30"].is_null(),
            "DPU-OS mode must not put 169.254.169.253/30 on pf0hpf_if:\n{output}"
        );
        // The startup-template interface (pf0dpu1_if) must carry it instead.
        assert!(
            !interfaces["pf0dpu1_if"]["ip"]["address"]["169.254.169.253/30"].is_null(),
            "DPU-OS mode must put 169.254.169.253/30 on pf0dpu1_if:\n{output}"
        );
    }

    // FNN with an L3 Physical port and a mismatched fmds_gateway_vlan should not
    // emit the FMDS address (and should not panic).
    #[test]
    fn test_fmds_gateway_fnn_l3_vlan_mismatch_emits_nothing() {
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.is_dpu_os = false;
        conf.fmds_gateway_vlan = Some(999); // no port has vlan 999
        conf.ct_routing_profile = Some(minimal_fnn_routing_profile());
        conf.ct_port_configs = vec![l3_phy_port_config(274)];
        let output = build(conf).expect("build should succeed even with mismatched vlan");
        assert!(
            !output.contains("169.254.169.253/30"),
            "mismatched vlan should not produce FMDS address on FNN L3 interface:\n{output}"
        );
    }

    #[test]
    fn test_fnn_no_interface_routing_profile_uses_vpc_allowed_anycast_prefixes() {
        // Build an FNN config with a VPC routing profile and no interface override.
        let mut conf = minimal_nvue_config();
        conf.is_fnn = true;
        conf.vpc_virtualization_type = VpcVirtualizationType::Fnn;
        conf.ct_routing_profile = Some(RoutingProfile {
            allowed_anycast_prefixes: vec!["192.0.2.0/24".into(), "2001:db8::/32".into()],
            ..minimal_fnn_routing_profile()
        });
        conf.ct_port_configs = vec![phy_port_config(274)];

        // Render through the template to exercise the VPC-level fallback path.
        let output = build(conf).expect("build should succeed");

        // Verify the VPC allow-list is emitted for both IPv4 and IPv6.
        assert!(
            output.contains("DPU_FROM_INSTANCE_PREFIX_LIST_vpc_100_if_10:"),
            "expected IPv4 VPC prefix-list in output:\n{output}"
        );
        assert!(output.contains("192.0.2.0/24:"));
        assert!(
            output.contains("DPU_FROM_INSTANCE_PREFIX_LIST_IPV6_vpc_100_if_10:"),
            "expected IPv6 VPC prefix-list in output:\n{output}"
        );
        assert!(output.contains("2001:db8::/32:"));
    }
}
