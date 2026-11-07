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

//! State handler implementation for VPC prefixes.

use carbide_uuid::vpc::VpcPrefixId;
use model::vpc_prefix::{VpcPrefix, VpcPrefixControllerState, VpcPrefixDeletionState};
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::VpcPrefixStateHandlerContextObjects;

/// The VPC prefix state handler.
#[derive(Debug, Clone)]
pub struct VpcPrefixStateHandler {
    drain_period: chrono::Duration,
}

impl VpcPrefixStateHandler {
    /// Creates a VPC prefix state handler with the configured drain period.
    pub fn new(drain_period: chrono::Duration) -> Self {
        // Store the drain period used to re-arm pending prefix deletion.
        Self { drain_period }
    }

    fn drain_state(&self) -> VpcPrefixControllerState {
        let delete_at = chrono::Utc::now()
            .checked_add_signed(self.drain_period)
            .unwrap_or_else(chrono::Utc::now);
        VpcPrefixControllerState::Deleting {
            deletion_state: VpcPrefixDeletionState::DrainNetworkPrefixes { delete_at },
        }
    }
}

#[async_trait::async_trait]
impl StateHandler for VpcPrefixStateHandler {
    type ObjectId = VpcPrefixId;
    type State = VpcPrefix;
    type ControllerState = VpcPrefixControllerState;
    type ContextObjects = VpcPrefixStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        vpc_prefix_id: &VpcPrefixId,
        state: &mut VpcPrefix,
        controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<VpcPrefixControllerState>, StateHandlerError> {
        match controller_state {
            VpcPrefixControllerState::Provisioning => {
                let new_state = if state.is_marked_as_deleted() {
                    // Prefixes deleted before their first controller pass should not record a
                    // spurious Ready transition before entering the deletion lifecycle.
                    self.drain_state()
                } else {
                    // New prefixes are immediately considered ready once persisted.
                    VpcPrefixControllerState::Ready
                };
                tracing::info!(%vpc_prefix_id, next_state = ?new_state, "VPC Prefix state transition");
                Ok(StateHandlerOutcome::transition(new_state))
            }
            VpcPrefixControllerState::Ready => {
                if state.is_marked_as_deleted() {
                    // Start the drain window after the API marks the prefix deleted.
                    let new_state = self.drain_state();
                    tracing::info!(%vpc_prefix_id, next_state = ?new_state, "VPC Prefix state transition");
                    Ok(StateHandlerOutcome::transition(new_state))
                } else {
                    // Ready prefixes have no periodic work until deletion is requested.
                    Ok(StateHandlerOutcome::do_nothing())
                }
            }
            VpcPrefixControllerState::Deleting { deletion_state } => match deletion_state {
                VpcPrefixDeletionState::DrainNetworkPrefixes { delete_at } => {
                    let mut txn = ctx.services.db_pool.begin().await?;

                    // Keep the prefix until generated segment prefixes have released it.
                    let referenced_prefixes =
                        db::vpc_prefix::count_network_prefixes_by_vpc_prefix_id(
                            &mut txn, &state.id,
                        )
                        .await?;
                    if referenced_prefixes > 0 {
                        let new_state = self.drain_state();
                        tracing::info!(
                            referenced_prefixes,
                            vpc_prefix = %state.id,
                            "VPC Prefix still has network prefix references",
                        );
                        tracing::info!(%vpc_prefix_id, next_state = ?new_state, "VPC Prefix state transition");
                        Ok(StateHandlerOutcome::transition(new_state).with_txn(txn))
                    } else if chrono::Utc::now() >= *delete_at {
                        // Move to the terminal database delete state after the drain deadline.
                        let new_state = VpcPrefixControllerState::Deleting {
                            deletion_state: VpcPrefixDeletionState::DBDelete,
                        };
                        tracing::info!(%vpc_prefix_id, next_state = ?new_state, "VPC Prefix state transition");
                        Ok(StateHandlerOutcome::transition(new_state).with_txn(txn))
                    } else {
                        // The dependency graph is drained, but the grace deadline has not passed.
                        Ok(StateHandlerOutcome::wait(format!(
                            "Cannot delete VPC prefix from database until draining completes at {}",
                            delete_at.to_rfc3339()
                        ))
                        .with_txn(txn))
                    }
                }
                VpcPrefixDeletionState::DBDelete => {
                    let mut txn = ctx.services.db_pool.begin().await?;

                    // Remove the prefix row after all network_prefix references are gone.
                    tracing::info!(
                        %vpc_prefix_id,
                        "VPC Prefix getting removed from the database",
                    );
                    db::vpc_prefix::final_delete(*vpc_prefix_id, &mut txn).await?;
                    Ok(StateHandlerOutcome::deleted().with_txn(txn))
                }
            },
        }
    }
}
