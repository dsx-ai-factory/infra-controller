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

use carbide_uuid::vpc::VpcId;
use db::ObjectColumnFilter;
use model::machine::LoadSnapshotOptions;
use sqlx::PgConnection;

use super::{
    VpcPrefixParticipant, overlap_error, prefixes_overlap_across_vpcs, receiver_sources,
    retained_pair_is_isolated, validate_retained_host,
};
use crate::CarbideResult;
use crate::api::Api;

/// `validate_retained_state` checks saved routing before controllers or API
/// listeners can serve it, including replicas that skip startup seeding.
/// Its shared lock excludes routing writers without blocking unrelated DPU reads.
pub(crate) async fn validate_retained_state(api: &Api) -> CarbideResult<()> {
    let mut txn = api.txn_begin().await?;
    db::tenant_prefix_overlap::lock_config(txn.as_mut()).await?;
    validate_retained_state_in_transaction(api, &mut txn).await?;
    txn.commit().await?;
    Ok(())
}

/// `validate_retained_state_in_transaction` also checks startup writes before
/// they commit. Callers hold the overlap lock before reading dependencies:
/// shared for a standalone audit, exclusive when the transaction also writes.
pub(crate) async fn validate_retained_state_in_transaction(
    api: &Api,
    txn: &mut PgConnection,
) -> CarbideResult<()> {
    let duplicate_vpc_ids = db::tenant_prefix_overlap::find_duplicate_vpc_ids(
        &mut *txn,
        api.runtime_config.tenant_prefix_overlap_enabled,
    )
    .await?;
    if !api.runtime_config.tenant_prefix_overlap_enabled && duplicate_vpc_ids.is_empty() {
        return Ok(());
    }
    validate_retained_prefixes(api, txn, &duplicate_vpc_ids)
        .await
        .inspect_err(|error| {
            tracing::error!(vpc_ids = ?duplicate_vpc_ids, %error,
                "Retained prefix validation failed at startup");
        })?;

    // Only receivers importing duplicate address space can have a collision.
    // Peers do not re-export imports, so one direct-neighbor lookup is enough.
    let mut receivers = duplicate_vpc_ids.iter().copied().collect::<HashSet<_>>();
    for vpc_id in duplicate_vpc_ids {
        receivers.extend(db::vpc_peering::get_vpc_peer_ids(txn, vpc_id).await?);
    }
    let receiver_ids = receivers.into_iter().collect::<Vec<_>>();
    for receiver in db::vpc::find_by(
        &mut *txn,
        ObjectColumnFilter::List(db::vpc::IdColumn, &receiver_ids),
    )
    .await?
    {
        let sources = receiver_sources(&api.runtime_config, txn, &receiver).await?;
        let prefixes = db::vpc_peering::get_retained_prefixes_by_vpcs(&mut *txn, &sources).await?;
        if prefixes_overlap_across_vpcs(&prefixes) {
            tracing::error!(vpc_id = %receiver.id, source_vpc_ids = ?sources,
                "Retained VPC imports overlap at startup");
            return Err(overlap_error());
        }
    }

    let instance_ids = db::instance::find_ids(&mut *txn, Default::default()).await?;
    for host in
        db::managed_host::load_by_instance_ids(txn, &instance_ids, LoadSnapshotOptions::default())
            .await?
    {
        validate_retained_host(api, txn, &host)
            .await
            .inspect_err(|error| {
                tracing::error!(machine_id = %host.host_snapshot.id,
                    instance_id = ?host.instance.as_ref().map(|instance| instance.id), %error,
                    "Retained Instance routing validation failed at startup");
            })?;
    }
    Ok(())
}

/// `validate_retained_prefixes` checks only overlaps involving the supplied
/// source VPCs. Config serving uses this without scanning unrelated Instances.
pub(super) async fn validate_retained_prefixes(
    api: &Api,
    txn: &mut PgConnection,
    vpc_ids: &[VpcId],
) -> CarbideResult<()> {
    if vpc_ids.is_empty() {
        return Ok(());
    }
    if db::tenant_prefix_overlap::has_direct_prefix_overlap(&mut *txn, vpc_ids).await? {
        return Err(overlap_error());
    }
    let pairs =
        db::tenant_prefix_overlap::find_overlapping_vpc_prefix_pairs(&mut *txn, vpc_ids).await?;
    if pairs.is_empty() {
        return Ok(());
    }
    let prefix_ids = pairs
        .iter()
        .flat_map(|(first, second)| [*first, *second])
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let prefixes = db::vpc_prefix::get_for_allocation_by_ids(txn, &prefix_ids)
        .await?
        .into_iter()
        .map(|prefix| (prefix.id, prefix))
        .collect::<HashMap<_, _>>();
    let mut vpc_ids = prefixes
        .values()
        .map(|prefix| prefix.vpc_id)
        .collect::<Vec<_>>();
    vpc_ids.sort_unstable();
    vpc_ids.dedup();
    let mut vpcs = HashMap::new();
    // Configuration readers share the advisory lock. Lock VPCs in one order
    // before taking any VNI allocation locks so overlapping reads cannot deadlock.
    for id in vpc_ids {
        let vpc = db::vpc::find_by_with_lock(
            txn,
            ObjectColumnFilter::One(db::vpc::IdColumn, &id),
            db::vpc::VpcRowLock::Mutation,
        )
        .await?
        .pop()
        .ok_or_else(overlap_error)?;
        vpcs.insert(id, vpc);
    }
    let site_prefix_ids = prefixes
        .values()
        .filter_map(|prefix| prefix.site_prefix_id)
        .collect::<Vec<_>>();
    let site_prefixes = db::site_prefix::find_by_ids(&mut *txn, &site_prefix_ids)
        .await?
        .into_iter()
        .map(|prefix| (prefix.id, prefix))
        .collect::<HashMap<_, _>>();
    let participant = |id| {
        let prefix = prefixes.get(&id).ok_or_else(overlap_error)?;
        Ok::<_, crate::CarbideError>(VpcPrefixParticipant {
            prefix: prefix.config.prefix,
            is_deleted: prefix.deleted.is_some(),
            vpc: vpcs.get(&prefix.vpc_id).ok_or_else(overlap_error)?,
            site_prefix: prefix
                .site_prefix_id
                .and_then(|id| site_prefixes.get(&id))
                .ok_or_else(overlap_error)?,
        })
    };
    for (first, second) in pairs {
        if !retained_pair_is_isolated(
            &api.runtime_config,
            participant(first)?,
            participant(second)?,
        ) {
            return Err(overlap_error());
        }
    }
    for vpc in vpcs.values() {
        crate::handlers::vpc_prefix::validate_overlap_vni(api, txn, vpc).await?;
    }

    Ok(())
}
