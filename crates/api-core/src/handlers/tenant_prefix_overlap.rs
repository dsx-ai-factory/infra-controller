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

//! Tenant prefix overlap checks shared by prefix, peering, VPC, and Instance writers.
//! NSG policies do not change route visibility and cannot bypass FNN null routes,
//! so overlap safety depends on routing profiles and imports rather than ACL permits.

mod instances;
mod startup;

use std::collections::HashSet;

use carbide_network::ip::prefix::IpNet;
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::vpc::VpcId;
use db::ObjectColumnFilter;
pub(super) use instances::validate_affected_instances;
use ipnetwork::IpNetwork;
use model::instance::InstanceSearchFilter;
use model::instance::config::InstanceConfig;
use model::instance::config::network::InstanceNetworkConfig;
use model::instance::snapshot::InstanceSnapshot;
use model::machine::{
    InstanceState, LoadSnapshotOptions, ManagedHostState, ManagedHostStateSnapshot,
};
use model::site_prefix::{
    SitePrefix, SitePrefixAuthority, SitePrefixLifecycleState, SitePrefixRoutingScope,
};
use model::vpc::{ALL_VPC_VIRTUALIZATION_TYPES, Vpc, VpcVirtualizationTypeCapabilities};
use sqlx::PgConnection;
pub(crate) use startup::{validate_retained_state, validate_retained_state_in_transaction};

use crate::api::Api;
use crate::cfg::file::{
    CarbideConfig, FnnRoutingProfileConfig, VpcIsolationBehaviorType, VpcPeeringPolicy,
};
use crate::{CarbideError, CarbideResult};

const INELIGIBLE_OVERLAP: &str =
    "the requested prefix overlaps address space that is not eligible for reuse";

/// `overlap_error` returns the common client error for an overlap that cannot
/// be reused.
///
/// It does not identify the conflicting resource because that resource may
/// belong to another tenant.
pub(super) fn overlap_error() -> CarbideError {
    CarbideError::InvalidArgument(INELIGIBLE_OVERLAP.to_string())
}

/// `VpcPrefixParticipant` groups the `VpcPrefix`, VPC, and `SitePrefix` facts
/// needed for one exact CIDR check.
pub(super) struct VpcPrefixParticipant<'a> {
    /// Exact `VpcPrefix` CIDR under consideration.
    pub(super) prefix: IpNetwork,
    /// Whether the `VpcPrefix` has started deletion.
    pub(super) is_deleted: bool,
    /// VPC that owns the `VpcPrefix`.
    pub(super) vpc: &'a Vpc,
    /// `SitePrefix` referenced by the `VpcPrefix`.
    pub(super) site_prefix: &'a SitePrefix,
}

/// `contains_prefix` returns whether `parent` contains `child` in the same
/// address family.
pub(super) fn contains_prefix(parent: IpNetwork, child: IpNetwork) -> bool {
    match (parent, child) {
        (IpNetwork::V4(parent), IpNetwork::V4(child)) => child.is_subnet_of(parent),
        (IpNetwork::V6(parent), IpNetwork::V6(child)) => child.is_subnet_of(parent),
        _ => false,
    }
}

/// `site_prefix_is_eligible` checks the `SitePrefix` requirements for one
/// `VpcPrefix`.
///
/// Retained checks allow deleting roots while their routes drain. Admission
/// separately requires a `Ready` root for the new `VpcPrefix`.
fn site_prefix_is_eligible(site_prefix: &SitePrefix, vpc: &Vpc, prefix: IpNetwork) -> bool {
    site_prefix.status.authority == SitePrefixAuthority::TenantManaged
        && site_prefix
            .config
            .tenant_organization_id
            .as_ref()
            .map(|id| id.as_str())
            == Some(vpc.config.tenant_organization_id.as_str())
        && site_prefix.config.routing_scope == SitePrefixRoutingScope::DatacenterOnly
        && contains_prefix(site_prefix.config.prefix, prefix)
        && matches!(
            site_prefix.status.lifecycle_state,
            SitePrefixLifecycleState::Ready | SitePrefixLifecycleState::Deleting
        )
}

/// Selects database scope for a new eligible tenant-managed IPv4 prefix.
/// Tenant-managed SitePrefix creation is IPv4-only; other families stay global.
/// This does not authorize overlap: pair, VNI, receiver, and Instance checks
/// still apply. Existing rows retain their stored scope.
pub(super) fn vpc_prefix_overlap_scope(
    runtime_config: &CarbideConfig,
    site_prefix: &SitePrefix,
    vpc: &Vpc,
    prefix: IpNetwork,
) -> Option<VpcId> {
    if !runtime_config.tenant_prefix_overlap_enabled
        || !prefix.is_ipv4()
        || vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn
        || site_prefix.status.lifecycle_state != SitePrefixLifecycleState::Ready
        || !site_prefix_is_eligible(site_prefix, vpc, prefix)
        || !site_policy_is_isolated(runtime_config)
        || runtime_config
            .site_fabric_null_routes
            .as_deref()
            .is_some_and(|routes| !prefix_has_isolation_route(routes, prefix))
    {
        return None;
    }

    runtime_config
        .fnn
        .as_ref()?
        .resolve_vpc_routing_profile(&vpc.config)
        .ok()?
        .is_eligible_for_tenant_prefix_overlap()
        .then_some(vpc.id)
}

/// `pair_is_eligible` returns whether two `VpcPrefix` records may reuse one
/// exact CIDR.
///
/// The pair eligibility checks ownership, FNN isolation, distinct VNIs,
/// `SitePrefix` state, and each VPC's resolved routing profile. Callers still
/// need `db::tenant_prefix_overlap::lock_checks`, must verify each VPC owns
/// exactly one allocation matching its VNI, and must reject every ineligible
/// overlap.
pub(super) fn pair_is_eligible(
    runtime_config: &crate::cfg::file::CarbideConfig,
    candidate: VpcPrefixParticipant<'_>,
    existing: VpcPrefixParticipant<'_>,
) -> bool {
    // Valid tenant parents supply inherited coverage even outside configured
    // operator ranges. Retiring operator roots do not authorize new reuse.
    let isolation_routes = runtime_config.resolved_site_fabric_null_routes(
        &[],
        &[
            candidate.site_prefix.config.prefix,
            existing.site_prefix.config.prefix,
        ],
    );
    runtime_config.tenant_prefix_overlap_enabled
        && !candidate.is_deleted
        && !existing.is_deleted
        && candidate.site_prefix.status.lifecycle_state == SitePrefixLifecycleState::Ready
        && [candidate.vpc, existing.vpc].iter().all(|vpc| {
            runtime_config.fnn.as_ref().is_some_and(|fnn| {
                fnn.resolve_vpc_routing_profile(&vpc.config)
                    .is_ok_and(|profile| profile.tenant_prefix_overlap_eligible)
            })
        })
        && retained_pair_is_isolated(runtime_config, &isolation_routes, candidate, existing)
}

