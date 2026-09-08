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
use std::net::{IpAddr, Ipv4Addr};

use ::rpc::forge as rpc;
use carbide_network::virtualization::{VpcVirtualizationType, get_svi_ip};
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, MachineInterfaceId};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use db::vpc::{self};
use db::vpc_peering::get_prefixes_by_vpcs;
use db::{self, ObjectColumnFilter, network_security_group};
use ipnetwork::IpNetwork;
use model::instance::config::network::{
    InstanceInterfaceConfig, InstanceInterfaceRoutingProfile, InstanceNetworkConfig,
    InterfaceFunctionId,
};
use model::machine::ManagedHostStateSnapshot;
use model::network_prefix::NetworkPrefix;
use model::network_security_group::{
    NetworkSecurityGroup, NetworkSecurityGroupRule, NetworkSecurityGroupRuleNet,
};
use model::network_segment::NetworkSegment;
use model::resource_pool::common::CommonPools;
use model::vpc::{ALL_VPC_VIRTUALIZATION_TYPES, VpcVirtualizationTypeCapabilities};
use sqlx::PgConnection;

use crate::CarbideError;
use crate::cfg::file::{FnnConfig, FnnRoutingProfileConfig, VpcPeeringPolicy};

#[derive(Default, Clone)]
pub(crate) struct EthVirtData {
    pub(crate) asn: u32,
    pub(crate) dhcp_servers: Vec<Ipv4Addr>,
    pub(crate) deny_prefixes: Vec<IpNetwork>,
    pub(crate) site_fabric_prefixes: Option<SiteFabricPrefixList>,
}

pub(crate) struct AdminNetworkOptions<'a> {
    pub(crate) fnn_enabled: bool,
    pub(crate) common_pools: &'a CommonPools,
    pub(crate) booturl: &'a Option<String>,
    pub(crate) use_vpc_vrf_loopback: bool,
    pub(crate) routing_profile: Option<&'a FnnRoutingProfileConfig>,
}

#[derive(Clone)]
pub(crate) struct SiteFabricPrefixList {
    prefixes: Vec<IpNetwork>,
}

impl SiteFabricPrefixList {
    pub(crate) fn from_ipnetwork_vec(prefixes: Vec<IpNetwork>) -> Option<Self> {
        // Under the current configuration semantics, an empty
        // site_fabric_prefixes list in the site config means we are not using
        // the VPC isolation feature built on top of it, and it is better not
        // to construct one of these at all (and thus the Option-wrapped return
        // type).
        prefixes.none_if_empty().map(|prefixes| Self { prefixes })
    }

    pub(crate) fn as_ip_slice(&self) -> &[IpNetwork] {
        &self.prefixes
    }

    // Check whether the given network matches any of our site fabric prefixes.
    pub(crate) fn contains(&self, network: IpNetwork) -> bool {
        use IpNetwork::*;
        self.prefixes
            .iter()
            .copied()
            .any(|site_prefix| match (network, site_prefix) {
                (V4(network), V4(site_prefix)) => network.is_subnet_of(site_prefix),
                (V6(network), V6(site_prefix)) => network.is_subnet_of(site_prefix),
                _ => false,
            })
    }
}

/// Returns whether `candidate` is equal to or narrower than `allowed`.
fn prefix_contains(allowed: IpNetwork, candidate: IpNetwork) -> bool {
    match (candidate, allowed) {
        (IpNetwork::V4(candidate), IpNetwork::V4(allowed)) => candidate.is_subnet_of(allowed),
        (IpNetwork::V6(candidate), IpNetwork::V6(allowed)) => candidate.is_subnet_of(allowed),
        _ => false,
    }
}

/// Validates that an interface-level routing profile only narrows the owning VPC profile.
fn validate_interface_routing_profile(
    vpc_profile: &FnnRoutingProfileConfig,
    interface_profile: &InstanceInterfaceRoutingProfile,
) -> Result<(), CarbideError> {
    // Validate every requested anycast prefix against the owning VPC profile.
    // This helper is used when tenant input is accepted. DPU render-time
    // validation is handled by the agent, which filters invalid prefixes and
    // logs warnings so operator profile changes after instance creation do not
    // block config rendering. If routing profiles become DB-backed, this
    // should also be enforced on profile updates before committing changes
    // that would invalidate existing interface overrides.
    for prefix in &interface_profile.allowed_anycast_prefixes {
        if !vpc_profile
            .allowed_anycast_prefixes
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|allowed| prefix_contains(allowed.prefix, *prefix))
        {
            return Err(CarbideError::InvalidArgument(format!(
                "instance interface routing_profile.allowed_anycast_prefixes entry `{prefix}` is not within the owning VPC routing profile"
            )));
        }
    }

    Ok(())
}

/// Validates all interface-level routing profiles in an instance network config.
pub(crate) async fn validate_instance_interface_routing_profiles(
    txn: &mut PgConnection,
    network_config: &InstanceNetworkConfig,
    fnn_config: Option<&FnnConfig>,
) -> Result<(), CarbideError> {
    for iface in &network_config.interfaces {
        let Some(interface_profile) = iface.routing_profile.as_ref() else {
            continue;
        };

        // Resolve the owning VPC after any VPC-prefix request has become a segment.
        let segment_id = iface.network_segment_id.ok_or_else(|| {
            CarbideError::InvalidArgument(
                "instance interface routing_profile requires a network segment".to_string(),
            )
        })?;
        let vpc = db::vpc::find_by_segment(&mut *txn, segment_id)
            .await?
            .ok_or_else(|| {
                CarbideError::InvalidArgument(
                    "network segment is not a member of a VPC".to_string(),
                )
            })?;

        // Interface routing profiles are only valid on FNN VPC interfaces.
        if vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn {
            return Err(CarbideError::InvalidArgument(
                "instance interface routing_profile is only supported for FNN VPC interfaces"
                    .to_string(),
            ));
        }

        let fnn = fnn_config.ok_or_else(|| {
            CarbideError::InvalidArgument(
                "instance interface routing_profile requires FNN routing profiles".to_string(),
            )
        })?;
        // The interface profile must narrow the fully resolved VPC profile.
        let vpc_profile = fnn.resolve_vpc_routing_profile(&vpc.config)?;
        validate_interface_routing_profile(vpc_profile.as_ref(), interface_profile)?;
    }

    Ok(())
}

