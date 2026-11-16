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
use std::net::{IpAddr, Ipv6Addr};

use ::rpc::forge as rpc;
use carbide_uuid::vpc::VpcId;
use db::resource_pool::ResourcePoolDatabaseError;
use db::{DatabaseError, ObjectColumnFilter, network_segment};
use ipnetwork::IpNetwork;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    NetworkSegment, NetworkSegmentControllerState, NetworkSegmentSearchConfig, NetworkSegmentType,
    NewNetworkSegment,
};
use model::vpc::VpcVirtualizationTypeCapabilities;
use sqlx::{PgConnection, PgTransaction};
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};
use crate::{CarbideError, CarbideResult};

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::NetworkSegmentSearchFilter>,
) -> Result<Response<rpc::NetworkSegmentIdList>, Status> {
    log_request_data(&request);

    let filter: model::network_segment::NetworkSegmentSearchFilter = request.into_inner().into();

    let network_segments_ids =
        db::network_segment::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::NetworkSegmentIdList {
        network_segments_ids,
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::NetworkSegmentsByIdsRequest>,
) -> Result<Response<rpc::NetworkSegmentList>, Status> {
    log_request_data(&request);
    let rpc::NetworkSegmentsByIdsRequest {
        network_segments_ids,
        include_history,
        include_num_free_ips,
        ..
    } = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if network_segments_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if network_segments_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let segments = db::network_segment::find_by(
        &mut api.db_reader(),
        ObjectColumnFilter::List(network_segment::IdColumn, &network_segments_ids),
        NetworkSegmentSearchConfig {
            include_history,
            include_num_free_ips,
        },
    )
    .await?;

    let mut result = Vec::with_capacity(segments.len());
    for seg in segments {
        result.push(seg.into());
    }
    Ok(Response::new(rpc::NetworkSegmentList {
        network_segments: result,
    }))
}

/// Rejects direct prefixes that conflict with a scoped VPC prefix. Supplying
/// `vpc_id` also rejects global parents for RPC creation and attachment.
/// Bootstrap omits it to preserve configured networks over global parents.
/// Stored scoped prefixes remain protected even when overlap is disabled.
///
/// The caller holds the overlap transaction lock from this probe through the
/// `NetworkSegment` write, so another participating `VpcPrefix` request cannot
/// commit between them.
async fn reject_vpc_prefix_overlaps(
    txn: &mut PgConnection,
    prefixes: &[IpNetwork],
    vpc_id: Option<VpcId>,
) -> CarbideResult<()> {
    for prefix in prefixes {
        if db::vpc_prefix::probe(*prefix, &mut *txn)
            .await?
            .iter()
            .any(|overlap| vpc_id.is_some() || overlap.overlap_vpc_id.is_some())
        {
            return Err(super::tenant_prefix_overlap::overlap_error());
        }
    }
    Ok(())
}

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::NetworkSegmentCreationRequest>,
) -> Result<Response<rpc::NetworkSegment>, Status> {
    crate::api::log_request_data(&request);

    let request = request.into_inner();

    let new_network_segment = NewNetworkSegment::try_from(request)?;

    // Only actual ::Tenant segments need to be contained within site
    // fabric prefixes. ::HostInband segments (which, yes, are also part
    // of `is_tenant()` for binding rules) are underlay networks not
    // part of the overlay/site fabric prefixes, exist before VPCs, and
    // can exist without ever needing to be associated with a VPC.
    if new_network_segment.segment_type == NetworkSegmentType::Tenant
        && let Some(site_fabric_prefixes) = api.eth_data.site_fabric_prefixes.as_ref()
    {
        let segment_prefixes: Vec<_> = new_network_segment
            .prefixes
            .iter()
            .map(|np| np.prefix)
            .collect();

        let uncontained_prefixes: Vec<_> = segment_prefixes
            .into_iter()
            .filter(|segment_prefix| !site_fabric_prefixes.contains(*segment_prefix))
            .collect();

        // Anything in uncontained_prefixes did not match any of our
        // site fabric prefixes, and if we allowed it to be used then VPC
        // isolation would not function properly for traffic addressed to
        // that prefix.
        if !uncontained_prefixes.is_empty() {
            let uncontained_prefixes = itertools::join(uncontained_prefixes, ", ");
            let msg = format!(
                "One or more requested network segment prefixes were not contained \
                        within the configured site fabric prefixes: {uncontained_prefixes}"
            );
            return Err(CarbideError::InvalidArgument(msg).into());
        }
    }

    let mut txn = api.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(txn.as_mut()).await?;

    let allocate_svi_ip = if let Some(vpc_id) = new_network_segment.vpc_id {
        let vpcs = db::vpc::find_by(
            &mut txn,
            ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id),
        )
        .await?;

        let vpc = vpcs
            .first()
            .ok_or_else(|| CarbideError::internal(format!("VPC ID: {vpc_id} not found")))?;

        let virtualization_type = vpc.config.network_virtualization_type;

        // Segment compatibility (segment-type binding + IPv6 support)
        // and SVI allocation are both expressed as capability checks
        // on the VPC's virtualization type; see `model::vpc::capability`.
        virtualization_type
            .ensure_supports_segment(&new_network_segment)
            .map_err(CarbideError::from)?;
        virtualization_type.allocates_svi_for(&new_network_segment)
    } else {
        false
    };

    if let Some(vpc_id) = new_network_segment.vpc_id {
        let prefixes = new_network_segment
            .prefixes
            .iter()
            .map(|prefix| prefix.prefix)
            .collect::<Vec<_>>();
        reject_vpc_prefix_overlaps(&mut txn, &prefixes, Some(vpc_id)).await?;
    }

    let network_segment = save(api, &mut txn, new_network_segment, false, allocate_svi_ip).await?;

    txn.commit().await?;

    Ok(Response::new(network_segment.into()))
}