/// `site_policy_is_isolated` checks the site-wide routes that could connect
/// otherwise isolated VPCs. Service VPC slots expose externally configured
/// connections whose isolation has not been qualified for duplicate prefixes.
fn site_policy_is_isolated(runtime_config: &CarbideConfig) -> bool {
    matches!(
        runtime_config.vpc_isolation_behavior,
        VpcIsolationBehaviorType::MutualIsolation
    )
        && runtime_config.site_global_vpc_vni.is_none()
        // The renderer falls back to this list when profile anycast is empty.
        && runtime_config.anycast_site_prefixes.is_empty()
        && runtime_config.dpu_config.service_vpc_slot_count == 0
        && runtime_config.fnn.as_ref().is_some_and(|fnn| {
            fnn.common_internal_route_target.is_none()
                && fnn.additional_route_target_imports.is_empty()
        })
}

/// A broader or equal blackhole protects the reused prefix while allowing an authorized
/// equal-length or more-specific imported route to win by distance or longest-prefix match.
pub(super) fn prefix_has_isolation_route(
    isolation_routes: &[IpNetwork],
    prefix: IpNetwork,
) -> bool {
    let to_ip_net = |prefix: IpNetwork| {
        IpNet::new(prefix.ip(), prefix.prefix())
            .expect("IpNetwork guarantees a valid address-family prefix length")
    };
    let prefix = to_ip_net(prefix);
    isolation_routes
        .iter()
        .copied()
        .map(to_ip_net)
        .any(|route| route.contains(&prefix))
}

/// `retained_pair_is_isolated` preserves routing checks during withdrawal.
/// Admission opt-ins and deletion intent prevent additions, but do not make
/// a safely isolated pair stop serving before its routes have drained.
fn retained_pair_is_isolated(
    runtime_config: &CarbideConfig,
    isolation_routes: &[IpNetwork],
    candidate: VpcPrefixParticipant<'_>,
    existing: VpcPrefixParticipant<'_>,
) -> bool {
    if !site_policy_is_isolated(runtime_config)
        || !prefix_has_isolation_route(isolation_routes, candidate.prefix)
        || candidate.prefix != existing.prefix
        || candidate.vpc.id == existing.vpc.id
        || candidate.vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn
        || existing.vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn
        || !site_prefix_is_eligible(candidate.site_prefix, candidate.vpc, candidate.prefix)
        || !site_prefix_is_eligible(existing.site_prefix, existing.vpc, existing.prefix)
        || !matches!(
            (candidate.vpc.status.vni, existing.vpc.status.vni),
            (Some(candidate_vni), Some(existing_vni)) if candidate_vni != existing_vni
        )
    {
        return false;
    }

    let Some(fnn) = runtime_config.fnn.as_ref() else {
        return false;
    };
    let Ok(candidate_profile) = fnn.resolve_vpc_routing_profile(&candidate.vpc.config) else {
        return false;
    };
    let Ok(existing_profile) = fnn.resolve_vpc_routing_profile(&existing.vpc.config) else {
        return false;
    };

    candidate_profile.is_isolated_for_tenant_prefixes()
        && existing_profile.is_isolated_for_tenant_prefixes()
}

/// `checks_required` keeps admission active after an operator disables the
/// gate while a tenant-managed `VpcPrefix` still overlaps another VPC's addresses.
/// Legacy overlaps alone do not enable these safeguards. Callers that mutate
/// routing acquire the overlap lock before reading their dependencies.
pub(super) async fn checks_required(api: &Api, txn: &mut PgConnection) -> CarbideResult<bool> {
    Ok(api.runtime_config.tenant_prefix_overlap_enabled
        || !db::tenant_prefix_overlap::find_duplicate_vpc_ids(txn, false)
            .await?
            .is_empty())
}

async fn vpc_uses_duplicate_space(
    api: &Api,
    txn: &mut PgConnection,
    vpc: &Vpc,
) -> CarbideResult<bool> {
    let sources = receiver_sources(&api.runtime_config, txn, vpc).await?;
    Ok(db::tenant_prefix_overlap::vpcs_use_duplicate_space(txn, &sources).await?)
}

/// Accepts a safe final profile or a restriction of its resolved routing
/// behavior. This compares effective defaults, not whether an override was supplied.
pub(super) fn routing_profile_is_nonexpanding(
    runtime_config: &CarbideConfig,
    previous: &FnnRoutingProfileConfig,
    candidate: &FnnRoutingProfileConfig,
) -> bool {
    let previous_anycast = previous
        .allowed_anycast_prefixes
        .as_deref()
        .unwrap_or_default();
    let candidate_anycast = candidate
        .allowed_anycast_prefixes
        .as_deref()
        .unwrap_or_default();
    // An empty IPv4 profile list exposes the legacy site list, bypassing
    // per-interface narrowing. It is not a safe final policy in that case.
    if !runtime_config.anycast_site_prefixes.is_empty()
        && previous_anycast.iter().any(|entry| entry.prefix.is_ipv4())
        && !candidate_anycast.iter().any(|entry| entry.prefix.is_ipv4())
    {
        return false;
    }
    if candidate.is_eligible_for_tenant_prefix_overlap() {
        return true;
    }
    // New profile fields need an explicit decision here as well as in the
    // eligibility check. Access tiers authorize selection; they do not route.
    let FnnRoutingProfileConfig {
        route_target_imports,
        route_targets_on_exports,
        internal,
        tenant_prefix_overlap_eligible,
        leak_default_route_from_underlay,
        leak_tenant_host_routes_to_underlay,
        tenant_leak_communities_accepted,
        accepted_leaks_from_underlay,
        allowed_anycast_prefixes: _,
        access_tier: _,
    } = candidate;
    internal.unwrap_or_default() == previous.internal.unwrap_or_default()
        && *tenant_prefix_overlap_eligible == previous.tenant_prefix_overlap_eligible
        && route_target_imports.as_deref().unwrap_or_default().iter().all(|target| {
            previous.route_target_imports.as_deref().unwrap_or_default().contains(target)
        })
        && route_targets_on_exports.as_deref().unwrap_or_default().iter().all(|target| {
            previous.route_targets_on_exports.as_deref().unwrap_or_default().contains(target)
        })
        && (!leak_default_route_from_underlay.unwrap_or_default()
            || previous.leak_default_route_from_underlay.unwrap_or_default())
        && (!leak_tenant_host_routes_to_underlay.unwrap_or_default()
            || previous.leak_tenant_host_routes_to_underlay.unwrap_or_default())
        // Disabling community handling can make suppressed routes exportable
        // through EVPN, so neither direction is assumed to restrict access.
        && tenant_leak_communities_accepted.unwrap_or_default()
            == previous.tenant_leak_communities_accepted.unwrap_or_default()
        && accepted_leaks_from_underlay.as_deref().unwrap_or_default().iter().all(|entry| {
            previous.accepted_leaks_from_underlay.as_deref().unwrap_or_default().iter()
                .any(|allowed| contains_prefix(allowed.prefix, entry.prefix))
        })
        && candidate_anycast.iter().all(|entry| {
            previous_anycast.iter().any(|allowed| contains_prefix(allowed.prefix, entry.prefix))
        })
}

