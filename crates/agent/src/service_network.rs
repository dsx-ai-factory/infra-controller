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

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv6Addr};

use ::rpc::forge as rpc;
use carbide_rpc_utils::dhcp::{
    ServiceConfig, ServiceDhcpInterfaceReservations, ServiceDhcpReservation, ServiceLink,
    ServiceLinkV4, ServiceLinkV6,
};
use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::vpc::VpcId;
use eyre::WrapErr;
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use prost::Message;
use uuid::Uuid;

use crate::{HBNDeviceNames, nvue};

const SERVICE_VPC_MTU: u32 = 9_000;

struct ServicePortGroup {
    interface: rpc::ManagedHostServiceInterfaceConfig,
    ipv4_addresses: BTreeSet<String>,
    ipv6_addresses: BTreeSet<String>,
    reservations: Vec<ServiceDhcpReservation>,
}

/// Validates service DTOs and groups their HBN addresses by local slot and VPC so one fixed port
/// represents every endpoint sharing that binding.
pub(super) fn validate_and_build_ports(
    config: &rpc::ManagedHostNetworkConfigResponse,
) -> eyre::Result<Vec<nvue::PortConfig>> {
    // Preserve initial host-only rolling upgrades without accepting endpoints whose slots are
    // unknown.
    let inventory = match config.service_vpc_slot_inventory.as_ref() {
        Some(inventory) => inventory,
        None if config.service_interfaces.is_empty() => return Ok(Vec::new()),
        None => eyre::bail!("service interfaces require a fixed-slot inventory"),
    };

    // Validate configured topology even when no endpoint currently occupies a slot.
    let host_port_names = normal_host_port_names(config);
    let mut slot_names = BTreeMap::new();
    let mut inventory_names = BTreeSet::new();
    for (expected_index, slot) in inventory.slots.iter().enumerate() {
        let expected_index =
            u32::try_from(expected_index).wrap_err("service slot index overflow")?;
        eyre::ensure!(
            slot.slot_index == expected_index,
            "service slot inventory index {} is not contiguous at expected index {expected_index}",
            slot.slot_index
        );
        eyre::ensure!(
            !slot.hbn_interface_name.is_empty(),
            "service slot {expected_index} has an empty HBN interface name"
        );
        eyre::ensure!(
            inventory_names.insert(slot.hbn_interface_name.as_str()),
            "duplicate service slot HBN interface name {}",
            slot.hbn_interface_name
        );
        eyre::ensure!(
            !host_port_names.contains(&slot.hbn_interface_name),
            "service slot HBN interface name {} collides with a host port",
            slot.hbn_interface_name
        );
        slot_names.insert(slot.slot_index, slot.hbn_interface_name.clone());
    }

    // Presence-bearing zero capacity is distinct from a legacy omission and cannot carry
    // endpoints.
    if inventory.slots.is_empty() {
        eyre::ensure!(
            config.service_interfaces.is_empty(),
            "service interfaces cannot use a zero-slot inventory"
        );
        return Ok(Vec::new());
    }
    eyre::ensure!(
        config.site_global_vpc_vni.is_none(),
        "service VPC slots are incompatible with site_global_vpc_vni"
    );
    if config.service_interfaces.is_empty() {
        return Ok(Vec::new());
    }
    eyre::ensure!(
        config.network_virtualization_type() == rpc::VpcVirtualizationType::Fnn,
        "service interfaces require FNN virtualization"
    );

    let mut slot_vpcs = BTreeMap::<u32, VpcId>::new();
    let mut vpc_slots = BTreeMap::<VpcId, u32>::new();
    let mut vni_vpcs = BTreeMap::<u32, VpcId>::new();
    let mut groups = BTreeMap::<(u32, VpcId), ServicePortGroup>::new();

    // Validate every endpoint before grouping so a shared slot/VPC cannot hide invalid IDs, links,
    // MAC addresses, or VPC policy fields.
    for (index, interface) in config.service_interfaces.iter().enumerate() {
        validate_service_interface_fields(interface)
            .wrap_err_with(|| format!("invalid service interface at index {index}"))?;

        let vpc_id = interface
            .vpc_id
            .ok_or_else(|| eyre::eyre!("service interface is missing vpc_id"))?;
        slot_names.get(&interface.slot_index).ok_or_else(|| {
            eyre::eyre!(
                "service interface references unconfigured slot {}",
                interface.slot_index
            )
        })?;

        match slot_vpcs.get(&interface.slot_index) {
            Some(existing) if *existing != vpc_id => eyre::bail!(
                "service slot {} is bound to both VPC {existing} and VPC {vpc_id}",
                interface.slot_index
            ),
            _ => {
                slot_vpcs.insert(interface.slot_index, vpc_id);
            }
        }
        match vpc_slots.get(&vpc_id) {
            Some(existing) if *existing != interface.slot_index => eyre::bail!(
                "service VPC {vpc_id} is bound to both slot {existing} and slot {}",
                interface.slot_index
            ),
            _ => {
                vpc_slots.insert(vpc_id, interface.slot_index);
            }
        }
        match vni_vpcs.get(&interface.vpc_vni) {
            Some(existing) if *existing != vpc_id => eyre::bail!(
                "service VNI {} is associated with both VPC {existing} and VPC {vpc_id}",
                interface.vpc_vni
            ),
            _ => {
                vni_vpcs.insert(interface.vpc_vni, vpc_id);
            }
        }

        validate_shared_host_vpc_state(config, interface)?;
        let (hbn_address, reservation_link) = parse_service_link(&interface.link_prefix)?;
        let attachment_id = required_uuid("attachment_id", interface.attachment_id.as_ref())?;
        let internal_uuid = required_uuid("internal_uuid", interface.internal_uuid.as_ref())?;
        let mac_address = interface
            .mac_address
            .parse::<MacAddress>()
            .wrap_err_with(|| format!("invalid service MAC {}", interface.mac_address))?;
        let reservation = ServiceDhcpReservation {
            attachment_id,
            interface_ordinal: interface.interface_ordinal,
            internal_uuid,
            mac_address,
            link: reservation_link,
        };

        let key = (interface.slot_index, vpc_id);
        let group = groups.entry(key).or_insert_with(|| ServicePortGroup {
            interface: interface.clone(),
            ipv4_addresses: BTreeSet::new(),
            ipv6_addresses: BTreeSet::new(),
            reservations: Vec::new(),
        });
        eyre::ensure!(
            same_group_policy(&group.interface, interface),
            "service endpoints on slot {} have inconsistent VPC or port policy",
            interface.slot_index
        );
        let addresses = match hbn_address {
            IpNetwork::V4(_) => &mut group.ipv4_addresses,
            IpNetwork::V6(_) => &mut group.ipv6_addresses,
        };
        eyre::ensure!(
            addresses.insert(hbn_address.to_string()),
            "duplicate service link {} on slot {}",
            interface.link_prefix,
            interface.slot_index
        );
        group.reservations.push(reservation);
    }

    // Reuse ServiceConfig's (attachment_id, interface_ordinal), internal_uuid, MAC, and link checks
    // before DHCP configuration or NVUE rendering consumes the response.
    let reservation_groups = groups
        .iter()
        .map(|((slot_index, _), group)| {
            Ok(ServiceDhcpInterfaceReservations {
                receiving_interface: slot_names
                    .get(slot_index)
                    .ok_or_else(|| eyre::eyre!("validated service slot {slot_index} disappeared"))?
                    .clone(),
                reservations: group.reservations.clone(),
            })
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    let service_config = ServiceConfig::try_new(reservation_groups)?;
    let provisioned_interfaces = slot_names.values().cloned().collect::<Vec<_>>();
    service_config.validate_receiving_interfaces(&provisioned_interfaces)?;

    // Convert validated groups in slot/VPC order for stable renderer input.
    groups
        .into_iter()
        .map(|((slot_index, _), group)| {
            let interface = group.interface;
            let ipv6_addresses = group.ipv6_addresses.into_iter().collect::<Vec<_>>();
            Ok(nvue::PortConfig {
                interface_name: slot_names
                    .get(&slot_index)
                    .ok_or_else(|| eyre::eyre!("validated service slot {slot_index} disappeared"))?
                    .clone(),
                vlan: 0,
                host_ip: String::new(),
                host_route: String::new(),
                host_ipv6: None,
                host_ipv6_route: None,
                vni: None,
                l3_vni: Some(interface.vpc_vni),
                gateway_cidrs: group.ipv4_addresses.into_iter().collect(),
                ipv6_port_config: (!ipv6_addresses.is_empty()).then_some(nvue::Ipv6PortConfig {
                    gateway_cidrs: ipv6_addresses,
                    svi_ip: None,
                    router_advertisement: None,
                }),
                vpc_prefixes: interface.vpc_prefixes,
                vpc_peer_prefixes: interface.vpc_peer_prefixes,
                // Ignore peer VNIs until Core marks its list authoritative.
                vpc_peer_vnis: if config.vpc_peer_vnis_authoritative {
                    interface.vpc_peer_vnis
                } else {
                    Vec::new()
                },
                svi_ip: None,
                tenant_vrf_loopback_ip: interface.tenant_vrf_loopback_ip,
                is_l2_segment: false,
                is_phy: false,
                is_service: true,
                network_security_group_id: interface
                    .network_security_group
                    .map(|security_group| security_group.id),
                routing_profile: interface
                    .vpc_routing_profile
                    .as_ref()
                    .map(nvue::RoutingProfile::try_from)
                    .transpose()?,
                interface_routing_profile: interface
                    .interface_routing_profile
                    .as_ref()
                    .map(nvue::InterfaceRoutingProfile::from),
            })
        })
        .collect()
}

/// Normalizes set-like fields within one service endpoint while preserving every ordered policy
/// distinction consumed by NVUE.
pub(super) fn normalize_interface(interface: &mut rpc::ManagedHostServiceInterfaceConfig) {
    interface.vpc_prefixes.sort_unstable();
    interface.vpc_peer_prefixes.sort_unstable();
    interface.vpc_peer_vnis.sort_unstable();
    if let Some(profile) = &mut interface.vpc_routing_profile {
        normalize_routing_profile(profile);
    }
    if let Some(profile) = &mut interface.interface_routing_profile {
        profile
            .allowed_anycast_prefixes
            .sort_by(|left, right| left.prefix.cmp(&right.prefix));
    }
    if let Some(security_group) = &mut interface.network_security_group {
        for rule in &mut security_group.rules {
            normalize_security_group_rule(rule);
        }
        normalize_security_group_rule_order(&mut security_group.rules);
    }
}

/// Normalizes endpoint member order after their set-like fields so equivalent Core response order
/// cannot trigger another HBN application.
pub(super) fn normalize_interfaces(interfaces: &mut [rpc::ManagedHostServiceInterfaceConfig]) {
    for interface in interfaces.iter_mut() {
        normalize_interface(interface);
    }
    interfaces.sort_by_key(|interface| interface.encode_to_vec());
}

/// Validates fields that remain observable even when they do not directly affect the HBN address
/// stanza so malformed fetched state cannot reach another consumer.
fn validate_service_interface_fields(
    interface: &rpc::ManagedHostServiceInterfaceConfig,
) -> eyre::Result<()> {
    let service_id = interface
        .service_id
        .parse::<ExtensionServiceId>()
        .wrap_err_with(|| format!("invalid service_id {}", interface.service_id))?;
    eyre::ensure!(
        service_id != ExtensionServiceId::nil(),
        "service_id must not be nil"
    );
    required_uuid("attachment_id", interface.attachment_id.as_ref())?;
    required_uuid("internal_uuid", interface.internal_uuid.as_ref())?;

    let vpc_id = interface
        .vpc_id
        .ok_or_else(|| eyre::eyre!("service interface is missing vpc_id"))?;
    eyre::ensure!(vpc_id != VpcId::nil(), "vpc_id must not be nil");
    eyre::ensure!(interface.vpc_vni != 0, "vpc_vni must not be zero");
    eyre::ensure!(
        interface.mtu == Some(SERVICE_VPC_MTU),
        "service interface MTU must be {SERVICE_VPC_MTU}"
    );
    eyre::ensure!(
        interface.vpc_routing_profile.is_some(),
        "service interface is missing its VPC routing profile"
    );
    eyre::ensure!(
        !interface.vpc_prefixes.is_empty(),
        "service interface is missing VPC prefixes"
    );

    let (link_prefix, _) = parse_service_link(&interface.link_prefix)?;
    let mac = interface
        .mac_address
        .parse::<MacAddress>()
        .wrap_err_with(|| format!("invalid service MAC {}", interface.mac_address))?;
    let mac_bytes = mac.bytes();
    eyre::ensure!(
        mac_bytes != [0; 6] && mac_bytes[0] & 1 == 0,
        "service MAC {mac} must be a nonzero unicast address"
    );

    let vpc_prefixes = parse_prefixes("VPC prefix", &interface.vpc_prefixes)?;
    parse_prefixes("VPC peer prefix", &interface.vpc_peer_prefixes).map(drop)?;

    // Keep every point-to-point allocation inside the selected VPC's address space.
    let link_belongs_to_vpc =
        vpc_prefixes
            .into_iter()
            .any(|vpc_prefix| match (link_prefix, vpc_prefix) {
                (IpNetwork::V4(link), IpNetwork::V4(vpc)) => link.is_subnet_of(vpc),
                (IpNetwork::V6(link), IpNetwork::V6(vpc)) => link.is_subnet_of(vpc),
                _ => false,
            });
    eyre::ensure!(
        link_belongs_to_vpc,
        "service link {} is outside the selected VPC prefixes",
        interface.link_prefix
    );
    if let Some(loopback) = interface.tenant_vrf_loopback_ip.as_deref() {
        loopback
            .parse::<IpAddr>()
            .wrap_err_with(|| format!("invalid tenant VRF loopback {loopback}"))?;
    }
    if let Some(profile) = interface.vpc_routing_profile.as_ref() {
        validate_routing_profile(profile)?;
    }
    if let Some(profile) = interface.interface_routing_profile.as_ref() {
        validate_prefix_entries(
            "interface allowed-anycast prefix",
            &profile.allowed_anycast_prefixes,
        )?;
    }
    if let Some(security_group) = interface.network_security_group.as_ref() {
        eyre::ensure!(
            !security_group.id.is_empty(),
            "service network security group ID must not be empty"
        );
        eyre::ensure!(
            security_group.source() == rpc::NetworkSecurityGroupSource::NsgSourceVpc,
            "service network security group must come from the selected VPC"
        );
        let mut rules = Vec::with_capacity(security_group.rules.len());
        for rule in &security_group.rules {
            rules.push(nvue::NetworkSecurityGroupRule::try_from(rule)?);
            parse_prefixes("security-group source prefix", &rule.src_prefixes).map(drop)?;
            parse_prefixes("security-group destination prefix", &rule.dst_prefixes).map(drop)?;
        }
        // Keep fetch admission within the renderer's per-NSG expansion limit.
        nvue::validate_network_security_group_rule_count(&rules)?;
    }

    Ok(())
}

/// Parses a canonical point-to-point prefix into the HBN address and the existing exact DHCP
/// reservation model so both consumers agree on base+0 and base+1.
fn parse_service_link(link_prefix: &str) -> eyre::Result<(IpNetwork, ServiceLink)> {
    let prefix = link_prefix
        .parse::<IpNetwork>()
        .wrap_err_with(|| format!("invalid service link prefix {link_prefix}"))?;
    match prefix {
        IpNetwork::V4(prefix) => {
            eyre::ensure!(
                prefix.prefix() == 31 && prefix.ip() == prefix.network(),
                "IPv4 service link {link_prefix} must be a canonical /31"
            );
            Ok((
                IpNetwork::V4(prefix),
                ServiceLink::Ipv4(ServiceLinkV4 {
                    address: prefix.broadcast(),
                    prefix,
                    gateway: prefix.network(),
                }),
            ))
        }
        IpNetwork::V6(prefix) => {
            eyre::ensure!(
                prefix.prefix() == 127 && prefix.ip() == prefix.network(),
                "IPv6 service link {link_prefix} must be a canonical /127"
            );
            let mut service_octets = prefix.network().octets();
            service_octets[15] += 1;
            Ok((
                IpNetwork::V6(prefix),
                ServiceLink::Ipv6(ServiceLinkV6 {
                    address: Ipv6Addr::from(service_octets),
                    prefix,
                    hbn_peer: prefix.network(),
                }),
            ))
        }
    }
}

/// Parses one required wire UUID and rejects nil values because endpoint replacement and
/// acknowledgement require stable identities.
fn required_uuid(field: &str, value: Option<&::rpc::common::Uuid>) -> eyre::Result<Uuid> {
    let value = value.ok_or_else(|| eyre::eyre!("service interface is missing {field}"))?;
    let value = value
        .value
        .parse::<Uuid>()
        .wrap_err_with(|| format!("invalid service {field} {}", value.value))?;
    eyre::ensure!(!value.is_nil(), "service {field} must not be nil");
    Ok(value)
}

/// Compares the shared VPC and grouped-port policy after excluding service_id, attachment_id,
/// interface_ordinal, slot_index, link_prefix, mac_address, and internal_uuid so endpoint-specific
/// values and equivalent set order do not split one fixed slot.
fn same_group_policy(
    first: &rpc::ManagedHostServiceInterfaceConfig,
    next: &rpc::ManagedHostServiceInterfaceConfig,
) -> bool {
    let normalize = |interface: &rpc::ManagedHostServiceInterfaceConfig| {
        let mut interface = interface.clone();
        interface.service_id.clear();
        interface.attachment_id = None;
        interface.interface_ordinal = 0;
        interface.slot_index = 0;
        interface.link_prefix.clear();
        interface.mac_address.clear();
        interface.internal_uuid = None;
        normalize_interface(&mut interface);
        interface
    };
    normalize(first) == normalize(next)
}

/// Rejects conflicting host/service descriptions of a shared VNI while allowing their intentionally
/// distinct interface profiles and effective NSGs.
// Read the host loopback from the family-neutral list or its legacy IPv4 fallback.
fn validate_shared_host_vpc_state(
    config: &rpc::ManagedHostNetworkConfigResponse,
    service: &rpc::ManagedHostServiceInterfaceConfig,
) -> eyre::Result<()> {
    for host in config
        .tenant_interfaces
        .iter()
        .filter(|host| host.vpc_vni == service.vpc_vni)
    {
        let mut host_prefixes = host.vpc_prefixes.clone();
        let mut service_prefixes = service.vpc_prefixes.clone();
        host_prefixes.sort_unstable();
        service_prefixes.sort_unstable();
        let mut host_peer_prefixes = host.vpc_peer_prefixes.clone();
        let mut service_peer_prefixes = service.vpc_peer_prefixes.clone();
        host_peer_prefixes.sort_unstable();
        service_peer_prefixes.sort_unstable();
        // Compare peer membership only when Core supplies authoritative lists.
        let (mut host_peer_vnis, mut service_peer_vnis) = if config.vpc_peer_vnis_authoritative {
            (host.vpc_peer_vnis.clone(), service.vpc_peer_vnis.clone())
        } else {
            (Vec::new(), Vec::new())
        };
        host_peer_vnis.sort_unstable();
        service_peer_vnis.sort_unstable();
        let mut host_profile = host.vpc_routing_profile.clone();
        let mut service_profile = service.vpc_routing_profile.clone();
        if let Some(profile) = &mut host_profile {
            normalize_routing_profile(profile);
        }
        if let Some(profile) = &mut service_profile {
            normalize_routing_profile(profile);
        }

        eyre::ensure!(
            host_prefixes == service_prefixes
                && host_peer_prefixes == service_peer_prefixes
                && host_peer_vnis == service_peer_vnis
                && host_profile == service_profile,
            "host and service ports describe inconsistent state for VNI {}",
            service.vpc_vni
        );
        // Shared host and service ports must agree when both include the optional VPC VRF loopback.
        if let (Some(host_loopback), Some(service_loopback)) = (
            crate::ethernet_virtualization::tenant_vrf_loopback_ip(host),
            service.tenant_vrf_loopback_ip.as_deref(),
        ) {
            eyre::ensure!(
                host_loopback.parse::<IpAddr>()? == service_loopback.parse::<IpAddr>()?,
                "host and service ports describe different loopbacks for VNI {}",
                service.vpc_vni
            );
        }
    }
    Ok(())
}

/// Enumerates both supported HBN naming conventions because the fetcher validates inventory before
/// the running HBN version is discovered.
fn normal_host_port_names(config: &rpc::ManagedHostNetworkConfigResponse) -> BTreeSet<String> {
    let naming = [HBNDeviceNames::pre_23(), HBNDeviceNames::hbn_23()];
    let mut names = BTreeSet::new();
    for interface in config
        .admin_interface
        .iter()
        .chain(config.tenant_interfaces.iter())
    {
        if interface.function_type == rpc::InterfaceFunctionType::Physical as i32 {
            names.extend(naming.iter().map(|names| names.reps[0].to_string()));
        } else if interface.function_type == rpc::InterfaceFunctionType::Virtual as i32
            && let Some(id) = interface.virtual_function_id
        {
            names.extend(naming.iter().map(|names| names.build_virt(id)));
        }
    }
    names
}

/// Parses one response collection so malformed policy cannot be cached and callers can apply
/// relationships among the resulting networks without parsing twice.
fn parse_prefixes(label: &str, prefixes: &[String]) -> eyre::Result<Vec<IpNetwork>> {
    prefixes
        .iter()
        .map(|prefix| {
            prefix
                .parse::<IpNetwork>()
                .wrap_err_with(|| format!("invalid {label} {prefix}"))
        })
        .collect()
}

/// Parses prefix-policy entries while retaining their original order for equal-priority behavior.
fn validate_prefix_entries(
    label: &str,
    entries: &[rpc::PrefixFilterPolicyEntry],
) -> eyre::Result<()> {
    for entry in entries {
        entry
            .prefix
            .parse::<IpNetwork>()
            .wrap_err_with(|| format!("invalid {label} {}", entry.prefix))?;
    }
    Ok(())
}

/// Validates concrete route targets and prefix lists before the profile becomes accepted agent
/// state.
fn validate_routing_profile(profile: &rpc::RoutingProfile) -> eyre::Result<()> {
    nvue::RoutingProfile::try_from(profile)?;
    validate_prefix_entries(
        "accepted underlay-leak prefix",
        &profile.accepted_leaks_from_underlay,
    )?;
    validate_prefix_entries("allowed-anycast prefix", &profile.allowed_anycast_prefixes)
}

/// Sorts routing collections whose order does not affect the generated NVUE policy.
fn normalize_routing_profile(profile: &mut rpc::RoutingProfile) {
    profile
        .route_target_imports
        .sort_by_key(|target| (target.asn, target.vni, target.vni_self));
    profile
        .route_targets_on_exports
        .sort_by_key(|target| (target.asn, target.vni, target.vni_self));
    profile
        .accepted_leaks_from_underlay
        .sort_by(|left, right| left.prefix.cmp(&right.prefix));
    profile
        .allowed_anycast_prefixes
        .sort_by(|left, right| left.prefix.cmp(&right.prefix));
}

/// Sorts resolved rule prefixes because their order does not affect ACL behavior.
fn normalize_security_group_rule(rule: &mut rpc::ResolvedNetworkSecurityGroupRule) {
    rule.src_prefixes.sort_unstable();
    rule.dst_prefixes.sort_unstable();
}

/// Mirrors the renderer's stable partition order while preserving relative order among rules with
/// the same direction, family and priority.
fn normalize_security_group_rule_order(rules: &mut [rpc::ResolvedNetworkSecurityGroupRule]) {
    rules.sort_by_key(|rule| {
        rule.rule.as_ref().map(|attributes| {
            (
                attributes.direction()
                    == rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress,
                attributes.ipv6,
                attributes.priority,
            )
        })
    });
}