/// Checks CIDR syntax, reservation and gateway/relay fields before opening a
/// transaction. Stored-family, eligibility and overlap checks use `lock_checks`.
fn parse_added_prefix(
    request: &rpc::AddNetworkSegmentPrefixRequest,
) -> CarbideResult<NewNetworkPrefix> {
    if !request
        .prefix
        .split_once('/')
        .is_some_and(|(_, length)| length.parse::<u8>().is_ok())
    {
        return Err(CarbideError::InvalidArgument(
            "prefix CIDR requires an explicit numeric prefix length".to_string(),
        ));
    }
    let prefix: IpNetwork = request.prefix.parse().map_err(|_| {
        CarbideError::InvalidArgument(format!("invalid prefix CIDR: {}", request.prefix))
    })?;
    if prefix.ip() != prefix.network() {
        return Err(CarbideError::InvalidArgument(
            "prefix must not have host bits set".to_string(),
        ));
    }
    // PostgreSQL requires strict gateway containment; a /32 equals its gateway.
    if prefix.is_ipv4() && prefix.prefix() == 32 {
        return Err(CarbideError::InvalidArgument(
            "an IPv4 prefix must be shorter than /32 to contain its gateway".to_string(),
        ));
    }
    if request.reserve_first < 0 {
        return Err(CarbideError::InvalidArgument(
            "reserve_first must be non-negative".to_string(),
        ));
    }
    let gateway: Option<IpAddr> = request
        .gateway
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|_| CarbideError::InvalidArgument("invalid gateway address".to_string()))?;
    match (prefix.is_ipv4(), gateway) {
        (true, Some(address)) if address.is_ipv4() && prefix.contains(address) => {}
        (true, _) => {
            return Err(CarbideError::InvalidArgument(
                "an IPv4 prefix requires an IPv4 gateway within the prefix".to_string(),
            ));
        }
        (false, Some(_)) => {
            return Err(CarbideError::InvalidArgument(
                "gateway is not accepted for an IPv6 prefix".to_string(),
            ));
        }
        (false, None) => {}
    }
    // DHCP advertises this as the client router. /31 point-to-point links use
    // both endpoints, but ordinary IPv4 networks cannot use either as a gateway.
    if prefix.is_ipv4()
        && prefix.prefix() < 31
        && gateway
            .is_some_and(|address| address == prefix.network() || address == prefix.broadcast())
    {
        return Err(CarbideError::InvalidArgument(
            "an IPv4 gateway must not be the network or broadcast address".to_string(),
        ));
    }
    let dhcpv6_link_address = request
        .dhcpv6_link_address
        .as_deref()
        .map(|address| address.parse::<Ipv6Addr>())
        .transpose()
        .map_err(|_| {
            CarbideError::InvalidArgument("dhcpv6_link_address must be an IPv6 address".to_string())
        })?
        .map(IpAddr::V6);
    if prefix.is_ipv4() && dhcpv6_link_address.is_some() {
        return Err(CarbideError::InvalidArgument(
            "dhcpv6_link_address requires an IPv6 prefix".to_string(),
        ));
    }
    Ok(NewNetworkPrefix {
        prefix,
        gateway,
        dhcpv6_link_address,
        num_reserved: request.reserve_first,
    })
}

