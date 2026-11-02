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

use std::collections::HashSet;

use carbide_uuid::vpc::VpcId;
use ipnetwork::IpNetwork;
use sqlx::PgConnection;

use super::{
    load_policy_hosts, needs_retained_policy_check, overlap_error, prefixes_overlap_across_vpcs,
    receiver_sources, retained_policy_vpcs,
};
use crate::CarbideResult;
use crate::api::Api;

/// `validate_affected_instances` checks the combined address space visible to
/// each DPU Instance using an affected receiver VPC. The caller holds the
/// overlap lock through the proposed write. A prefix not yet stored is supplied
/// as `candidate`; peering and VPC type changes are read from the transaction.
pub(in crate::handlers) async fn validate_affected_instances(
    api: &Api,
    txn: &mut PgConnection,
    receiver_ids: &[VpcId],
    candidate: Option<(VpcId, IpNetwork)>,
) -> CarbideResult<()> {
    for host in load_policy_hosts(txn, receiver_ids).await? {
        if !needs_retained_policy_check(&host) {
            continue;
        }
        // A waiting Instance can start its tenant network without another
        // admission request. Pending updates also retain both configurations
        // until the DPUs acknowledge the replacement.
        let vpcs = retained_policy_vpcs(txn, &host).await?;
        if !vpcs.iter().any(|vpc| receiver_ids.contains(&vpc.id)) {
            continue;
        }
        let mut sources = HashSet::new();
        for vpc in vpcs {
            sources.extend(receiver_sources(&api.runtime_config, txn, &vpc).await?);
        }
        if candidate.is_some_and(|(vpc_id, _)| !sources.contains(&vpc_id)) {
            continue;
        }
        let sources = sources.into_iter().collect::<Vec<_>>();
        let mut prefixes =
            db::vpc_peering::get_retained_prefixes_by_vpcs(&mut *txn, &sources).await?;
        prefixes.extend(candidate);
        if prefixes_overlap_across_vpcs(&prefixes) {
            return Err(overlap_error());
        }
    }
    Ok(())
}
