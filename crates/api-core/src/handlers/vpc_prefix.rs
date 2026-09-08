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

use ::db::{ObjectColumnFilter, vpc_prefix as db};
use ::rpc::forge as rpc;
use ::rpc::forge::PrefixMatchType;
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::vpc::VpcId;
use ipnetwork::IpNetwork;
use model::network_prefix::NetworkPrefix;
use model::network_segment::NetworkSegmentType;
use model::site_prefix::{
    SitePrefix, SitePrefixAuthority, SitePrefixLifecycleState, SitePrefixRoutingScope,
};
use model::vpc::{Vpc, VpcVirtualizationTypeCapabilities};
use model::vpc_prefix;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};
use crate::cfg::file::CarbideConfig;
use crate::{CarbideError, CarbideResult};

fn validate_site_prefix_attachment(
    site_prefix: &SitePrefix,
    vpc: &Vpc,
    vpc_prefix: IpNetwork,
) -> Result<(), CarbideError> {
    if site_prefix.status.authority == SitePrefixAuthority::TenantManaged
        && site_prefix
            .config
            .tenant_organization_id
            .as_ref()
            .map(|id| id.as_str())
            != Some(vpc.config.tenant_organization_id.as_str())
    {
        return Err(CarbideError::PermissionDeniedError(format!(
            "SitePrefix {} is not owned by the VPC tenant",
            site_prefix.id
        )));
    }

    if site_prefix.status.lifecycle_state != SitePrefixLifecycleState::Ready {
        return Err(CarbideError::FailedPrecondition(format!(
            "SitePrefix {} is not ready for new VPC prefixes",
            site_prefix.id
        )));
    }

    if site_prefix.config.routing_scope != SitePrefixRoutingScope::DatacenterOnly {
        return Err(CarbideError::FailedPrecondition(format!(
            "SitePrefix {} has an unsupported routing scope",
            site_prefix.id
        )));
    }

    if !super::tenant_prefix_overlap::contains_prefix(site_prefix.config.prefix, vpc_prefix) {
        return Err(CarbideError::InvalidArgument(format!(
            "the VPC prefix {vpc_prefix} is not contained within SitePrefix {} ({})",
            site_prefix.id, site_prefix.config.prefix
        )));
    }

    if site_prefix.status.authority == SitePrefixAuthority::TenantManaged
        && vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn
    {
        return Err(CarbideError::FailedPrecondition(
            "tenant-managed SitePrefixes can only be used by FNN VPCs".to_string(),
        ));
    }

    Ok(())
}

/// `validate_vpc_prefix_overlaps` rejects `candidate` unless every existing
/// overlap passes `pair_is_eligible`.
///
/// The caller acquires the overlap lock before reading the candidate `Vpc` and
/// any selected `SitePrefix`, so a waiting create sees every competing prefix
/// that committed first.
async fn validate_vpc_prefix_overlaps(
    runtime_config: &CarbideConfig,
    txn: &mut PgConnection,
    candidate: &vpc_prefix::NewVpcPrefix,
    candidate_vpc: &Vpc,
    candidate_site_prefix: Option<&SitePrefix>,
    overlaps: &[vpc_prefix::VpcPrefix],
) -> CarbideResult<()> {
    if overlaps.is_empty() {
        return Ok(());
    }

    let Some(candidate_site_prefix) = candidate_site_prefix else {
        return Err(super::tenant_prefix_overlap::overlap_error());
    };
    let vpc_ids = overlaps
        .iter()
        .map(|prefix| prefix.vpc_id)
        .collect::<Vec<_>>();
    let vpcs = ::db::vpc::find_by(
        &mut *txn,
        ObjectColumnFilter::List(::db::vpc::IdColumn, &vpc_ids),
    )
    .await?
    .into_iter()
    .map(|vpc| (vpc.id, vpc))
    .collect::<HashMap<_, _>>();
    let Some(site_prefix_ids) = overlaps
        .iter()
        .map(|prefix| prefix.site_prefix_id)
        .collect::<Option<Vec<_>>>()
    else {
        return Err(super::tenant_prefix_overlap::overlap_error());
    };
    let site_prefixes = ::db::site_prefix::find_by_ids(&mut *txn, &site_prefix_ids)
        .await?
        .into_iter()
        .map(|site_prefix| (site_prefix.id, site_prefix))
        .collect::<HashMap<_, _>>();

    for existing in overlaps {
        let Some(existing_vpc) = vpcs.get(&existing.vpc_id) else {
            return Err(super::tenant_prefix_overlap::overlap_error());
        };
        let Some(existing_site_prefix) = existing
            .site_prefix_id
            .and_then(|id| site_prefixes.get(&id))
        else {
            return Err(super::tenant_prefix_overlap::overlap_error());
        };
        if !super::tenant_prefix_overlap::pair_is_eligible(
            runtime_config,
            super::tenant_prefix_overlap::VpcPrefixParticipant {
                prefix: candidate.config.prefix,
                is_deleted: false,
                vpc: candidate_vpc,
                site_prefix: candidate_site_prefix,
            },
            super::tenant_prefix_overlap::VpcPrefixParticipant {
                prefix: existing.config.prefix,
                is_deleted: existing.deleted.is_some(),
                vpc: existing_vpc,
                site_prefix: existing_site_prefix,
            },
        ) {
            return Err(super::tenant_prefix_overlap::overlap_error());
        }
    }

    Ok(())
}