/// Groups the optional IPv4 and IPv6 prefixes for a network segment and
/// provides convenience methods for extracting addresses and interface
/// prefixes from an InstanceInterfaceConfig.
struct PrefixPair<'a> {
    v4: Option<&'a NetworkPrefix>,
    v6: Option<&'a NetworkPrefix>,
}

// Find the IPv4 NetworkPrefix without relying on database result ordering.
fn find_ipv4_prefix(prefixes: &[NetworkPrefix]) -> Option<&NetworkPrefix> {
    prefixes.iter().find(|prefix| prefix.prefix.is_ipv4())
}

impl<'a> PrefixPair<'a> {
    /// Find the IPv4 (optional) and IPv6 (optional) prefixes from a slice.
    fn from_segment_prefixes(
        prefixes: &'a [NetworkPrefix],
        _instance_id: InstanceId,
        _segment_id: carbide_uuid::network::NetworkSegmentId,
    ) -> Result<Self, CarbideError> {
        let v4 = find_ipv4_prefix(prefixes);
        let v6 = prefixes.iter().find(|p| p.prefix.is_ipv6());
        Ok(Self { v4, v6 })
    }

    /// Return the IPv4 prefix, if present.
    fn v4(&self) -> Option<&NetworkPrefix> {
        self.v4
    }

    /// Return the IPv6 prefix, if present.
    fn v6(&self) -> Option<&NetworkPrefix> {
        self.v6
    }

    /// Extract the IPv4 address from the interface config, if a v4 prefix is present.
    fn v4_address(&self, iface: &InstanceInterfaceConfig) -> Option<IpAddr> {
        self.v4.and_then(|p| iface.ip_addrs.get(&p.id).copied())
    }

    /// Extract the IPv6 address (if an IPv6 prefix exists and the interface
    /// has an address allocated for it).
    fn v6_address(&self, iface: &InstanceInterfaceConfig) -> Option<IpAddr> {
        self.v6.and_then(|p| iface.ip_addrs.get(&p.id).copied())
    }

    /// Extract the IPv4 interface prefix, falling back to a /32 derived from
    /// the address for backwards compatibility. Returns None if no v4 prefix.
    fn v4_interface_prefix(
        &self,
        iface: &InstanceInterfaceConfig,
        address: IpAddr,
    ) -> Result<Option<IpNetwork>, CarbideError> {
        let Some(v4) = self.v4 else {
            return Ok(None);
        };
        match iface.interface_prefixes.get(&v4.id) {
            Some(p) => Ok(Some(*p)),
            None => IpNetwork::new(address, 32)
                .map(Some)
                .map_err(|e| CarbideError::Internal {
                    message: format!(
                        "failed to build default interface_prefix for {address}/32: {e}"
                    ),
                }),
        }
    }

    /// Extract the IPv6 interface prefix (if present).
    fn v6_interface_prefix(&self, iface: &InstanceInterfaceConfig) -> Option<IpNetwork> {
        self.v6
            .and_then(|p| iface.interface_prefixes.get(&p.id).copied())
    }

    /// Compute the SVI IP for an L2 FNN segment, returning both v4 and v6
    /// SVI IPs as optional strings.
    fn svi_ips(
        &self,
        network_virtualization_type: VpcVirtualizationType,
        is_l2_segment: bool,
    ) -> Result<(Option<String>, Option<String>), CarbideError> {
        let svi_ip = self
            .v4
            .map(|p| {
                get_svi_ip(
                    &p.svi_ip,
                    network_virtualization_type,
                    is_l2_segment,
                    p.prefix.prefix(),
                )
            })
            .transpose()
            .map_err(|e| CarbideError::Internal {
                message: format!("failed to configure FlatInterfaceConfig.svi_ip: {e}"),
            })?
            .flatten()
            .map(|ip| ip.to_string());

        let svi_ip_v6 = self
            .v6
            .and_then(|p| {
                get_svi_ip(
                    &p.svi_ip,
                    network_virtualization_type,
                    is_l2_segment,
                    p.prefix.prefix(),
                )
                .transpose()
            })
            .transpose()
            .map_err(|e| CarbideError::Internal {
                message: format!("failed to configure FlatInterfaceConfig.svi_ip_v6: {e}"),
            })?
            .map(|ip| ip.to_string());

        Ok((svi_ip, svi_ip_v6))
    }
}

// This is intentionally a one-line wrapper. It gives the IPv4-only
// compatibility rule one obvious name and keeps the explanation here instead
// of repeating it at each producer call site.
fn tenant_vrf_loopback_for_legacy_ipv4_field(loopback_ip: Option<IpAddr>) -> Option<String> {
    loopback_ip.filter(IpAddr::is_ipv4).map(|ip| ip.to_string())
}

/// Mirrors the deprecated family-specific fields into the family-neutral address list.
///
/// Values are emitted in V4/V6 order. The tenant VRF loopback is placed on the
/// entry matching its address family, creating a loopback-only entry when that
/// family has no interface address data.
#[allow(deprecated)]
fn interface_address_configs(
    config: &rpc::FlatInterfaceConfig,
    ipv6_segment_prefix: Option<&str>,
    tenant_vrf_loopback_ip: Option<IpAddr>,
) -> Vec<rpc::InterfaceAddressConfig> {
    let mut addresses = Vec::with_capacity(2);
    let ipv4_loopback = tenant_vrf_loopback_ip
        .filter(IpAddr::is_ipv4)
        .map(|ip| ip.to_string());
    let ipv6_loopback = tenant_vrf_loopback_ip
        .filter(IpAddr::is_ipv6)
        .map(|ip| ip.to_string());

    if config.gateway.is_some()
        || config.ip.is_some()
        || config.interface_prefix.is_some()
        || config.prefix.is_some()
        || config.svi_ip.is_some()
        || ipv4_loopback.is_some()
    {
        addresses.push(rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V4.into(),
            ip: config.ip.clone().unwrap_or_default(),
            interface_prefix: config.interface_prefix.clone().unwrap_or_default(),
            prefix: config.prefix.clone().unwrap_or_default(),
            gateway: config.gateway.clone(),
            svi_ip: config.svi_ip.clone(),
            tenant_vrf_loopback_ip: ipv4_loopback,
        });
    }

    if config.ipv6_interface_config.is_some() || ipv6_loopback.is_some() {
        let ipv6 = config.ipv6_interface_config.as_ref();
        addresses.push(rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V6.into(),
            ip: ipv6.map(|v6| v6.ip.clone()).unwrap_or_default(),
            interface_prefix: ipv6
                .map(|v6| v6.interface_prefix.clone())
                .unwrap_or_default(),
            prefix: ipv6_segment_prefix.unwrap_or_default().to_string(),
            gateway: None,
            svi_ip: ipv6.and_then(|v6| v6.svi_ip.clone()),
            tenant_vrf_loopback_ip: ipv6_loopback,
        });
    }

    addresses
}