fn policy_error() -> CarbideError {
    CarbideError::FailedPrecondition(
        "the requested policy is not safe for tenant prefix reuse".to_string(),
    )
}

/// `validate_retained_host` checks the networks an admitted Instance can use
/// without another API request, including a pending replacement. The caller
/// holds the overlap lock before loading the host and through configuration
/// generation. Admission gates may be off during drain; isolation must remain.
pub(super) async fn validate_retained_host(
    api: &Api,
    txn: &mut PgConnection,
    host: &ManagedHostStateSnapshot,
) -> CarbideResult<()> {
    if !needs_retained_policy_check(host) {
        return Ok(());
    }
    let instance = host.instance.as_ref().ok_or_else(policy_error)?;
    let vpcs = retained_policy_vpcs(txn, host).await?;
    let mut sources = HashSet::new();
    for vpc in &vpcs {
        sources.extend(receiver_sources(&api.runtime_config, txn, vpc).await?);
    }
    let sources = sources.into_iter().collect::<Vec<_>>();
    if !api.runtime_config.tenant_prefix_overlap_enabled
        && !db::tenant_prefix_overlap::vpcs_use_duplicate_space(&mut *txn, &sources).await?
    {
        return Ok(());
    }
    let prefixes = db::vpc_peering::get_retained_prefixes_by_vpcs(&mut *txn, &sources).await?;
    if prefixes_overlap_across_vpcs(&prefixes) {
        return Err(overlap_error());
    }
    startup::validate_retained_prefixes(api, txn, &sources).await?;

    for network in std::iter::once(&instance.config.network).chain(
        instance
            .update_network_config_request
            .iter()
            .flat_map(|update| [&update.old_config, &update.new_config]),
    ) {
        crate::ethernet_virtualization::validate_instance_interface_routing_profiles(
            txn,
            network,
            api.runtime_config.fnn.as_ref(),
        )
        .await?;
    }
    for vpc in vpcs {
        if vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn {
            continue;
        }
        let fnn = api.runtime_config.fnn.as_ref().ok_or_else(policy_error)?;
        if !site_policy_is_isolated(&api.runtime_config)
            || !fnn
                .resolve_vpc_routing_profile(&vpc.config)?
                .is_isolated_for_tenant_prefixes()
        {
            return Err(policy_error());
        }
    }
    Ok(())
}

/// Returns the direct source VPCs whose prefixes or VNIs the receiver imports,
/// including the receiver itself. This follows the renderer's directional rules.
pub(super) async fn receiver_sources(
    runtime_config: &CarbideConfig,
    txn: &mut PgConnection,
    receiver: &Vpc,
) -> CarbideResult<Vec<VpcId>> {
    let Some(policy) = runtime_config
        .vpc_peering_policy_on_existing
        .or(runtime_config.vpc_peering_policy)
    else {
        return Ok(vec![receiver.id]);
    };
    let receiver_type = receiver.config.network_virtualization_type;
    let mut sources = match policy {
        VpcPeeringPolicy::Exclusive | VpcPeeringPolicy::Mixed => {
            db::vpc_peering::get_vpc_peer_vnis(
                txn,
                receiver.id,
                receiver_type.capabilities().peers_with.to_vec(),
            )
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
        }
        VpcPeeringPolicy::None => Vec::new(),
    };
    if policy != VpcPeeringPolicy::None && receiver_type.imports_peer_vnis_into_overlay() {
        let peer_types = ALL_VPC_VIRTUALIZATION_TYPES
            .iter()
            .copied()
            .filter(|peer_type| peer_type.vni_advertised_to_peers())
            .collect();
        sources.extend(
            db::vpc_peering::get_vpc_peer_vnis(txn, receiver.id, peer_types)
                .await?
                .into_iter()
                .map(|(id, _)| id),
        );
    }
    sources.push(receiver.id);
    sources.sort_unstable();
    sources.dedup();
    Ok(sources)
}

/// Repeated paths to one source do not create a collision. Distinct source
/// VPCs must not contribute overlapping address space to the same receiver.
pub(super) fn prefixes_overlap_across_vpcs(prefixes: &[(VpcId, IpNetwork)]) -> bool {
    prefixes
        .iter()
        .enumerate()
        .any(|(index, (vpc_id, prefix))| {
            prefixes[index + 1..].iter().any(|(other_vpc_id, other)| {
                vpc_id != other_vpc_id
                    && (contains_prefix(*prefix, *other) || contains_prefix(*other, *prefix))
            })
        })
}