/// `adoptable_segment_prefixes` returns direct Tenant prefixes the `VpcPrefix`
/// may adopt.
///
/// A direct prefix on another VPC or a non-Tenant segment cannot be linked to
/// this `VpcPrefix`, so the request is rejected without identifying its owner.
fn adoptable_segment_prefixes(
    overlaps: Vec<db::AttachedSegmentPrefix>,
    candidate_vpc_id: VpcId,
) -> CarbideResult<Vec<NetworkPrefix>> {
    let mut adoptable = Vec::with_capacity(overlaps.len());
    for overlap in overlaps {
        if overlap.vpc_id != candidate_vpc_id || overlap.segment_type != NetworkSegmentType::Tenant
        {
            return Err(super::tenant_prefix_overlap::overlap_error());
        }
        adoptable.push(overlap.prefix);
    }
    Ok(adoptable)
}

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::VpcPrefixCreationRequest>,
) -> Result<Response<rpc::VpcPrefix>, Status> {
    log_request_data(&request);

    let mut new_prefix = vpc_prefix::NewVpcPrefix::try_from(request.into_inner())?;

    // Validate that the new VPC prefix is in canonical form (no bits set to
    // 1 after the prefix).
    let canonical_address = new_prefix.config.prefix.network();
    let prefix_address = new_prefix.config.prefix.ip();
    if canonical_address != prefix_address {
        let prefix_len = new_prefix.config.prefix.prefix();
        let msg = format!(
            "IP prefixes must be in canonical format. This prefix should be \
            specified as {canonical_address}/{prefix_len} and not \
            {prefix_address}/{prefix_len}."
        );
        return Err(CarbideError::InvalidArgument(msg).into());
    }

    let mut txn = api.txn_begin().await?;
    ::db::tenant_prefix_overlap::lock_checks(txn.as_mut()).await?;

    // Resolve and lock the exact SitePrefix before locking the VPC. The shared
    // row lock permits concurrent child creation but conflicts with retirement
    // and configured-prefix reconciliation, which use FOR UPDATE.
    let selected_site_prefix = if let Some(site_prefix_id) = new_prefix.site_prefix_id {
        Some(
            ::db::site_prefix::find_by_id_for_vpc_prefix_attachment(&mut txn, site_prefix_id)
                .await?
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "site prefix",
                    id: site_prefix_id.to_string(),
                })?,
        )
    } else {
        // Reconciliation uses the corresponding exclusive namespace lock.
        // Take the shared form before reading so a concurrent first operator
        // root cannot be missed when there is not yet a row to lock.
        ::db::site_prefix::lock_operator_managed_site_prefix_attachments(&mut txn).await?;
        let mut candidates =
            ::db::site_prefix::find_legacy_operator_managed_for_vpc_prefix_attachment(
                &mut txn,
                new_prefix.config.prefix,
            )
            .await?;
        match candidates.len() {
            0 => {
                let vpc = ::db::vpc::find_by(
                    &mut txn,
                    ObjectColumnFilter::One(::db::vpc::IdColumn, &new_prefix.vpc_id),
                )
                .await?
                .pop()
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "vpc",
                    id: new_prefix.vpc_id.to_string(),
                })?;
                ::db::site_prefix::lock_tenant_site_prefix_attachments(
                    &vpc.config.tenant_organization_id,
                    &mut txn,
                )
                .await?;
                let tenant_managed =
                    ::db::site_prefix::find_containing_tenant_managed_for_vpc_prefix_attachment(
                        &mut txn,
                        new_prefix.config.prefix,
                        &vpc.config.tenant_organization_id,
                    )
                    .await?;
                if !tenant_managed.is_empty() {
                    return Err(CarbideError::FailedPrecondition(format!(
                        "the VPC prefix {} is contained by a tenant-managed SitePrefix; specify site_prefix_id explicitly",
                        new_prefix.config.prefix
                    ))
                    .into());
                }
                None
            }
            // Preserve the legacy configured-root behavior. A caller omitting
            // the ID attaches to its unique operator root even if the tenant
            // also owns a containing root; tenant-managed roots are explicit.
            1 => candidates.pop(),
            count => {
                return Err(CarbideError::FailedPrecondition(format!(
                    "the VPC prefix {} has {count} containing operator-managed SitePrefixes; specify site_prefix_id explicitly",
                    new_prefix.config.prefix
                ))
                .into());
            }
        }
    };

    let vpcs = ::db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(::db::vpc::IdColumn, &new_prefix.vpc_id),
        ::db::vpc::VpcRowLock::Mutation,
    )
    .await?;
    let vpc = vpcs.first().ok_or_else(|| CarbideError::NotFoundError {
        kind: "vpc",
        id: new_prefix.vpc_id.to_string(),
    })?;

    if let Some(ref site_prefix) = selected_site_prefix {
        validate_site_prefix_attachment(site_prefix, vpc, new_prefix.config.prefix)?;
        new_prefix.site_prefix_id = Some(site_prefix.id);
    } else if let Some(ref site_prefixes) = api.eth_data.site_fabric_prefixes {
        // Preserve the mixed-version configured-root path. Production startup
        // reconciles configured roots into SitePrefix rows; this fallback keeps
        // feature-off sites and older writers behaviorally compatible while
        // the nullable expand phase is deployed.
        let prefix = new_prefix.config.prefix;
        if !site_prefixes.contains(prefix) {
            return Err(CarbideError::InvalidArgument(format!(
                "the VPC prefix {prefix} is not contained within the site fabric prefixes"
            ))
            .into());
        }
    }

    if new_prefix.config.prefix.is_ipv6() {
        vpc.config
            .network_virtualization_type
            .ensure_supports_ipv6_prefix()
            .map_err(CarbideError::from)?;
    }
    let expected_vpc_version = vpc.version;

    let conflicting_vpc_prefixes = db::probe(new_prefix.config.prefix, &mut txn).await?;
    validate_vpc_prefix_overlaps(
        &api.runtime_config,
        &mut txn,
        &new_prefix,
        vpc,
        selected_site_prefix.as_ref(),
        &conflicting_vpc_prefixes,
    )
    .await?;

    let segment_prefixes = db::probe_segment_prefixes(new_prefix.config.prefix, &mut txn).await?;
    let segment_prefixes = adoptable_segment_prefixes(segment_prefixes, new_prefix.vpc_id)?;

    // Check that the network segment prefixes we found can actually fit into
    // this new VPC prefix container.
    if let Some(larger_segment_prefix) = segment_prefixes.iter().find(|segment_prefix| {
        let segment_prefix_len = segment_prefix.prefix.prefix();
        let vpc_prefix_len = new_prefix.config.prefix.prefix();
        segment_prefix_len < vpc_prefix_len
    }) {
        let msg = format!(
            "The requested VPC prefix ({vpc_prefix}) is too small to contain \
            an existing network segment prefix ({larger_segment_prefix})",
            vpc_prefix = new_prefix.config.prefix,
            larger_segment_prefix = larger_segment_prefix.prefix,
        );
        return Err(CarbideError::InvalidArgument(msg).into());
    }

    new_prefix
        .metadata
        .validate(true)
        .map_err(CarbideError::from)?;

    let vpc_prefix = db::persist(new_prefix, expected_vpc_version, &mut txn).await?;
    let vpc_prefix_id = vpc_prefix.id;
    let vpc_prefix_network = vpc_prefix.config.prefix;

    // Associate all of the network segment prefixes with the new VPC prefix.
    for mut segment_prefix in segment_prefixes {
        ::db::network_prefix::set_vpc_prefix(
            &mut segment_prefix,
            &mut txn,
            &vpc_prefix_id,
            &vpc_prefix_network,
        )
        .await?;
    }

    // Reload through the normal read path so create responses include computed utilization stats.
    let vpc_prefix = db::get_by_id(
        &mut txn,
        ObjectColumnFilter::One(db::IdColumn, &vpc_prefix_id),
        model::DeletedFilter::Exclude,
    )
    .await?
    .pop()
    .ok_or_else(|| {
        CarbideError::internal(format!("created VPC prefix {vpc_prefix_id} was not found"))
    })?;

    txn.commit().await?;

    Ok(tonic::Response::new(vpc_prefix.into()))
}