/// Builds the legacy IPv6 projection used by DPU agents.
///
/// Existing non-SLAAC configurations still require a concrete host address.
/// SLAAC instead sends the selected interface prefix with an intentionally
/// empty host address so the agent can configure the IPv6 prefix in the DPU.
/// Router advertisement (RA) support is tracked by
/// https://github.com/NVIDIA/infra-controller/issues/2398.
fn build_ipv6_interface_config(
    address: Option<IpAddr>,
    interface_prefix: Option<IpNetwork>,
    svi_ip: Option<String>,
    slaac_enabled: bool,
) -> Option<rpc::FlatInterfaceIpv6Config> {
    match (address, interface_prefix, slaac_enabled) {
        (Some(address), interface_prefix, _) => Some(rpc::FlatInterfaceIpv6Config {
            ip: address.to_string(),
            interface_prefix: interface_prefix
                .map(|prefix| prefix.to_string())
                .unwrap_or_default(),
            svi_ip,
        }),
        (None, Some(interface_prefix), true) => Some(rpc::FlatInterfaceIpv6Config {
            ip: String::new(),
            interface_prefix: interface_prefix.to_string(),
            svi_ip,
        }),
        (None, _, _) => None,
    }
}

// This writer keeps the deprecated fields populated for older agents during the rollout.
#[allow(deprecated)]
pub(crate) async fn admin_network(
    txn: &mut PgConnection,
    snapshot: &ManagedHostStateSnapshot,
    dpu_machine_id: &DpuMachineId,
    options: AdminNetworkOptions<'_>,
) -> Result<(rpc::FlatInterfaceConfig, MachineInterfaceId), tonic::Status> {
    let AdminNetworkOptions {
        fnn_enabled: fnn_enabled_on_admin,
        common_pools,
        booturl,
        use_vpc_vrf_loopback,
        routing_profile: admin_vpc_routing_profile,
    } = options;

    let admin_segments = db::network_segment::admin(txn).await?;
    let admin_segment_ids = admin_segments
        .iter()
        .map(|s| s.id)
        .collect::<Vec<NetworkSegmentId>>();

    // Admin network interfaces are machine_interfaces records where the machine_id
    // is a host machine ID and attached_dpu_machine_id is a DPU ID.
    // If we loop through the machine interfaces for the host snapshot and look for
    // that combo, the segment_id of that interface should be the network segment we want,
    // but checking against known admin segments adds a little bit of defense.
    let interface = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|interface| {
            interface.attached_dpu_machine_id.as_ref() == Some(dpu_machine_id)
                && admin_segment_ids.contains(&interface.segment_id)
        });

    let host_machine_id = snapshot.host_snapshot.id;
    let Some(interface) = interface else {
        return Err(CarbideError::InvalidArgument(format!(
            "no admin interface found attached on host: {host_machine_id} with dpu: {dpu_machine_id}"
        ))
        .into());
    };

    // Keep host_interface_id tied to the requesting DPU's host link, but source
    // all address-bearing admin config from the host's primary admin interface.
    // In multi-DPU FNN mode that primary can be a non-DPU host NIC; dpu-agent
    // still disables the admin DHCP path on non-primary DPUs via is_primary_dpu.
    let active_interface = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|interface| {
            interface.primary_interface && admin_segment_ids.contains(&interface.segment_id)
        })
        .ok_or_else(|| {
            CarbideError::InvalidArgument(format!(
                "no primary admin interface found on host: {host_machine_id}"
            ))
        })?;

    let admin_segment = admin_segments
        .iter()
        .find(|v| v.id == active_interface.segment_id);

    let Some(admin_segment) = admin_segment else {
        return Err(CarbideError::internal(format!(
            "unknown primary admin segment `{}` attached on host: {host_machine_id}",
            active_interface.segment_id
        ))
        .into());
    };

    let prefix = match find_ipv4_prefix(&admin_segment.prefixes) {
        Some(p) => p,
        None => {
            return Err(CarbideError::Internal {
                message: format!(
                    "admin network segment '{}' has no IPv4 network prefix",
                    admin_segment.id,
                ),
            }
            .into());
        }
    };

    let domain = match admin_segment.config.subdomain_id {
        Some(domain_id) => {
            db::dns::domain::find_by_uuid(&mut *txn, domain_id)
                .await
                .map_err(CarbideError::from)?
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "domain",
                    id: domain_id.to_string(),
                })?
                .name
        }
        None => "unknowndomain".to_string(),
    };

    let address = active_interface
        .addresses
        .iter()
        .copied()
        .find(|address| address.is_ipv4())
        .ok_or_else(|| {
            CarbideError::InvalidArgument(format!(
                "no IPv4 address found on primary host admin interface {}",
                active_interface.id
            ))
        })?;

    // On the admin network, the interface_prefix is always
    // just going to be a /32 derived from the machine interface
    // address.
    let address_prefix = IpNetwork::new(address, 32).map_err(|e| CarbideError::Internal {
        message: format!("failed to build default admin address prefix for {address}/32: {e}"),
    })?;

    let svi_ip = if !fnn_enabled_on_admin {
        None
    } else {
        get_svi_ip(
            &prefix.svi_ip,
            VpcVirtualizationType::Fnn,
            true,
            prefix.prefix.prefix(),
        )
        .map_err(|e| CarbideError::Internal {
            message: format!("failed to configure FlatInterfaceConfig.svi_ip: {e}"),
        })?
        .map(|ip| ip.to_string())
    };

    let (vpc_vni, tenant_vrf_loopback_ip) = if !fnn_enabled_on_admin {
        (0, None)
    } else {
        match admin_segment.config.vpc_id {
            Some(vpc_id) => {
                let mut vpcs =
                    db::vpc::find_by(&mut *txn, ObjectColumnFilter::One(vpc::IdColumn, &vpc_id))
                        .await?;
                if vpcs.is_empty() {
                    return Err(CarbideError::FindOneReturnedNoResultsError(vpc_id.into()).into());
                }
                let vpc = vpcs.remove(0);
                match vpc.status.vni {
                    Some(vpc_vni) => {
                        let tenant_loopback_ip = if use_vpc_vrf_loopback {
                            Some(
                                db::vpc_dpu_loopback::get_or_allocate_loopback_ip_for_vpc(
                                    common_pools,
                                    txn,
                                    dpu_machine_id,
                                    &vpc.id,
                                )
                                .await?,
                            )
                        } else {
                            None
                        };

                        (vpc_vni as u32, tenant_loopback_ip)
                    }
                    None => {
                        // if FNN is enabled, VPC must be created and updated in admin_segment.
                        return Err(CarbideError::internal(format!(
                            "admin VPC is not found with id: {vpc_id}"
                        ))
                        .into());
                    }
                }
            }
            None => {
                // if FNN is enabled, VPC must be created and updated in admin_segment.
                return Err(CarbideError::internal(
                    "admin VPC is not attached to admin segment".to_string(),
                )
                .into());
            }
        }
    };

    let mut cfg = rpc::FlatInterfaceConfig {
        function_type: rpc::InterfaceFunctionType::Physical.into(),
        virtual_function_id: None,
        vlan_id: admin_segment.status.vlan_id.unwrap_or_default() as u32,
        vni: if fnn_enabled_on_admin {
            admin_segment.status.vni.unwrap_or_default() as u32
        } else {
            0
        },
        vpc_vni,
        gateway: prefix.gateway_cidr(),
        ip: Some(address.to_string()),
        interface_prefix: Some(address_prefix.to_string()),
        vpc_prefixes: if fnn_enabled_on_admin {
            vec![format!("{address}/32")]
        } else {
            vec![]
        },
        prefix: Some(prefix.prefix.to_string()),
        fqdn: format!("{}.{}", active_interface.hostname, domain),
        booturl: booturl.clone(),
        svi_ip,
        tenant_vrf_loopback_ip: tenant_vrf_loopback_for_legacy_ipv4_field(tenant_vrf_loopback_ip),
        is_l2_segment: true,
        vpc_peer_prefixes: vec![],
        vpc_peer_vnis: vec![],
        network_security_group: None,
        internal_uuid: None,
        mtu: u32::try_from(admin_segment.config.mtu).ok(),
        ipv6_interface_config: None,
        vpc_routing_profile: admin_vpc_routing_profile.map(rpc::RoutingProfile::from),
        interface_routing_profile: None,
        addresses: vec![],
    };
    cfg.addresses = interface_address_configs(&cfg, None, tenant_vrf_loopback_ip);
    Ok((cfg, interface.id))
}