/// Accepts an unchanged network or removal of interfaces without changing the
/// retained interfaces. Use the model's intent comparison so generated addresses
/// and omitted resolved VPC IDs do not turn metadata edits into routing changes.
pub(super) fn instance_network_is_nonexpanding(
    previous: &InstanceNetworkConfig,
    candidate: &InstanceNetworkConfig,
) -> bool {
    if previous.auto_config != candidate.auto_config
        || candidate.interfaces.len() > previous.interfaces.len()
    {
        return false;
    }
    candidate.interfaces.iter().all(|requested| {
        previous.interfaces.iter().any(|retained| {
            let previous_interface = InstanceNetworkConfig {
                interfaces: vec![retained.clone()],
                auto_config: previous.auto_config,
            };
            let requested_interface = InstanceNetworkConfig {
                interfaces: vec![requested.clone()],
                auto_config: candidate.auto_config,
            };
            !previous_interface.is_network_config_update_requested(&requested_interface)
        })
    })
}

/// Checks the resolved candidate together with every network configuration
/// retained by the Instance. The caller holds the overlap lock before reading
/// dependencies and keeps it through the Instance write. Allocation deliberately
/// does not use the policy writers' active-host filter: this configuration has
/// not started serving tenant traffic yet.
pub(crate) async fn validate_instance_network(
    api: &Api,
    txn: &mut PgConnection,
    candidate: &InstanceConfig,
    retained: Option<&InstanceSnapshot>,
) -> CarbideResult<()> {
    let mut networks = vec![&candidate.network];
    if let Some(instance) = retained {
        networks.push(&instance.config.network);
        if let Some(update) = &instance.update_network_config_request {
            networks.extend([&update.old_config, &update.new_config]);
        }
    }
    let mut sources = HashSet::new();
    let mut checked_vpcs = HashSet::new();
    let mut vpcs = Vec::new();
    for network in networks {
        crate::ethernet_virtualization::validate_instance_interface_routing_profiles(
            txn,
            network,
            api.runtime_config.fnn.as_ref(),
        )
        .await?;
        for interface in &network.interfaces {
            let segment_id = interface.network_segment_id.ok_or_else(policy_error)?;
            let vpc = db::vpc::find_by_segment(&mut *txn, segment_id)
                .await?
                .ok_or_else(policy_error)?;
            if interface.vpc_id.is_some_and(|id| id != vpc.id) {
                return Err(policy_error());
            }
            if !checked_vpcs.insert(vpc.id) {
                continue;
            }
            sources.extend(receiver_sources(&api.runtime_config, txn, &vpc).await?);
            vpcs.push(vpc);
        }
    }
    let sources = sources.into_iter().collect::<Vec<_>>();
    let prefixes = db::vpc_peering::get_retained_prefixes_by_vpcs(&mut *txn, &sources).await?;
    if prefixes_overlap_across_vpcs(&prefixes) {
        return Err(overlap_error());
    }

    // Turning the gate off freezes expansion where another VPC still uses
    // the same addresses, even when that VPC is isolated from this receiver.
    // Both probes retain deleting rows; no site inventory scan is needed.
    if !api.runtime_config.tenant_prefix_overlap_enabled {
        let mut duplicate_space = false;
        for (source_vpc_id, prefix) in &prefixes {
            if db::vpc_prefix::probe(*prefix, txn)
                .await?
                .iter()
                .any(|other| other.vpc_id != *source_vpc_id)
                || db::vpc_prefix::probe_segment_prefixes(*prefix, txn)
                    .await?
                    .iter()
                    .any(|other| {
                        other.vpc_id != Some(*source_vpc_id)
                            && (other.vpc_id.is_some() || other.prefix.vpc_prefix_id.is_some())
                    })
            {
                duplicate_space = true;
                break;
            }
        }
        if !duplicate_space {
            return Ok(());
        }
        return Err(overlap_error());
    }

    for vpc in vpcs {
        if vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn {
            continue;
        }
        let fnn = api.runtime_config.fnn.as_ref().ok_or_else(policy_error)?;
        if !fnn
            .resolve_vpc_routing_profile(&vpc.config)?
            .is_eligible_for_tenant_prefix_overlap()
            || !site_policy_is_isolated(&api.runtime_config)
        {
            return Err(policy_error());
        }
    }
    Ok(())
}

/// `validate_vpc_policy` checks a changed routing profile against retained
/// overlapping prefixes and the tenant interfaces that use it. The caller
/// holds the overlap lock before locking the VPC and keeps it through the write.
pub(super) async fn validate_vpc_policy(
    api: &Api,
    txn: &mut PgConnection,
    candidate: &Vpc,
) -> CarbideResult<()> {
    if candidate.config.network_virtualization_type != VpcVirtualizationType::Fnn
        || (!api.runtime_config.tenant_prefix_overlap_enabled
            && !vpc_uses_duplicate_space(api, txn, candidate).await?)
    {
        return Ok(());
    }
    // An empty VPC can still own overlapping prefixes that startup and other
    // VPCs' DPU configurations must validate. Its routing must remain isolated.
    if db::tenant_prefix_overlap::vpcs_use_duplicate_space(&mut *txn, &[candidate.id]).await? {
        let fnn = api.runtime_config.fnn.as_ref().ok_or_else(policy_error)?;
        if !fnn
            .resolve_vpc_routing_profile(&candidate.config)?
            .is_isolated_for_tenant_prefixes()
        {
            return Err(policy_error());
        }
    }
    for host in load_policy_hosts(txn, &[candidate.id]).await? {
        if !needs_retained_policy_check(&host) {
            continue;
        }
        let vpcs = retained_policy_vpcs(txn, &host).await?;
        if !vpcs.iter().any(|vpc| vpc.id == candidate.id) {
            continue;
        }
        tracing::warn!(vpc_id = %candidate.id, machine_id = %host.host_snapshot.id,
            "VPC routing policy would make tenant prefix reuse unsafe");
        return Err(policy_error());
    }
    Ok(())
}

fn needs_retained_policy_check(host: &ManagedHostStateSnapshot) -> bool {
    // Even a canceled allocation can advance to tenant networking. Only the
    // controller's return-to-Admin state guarantees no later activation.
    let removing_tenant_network = host.use_admin_network()
        && matches!(
            host.managed_state,
            ManagedHostState::Assigned {
                instance_state: InstanceState::WaitingForNetworkReconfig,
            }
        );
    host.has_managed_dpus()
        && host
            .instance
            .as_ref()
            .is_some_and(|instance| instance.deleted.is_none() || !removing_tenant_network)
}