pub(crate) async fn search(
    api: &Api,
    request: Request<rpc::VpcPrefixSearchQuery>,
) -> Result<Response<rpc::VpcPrefixIdList>, Status> {
    log_request_data(&request);
    let rpc::VpcPrefixSearchQuery {
        vpc_id,
        tenant_prefix_id,
        name,
        prefix_match,
        prefix_match_type,
        deleted,
        site_prefix_id,
    } = request.into_inner();

    // We don't have tenant prefixes in this version, so searching against them
    // isn't allowed.
    tenant_prefix_id
        .map(|_| -> Result<(), CarbideError> {
            Err(CarbideError::InvalidArgument(
                "searching on tenant_prefix_id is currently unsupported".to_owned(),
            ))
        })
        .transpose()?;

    // If prefix_match was specified, we'll combine it with prefix_match_type to
    // determine the match semantics.
    let prefix_match = prefix_match
        .map(|prefix| -> Result<_, CarbideError> {
            let prefix =
                IpNetwork::try_from(prefix.as_str()).map_err(CarbideError::NetworkParseError)?;
            let prefix_match_type = prefix_match_type
                .ok_or_else(|| CarbideError::MissingArgument("prefix_match_type"))?;
            let prefix_match_type = PrefixMatchType::try_from(prefix_match_type).map_err(|_e| {
                CarbideError::InvalidArgument(format!(
                    "unknown PrefixMatchType value: {prefix_match_type}"
                ))
            })?;
            use model::vpc_prefix::PrefixMatch;
            let prefix_match = match prefix_match_type {
                PrefixMatchType::PrefixExact => PrefixMatch::Exact(prefix),
                PrefixMatchType::PrefixContains => PrefixMatch::Contains(prefix),
                PrefixMatchType::PrefixContainedBy => PrefixMatch::ContainedBy(prefix),
            };
            Ok(prefix_match)
        })
        .transpose()?;

    let mut txn = api.txn_begin().await?;

    let vpc_prefix_ids = db::search(
        &mut txn,
        vpc_prefix::VpcPrefixSearch {
            vpc_id,
            site_prefix_id,
            name,
            prefix_match,
            deleted_filter: model::DeletedFilter::from(deleted),
        },
    )
    .await?;

    txn.commit().await?;

    Ok(tonic::Response::new(rpc::VpcPrefixIdList {
        vpc_prefix_ids,
    }))
}