#[allow(clippy::too_many_arguments)]
// This writer keeps the deprecated IPv4 fields populated when IPv4 is configured so older
// agents can consume IPv4-only and dual-stack payloads during the rollout.
#[allow(deprecated)]
pub(crate) async fn tenant_network(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    iface: &InstanceInterfaceConfig,
    fqdn: String,
    loopback_ip: Option<IpAddr>,
    network_virtualization_type: VpcVirtualizationType,
    suppress_tenant_security_groups: bool,
    network_security_group_details: Option<(i32, NetworkSecurityGroup)>,
    segment: &NetworkSegment,
    vpc_peering_policy_on_existing: Option<VpcPeeringPolicy>,
    booturl: &Option<String>,
    fnn_config: Option<&FnnConfig>,
) -> Result<rpc::FlatInterfaceConfig, tonic::Status> {
    // Any stretchable segment is treated as L2 segment by FNN.
    let is_l2_segment = segment.status.can_stretch.unwrap_or(true);

    let ds = PrefixPair::from_segment_prefixes(&segment.prefixes, instance_id, segment.id)?;
    let address = match ds.v4() {
        Some(_) => Some(ds.v4_address(iface).ok_or_else(|| CarbideError::Internal {
            message: format!(
                "no IPv4 address is available for instance {instance_id} on segment {}",
                segment.id,
            ),
        })?),
        None => None,
    };

    // If not, default to a /32 -- backwards compatibility for instances
    // configured before interface_prefixes were introduced.
    //
    // TODO(chet): This can eventually be phased out once all of the
    // InstanceInterfaceConfigs stored contain the prefix.
    let interface_prefix =
        match address {
            Some(address) => Some(ds.v4_interface_prefix(iface, address)?.ok_or_else(|| {
                CarbideError::Internal {
                    message: format!(
                        "no IPv4 prefix is available for instance {instance_id} on segment {}",
                        segment.id,
                    ),
                }
            })?),
            None => None,
        };

    let v6_address = ds.v6_address(iface);
    let v6_interface_prefix = ds.v6_interface_prefix(iface);

    let vpc_prefixes: Vec<String> = match segment.config.vpc_id {
        Some(vpc_id) => {
            let vpc_prefixes = db::vpc_prefix::find_by_vpc(txn, vpc_id)
                .await?
                .into_iter()
                .map(|vpc_prefix| vpc_prefix.config.prefix.to_string());
            let vpc_segment_prefixes = db::network_prefix::find_by_vpc(txn, vpc_id)
                .await?
                .into_iter()
                .map(|segment_prefix| segment_prefix.prefix.to_string());
            vpc_prefixes.chain(vpc_segment_prefixes).collect()
        }
        None => ds
            .v4()
            .map(|p| vec![p.prefix.to_string()])
            .unwrap_or_default(),
    };

    let mut vpc_peer_vnis = vec![];
    let mut vpc_peer_prefixes = vec![];
    if let Some(policy) = vpc_peering_policy_on_existing
        && let Some(vpc_id) = segment.config.vpc_id
    {
        // The peer-ID universe depends on the site policy. Under
        // `Exclusive`, the per-type capability layer dictates which
        // peer types are compatible (e.g. an FNN VPC can have Flat
        // peers via Flat's `peers_with` listing). Under `Mixed`, the
        // operator opts out of capability enforcement and we accept
        // any peering record. `None` disables peering entirely.
        let vpc_peer_ids: Vec<VpcId> = match policy {
            VpcPeeringPolicy::Exclusive => {
                let allowed_peer_types = network_virtualization_type
                    .capabilities()
                    .peers_with
                    .to_vec();
                db::vpc_peering::get_vpc_peer_vnis(txn, vpc_id, allowed_peer_types)
                    .await?
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()
            }
            VpcPeeringPolicy::Mixed => db::vpc_peering::get_vpc_peer_ids(txn, vpc_id).await?,
            VpcPeeringPolicy::None => vec![],
        };

        vpc_peer_prefixes = get_prefixes_by_vpcs(txn, &vpc_peer_ids).await?;

        // VNI-based peer route imports are independent of peering
        // policy: they're a per-type question on both sides.
        // - Self: does this VPC's DPU plumb peer VNIs into its VRF?
        //   (`imports_peer_vnis_into_overlay`, FNN-only today.)
        // - Peer: should this peer's VNI be exposed for the self-side
        //   to pick up? (`vni_advertised_to_peers`, FNN + Flat today --
        //   Flat advertises its VNI so pluggable SDN integrations on
        //   the network operator's fabric can use it.)
        if network_virtualization_type.imports_peer_vnis_into_overlay() {
            let vni_peer_types: Vec<_> = ALL_VPC_VIRTUALIZATION_TYPES
                .iter()
                .copied()
                .filter(|t| t.vni_advertised_to_peers())
                .collect();
            vpc_peer_vnis = db::vpc_peering::get_vpc_peer_vnis(txn, vpc_id, vni_peer_types)
                .await?
                .iter()
                .map(|(_, vni)| *vni as u32)
                .collect();
        }
    }
    // Keep API responses deterministic so downstream config rendering
    // does not flap due to ordering jitter in peering query results.
    vpc_peer_vnis.sort_unstable();
    vpc_peer_prefixes.sort_unstable();

    let vpc = match segment.config.vpc_id {
        Some(vpc_id) => {
            let mut vpcs =
                db::vpc::find_by(&mut *txn, ObjectColumnFilter::One(vpc::IdColumn, &vpc_id))
                    .await?;
            if vpcs.is_empty() {
                return Err(CarbideError::FindOneReturnedNoResultsError(vpc_id.into()).into());
            }
            vpcs.pop()
        }
        None => None,
    };

    let vpc_vni = vpc.as_ref().and_then(|v| v.status.vni).unwrap_or_default() as u32;
    let slaac_enabled = vpc.as_ref().is_some_and(|vpc| vpc.config.slaac_enabled);

    // Resolve the routing profile from the VPC attached to this interface.
    let (vpc_routing_profile, interface_routing_profile) = match (vpc.as_ref(), fnn_config) {
        (Some(vpc), Some(fnn))
            if vpc.config.network_virtualization_type == VpcVirtualizationType::Fnn =>
        {
            let profile = fnn.resolve_vpc_routing_profile(&vpc.config)?;

            (
                Some(rpc::RoutingProfile::from(profile.as_ref())),
                iface
                    .routing_profile
                    .as_ref()
                    .map(rpc::FlatInterfaceRoutingProfile::from),
            )
        }
        (Some(vpc), None)
            if vpc.config.network_virtualization_type == VpcVirtualizationType::Fnn =>
        {
            return Err(CarbideError::Internal {
                message: "FNN VPC found but no FNN config found".to_string(),
            }
            .into());
        }
        _ if iface.routing_profile.is_some() => {
            return Err(CarbideError::InvalidArgument(
                "instance interface routing_profile is only supported for FNN VPC interfaces"
                    .to_string(),
            )
            .into());
        }
        _ => (None, None),
    };

    let rpc_ft: rpc::InterfaceFunctionType = iface.function_id.function_type().into();
    let (svi_ip, svi_ip_v6) = ds.svi_ips(network_virtualization_type, is_l2_segment)?;

    let network_security_group_details = match (
        suppress_tenant_security_groups,
        network_security_group_details,
        vpc.as_ref(),
    ) {
        // If NSGs aren't being suppressed, and there are no
        // details coming from the parent instance,
        // see if there's an associated VPC (there should be),
        // and see if the VPC has an NSG attached.
        (false, None, Some(v)) => {
            match v.config.network_security_group_id.as_ref() {
                None => None,
                Some(vpc_nsg_id) => {
                    // Make our DB query for the IDs to get our NetworkSecurityGroup
                    let network_security_group = network_security_group::find_by_ids(
                        txn,
                        std::slice::from_ref(vpc_nsg_id),
                        Some(&v.config.tenant_organization_id.parse().map_err(|_| {
                            CarbideError::Internal {
                                message: "invalid tenant org in VPC data".to_string(),
                            }
                        })?),
                        false,
                    )
                    .await?
                    .pop()
                    .ok_or(CarbideError::NotFoundError {
                        kind: "NetworkSecurityGroup",
                        id: vpc_nsg_id.to_string(),
                    })?;

                    Some((
                        i32::from(rpc::NetworkSecurityGroupSource::NsgSourceVpc),
                        network_security_group,
                    ))
                }
            }
        }

        // If NSGs aren't being suppressed and details are already coming from
        // the parent instance, use those.
        (false, d, _) => d,

        // Otherwise, we either have no details or we want no details.
        _ => None,
    };

    let mut config = rpc::FlatInterfaceConfig {
        function_type: rpc_ft.into(),
        virtual_function_id: match iface.function_id {
            InterfaceFunctionId::Physical {} => None,
            InterfaceFunctionId::Virtual { id } => Some(id.into()),
        },
        vlan_id: segment.status.vlan_id.unwrap_or_default() as u32,
        vni: segment.status.vni.unwrap_or_default() as u32,
        vpc_vni,
        gateway: ds.v4().and_then(NetworkPrefix::gateway_cidr),
        ip: address.map(|address| address.to_string()),
        interface_prefix: interface_prefix.map(|prefix| prefix.to_string()),
        vpc_prefixes,
        prefix: ds.v4().map(|p| p.prefix.to_string()),
        // FIXME: Right now we are sending instance IP as hostname. This should be replaced by
        // user's provided fqdn later.
        fqdn,
        booturl: booturl.clone(),
        svi_ip,
        tenant_vrf_loopback_ip: tenant_vrf_loopback_for_legacy_ipv4_field(loopback_ip),
        is_l2_segment,
        vpc_peer_prefixes,
        vpc_peer_vnis,
        network_security_group: network_security_group_details
            .map(|(source, nsg)| {
                Ok(
                        rpc::FlatInterfaceNetworkSecurityGroupConfig {
                            id: nsg.id.to_string(),
                            version: nsg.version.to_string(),
                            source,
                            stateful_egress: nsg.stateful_egress,
                            rules:
                                nsg.rules
                                    .into_iter()
                                    .map(resolve_security_group_rule)
                                    .collect::<Result<
                                        Vec<rpc::ResolvedNetworkSecurityGroupRule>,
                                        CarbideError,
                                    >>()?,
                        },
                    )
            })
            .transpose()
            .map_err(|e: CarbideError| CarbideError::Internal {
                message: format!(
                    "failed to configure FlatInterfaceConfig.network_security_group: {e}"
                ),
            })?,
        internal_uuid: Some(iface.internal_uuid.into()),
        mtu: u32::try_from(segment.config.mtu).ok(),
        ipv6_interface_config: build_ipv6_interface_config(
            v6_address,
            v6_interface_prefix,
            svi_ip_v6,
            slaac_enabled,
        ),
        vpc_routing_profile,
        interface_routing_profile,
        addresses: vec![],
    };
    let ipv6_segment_prefix = ds.v6().map(|prefix| prefix.prefix.to_string());
    config.addresses =
        interface_address_configs(&config, ipv6_segment_prefix.as_deref(), loopback_ip);

    Ok(config)
}

