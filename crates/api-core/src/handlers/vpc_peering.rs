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

use ::db::{ObjectColumnFilter, vpc, vpc_peering as db};
use ::rpc::forge as rpc;
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::vpc::VpcId;
use carbide_uuid::vpc_peering::VpcPeeringId;
use ipnetwork::IpNetwork;
use model::vpc::{ALL_VPC_VIRTUALIZATION_TYPES, VpcVirtualizationTypeCapabilities};
use sqlx::PgConnection;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use super::tenant_prefix_overlap::{prefixes_overlap_across_vpcs, receiver_sources};
use crate::api::{Api, log_request_data};
use crate::cfg::file::VpcPeeringPolicy;
use crate::{CarbideError, CarbideResult};

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::VpcPeeringCreationRequest>,
) -> Result<Response<rpc::VpcPeering>, Status> {
    log_request_data(&request);

    let rpc::VpcPeeringCreationRequest {
        vpc_id,
        peer_vpc_id,
        id,
    } = request.into_inner();

    let id = match id {
        None => VpcPeeringId::from(Uuid::new_v4()),
        Some(id) => id,
    };

    let vpc_id = vpc_id.ok_or_else(|| CarbideError::MissingArgument("vpc_id cannot be null"))?;

    let peer_vpc_id =
        peer_vpc_id.ok_or_else(|| CarbideError::MissingArgument("peer_vpc_id cannot be null"))?;

    let mut txn = api.txn_begin().await?;
    if api.runtime_config.tenant_prefix_overlap_enabled {
        ::db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    }

    // Check this VPC peering is permitted under current site vpc_peering_policy
    match api.runtime_config.vpc_peering_policy {
        None | Some(VpcPeeringPolicy::None) => {
            return Err(CarbideError::internal("VPC peering feature disabled".to_string()).into());
        }
        Some(VpcPeeringPolicy::Exclusive) => {
            let vpcs1 =
                vpc::find_by(&mut txn, ObjectColumnFilter::One(vpc::IdColumn, &vpc_id)).await?;
            let vpc1 = vpcs1.first().ok_or_else(|| CarbideError::NotFoundError {
                kind: "VPC",
                id: vpc_id.to_string(),
            })?;
            let vpcs2 = vpc::find_by(
                &mut txn,
                ObjectColumnFilter::One(vpc::IdColumn, &peer_vpc_id),
            )
            .await?;
            let vpc2 = vpcs2.first().ok_or_else(|| CarbideError::NotFoundError {
                kind: "VPC",
                id: peer_vpc_id.to_string(),
            })?;

            // Make sure the VPCs are allowed to peer based on their
            // virtualization types. Their capabilities will determine
            // if they are allowed or not.
            vpc1.config
                .network_virtualization_type
                .ensure_can_peer_with(vpc2.config.network_virtualization_type)
                .map_err(CarbideError::from)?;
        }
        Some(VpcPeeringPolicy::Mixed) => {
            // Any combination of network virtualization types allowed
        }
    }

    let vpc_peering = db::create(&mut txn, vpc_id, peer_vpc_id, id).await?;

    if api.runtime_config.tenant_prefix_overlap_enabled {
        // Only these two receivers gain a source. Peers do not re-export
        // imported prefixes, but each endpoint may already import siblings.
        validate_receiver_prefixes(api, &mut txn, &[vpc_id, peer_vpc_id], None).await?;
    }

    txn.commit().await?;

    Ok(tonic::Response::new(vpc_peering.into()))
}

/// Checks a new prefix against the VPCs that would receive it. The caller
/// holds the overlap lock until the prefix is committed or rolled back.
pub(super) async fn validate_prefix_attachment(
    api: &Api,
    txn: &mut PgConnection,
    vpc_id: VpcId,
    prefix: IpNetwork,
) -> CarbideResult<()> {
    if !api.runtime_config.tenant_prefix_overlap_enabled {
        return Ok(());
    }
    let mut receivers = db::get_vpc_peer_ids(txn, vpc_id).await?;
    if receivers.is_empty() {
        return Ok(());
    }
    receivers.push(vpc_id);
    validate_receiver_prefixes(api, txn, &receivers, Some((vpc_id, prefix))).await
}

/// A VPC type change can alter its own imports and what direct peers import
/// from it. The caller holds the overlap lock and has written the new type in
/// the same transaction, so these reads see the proposed routing behavior.
pub(super) async fn validate_vpc_type_change(
    api: &Api,
    txn: &mut PgConnection,
    vpc_id: VpcId,
) -> CarbideResult<()> {
    let mut receivers = db::get_vpc_peer_ids(txn, vpc_id).await?;
    receivers.push(vpc_id);
    validate_receiver_prefixes(api, txn, &receivers, None).await
}