async fn load_policy_hosts(
    txn: &mut PgConnection,
    vpc_ids: &[VpcId],
) -> CarbideResult<Vec<ManagedHostStateSnapshot>> {
    let mut instance_ids = HashSet::new();
    for vpc_id in vpc_ids {
        instance_ids.extend(
            db::instance::find_ids(
                &mut *txn,
                InstanceSearchFilter {
                    vpc_id: Some(vpc_id.to_string()),
                    ..Default::default()
                },
            )
            .await?,
        );
    }
    if instance_ids.is_empty() {
        return Ok(Vec::new());
    }
    let instance_ids = instance_ids.into_iter().collect::<Vec<_>>();
    Ok(
        db::managed_host::load_by_instance_ids(txn, &instance_ids, LoadSnapshotOptions::default())
            .await?,
    )
}

async fn retained_policy_vpcs(
    txn: &mut PgConnection,
    host: &ManagedHostStateSnapshot,
) -> CarbideResult<Vec<Vpc>> {
    let Some(instance) = &host.instance else {
        return Ok(Vec::new());
    };
    // A queued network change can start serving its new interfaces without
    // another API request. The old interfaces remain until DPU acknowledgement.
    let configs = std::iter::once(&instance.config.network).chain(
        instance
            .update_network_config_request
            .iter()
            .flat_map(|update| [&update.old_config, &update.new_config]),
    );
    let mut vpc_ids = HashSet::new();
    let mut segment_ids = HashSet::new();
    for interface in configs.flat_map(|config| &config.interfaces) {
        let has_dpu =
            host.dpu_snapshots
                .iter()
                .any(|dpu| match interface.device_locator.as_ref() {
                    None => host.host_snapshot.primary_attached_dpu_machine_id() == Some(dpu.id),
                    Some(locator) => host
                        .host_snapshot
                        .get_device_locator_for_dpu_id(&dpu.id)
                        .is_ok_and(|device| device == *locator),
                });
        if !has_dpu {
            continue;
        }
        // Allocation records the VPC before staging a network update. Legacy
        // interfaces derive their VPC from the segment instead.
        if let Some(vpc_id) = interface.vpc_id {
            vpc_ids.insert(vpc_id);
        } else if let Some(segment_id) = interface.network_segment_id {
            segment_ids.insert(segment_id);
        }
    }
    let mut vpcs = Vec::new();
    for segment_id in segment_ids {
        let vpc = db::vpc::find_by_segment(&mut *txn, segment_id)
            .await?
            .ok_or_else(policy_error)?;
        if vpc_ids.insert(vpc.id) {
            vpcs.push(vpc);
        }
    }
    let loaded_ids: HashSet<_> = vpcs.iter().map(|vpc| vpc.id).collect();
    let direct_ids = vpc_ids.difference(&loaded_ids).copied().collect::<Vec<_>>();
    if !direct_ids.is_empty() {
        let loaded = db::vpc::find_by(
            txn,
            ObjectColumnFilter::List(db::vpc::IdColumn, &direct_ids),
        )
        .await?;
        if loaded.len() != direct_ids.len() {
            return Err(policy_error());
        }
        vpcs.extend(loaded);
    }
    Ok(vpcs)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_network::virtualization::VpcVirtualizationType;
    use carbide_test_support::{Check, check_values};
    use carbide_uuid::network::NetworkSegmentId;
    use carbide_uuid::site_prefix::SitePrefixId;
    use carbide_uuid::vpc::VpcId;
    use chrono::Utc;
    use config_version::ConfigVersion;
    use model::metadata::Metadata;
    use model::site_prefix::{SitePrefixConfig, SitePrefixStatus};
    use model::vpc::{VpcConfig, VpcStatus};

    use super::*;
    use crate::cfg::file::{
        FnnConfig, FnnRoutingProfileConfig, PrefixFilterPolicyEntry, RouteTargetConfig,
    };

    #[test]
    fn instance_network_contraction_keeps_retained_interface_intent() {
        let segment_ids = [NetworkSegmentId::new(), NetworkSegmentId::new()];
        let current = InstanceNetworkConfig::for_segment_ids(&segment_ids, &[], &[]);
        let mut removed = current.clone();
        removed.interfaces.clear();
        let mut changed = current.clone();
        changed.interfaces[0].network_segment_id = Some(segment_ids[1]);
        changed.interfaces[0].network_details = None;
        let mut expanded = current.clone();
        expanded.interfaces.push(current.interfaces[0].clone());

        check_values(
            [
                Check {
                    scenario: "unchanged",
                    input: current.clone(),
                    expect: true,
                },
                Check {
                    scenario: "removed interface",
                    input: removed,
                    expect: true,
                },
                Check {
                    scenario: "changed network",
                    input: changed,
                    expect: false,
                },
                Check {
                    scenario: "duplicated interface is not contraction",
                    input: expanded,
                    expect: false,
                },
            ],
            |candidate| instance_network_is_nonexpanding(&current, &candidate),
        );
    }

    /// Test-specific enum that selects one eligibility rule to vary.
    #[derive(Clone, Copy, Debug)]
    enum Variation {
        Eligible,
        RetainedRootDeleting,
        SiteGateDisabled,
        OpenIsolation,
        SiteGlobalVpcVni,
        LegacyAnycastSitePrefixes,
        CommonInternalRouteTarget,
        AdditionalRouteTargetImport,
        MissingIsolationRoute,
        NestedPrefix,
        SameVpc,
        SameTenant,
        ExistingNotFnn,
        CandidateOperatorRoot,
        ExistingWrongTenantRoot,
        ExistingRootProvisioning,
        CandidateUnsafeProfile,
        ExistingUnsafeProfile,
        CandidateVniMissing,
        ExistingVniMissing,
        SameVni,
        ExistingPrefixDeleting,
    }

    /// Test-specific function that enables every site and profile condition required for reuse.
    fn eligible_config() -> crate::cfg::file::CarbideConfig {
        let mut config = crate::test_support::default_config::get();
        config.tenant_prefix_overlap_enabled = true;
        config.vpc_isolation_behavior = VpcIsolationBehaviorType::MutualIsolation;
        config.site_fabric_prefixes = vec![
            "10.0.0.0/8".parse().unwrap(),
            "192.0.2.0/24".parse().unwrap(),
        ];
        config.fnn = Some(FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([
                (
                    "ELIGIBLE".to_string(),
                    FnnRoutingProfileConfig {
                        tenant_prefix_overlap_eligible: true,
                        internal: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "UNSAFE".to_string(),
                    FnnRoutingProfileConfig {
                        internal: Some(true),
                        ..Default::default()
                    },
                ),
            ]),
            use_vpc_vrf_loopback: false,
        });
        config
    }

    /// Test-specific function that builds an FNN VPC with the requested tenant and VNI.
    fn vpc(tenant: &str, vni: Option<i32>) -> Vpc {
        Vpc {
            id: VpcId::new(),
            version: ConfigVersion::initial(),
            config: VpcConfig {
                tenant_organization_id: tenant.to_string(),
                tenant_keyset_id: None,
                network_virtualization_type: VpcVirtualizationType::Fnn,
                network_security_group_id: None,
                default_nvlink_logical_partition_id: None,
                vni: None,
                routing_profile_type: Some("ELIGIBLE".to_string()),
                routing_profile_overrides: None,
                power_resource_group: None,
                slaac_enabled: false,
            },
            status: VpcStatus { vni },
            metadata: Metadata::default(),
            created: Utc::now(),
            updated: Utc::now(),
            deleted: None,
        }
    }

    /// Test-specific function that builds a ready tenant-managed SitePrefix for one tenant.
    fn site_prefix(tenant: &str, prefix: IpNetwork) -> SitePrefix {
        SitePrefix {
            id: SitePrefixId::new(),
            config: SitePrefixConfig {
                prefix,
                tenant_organization_id: Some(tenant.parse().unwrap()),
                routing_scope: SitePrefixRoutingScope::DatacenterOnly,
            },
            metadata: Metadata::default(),
            status: SitePrefixStatus {
                authority: SitePrefixAuthority::TenantManaged,
                lifecycle_state: SitePrefixLifecycleState::Ready,
            },
            version: ConfigVersion::initial(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn database_scope_requires_an_eligible_tenant_prefix() {
        #[derive(Debug)]
        enum Ineligible {
            SiteGateDisabled,
            OperatorRoot,
            ProfileNotEligible,
            MissingIsolationRoute,
            Ipv6,
        }
        let prefix = "10.123.1.0/24".parse().unwrap();
        let vpc = vpc("scope-test", Some(10200));
        let root = site_prefix("scope-test", "10.123.0.0/16".parse().unwrap());
        let mut config = eligible_config();
        config.site_fabric_prefixes.clear();
        for routes in [None, Some(vec!["10.1.2.3/8".parse().unwrap()])] {
            config.site_fabric_null_routes = routes;
            assert_eq!(
                vpc_prefix_overlap_scope(&config, &root, &vpc, prefix),
                Some(vpc.id),
                "override: {:?}",
                config.site_fabric_null_routes
            );
        }

        for scenario in [
            Ineligible::SiteGateDisabled,
            Ineligible::OperatorRoot,
            Ineligible::ProfileNotEligible,
            Ineligible::MissingIsolationRoute,
            Ineligible::Ipv6,
        ] {
            let mut config = config.clone();
            let mut root = root.clone();
            let mut vpc = vpc.clone();
            let mut prefix = prefix;
            match scenario {
                Ineligible::SiteGateDisabled => config.tenant_prefix_overlap_enabled = false,
                Ineligible::OperatorRoot => {
                    root.status.authority = SitePrefixAuthority::OperatorManaged
                }
                Ineligible::ProfileNotEligible => {
                    vpc.config.routing_profile_type = Some("UNSAFE".into())
                }
                Ineligible::MissingIsolationRoute => config.site_fabric_null_routes = Some(vec![]),
                Ineligible::Ipv6 => {
                    root.config.prefix = "fd00:3891::/64".parse().unwrap();
                    prefix = "fd00:3891::/80".parse().unwrap();
                    config.site_fabric_null_routes = Some(vec![root.config.prefix]);
                }
            }
            assert_eq!(
                vpc_prefix_overlap_scope(&config, &root, &vpc, prefix),
                None,
                "{scenario:?}"
            );
        }
    }

    #[test]
    fn retained_pairs_preserve_isolation_when_admission_stops() {
        enum StopAdmission {
            SiteGate,
            ProfileGate,
            Deletion,
            ServiceVpcSlots,
        }
        for reason in [
            StopAdmission::SiteGate,
            StopAdmission::ProfileGate,
            StopAdmission::Deletion,
            StopAdmission::ServiceVpcSlots,
        ] {
            let mut config = eligible_config();
            let first = vpc("first", Some(100));
            let second = vpc("second", Some(200));
            let prefix = "192.0.2.0/24".parse().unwrap();
            let mut first_root = site_prefix("first", prefix);
            let mut second_root = site_prefix("second", prefix);
            let deleting = matches!(reason, StopAdmission::Deletion);
            match reason {
                StopAdmission::SiteGate => config.tenant_prefix_overlap_enabled = false,
                StopAdmission::ProfileGate => {
                    config
                        .fnn
                        .as_mut()
                        .unwrap()
                        .routing_profiles
                        .get_mut("ELIGIBLE")
                        .unwrap()
                        .tenant_prefix_overlap_eligible = false;
                }
                StopAdmission::Deletion => {
                    first_root.status.lifecycle_state = SitePrefixLifecycleState::Deleting;
                    second_root.status.lifecycle_state = SitePrefixLifecycleState::Deleting;
                }
                StopAdmission::ServiceVpcSlots => config.dpu_config.service_vpc_slot_count = 1,
            }
            let participants = || {
                (
                    VpcPrefixParticipant {
                        prefix,
                        is_deleted: deleting,
                        vpc: &first,
                        site_prefix: &first_root,
                    },
                    VpcPrefixParticipant {
                        prefix,
                        is_deleted: deleting,
                        vpc: &second,
                        site_prefix: &second_root,
                    },
                )
            };
            let (a, b) = participants();
            assert!(!pair_is_eligible(&config, a, b));
            let isolation_routes = config.resolved_site_fabric_null_routes(&[], &[]);
            let (a, b) = participants();
            assert_eq!(
                retained_pair_is_isolated(&config, &isolation_routes, a, b),
                !matches!(reason, StopAdmission::ServiceVpcSlots)
            );
        }
    }

    /// Checks coverage against the same resolved boundaries FNN renders, so
    /// explicit child routes cannot accidentally authorize broader prefix reuse.
    #[test]
    fn isolation_route_coverage_matches_resolved_rendered_routes() {
        // Adjacent roots cover a parent only if inherited inventory is aggregated.
        let adjacent_roots = || {
            vec![
                "10.0.0.0/9".parse().unwrap(),
                "10.128.0.0/9".parse().unwrap(),
            ]
        };
        check_values(
            [
                // Explicit child boundaries must remain separate from an importable parent.
                Check {
                    scenario: "explicit boundaries remain distinct",
                    input: Some(adjacent_roots()),
                    expect: false,
                },
                // Inherited inventory is rendered as one parent that covers the reused prefix.
                Check {
                    scenario: "inherited roots use the rendered exact union",
                    input: None,
                    expect: true,
                },
            ],
            |null_routes| {
                let mut config = eligible_config();
                config.site_fabric_prefixes = adjacent_roots();
                config.site_fabric_null_routes = null_routes;
                let isolation_routes = config.resolved_site_fabric_null_routes(&[], &[]);
                prefix_has_isolation_route(&isolation_routes, "10.0.0.0/8".parse().unwrap())
            },
        );
    }

    /// Ready tenant parents supply inherited isolation outside the configured
    /// operator ranges; an explicit override must provide its own coverage.
    #[test]
    fn tenant_roots_supply_inherited_isolation_for_admission() {
        let prefix = "10.1.0.0/24".parse().unwrap();
        let first = vpc("first", Some(100));
        let second = vpc("second", Some(200));
        let first_root = site_prefix("first", prefix);
        let second_root = site_prefix("second", prefix);
        check_values(
            [
                Check {
                    scenario: "tenant parents supply inherited coverage",
                    input: None,
                    expect: true,
                },
                Check {
                    scenario: "tenant parents cannot augment an explicit override",
                    input: Some(vec![]),
                    expect: false,
                },
            ],
            |override_routes| {
                let mut config = eligible_config();
                config.site_fabric_prefixes.clear();
                config.site_fabric_null_routes = override_routes;
                pair_is_eligible(
                    &config,
                    VpcPrefixParticipant {
                        prefix,
                        is_deleted: false,
                        vpc: &first,
                        site_prefix: &first_root,
                    },
                    VpcPrefixParticipant {
                        prefix,
                        is_deleted: false,
                        vpc: &second,
                        site_prefix: &second_root,
                    },
                )
            },
        );
    }

    #[test]
    fn routing_profile_contractions_use_effective_policy() {
        let target_a = RouteTargetConfig {
            asn: 65000,
            vni: 100,
        };
        let target_b = RouteTargetConfig {
            asn: 65000,
            vni: 200,
        };
        let previous = FnnRoutingProfileConfig {
            tenant_prefix_overlap_eligible: true,
            internal: Some(true),
            route_target_imports: Some(vec![target_a, target_b.clone()]),
            ..Default::default()
        };
        let explicit_defaults = FnnRoutingProfileConfig {
            route_targets_on_exports: Some(vec![]),
            leak_default_route_from_underlay: Some(false),
            leak_tenant_host_routes_to_underlay: Some(false),
            tenant_leak_communities_accepted: Some(false),
            accepted_leaks_from_underlay: Some(vec![]),
            allowed_anycast_prefixes: Some(vec![]),
            ..previous.clone()
        };
        let broad_prefix = PrefixFilterPolicyEntry {
            prefix: "10.0.0.0/16".parse().unwrap(),
        };
        let narrow_prefix = PrefixFilterPolicyEntry {
            prefix: "10.0.1.0/24".parse().unwrap(),
        };
        let with_prefixes = FnnRoutingProfileConfig {
            accepted_leaks_from_underlay: Some(vec![broad_prefix.clone()]),
            allowed_anycast_prefixes: Some(vec![broad_prefix]),
            ..previous.clone()
        };
        let narrowed = FnnRoutingProfileConfig {
            accepted_leaks_from_underlay: Some(vec![narrow_prefix.clone()]),
            allowed_anycast_prefixes: Some(vec![narrow_prefix]),
            ..previous.clone()
        };
        let otherwise_eligible = FnnRoutingProfileConfig {
            tenant_prefix_overlap_eligible: true,
            internal: Some(true),
            ..Default::default()
        };
        check_values(
            [
                Check {
                    scenario: "explicit false and empty values preserve omitted defaults",
                    input: (previous.clone(), explicit_defaults, vec![]),
                    expect: true,
                },
                Check {
                    scenario: "remove one import while another remains",
                    input: (
                        previous.clone(),
                        FnnRoutingProfileConfig {
                            route_target_imports: Some(vec![target_b]),
                            ..previous
                        },
                        vec![],
                    ),
                    expect: true,
                },
                Check {
                    scenario: "prefix filters narrow without removing other imports",
                    input: (with_prefixes.clone(), narrowed, vec![]),
                    expect: true,
                },
                Check {
                    scenario: "disabling community handling can newly export a route",
                    input: (
                        FnnRoutingProfileConfig {
                            tenant_leak_communities_accepted: Some(true),
                            ..with_prefixes.clone()
                        },
                        with_prefixes.clone(),
                        vec![],
                    ),
                    expect: false,
                },
                Check {
                    scenario: "empty IPv4 anycast exposes legacy fallback even with an eligible profile",
                    input: (
                        with_prefixes,
                        otherwise_eligible,
                        vec!["10.0.0.0/8".parse().unwrap()],
                    ),
                    expect: false,
                },
            ],
            |(previous, candidate, legacy_anycast)| {
                let mut config = eligible_config();
                config.anycast_site_prefixes = legacy_anycast;
                routing_profile_is_nonexpanding(&config, &previous, &candidate)
            },
        );
    }

    /// Test-specific function that checks each pair eligibility rule from an eligible pair.
    #[test]
    fn exact_pair_eligibility_is_fail_closed() {
        let exact: IpNetwork = "10.0.1.0/24".parse().unwrap();

        check_values(
            [
                Check {
                    scenario: "eligible pair",
                    input: Variation::Eligible,
                    expect: true,
                },
                Check {
                    scenario: "existing SitePrefix is deleting",
                    input: Variation::RetainedRootDeleting,
                    expect: true,
                },
                Check {
                    scenario: "site gate is disabled",
                    input: Variation::SiteGateDisabled,
                    expect: false,
                },
                Check {
                    scenario: "site isolation is open",
                    input: Variation::OpenIsolation,
                    expect: false,
                },
                Check {
                    scenario: "site uses a global VPC VNI",
                    input: Variation::SiteGlobalVpcVni,
                    expect: false,
                },
                Check {
                    scenario: "site uses deprecated anycast prefixes",
                    input: Variation::LegacyAnycastSitePrefixes,
                    expect: false,
                },
                Check {
                    scenario: "site uses a common internal route target",
                    input: Variation::CommonInternalRouteTarget,
                    expect: false,
                },
                Check {
                    scenario: "site adds route target imports",
                    input: Variation::AdditionalRouteTargetImport,
                    expect: false,
                },
                Check {
                    scenario: "reused prefix has no isolation route",
                    input: Variation::MissingIsolationRoute,
                    expect: false,
                },
                Check {
                    scenario: "candidate CIDR is nested",
                    input: Variation::NestedPrefix,
                    expect: false,
                },
                Check {
                    scenario: "prefixes belong to the same VPC",
                    input: Variation::SameVpc,
                    expect: false,
                },
                Check {
                    scenario: "distinct VPCs share one tenant SitePrefix",
                    input: Variation::SameTenant,
                    expect: true,
                },
                Check {
                    scenario: "existing VPC does not use FNN",
                    input: Variation::ExistingNotFnn,
                    expect: false,
                },
                Check {
                    scenario: "candidate SitePrefix is operator managed",
                    input: Variation::CandidateOperatorRoot,
                    expect: false,
                },
                Check {
                    scenario: "existing SitePrefix belongs to another tenant",
                    input: Variation::ExistingWrongTenantRoot,
                    expect: false,
                },
                Check {
                    scenario: "existing SitePrefix is provisioning",
                    input: Variation::ExistingRootProvisioning,
                    expect: false,
                },
                Check {
                    scenario: "candidate profile is unsafe",
                    input: Variation::CandidateUnsafeProfile,
                    expect: false,
                },
                Check {
                    scenario: "existing profile is unsafe",
                    input: Variation::ExistingUnsafeProfile,
                    expect: false,
                },
                Check {
                    scenario: "candidate VNI is missing",
                    input: Variation::CandidateVniMissing,
                    expect: false,
                },
                Check {
                    scenario: "existing VNI is missing",
                    input: Variation::ExistingVniMissing,
                    expect: false,
                },
                Check {
                    scenario: "VPCs use the same VNI",
                    input: Variation::SameVni,
                    expect: false,
                },
                Check {
                    scenario: "existing VpcPrefix is deleting",
                    input: Variation::ExistingPrefixDeleting,
                    expect: false,
                },
            ],
            |variation| {
                let mut config = eligible_config();
                let mut candidate_prefix = exact;
                let mut candidate_vpc = vpc("tenant-a", Some(100));
                let mut existing_vpc = vpc("tenant-b", Some(200));
                let mut candidate_site_prefix =
                    site_prefix("tenant-a", "10.0.0.0/16".parse().unwrap());
                let mut existing_site_prefix =
                    site_prefix("tenant-b", "10.0.0.0/16".parse().unwrap());
                let mut existing_deleted = false;

                match variation {
                    Variation::Eligible => {}
                    Variation::RetainedRootDeleting => {
                        existing_site_prefix.status.lifecycle_state =
                            SitePrefixLifecycleState::Deleting;
                    }
                    Variation::SiteGateDisabled => config.tenant_prefix_overlap_enabled = false,
                    Variation::OpenIsolation => {
                        config.vpc_isolation_behavior = VpcIsolationBehaviorType::Open;
                    }
                    Variation::SiteGlobalVpcVni => config.site_global_vpc_vni = Some(5_000),
                    Variation::LegacyAnycastSitePrefixes => {
                        config.anycast_site_prefixes = vec!["10.0.0.0/16".parse().unwrap()];
                    }
                    Variation::CommonInternalRouteTarget => {
                        config.fnn.as_mut().unwrap().common_internal_route_target =
                            Some(crate::cfg::file::RouteTargetConfig { asn: 1, vni: 2 });
                    }
                    Variation::AdditionalRouteTargetImport => {
                        config.fnn.as_mut().unwrap().additional_route_target_imports =
                            vec![crate::cfg::file::RouteTargetConfig { asn: 3, vni: 4 }];
                    }
                    Variation::MissingIsolationRoute => {
                        config.site_fabric_null_routes = Some(vec![]);
                    }
                    Variation::NestedPrefix => {
                        candidate_prefix = "10.0.1.0/25".parse().unwrap();
                    }
                    Variation::SameVpc => existing_vpc.id = candidate_vpc.id,
                    Variation::SameTenant => {
                        existing_vpc.config.tenant_organization_id = "tenant-a".to_string();
                        existing_site_prefix = candidate_site_prefix.clone();
                    }
                    Variation::ExistingNotFnn => {
                        existing_vpc.config.network_virtualization_type =
                            VpcVirtualizationType::Flat;
                    }
                    Variation::CandidateOperatorRoot => {
                        candidate_site_prefix.status.authority =
                            SitePrefixAuthority::OperatorManaged;
                    }
                    Variation::ExistingWrongTenantRoot => {
                        existing_site_prefix.config.tenant_organization_id =
                            Some("tenant-c".parse().unwrap());
                    }
                    Variation::ExistingRootProvisioning => {
                        existing_site_prefix.status.lifecycle_state =
                            SitePrefixLifecycleState::Provisioning;
                    }
                    Variation::CandidateUnsafeProfile => {
                        candidate_vpc.config.routing_profile_type = Some("UNSAFE".to_string());
                    }
                    Variation::ExistingUnsafeProfile => {
                        existing_vpc.config.routing_profile_type = Some("UNSAFE".to_string());
                    }
                    Variation::CandidateVniMissing => candidate_vpc.status.vni = None,
                    Variation::ExistingVniMissing => existing_vpc.status.vni = None,
                    Variation::SameVni => existing_vpc.status.vni = candidate_vpc.status.vni,
                    Variation::ExistingPrefixDeleting => existing_deleted = true,
                }

                pair_is_eligible(
                    &config,
                    VpcPrefixParticipant {
                        prefix: candidate_prefix,
                        is_deleted: false,
                        vpc: &candidate_vpc,
                        site_prefix: &candidate_site_prefix,
                    },
                    VpcPrefixParticipant {
                        prefix: exact,
                        is_deleted: existing_deleted,
                        vpc: &existing_vpc,
                        site_prefix: &existing_site_prefix,
                    },
                )
            },
        );
    }
}