pub(crate) async fn get(
    api: &Api,
    request: Request<rpc::VpcPrefixGetRequest>,
) -> Result<Response<rpc::VpcPrefixList>, Status> {
    log_request_data(&request);

    let rpc::VpcPrefixGetRequest {
        vpc_prefix_ids,
        deleted,
    } = request.into_inner();
    if vpc_prefix_ids.len() > (api.runtime_config.max_find_by_ids as usize) {
        let msg = format!(
            "Too many VPC prefix IDs were specified (the limit is {maximum})",
            maximum = api.runtime_config.max_find_by_ids,
        );
        return Err(CarbideError::InvalidArgument(msg).into());
    }

    let mut txn = api.txn_begin().await?;

    let vpc_prefixes = db::get_by_id(
        &mut txn,
        ObjectColumnFilter::List(db::IdColumn, vpc_prefix_ids.as_slice()),
        model::DeletedFilter::from(deleted),
    )
    .await?;

    txn.commit().await?;

    let vpc_prefixes: Vec<_> = vpc_prefixes.into_iter().map(rpc::VpcPrefix::from).collect();
    Ok(tonic::Response::new(rpc::VpcPrefixList { vpc_prefixes }))
}

/// Finds controller state-history records for VPC prefixes.
pub(crate) async fn find_state_histories(
    api: &Api,
    request: Request<rpc::VpcPrefixStateHistoriesRequest>,
) -> Result<Response<rpc::StateHistories>, Status> {
    log_request_data(&request);

    // Extract and validate the requested VPC prefix IDs before querying.
    let vpc_prefix_ids = request.into_inner().vpc_prefix_ids;
    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if vpc_prefix_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if vpc_prefix_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    // Fetch state-history rows through the generic state-history DB API.
    let mut txn = api.txn_begin().await?;
    let results = ::db::state_history::find_by_object_ids(
        &mut txn,
        ::db::state_history::StateHistoryTableId::VpcPrefix,
        &vpc_prefix_ids,
    )
    .await?;

    // Re-key the DB records into the generic RPC response shape.
    let mut response = rpc::StateHistories::default();
    for (vpc_prefix_id, records) in results {
        response.histories.insert(
            vpc_prefix_id,
            rpc::StateHistoryRecords {
                records: records.into_iter().map(Into::into).collect(),
            },
        );
    }

    txn.commit().await?;
    Ok(tonic::Response::new(response))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::VpcPrefixUpdateRequest>,
) -> Result<Response<rpc::VpcPrefix>, Status> {
    log_request_data(&request);

    let update_prefix = vpc_prefix::UpdateVpcPrefix::try_from(request.into_inner())?;

    let mut txn = api.txn_begin().await?;

    update_prefix
        .metadata
        .validate(true)
        .map_err(CarbideError::from)?;

    let updated = db::update(&update_prefix, &mut txn).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(updated.into()))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::VpcPrefixDeletionRequest>,
) -> Result<Response<rpc::VpcPrefixDeletionResult>, Status> {
    log_request_data(&request);

    let delete_prefix = vpc_prefix::DeleteVpcPrefix::try_from(request.into_inner())?;

    let mut txn = api.txn_begin().await?;

    // Load the active prefix so repeat deletes preserve current NotFound
    // behavior unless the DB layer deliberately makes soft-delete idempotent.
    let vpc_prefixes = db::get_by_id(
        &mut txn,
        ObjectColumnFilter::One(db::IdColumn, &delete_prefix.id),
        model::DeletedFilter::Exclude,
    )
    .await?;
    let vpc_prefix = vpc_prefixes
        .first()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "vpc_prefix",
            id: delete_prefix.id.to_string(),
        })?;

    let vpcs = ::db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(::db::vpc::IdColumn, &vpc_prefix.vpc_id),
        ::db::vpc::VpcRowLock::Mutation,
    )
    .await?;
    let vpc = vpcs.first().ok_or_else(|| CarbideError::NotFoundError {
        kind: "vpc",
        id: vpc_prefix.vpc_id.to_string(),
    })?;

    // Mark the prefix deleted and let the lifecycle controller wait for any
    // network-prefix references to drain before hard-deleting the row.
    db::mark_as_deleted(&delete_prefix, vpc.version, &mut txn).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(rpc::VpcPrefixDeletionResult {}))
}
