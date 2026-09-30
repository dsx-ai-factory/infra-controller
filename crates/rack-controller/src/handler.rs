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

//! State Handler implementation for Racks.

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_rack_controller::created::handle_created;
use carbide_rack_controller::deleting::handle_deleting;
use carbide_rack_controller::discovering::handle_discovering;
use carbide_rack_controller::error_state::handle_error;
use carbide_rack_controller::maintenance::handle_maintenance;
use carbide_rack_controller::ready::handle_ready;
use carbide_rack_controller::validating::handle_validating;
use carbide_uuid::rack::RackId;
use model::rack::{Rack, RackState, derive_rack_aggregate_health};
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;
use crate::write_ops::PersistRackHealthHistory;

//------------------------------------------------------------------------------

// STATE HANDLER IMPLEMENTATION

#[derive(Debug, Default, Clone)]
pub struct RackStateHandler {}

impl RackStateHandler {
    fn record_metrics(
        &self,
        state: &Rack,
        aggregate_health: &health_report::HealthReport,
        ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
    ) {
        ctx.metrics.health.populate(
            state.id.to_string(),
            aggregate_health,
            &state.health_reports,
        );
        ctx.services.per_object_metrics_registry.record(
            "rack",
            state.id.as_ref(),
            &ctx.metrics.health.health_alert_classifications,
            vec![],
        );
    }

    /// Persists a snapshot of the rack's aggregate health so it appears in the
    /// health history timeline. Deduplication of unchanged observations is
    /// handled in the database layer.
    fn record_health_history(
        &self,
        state: &Rack,
        aggregate_health: &health_report::HealthReport,
        ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
    ) {
        ctx.pending_db_writes.push(PersistRackHealthHistory {
            rack_id: state.id.clone(),
            health_report: aggregate_health.clone(),
        });
    }

    async fn attempt_state_transition(
        &self,
        id: &RackId,
        state: &mut Rack,
        controller_state: &RackState,
        ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
        let rack_profile_id = state.rack_profile_id.clone();
        let config = state.config.clone();

        match controller_state {
            RackState::Created => handle_created(id, rack_profile_id.as_ref(), ctx).await,
            RackState::Discovering => handle_discovering(id, rack_profile_id.as_ref(), ctx).await,
            RackState::Maintenance { maintenance_state } => {
                handle_maintenance(id, state, rack_profile_id.as_ref(), maintenance_state, ctx)
                    .await
            }
            RackState::Validating { validating_state } => {
                handle_validating(id, state, validating_state, ctx).await
            }
            RackState::Ready => handle_ready(id, state, &config, ctx).await,
            RackState::Error {
                cause,
                recovery_policy,
            } => handle_error(id, state, &config, cause, *recovery_policy, ctx).await,
            RackState::Deleting => handle_deleting().await,
        }
    }
}

#[async_trait::async_trait]
impl StateHandler for RackStateHandler {
    type ObjectId = RackId;
    type State = Rack;
    type ControllerState = RackState;
    type ContextObjects = RackStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        id: &Self::ObjectId,
        state: &mut Rack,
        controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
        tracing::info!(
            rack_id = %id,
            rack_state = %controller_state,
            "Rack is in state",
        );

        let aggregate_health = derive_rack_aggregate_health(&state.health_reports);
        self.record_metrics(state, &aggregate_health, ctx);
        self.record_health_history(state, &aggregate_health, ctx);

        if state.deleted.is_some() && !matches!(controller_state, RackState::Deleting) {
            tracing::info!(
                rack_id = %id,
                rack_state = %controller_state,
                "Rack is marked as deleted, transitioning to Deleting",
            );
            return Ok(StateHandlerOutcome::transition(RackState::Deleting));
        }

        self.attempt_state_transition(id, state, controller_state, ctx)
            .await
    }
}