/// Only newly visible source VPCs can introduce a prefix collision. Gaining
/// VNI imports alone is harmless if the receiver already imports those CIDRs.
pub(super) fn vpc_type_change_expands_receivers(
    api: &Api,
    old_type: VpcVirtualizationType,
    new_type: VpcVirtualizationType,
) -> bool {
    let Some(policy) = api
        .runtime_config
        .vpc_peering_policy_on_existing
        .or(api.runtime_config.vpc_peering_policy)
    else {
        return false;
    };
    let imports = |receiver: VpcVirtualizationType, source: VpcVirtualizationType| {
        let imports_prefixes = match policy {
            VpcPeeringPolicy::Mixed => true,
            VpcPeeringPolicy::Exclusive => receiver.capabilities().peers_with.contains(&source),
            VpcPeeringPolicy::None => false,
        };
        imports_prefixes
            || (receiver.imports_peer_vnis_into_overlay() && source.vni_advertised_to_peers())
    };
    ALL_VPC_VIRTUALIZATION_TYPES.iter().copied().any(|peer| {
        (imports(new_type, peer) && !imports(old_type, peer))
            || (imports(peer, new_type) && !imports(peer, old_type))
    })
}

async fn validate_receiver_prefixes(
    api: &Api,
    txn: &mut PgConnection,
    receiver_ids: &[VpcId],
    candidate: Option<(VpcId, IpNetwork)>,
) -> CarbideResult<()> {
    if api
        .runtime_config
        .vpc_peering_policy_on_existing
        .or(api.runtime_config.vpc_peering_policy)
        .is_none()
    {
        return Ok(());
    }
    for receiver_id in receiver_ids {
        let receiver = vpc::find_by(
            &mut *txn,
            ObjectColumnFilter::One(vpc::IdColumn, receiver_id),
        )
        .await?
        .pop()
        .ok_or_else(super::tenant_prefix_overlap::overlap_error)?;
        let sources = receiver_sources(&api.runtime_config, txn, &receiver).await?;
        let mut prefixes = db::get_retained_prefixes_by_vpcs(&mut *txn, &sources).await?;
        if let Some((vpc_id, prefix)) = candidate
            && sources.contains(&vpc_id)
        {
            prefixes.push((vpc_id, prefix));
        }
        if prefixes_overlap_across_vpcs(&prefixes) {
            return Err(super::tenant_prefix_overlap::overlap_error());
        }
    }
    Ok(())
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::VpcPeeringSearchFilter>,
) -> Result<Response<rpc::VpcPeeringIdList>, Status> {
    log_request_data(&request);

    let rpc::VpcPeeringSearchFilter { vpc_id } = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let vpc_peering_ids = db::find_ids(&mut txn, vpc_id).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(rpc::VpcPeeringIdList {
        vpc_peering_ids,
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::VpcPeeringsByIdsRequest>,
) -> Result<Response<rpc::VpcPeeringList>, Status> {
    log_request_data(&request);

    let rpc::VpcPeeringsByIdsRequest { vpc_peering_ids } = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let vpc_peerings = db::find_by_ids(&mut txn, vpc_peering_ids).await?;

    txn.commit().await?;

    let vpc_peerings = vpc_peerings.into_iter().map(Into::into).collect();

    Ok(tonic::Response::new(rpc::VpcPeeringList { vpc_peerings }))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::VpcPeeringDeletionRequest>,
) -> Result<Response<rpc::VpcPeeringDeletionResult>, Status> {
    log_request_data(&request);

    let rpc::VpcPeeringDeletionRequest { id } = request.into_inner();

    let id = id.ok_or_else(|| CarbideError::MissingArgument("id cannot be null"))?;

    let mut txn = api.txn_begin().await?;

    let _ = db::delete(&mut txn, id).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(rpc::VpcPeeringDeletionResult {}))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn receiver_prefixes_must_not_overlap_across_source_vpcs() {
        let source = VpcId::new();
        let other = VpcId::new();
        check_values(
            [
                Check {
                    scenario: "identical prefixes from different VPCs",
                    input: vec![(source, "10.0.0.0/24"), (other, "10.0.0.0/24")],
                    expect: true,
                },
                Check {
                    scenario: "a peer prefix contains another source's prefix",
                    input: vec![(source, "2001:db8::/48"), (other, "2001:db8:0:1::/64")],
                    expect: true,
                },
                Check {
                    scenario: "containers and children within one VPC",
                    input: vec![(source, "10.0.0.0/16"), (source, "10.0.1.0/24")],
                    expect: false,
                },
                Check {
                    scenario: "disjoint prefixes and different address families",
                    input: vec![
                        (source, "10.0.0.0/24"),
                        (other, "10.0.1.0/24"),
                        (other, "2001:db8::/64"),
                    ],
                    expect: false,
                },
            ],
            |prefixes| {
                prefixes_overlap_across_vpcs(
                    &prefixes
                        .into_iter()
                        .map(|(vpc_id, prefix)| (vpc_id, prefix.parse().unwrap()))
                        .collect::<Vec<_>>(),
                )
            },
        );
    }
}