/// Adds one missing family without recreating an occupied underlay. Prefix,
/// version and reverse DNS share the final commit; exact retries do not write.
pub(crate) async fn add_prefix(
    api: &Api,
    request: Request<rpc::AddNetworkSegmentPrefixRequest>,
) -> Result<Response<rpc::NetworkSegment>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let segment_id = request
        .network_segment_id
        .ok_or(CarbideError::MissingArgument("network_segment_id"))?;
    let prefix = parse_added_prefix(&request)?;
    let mut txn = api.txn_begin().await?;
    // Read only after `lock_checks`: another prefix writer must not commit an
    // overlap between the probe and insert, or leave this request a stale family.
    db::tenant_prefix_overlap::lock_checks(txn.as_mut()).await?;
    let segment = db::network_segment::find_by(
        &mut txn,
        ObjectColumnFilter::One(network_segment::IdColumn, &segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "network segment",
        id: segment_id.to_string(),
    })?;
    // A known, deleted target is an unsupported mutation, not an unknown ID.
    if segment.is_marked_as_deleted() {
        return Err(CarbideError::FailedPrecondition(format!(
            "network segment {segment_id} is deleted; a live segment is required"
        ))
        .into());
    }
    if segment.config.segment_type != NetworkSegmentType::Underlay {
        return Err(CarbideError::FailedPrecondition(format!(
            "network segment {segment_id} has type {:?}; underlay is required",
            segment.config.segment_type
        ))
        .into());
    }
    if let Some(vpc_id) = segment.config.vpc_id {
        return Err(CarbideError::FailedPrecondition(format!(
            "network segment {segment_id} is attached to VPC {vpc_id}; an unattached segment is required"
        ))
        .into());
    }
    if segment.status.controller_state.value != NetworkSegmentControllerState::Ready {
        return Err(CarbideError::FailedPrecondition(format!(
            "network segment {segment_id} has state {:?}; ready is required",
            segment.status.controller_state.value
        ))
        .into());
    }
    if let Some(existing) = segment
        .prefixes
        .iter()
        .find(|existing| existing.prefix.is_ipv4() == prefix.prefix.is_ipv4())
    {
        if existing.prefix == prefix.prefix
            && existing.gateway == prefix.gateway
            && existing.num_reserved == prefix.num_reserved
            && existing.dhcpv6_link_address == prefix.dhcpv6_link_address
        {
            // A lost successful reply must be safe to retry without another
            // version increment or a second prefix.
            txn.commit().await?;
            return Ok(Response::new(segment.into()));
        }
        return Err(CarbideError::FailedPrecondition(
            "network segment already has a different prefix or settings for this family"
                .to_string(),
        )
        .into());
    }
    if segment.prefixes.len() != 1 {
        return Err(CarbideError::FailedPrecondition(
            "adding a prefix requires exactly one existing opposite-family prefix".to_string(),
        )
        .into());
    }
    if !db::network_prefix::containing_prefix(&mut txn, &prefix.prefix.to_string())
        .await?
        .is_empty()
    {
        return Err(CarbideError::InvalidArgument(
            "prefix overlaps with an existing one".to_string(),
        )
        .into());
    }
    reject_vpc_prefix_overlaps(txn.as_mut(), &[prefix.prefix], None).await?;
    let segment = db::network_segment::add_prefix(&segment, &prefix, txn.as_mut())
        .await
        .map_err(|error| {
            // Keep backstop constraints diagnostic even for writers outside the
            // overlap guard. The normal range/family probes reject first.
            let constraint = match &error {
                DatabaseError::Sqlx(error) => error
                    .source
                    .as_database_error()
                    .and_then(|error| error.constraint()),
                _ => None,
            };
            if db::network_prefix::is_overlap_constraint(constraint) {
                CarbideError::InvalidArgument("prefix overlaps with an existing one".to_string())
            } else if db::network_prefix::is_family_constraint(constraint) {
                CarbideError::FailedPrecondition(
                    "network segment already has a prefix for this family".to_string(),
                )
            } else if db::network_prefix::is_dhcpv6_link_address_constraint(constraint) {
                CarbideError::InvalidArgument(
                    "DHCPv6 link-address conflicts with an existing prefix".to_string(),
                )
            } else {
                CarbideError::from(error)
            }
        })?;
    db::dns::ensure_reverse_zones(&[prefix.prefix], &mut txn).await?;
    txn.commit().await?;
    Ok(Response::new(segment.into()))
}

