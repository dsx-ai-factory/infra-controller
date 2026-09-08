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

//! State Handler implementation for Network Segments

use std::sync::Arc;

use carbide_uuid::network::NetworkSegmentId;
use model::network_prefix::NetworkPrefix;
use model::network_segment::{
    NetworkSegment, NetworkSegmentControllerState, NetworkSegmentDeletionState, NetworkSegmentType,
};
use model::resource_pool::ResourcePool;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::NetworkSegmentStateHandlerContextObjects;

/// The actual Network Segment State handler
#[derive(Debug, Clone)]
pub struct NetworkSegmentStateHandler {
    /// Specifies for how long the number of allocated IPs on network prefixes
    /// need to be zero until the segment is deleted
    drain_period: chrono::Duration,

    pool_vlan_id: Arc<ResourcePool<i16>>,
    pool_vni: Arc<ResourcePool<i32>>,
}

fn available_ip_metric_value(count: Option<u128>) -> usize {
    count
        .map(|count| usize::try_from(count).unwrap_or(usize::MAX))
        .unwrap_or_default()
}

fn compatibility_metric_prefix(prefixes: &[NetworkPrefix]) -> Option<&NetworkPrefix> {
    // These metrics expose one compatibility series per segment and historically
    // describe IPv4. Prefer it explicitly so dual-stack DB row order cannot
    // change the reported prefix or counts.
    prefixes
        .iter()
        .find(|prefix| prefix.prefix.is_ipv4())
        .or_else(|| prefixes.first())
}

impl NetworkSegmentStateHandler {
    pub fn new(
        drain_period: chrono::Duration,
        pool_vlan_id: Arc<ResourcePool<i16>>,
        pool_vni: Arc<ResourcePool<i32>>,
    ) -> Self {
        Self {
            drain_period,
            pool_vlan_id,
            pool_vni,
        }
    }

    fn record_metrics(
        &self,
        state: &mut NetworkSegment,
        ctx: &mut StateHandlerContext<NetworkSegmentStateHandlerContextObjects>,
    ) {
        // If there are no prefixes return.
        // Also, we don't want to put out stats for Tenant segments, as they are not under our control.
        if state.prefixes.is_empty() || state.config.segment_type == NetworkSegmentType::Tenant {
            return;
        }

        let metric_prefix = compatibility_metric_prefix(&state.prefixes)
            .expect("non-empty prefix list was checked above");
        ctx.metrics.available_ips = available_ip_metric_value(metric_prefix.num_free_ips);
        ctx.metrics.reserved_ips = metric_prefix.num_reserved as usize;
        ctx.metrics.seg_name = state.config.name.clone();

        ctx.metrics.seg_type = state.config.segment_type.to_string();
        ctx.metrics.seg_id = state.id.to_string();
        ctx.metrics.prefix = metric_prefix.prefix.to_string();

        let total = metric_prefix.prefix.size();

        let total_cnt: u32 = match total {
            ipnetwork::NetworkSize::V4(nf) => nf,
            ipnetwork::NetworkSize::V6(_n128) => 0,
        };
        ctx.metrics.total_ips = total_cnt as usize;
    }
}