pub(crate) fn resolve_security_group_rule(
    rule: NetworkSecurityGroupRule,
) -> Result<rpc::ResolvedNetworkSecurityGroupRule, CarbideError> {
    Ok(rpc::ResolvedNetworkSecurityGroupRule {
        // When we decide to allow object references,
        // they would be resolved to their actual prefix
        // lists and stored here.
        src_prefixes: match rule.src_net {
            NetworkSecurityGroupRuleNet::Prefix(ref p) => {
                vec![p.to_string()]
            }
        },
        dst_prefixes: match rule.dst_net {
            NetworkSecurityGroupRuleNet::Prefix(ref p) => {
                vec![p.to_string()]
            }
        },
        rule: Some(rule.try_into()?),
    })
}

#[cfg(test)]
mod test {
    use carbide_test_support::value_scenarios;
    use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};

    use super::*;

    // Build a NetworkPrefix with only the fields needed by the selection test.
    fn network_prefix(prefix: &str) -> NetworkPrefix {
        NetworkPrefix {
            id: NetworkPrefixId::new(),
            segment_id: NetworkSegmentId::new(),
            prefix: prefix.parse().unwrap(),
            gateway: None,
            dhcpv6_link_address: None,
            num_reserved: 0,
            vpc_prefix_id: None,
            vpc_prefix: None,
            svi_ip: None,
            num_free_ips: None,
        }
    }

    // Verify that the admin IPv4 selector never depends on family ordering and
    // does not treat an IPv6-only segment as IPv4-capable.
    #[test]
    fn ipv4_prefix_selection_is_independent_of_family_order() {
        let v4 = network_prefix("192.0.2.0/24");
        let v6 = network_prefix("2001:db8::/64");

        value_scenarios!(
            run = |prefixes: Vec<NetworkPrefix>| {
                find_ipv4_prefix(&prefixes).map(|prefix| prefix.prefix)
            };
            "IPv4 before IPv6" {
                vec![v4.clone(), v6.clone()] => Some(v4.prefix),
            }
            "IPv6 before IPv4" {
                vec![v6.clone(), v4.clone()] => Some(v4.prefix),
            }
            "IPv6 only" {
                vec![v6] => None,
            }
        );
    }

    #[allow(deprecated)]
    fn legacy_interface_config(
        ipv6_interface_config: Option<rpc::FlatInterfaceIpv6Config>,
    ) -> rpc::FlatInterfaceConfig {
        rpc::FlatInterfaceConfig {
            gateway: Some("192.0.2.1/24".to_string()),
            ip: Some("192.0.2.10".to_string()),
            interface_prefix: Some("192.0.2.10/32".to_string()),
            prefix: Some("192.0.2.0/24".to_string()),
            svi_ip: Some("192.0.2.2/24".to_string()),
            ipv6_interface_config,
            ..Default::default()
        }
    }

    fn ipv4_address_config() -> rpc::InterfaceAddressConfig {
        rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V4.into(),
            ip: "192.0.2.10".to_string(),
            interface_prefix: "192.0.2.10/32".to_string(),
            prefix: "192.0.2.0/24".to_string(),
            gateway: Some("192.0.2.1/24".to_string()),
            svi_ip: Some("192.0.2.2/24".to_string()),
            tenant_vrf_loopback_ip: None,
        }
    }

    fn ipv6_interface_config() -> rpc::FlatInterfaceIpv6Config {
        rpc::FlatInterfaceIpv6Config {
            ip: "2001:db8::1".to_string(),
            interface_prefix: "2001:db8::/127".to_string(),
            svi_ip: Some("2001:db8::2/64".to_string()),
        }
    }

    fn ipv6_address_config() -> rpc::InterfaceAddressConfig {
        rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V6.into(),
            ip: "2001:db8::1".to_string(),
            interface_prefix: "2001:db8::/127".to_string(),
            prefix: "2001:db8::/64".to_string(),
            gateway: None,
            svi_ip: Some("2001:db8::2/64".to_string()),
            tenant_vrf_loopback_ip: None,
        }
    }

    fn slaac_ipv6_address_config() -> rpc::InterfaceAddressConfig {
        rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V6.into(),
            ip: String::new(),
            interface_prefix: "2001:db8::/64".to_string(),
            prefix: "2001:db8::/64".to_string(),
            gateway: None,
            svi_ip: None,
            tenant_vrf_loopback_ip: None,
        }
    }

    #[test]
    fn ipv6_interface_projection_requires_an_address_unless_slaac_is_enabled() {
        let address: IpAddr = "2001:db8::1".parse().unwrap();
        let prefix: IpNetwork = "2001:db8::/127".parse().unwrap();
        let slaac_prefix: IpNetwork = "2001:db8::/64".parse().unwrap();
        let configured = rpc::FlatInterfaceIpv6Config {
            ip: address.to_string(),
            interface_prefix: prefix.to_string(),
            svi_ip: None,
        };
        let slaac = rpc::FlatInterfaceIpv6Config {
            ip: String::new(),
            interface_prefix: slaac_prefix.to_string(),
            svi_ip: None,
        };

        value_scenarios!(
            run = |(address, prefix, slaac_enabled)| {
                build_ipv6_interface_config(address, prefix, None, slaac_enabled)
            };
            "ordinary IPv6 configuration" {
                (Some(address), Some(prefix), false) => Some(configured.clone()),
                (Some(address), Some(prefix), true) => Some(configured.clone()),
            }
            "SLAAC keeps a prefix without a host address" {
                (None, Some(slaac_prefix), true) => Some(slaac),
            }
            "non-SLAAC configuration with only a prefix remains absent" {
                (None, Some(prefix), false) => None,
            }
            "SLAAC without a selected prefix remains absent" {
                (None, None, true) => None,
            }
            "non-SLAAC configuration without an address or prefix remains absent" {
                (None, None, false) => None,
            }
            "legacy address without an interface prefix remains projected" {
                (Some(address), None, false) => Some(rpc::FlatInterfaceIpv6Config {
                    interface_prefix: String::new(),
                    ..configured.clone()
                }),
                (Some(address), None, true) => Some(rpc::FlatInterfaceIpv6Config {
                    interface_prefix: String::new(),
                    ..configured
                }),
            }
        );
    }

    #[test]
    #[allow(deprecated)]
    fn interface_address_configs_mirror_legacy_fields_in_family_order() {
        value_scenarios!(
            run = |(config, ipv6_prefix, loopback_ip): (
                rpc::FlatInterfaceConfig,
                Option<&str>,
                Option<IpAddr>,
            )| {
                interface_address_configs(&config, ipv6_prefix, loopback_ip)
            };
            "IPv4-only config" {
                (legacy_interface_config(None), None, None) => vec![ipv4_address_config()],
            }
            "no configured address families" {
                (rpc::FlatInterfaceConfig::default(), None, None) => vec![],
            }
            "IPv6-only config" {
                (
                    rpc::FlatInterfaceConfig {
                        ipv6_interface_config: Some(ipv6_interface_config()),
                        ..Default::default()
                    },
                    Some("2001:db8::/64"),
                    None,
                ) => vec![ipv6_address_config()],
            }
            "IPv6 segment without an interface address" {
                (legacy_interface_config(None), Some("2001:db8::/64"), None) => vec![ipv4_address_config()],
            }
            "IPv6 address without an interface prefix" {
                (
                    legacy_interface_config(Some(rpc::FlatInterfaceIpv6Config {
                        ip: "2001:db8::1".to_string(),
                        interface_prefix: String::new(),
                        svi_ip: None,
                    })),
                    Some("2001:db8::/64"),
                    None,
                ) => vec![
                    ipv4_address_config(),
                    rpc::InterfaceAddressConfig {
                        interface_prefix: String::new(),
                        svi_ip: None,
                        ..ipv6_address_config()
                    },
                ],
            }
            "SLAAC prefix without a host address" {
                (
                    rpc::FlatInterfaceConfig {
                        ipv6_interface_config: Some(rpc::FlatInterfaceIpv6Config {
                            ip: String::new(),
                            interface_prefix: "2001:db8::/64".to_string(),
                            svi_ip: None,
                        }),
                        ..Default::default()
                    },
                    Some("2001:db8::/64"),
                    None,
                ) => vec![slaac_ipv6_address_config()],
            }
            "IPv6-only config with an IPv4 tenant VRF loopback" {
                (
                    rpc::FlatInterfaceConfig {
                        ipv6_interface_config: Some(ipv6_interface_config()),
                        ..Default::default()
                    },
                    Some("2001:db8::/64"),
                    Some("192.0.2.3".parse().unwrap()),
                ) => vec![
                    rpc::InterfaceAddressConfig {
                        address_family: rpc::AddressFamily::V4.into(),
                        tenant_vrf_loopback_ip: Some("192.0.2.3".to_string()),
                        ..Default::default()
                    },
                    ipv6_address_config(),
                ],
            }
            "IPv4-only config with an IPv6 tenant VRF loopback" {
                (
                    legacy_interface_config(None),
                    None,
                    Some("2001:db8::3".parse().unwrap()),
                ) => vec![
                    ipv4_address_config(),
                    rpc::InterfaceAddressConfig {
                        address_family: rpc::AddressFamily::V6.into(),
                        tenant_vrf_loopback_ip: Some("2001:db8::3".to_string()),
                        ..Default::default()
                    },
                ],
            }
            "dual-stack config" {
                (
                    legacy_interface_config(Some(ipv6_interface_config())),
                    Some("2001:db8::/64"),
                    None,
                ) => vec![ipv4_address_config(), ipv6_address_config()],
            }
        );
    }

    #[test]
    fn tenant_vrf_loopback_for_legacy_ipv4_field_is_ipv4_only() {
        value_scenarios!(run = tenant_vrf_loopback_for_legacy_ipv4_field;
            "absent loopback remains absent" {
                None => None,
            }
            "IPv4 loopback is projected" {
                Some("192.0.2.3".parse().unwrap()) => Some("192.0.2.3".to_string()),
            }
            "IPv6 loopback remains family-neutral" {
                Some("2001:db8::3".parse().unwrap()) => None,
            }
        );
    }

    /// Returns a test FNN routing profile with the provided allowed anycast prefixes.
    fn routing_profile_with_anycast(prefixes: &[&str]) -> FnnRoutingProfileConfig {
        FnnRoutingProfileConfig {
            allowed_anycast_prefixes: Some(
                prefixes
                    .iter()
                    .map(|prefix| crate::cfg::file::PrefixFilterPolicyEntry {
                        prefix: prefix.parse().unwrap(),
                    })
                    .collect(),
            ),
            leak_default_route_from_underlay: Some(true),
            ..Default::default()
        }
    }

    /// Returns a test interface routing profile with the provided anycast prefixes.
    fn interface_routing_profile(prefixes: &[&str]) -> InstanceInterfaceRoutingProfile {
        InstanceInterfaceRoutingProfile {
            allowed_anycast_prefixes: prefixes
                .iter()
                .map(|prefix| prefix.parse().unwrap())
                .collect(),
        }
    }

    #[test]
    fn test_interface_routing_profile_conversion_preserves_interface_anycast_prefixes() {
        let vpc_profile = routing_profile_with_anycast(&["192.0.2.0/24", "2001:db8::/32"]);
        let interface_profile = interface_routing_profile(&["192.0.2.64/26", "2001:db8:1::/64"]);

        validate_interface_routing_profile(&vpc_profile, &interface_profile).unwrap();
        let vpc_routing_profile = rpc::RoutingProfile::from(&vpc_profile);
        let interface_routing_profile = rpc::FlatInterfaceRoutingProfile::from(&interface_profile);

        assert!(vpc_routing_profile.leak_default_route_from_underlay);
        assert_eq!(
            vpc_routing_profile
                .allowed_anycast_prefixes
                .into_iter()
                .map(|entry| entry.prefix)
                .collect::<Vec<_>>(),
            vec!["192.0.2.0/24".to_string(), "2001:db8::/32".to_string()]
        );
        assert_eq!(
            interface_routing_profile
                .allowed_anycast_prefixes
                .into_iter()
                .map(|entry| entry.prefix)
                .collect::<Vec<_>>(),
            vec!["192.0.2.64/26".to_string(), "2001:db8:1::/64".to_string()]
        );
    }

    #[test]
    fn test_interface_routing_profile_rejects_anycast_prefix_outside_vpc_profile() {
        let vpc_profile = routing_profile_with_anycast(&["192.0.2.0/24"]);
        let interface_profile = interface_routing_profile(&["198.51.100.0/24"]);

        // Attempt to validate an interface profile outside the VPC profile.
        let err = validate_interface_routing_profile(&vpc_profile, &interface_profile)
            .expect_err("interface profile should be rejected");

        // Verify the API surfaces the violation as caller-provided invalid input.
        assert!(matches!(err, CarbideError::InvalidArgument(_)));
    }

    #[test]
    fn test_site_prefix_list() {
        let prefixes: Vec<IpNetwork> = vec![
            IpNetwork::V4("192.0.2.0/25".parse().unwrap()),
            IpNetwork::V6("2001:DB8::/64".parse().unwrap()),
        ];
        let site_prefix_list = SiteFabricPrefixList::from_ipnetwork_vec(prefixes).unwrap();

        let contained_smaller = IpNetwork::V4("192.0.2.64/26".parse().unwrap());
        let contained_equal = IpNetwork::V4("192.0.2.0/25".parse().unwrap());
        let uncontained_larger = IpNetwork::V4("192.0.2.0/24".parse().unwrap());
        let uncontained_different = IpNetwork::V4("198.51.100.0/24".parse().unwrap());
        assert!(site_prefix_list.contains(contained_smaller));
        assert!(site_prefix_list.contains(contained_equal));
        assert!(!site_prefix_list.contains(uncontained_larger));
        assert!(!site_prefix_list.contains(uncontained_different));

        assert!(SiteFabricPrefixList::from_ipnetwork_vec(vec![]).is_none());
    }

    #[test]
    fn test_site_prefix_list_ipv6_containment() {
        let prefixes: Vec<IpNetwork> = vec![IpNetwork::V6("2001:db8:abcd::/48".parse().unwrap())];
        let site_prefix_list = SiteFabricPrefixList::from_ipnetwork_vec(prefixes).unwrap();

        // Make sure a /64 subnet is contained within the /48.
        assert!(site_prefix_list.contains("2001:db8:abcd:1::/64".parse().unwrap()));
        // Make sure an exact match is contained.
        assert!(site_prefix_list.contains("2001:db8:abcd::/48".parse().unwrap()));
        // Make sure a larger prefix is NOT contained.
        assert!(!site_prefix_list.contains("2001:db8::/32".parse().unwrap()));
        // Make sure a completely different prefix is also not contained.
        assert!(!site_prefix_list.contains("2001:db8:ffff::/48".parse().unwrap()));
    }

    #[test]
    fn test_site_prefix_list_cross_family_never_matches() {
        // IPv4-only site fabric prefixes should never match IPv6
        // segments and vice versa.
        let ipv4_only = SiteFabricPrefixList::from_ipnetwork_vec(vec![IpNetwork::V4(
            "10.0.0.0/8".parse().unwrap(),
        )])
        .unwrap();
        assert!(!ipv4_only.contains("2001:db8::/32".parse().unwrap()));

        let ipv6_only = SiteFabricPrefixList::from_ipnetwork_vec(vec![IpNetwork::V6(
            "2001:db8::/32".parse().unwrap(),
        )])
        .unwrap();
        assert!(!ipv6_only.contains("10.0.0.0/24".parse().unwrap()));
    }

    #[test]
    fn test_site_prefix_list_dual_stack() {
        // A dual-stack site with both IPv4 and IPv6 fabric prefixes.
        let prefixes: Vec<IpNetwork> = vec![
            IpNetwork::V4("10.100.0.0/16".parse().unwrap()),
            IpNetwork::V6("fd00:100::/32".parse().unwrap()),
        ];
        let site_prefix_list = SiteFabricPrefixList::from_ipnetwork_vec(prefixes).unwrap();

        // This IPv4 subnet should be contained in the SiteFabricPrefixList.
        assert!(site_prefix_list.contains("10.100.1.0/24".parse().unwrap()));
        // ...and so should this IPv6 prefix.
        assert!(site_prefix_list.contains("fd00:100:1::/48".parse().unwrap()));
        // ...but not this IPv4 prefix.
        assert!(!site_prefix_list.contains("10.200.0.0/24".parse().unwrap()));
        // ...or this IPv6 prefix.
        assert!(!site_prefix_list.contains("fd00:200::/48".parse().unwrap()));
    }
}