pub(crate) async fn attach_to_vpc(
    api: &Api,
    request: Request<rpc::AttachNetworkSegmentToVpcRequest>,
) -> Result<Response<rpc::NetworkSegment>, Status> {
    crate::api::log_request_data(&request);

    let rpc::AttachNetworkSegmentToVpcRequest {
        network_segment_id,
        vpc_id,
        allow_replace,
        ..
    } = request.into_inner();

    let segment_id =
        network_segment_id.ok_or(CarbideError::MissingArgument("network_segment_id"))?;
    let vpc_id = vpc_id.ok_or(CarbideError::MissingArgument("vpc_id"))?;

    let mut txn = api.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(txn.as_mut()).await?;

    let vpcs = db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await?;
    let vpc = vpcs.first().ok_or_else(|| CarbideError::NotFoundError {
        kind: "vpc",
        id: vpc_id.to_string(),
    })?;

    let segment = db::network_segment::find_by(
        &mut txn,
        ObjectColumnFilter::One(network_segment::IdColumn, &segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?
    .into_iter()
    .find(|segment| !segment.is_marked_as_deleted())
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "network segment",
        id: segment_id.to_string(),
    })?;

    if segment.config.segment_type != NetworkSegmentType::HostInband {
        return Err(CarbideError::InvalidArgument(format!(
            "only host_inband network segments can be attached to a VPC with this API, got {}",
            segment.config.segment_type
        ))
        .into());
    }

    vpc.config
        .network_virtualization_type
        .ensure_supports_segment(&segment)
        .map_err(CarbideError::from)?;

    let network_segment = match segment.config.vpc_id {
        Some(current_vpc_id) if current_vpc_id == vpc_id => segment,
        Some(current_vpc_id) if !allow_replace => {
            return Err(CarbideError::FailedPrecondition(format!(
                "network segment {} is already attached to VPC {}",
                segment.id, current_vpc_id
            ))
            .into());
        }
        _ => {
            let prefixes = segment
                .prefixes
                .iter()
                .filter(|prefix| prefix.vpc_prefix_id.is_none())
                .map(|prefix| prefix.prefix)
                .collect::<Vec<_>>();
            reject_vpc_prefix_overlaps(&mut txn, &prefixes, Some(vpc_id)).await?;
            db::network_segment::attach_to_vpc(&segment, txn.as_mut(), vpc_id).await?
        }
    };

    txn.commit().await?;
    Ok(Response::new(network_segment.into()))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::NetworkSegmentDeletionRequest>,
) -> Result<Response<rpc::NetworkSegmentDeletionResult>, Status> {
    crate::api::log_request_data(&request);

    let rpc::NetworkSegmentDeletionRequest { id, .. } = request.into_inner();
    let segment_id = id.ok_or(CarbideError::MissingArgument("id"))?;
    let mut txn = api.txn_begin().await?;
    // Read the prefix list after any append commits, so deletion removes every
    // reverse zone rather than acting on a pre-append snapshot. Reuse the
    // existing overlap lock/order; deletes also wait for its site-wide readers.
    db::tenant_prefix_overlap::lock_checks(txn.as_mut()).await?;

    let mut segments = db::network_segment::find_by(
        &mut txn,
        ObjectColumnFilter::One(network_segment::IdColumn, &segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;

    let segment = match segments.len() {
        1 => segments.remove(0),
        _ => {
            return Err(CarbideError::NotFoundError {
                kind: "network segment",
                id: segment_id.to_string(),
            }
            .into());
        }
    };

    db::network_segment::mark_as_deleted(&segment, &mut txn).await?;

    // A network's reverse-DNS zone exists only because the network does, so it
    // is dropped with the segment -- the inverse of the create-time hook in
    // `save`.
    let prefixes = segment
        .prefixes
        .iter()
        .map(|network_prefix| network_prefix.prefix)
        .collect::<Vec<_>>();
    db::dns::remove_reverse_zones(&prefixes, segment.id, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::NetworkSegmentDeletionResult {}))
}

pub(crate) async fn for_vpc(
    api: &Api,
    request: Request<rpc::VpcSearchQuery>,
) -> Result<Response<rpc::NetworkSegmentList>, Status> {
    crate::api::log_request_data(&request);

    let rpc::VpcSearchQuery { id, .. } = request.into_inner();

    let uuid = id.ok_or_else(|| CarbideError::InvalidArgument("id".to_string()))?;

    let results = db::network_segment::for_vpc(&api.database_connection, uuid).await?;

    Ok(Response::new(rpc::NetworkSegmentList {
        network_segments: results.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn find_state_histories(
    api: &Api,
    request: Request<rpc::NetworkSegmentStateHistoriesRequest>,
) -> Result<Response<rpc::StateHistories>, Status> {
    log_request_data(&request);
    let segment_ids = request.into_inner().network_segment_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if segment_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if segment_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;
    let results = db::state_history::find_by_object_ids(
        &mut txn,
        db::state_history::StateHistoryTableId::NetworkSegment,
        &segment_ids,
    )
    .await?;

    let mut response = rpc::StateHistories::default();
    for (segment_id, records) in results {
        response.histories.insert(
            segment_id,
            rpc::StateHistoryRecords {
                records: records.into_iter().map(Into::into).collect(),
            },
        );
    }

    txn.commit().await?;
    Ok(tonic::Response::new(response))
}

/// `save` is the single-segment persistence path used by the
/// `CreateNetworkSegment` handler. It writes the segment, performs its resource
/// allocations, and creates every reverse-DNS zone derived from the persisted
/// prefixes before the caller commits. The segment and its zones therefore
/// become visible together, and a zone failure rolls the segment back as well.
/// These rows support rollback to zone-backed DNS; derived PTR lookup ignores
/// them and does not claim authority over their reverse zones.
///
/// Startup uses [`save_without_reverse_zones`] instead. It persists every
/// configured segment first, resolves config drift through the stored
/// `network_def.segment_id` links, then acquires the complete sorted zone-lock
/// set once before creating any missing zones.
pub(crate) async fn save(
    api: &Api,
    txn: &mut PgTransaction<'_>,
    ns: NewNetworkSegment,
    set_to_ready: bool,
    allocate_svi_ip: bool,
) -> Result<NetworkSegment, CarbideError> {
    let network_segment =
        save_without_reverse_zones(api, txn, ns, set_to_ready, allocate_svi_ip).await?;
    let prefixes = network_segment
        .prefixes
        .iter()
        .map(|network_prefix| network_prefix.prefix)
        .collect::<Vec<_>>();
    db::dns::ensure_reverse_zones(&prefixes, txn).await?;
    Ok(network_segment)
}

/// `save_without_reverse_zones` performs the segment write, resource-pool
/// allocations, and optional SVI allocation without updating DNS.
///
/// [`save`] follows it immediately with one segment's DNS update.
/// [`crate::db_init::create_initial_networks`] uses it for configured segments
/// and the static-assignments anchor so startup can update all reverse zones
/// once, in the same transaction and with a stable lock order. Any other caller
/// must arrange the matching DNS update before committing.
///
/// Callers hold the overlap transaction lock through validation and persistence.
pub(crate) async fn save_without_reverse_zones(
    api: &Api,
    txn: &mut PgTransaction<'_>,
    mut ns: NewNetworkSegment,
    set_to_ready: bool,
    allocate_svi_ip: bool,
) -> Result<NetworkSegment, CarbideError> {
    if let Some(domain_id) = ns.subdomain_id {
        db::dns::domain::lock_live_for_reference(txn.as_mut(), domain_id).await?;
    }

    let prefixes = ns
        .prefixes
        .iter()
        .map(|prefix| prefix.prefix)
        .collect::<Vec<_>>();
    for prefix in &prefixes {
        if !db::network_prefix::containing_prefix(txn.as_mut(), &prefix.to_string())
            .await?
            .is_empty()
        {
            return Err(CarbideError::InvalidArgument(
                "prefix overlaps with an existing one".to_string(),
            ));
        }
    }
    // Configured networks also need these checks: scoped prefixes must not
    // overlap direct prefixes. Bootstrap still permits global VPC parents;
    // the RPC's attached-segment rule above is intentionally stricter.
    reject_vpc_prefix_overlaps(txn.as_mut(), &prefixes, None).await?;

    if ns.segment_type != NetworkSegmentType::Underlay {
        ns.vlan_id = Some(allocate_vlan_id(api, txn, &ns.name).await?);
        ns.vni = Some(allocate_vni(api, txn, &ns.name).await?);
    }
    let initial_state = if set_to_ready {
        NetworkSegmentControllerState::Ready
    } else {
        NetworkSegmentControllerState::Provisioning
    };
    let mut network_segment = match db::network_segment::persist(ns, txn, initial_state).await {
        Ok(segment) => segment,
        Err(DatabaseError::Sqlx(error))
            if db::network_prefix::is_overlap_constraint(
                error
                    .source
                    .as_database_error()
                    .and_then(|error| error.constraint()),
            ) =>
        {
            return Err(CarbideError::InvalidArgument(
                "prefix overlaps with an existing one".to_string(),
            ));
        }
        Err(err) => {
            return Err(err.into());
        }
    };

    if allocate_svi_ip {
        db::network_segment::allocate_svi_ip(&network_segment, txn).await?;
        let network_segments = db::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(network_segment::IdColumn, &network_segment.id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?;

        network_segment = network_segments
            .first()
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "NetworkSegment",
                id: network_segment.id.to_string(),
            })?
            .clone();
    }

    Ok(network_segment)
}

/// Allocate a value from the vni resource pool.
///
/// If the pool exists but is empty or has en error, return that.
async fn allocate_vni(
    api: &Api,
    txn: &mut PgConnection,
    owner_id: &str,
) -> Result<i32, CarbideError> {
    match db::resource_pool::allocate(
        &api.common_pools.ethernet.pool_vni,
        txn,
        model::resource_pool::OwnerType::NetworkSegment,
        owner_id,
        None,
    )
    .await
    {
        Ok(val) => Ok(val),
        Err(
            error @ ResourcePoolDatabaseError::ResourcePool(
                model::resource_pool::ResourcePoolError::Empty,
            ),
        ) => {
            db::resource_pool::emit_allocation_failure(
                &api.instrumentation,
                api.common_pools.ethernet.pool_vni.value_type,
                owner_id,
                false,
                "vni",
                &error,
            );
            Err(CarbideError::ResourceExhausted("pool vni".to_string()))
        }
        Err(err) => {
            db::resource_pool::emit_allocation_failure(
                &api.instrumentation,
                api.common_pools.ethernet.pool_vni.value_type,
                owner_id,
                false,
                "vni",
                &err,
            );
            Err(err.into())
        }
    }
}

/// Allocate a value from the vlan id resource pool.
///
/// If the pool exists but is empty or has en error, return that.
async fn allocate_vlan_id(
    api: &Api,
    txn: &mut PgConnection,
    owner_id: &str,
) -> Result<i16, CarbideError> {
    match db::resource_pool::allocate(
        &api.common_pools.ethernet.pool_vlan_id,
        txn,
        model::resource_pool::OwnerType::NetworkSegment,
        owner_id,
        None,
    )
    .await
    {
        Ok(val) => Ok(val),
        Err(
            error @ ResourcePoolDatabaseError::ResourcePool(
                model::resource_pool::ResourcePoolError::Empty,
            ),
        ) => {
            db::resource_pool::emit_allocation_failure(
                &api.instrumentation,
                api.common_pools.ethernet.pool_vlan_id.value_type,
                owner_id,
                false,
                "vlan_id",
                &error,
            );
            Err(CarbideError::ResourceExhausted("pool vlan_id".to_string()))
        }
        Err(err) => {
            db::resource_pool::emit_allocation_failure(
                &api.instrumentation,
                api.common_pools.ethernet.pool_vlan_id.value_type,
                owner_id,
                false,
                "vlan_id",
                &err,
            );
            Err(err.into())
        }
    }
}

#[cfg(test)]
mod add_prefix_validation_tests {
    use super::*;

    /// Invalid caller data must fail before a database transaction. Each row
    /// protects a different syntax or cross-field validation boundary.
    #[test]
    fn rejects_invalid_added_prefix_settings() {
        let cases = [
            // A malformed CIDR is not a storage or lifecycle failure.
            (
                "malformed network",
                "not-a-cidr/64",
                None,
                0,
                None,
                "invalid prefix CIDR: not-a-cidr/64",
            ),
            // ipnetwork otherwise treats a bare address as a /128 host network.
            (
                "explicit CIDR length",
                "2001:db8::",
                None,
                0,
                None,
                "prefix CIDR requires an explicit numeric prefix length",
            ),
            // PostgreSQL CIDR rejects host bits; diagnose them at the RPC boundary.
            (
                "IPv6 host bits",
                "2001:db8::1/64",
                None,
                0,
                None,
                "prefix must not have host bits set",
            ),
            // A negative stored count makes allocation fail internally; reject it here.
            (
                "negative reservation",
                "2001:db8::/64",
                None,
                -1,
                None,
                "reserve_first must be non-negative",
            ),
            // IPv4 routing metadata cannot be omitted.
            (
                "missing IPv4 gateway",
                "192.0.2.0/24",
                None,
                0,
                None,
                "an IPv4 prefix requires an IPv4 gateway within the prefix",
            ),
            // A syntactically invalid gateway must not become an INTERNAL error.
            (
                "malformed gateway",
                "192.0.2.0/24",
                Some("invalid"),
                0,
                None,
                "invalid gateway address",
            ),
            // Address family and containment are independent of CIDR syntax.
            (
                "wrong-family gateway",
                "192.0.2.0/24",
                Some("2001:db8::1"),
                0,
                None,
                "an IPv4 prefix requires an IPv4 gateway within the prefix",
            ),
            // A same-family gateway must also be in the supplied network.
            (
                "out-of-prefix gateway",
                "192.0.2.0/24",
                Some("192.0.3.1"),
                0,
                None,
                "an IPv4 prefix requires an IPv4 gateway within the prefix",
            ),
            // Ordinary IPv4 network endpoints cannot be advertised as routers.
            (
                "network address gateway",
                "192.0.2.0/24",
                Some("192.0.2.0"),
                0,
                None,
                "an IPv4 gateway must not be the network or broadcast address",
            ),
            // Containment alone also admits the directed broadcast address.
            (
                "broadcast address gateway",
                "192.0.2.0/24",
                Some("192.0.2.255"),
                0,
                None,
                "an IPv4 gateway must not be the network or broadcast address",
            ),
            // IPv6 uses relay selection, not the IPv4 gateway field.
            (
                "gateway on IPv6",
                "2001:db8::/64",
                Some("2001:db8::1"),
                0,
                None,
                "gateway is not accepted for an IPv6 prefix",
            ),
            // Relay selectors must be IPv6, rather than silently ignored input.
            (
                "wrong-family relay",
                "2001:db8::/64",
                None,
                0,
                Some("192.0.2.1"),
                "dhcpv6_link_address must be an IPv6 address",
            ),
            // Relay metadata is stored only on the appended IPv6 prefix.
            (
                "relay on IPv4",
                "192.0.2.0/24",
                Some("192.0.2.1"),
                0,
                Some("2001:db8::1"),
                "dhcpv6_link_address requires an IPv6 prefix",
            ),
        ]
        .map(
            |(scenario, prefix, gateway, reserve_first, relay, diagnostic)| {
                carbide_test_support::Case {
                    scenario,
                    input: rpc::AddNetworkSegmentPrefixRequest {
                        prefix: prefix.to_string(),
                        gateway: gateway.map(str::to_string),
                        reserve_first,
                        dhcpv6_link_address: relay.map(str::to_string),
                        ..Default::default()
                    },
                    expect: carbide_test_support::Outcome::FailsWith((
                        tonic::Code::InvalidArgument,
                        diagnostic.to_string(),
                    )),
                }
            },
        );
        carbide_test_support::check_cases(cases, |request| {
            parse_added_prefix(&request).map(|_| ()).map_err(|error| {
                let error: Status = error.into();
                (error.code(), error.message().to_string())
            })
        });
    }

    /// A /31 uses both endpoint addresses on a point-to-point link, so the
    /// ordinary network/broadcast guard must not reject either gateway.
    #[test]
    fn accepts_point_to_point_gateway_endpoints() {
        let cases = [
            // The low endpoint is usable despite equaling the network address.
            ("low /31 endpoint", "192.0.2.0"),
            // The high endpoint is usable despite equaling broadcast().
            ("high /31 endpoint", "192.0.2.1"),
        ]
        .map(|(scenario, gateway)| carbide_test_support::Case {
            scenario,
            input: gateway,
            expect: carbide_test_support::Outcome::Yields(()),
        });
        carbide_test_support::check_cases(cases, |gateway| {
            parse_added_prefix(&rpc::AddNetworkSegmentPrefixRequest {
                prefix: "192.0.2.0/31".to_string(),
                gateway: Some(gateway.to_string()),
                ..Default::default()
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
        });
    }
}