#[async_trait::async_trait]
impl StateHandler for NetworkSegmentStateHandler {
    type ObjectId = NetworkSegmentId;
    type State = NetworkSegment;
    type ControllerState = NetworkSegmentControllerState;
    type ContextObjects = NetworkSegmentStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        segment_id: &NetworkSegmentId,
        state: &mut NetworkSegment,
        controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<NetworkSegmentControllerState>, StateHandlerError> {
        // record metrics irrespective of the state of the network segment
        self.record_metrics(state, ctx);
        match controller_state {
            NetworkSegmentControllerState::Provisioning => {
                let new_state = NetworkSegmentControllerState::Ready;
                tracing::info!(network_segment_id = %segment_id, next_state = ?new_state, "Network Segment state transition");
                Ok(StateHandlerOutcome::transition(new_state))
            }
            NetworkSegmentControllerState::Ready => {
                if state.is_marked_as_deleted() {
                    let delete_at = chrono::Utc::now()
                        .checked_add_signed(self.drain_period)
                        .unwrap_or_else(chrono::Utc::now);
                    let new_state = NetworkSegmentControllerState::Deleting {
                        deletion_state: NetworkSegmentDeletionState::DrainAllocatedIps {
                            delete_at,
                        },
                    };
                    tracing::info!(network_segment_id = %segment_id, next_state = ?new_state, "Network Segment state transition");
                    Ok(StateHandlerOutcome::transition(new_state))
                } else {
                    Ok(StateHandlerOutcome::do_nothing())
                }
            }
            NetworkSegmentControllerState::Deleting { deletion_state } => {
                match deletion_state {
                    NetworkSegmentDeletionState::DrainAllocatedIps { delete_at } => {
                        // Check here whether the IPs are actually freed.
                        // If ones are still allocated, we can not delete and have to
                        // update the `delete_at` timestamp.
                        let mut txn = ctx.services.db_pool.begin().await?;
                        if db::instance_address::segment_has_allocations(&mut txn, &state.id)
                            .await?
                        {
                            let delete_at = chrono::Utc::now()
                                .checked_add_signed(self.drain_period)
                                .unwrap_or_else(chrono::Utc::now);
                            tracing::info!(
                                ?delete_at,
                                network_segment_id = %segment_id,
                                "Segment still has allocated IPs; waiting until the drain deadline to delete",
                            );
                            let new_state = NetworkSegmentControllerState::Deleting {
                                deletion_state: NetworkSegmentDeletionState::DrainAllocatedIps {
                                    delete_at,
                                },
                            };
                            tracing::info!(network_segment_id = %segment_id, next_state = ?new_state, "Network Segment state transition");
                            Ok(StateHandlerOutcome::transition(new_state).with_txn(txn))
                        } else if chrono::Utc::now() >= *delete_at {
                            let new_state = NetworkSegmentControllerState::Deleting {
                                deletion_state: NetworkSegmentDeletionState::DBDelete,
                            };
                            tracing::info!(network_segment_id = %segment_id, next_state = ?new_state, "Network Segment state transition");
                            Ok(StateHandlerOutcome::transition(new_state).with_txn(txn))
                        } else {
                            Ok(StateHandlerOutcome::wait(format!(
                                "Cannot delete from database until draining completes at {}",
                                delete_at.to_rfc3339()
                            ))
                            .with_txn(txn))
                        }
                    }
                    NetworkSegmentDeletionState::DBDelete => {
                        let mut txn = ctx.services.db_pool.begin().await?;
                        if let Some(vni) = state.status.vni.take() {
                            db::resource_pool::release(&self.pool_vni, &mut txn, vni).await?;
                        }
                        if let Some(vlan_id) = state.status.vlan_id.take() {
                            db::resource_pool::release(&self.pool_vlan_id, &mut txn, vlan_id)
                                .await?;
                        }
                        tracing::info!(
                            network_segment_id = %segment_id,
                            "Network Segment getting removed from the database",
                        );
                        db::network_segment::final_delete(*segment_id, &mut txn).await?;
                        Ok(StateHandlerOutcome::deleted().with_txn(txn))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
    use model::network_prefix::NetworkPrefix;

    use super::{available_ip_metric_value, compatibility_metric_prefix};

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

    #[test]
    fn available_ip_metric_preserves_or_saturates_counts() {
        value_scenarios!(run = available_ip_metric_value;
            "omitted count" {
                None => 0,
            }

            "representable count" {
                Some(42) => 42,
            }

            "overflowing count" {
                Some(u128::MAX) => usize::MAX,
            }
        );
    }

    #[test]
    fn compatibility_metric_prefix_prefers_ipv4() {
        value_scenarios!(run = |prefixes: Vec<NetworkPrefix>| {
            compatibility_metric_prefix(&prefixes).map(|prefix| prefix.prefix)
        };
            "dual-stack prefix order" {
                vec![network_prefix("192.0.2.0/24"), network_prefix("2001:db8::/64")]
                    => Some("192.0.2.0/24".parse().unwrap()),
                vec![network_prefix("2001:db8::/64"), network_prefix("192.0.2.0/24")]
                    => Some("192.0.2.0/24".parse().unwrap()),
            }

            "IPv6-only segment" {
                vec![network_prefix("2001:db8::/64")]
                    => Some("2001:db8::/64".parse().unwrap()),
            }
        );
    }
}
