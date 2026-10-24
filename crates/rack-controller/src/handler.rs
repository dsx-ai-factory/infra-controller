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
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;
use crate::component_inventory::{COMPONENT_INVENTORY_HEALTH_SOURCE, RackComponentCounts};
use crate::resolve_capabilities;
use crate::write_ops::{
    PersistRackComponentHealth, PersistRackHealthHistory, RemoveRackComponentHealth,
};

//------------------------------------------------------------------------------

// STATE HANDLER IMPLEMENTATION

#[derive(Debug, Default, Clone)]
pub struct RackStateHandler {}

impl RackStateHandler {
    fn record_health(
        &self,
        state: &Rack,
        ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
    ) {
        let aggregate_health = derive_rack_aggregate_health(&state.health_reports);
        self.record_metrics(state, &aggregate_health, ctx);
        self.record_health_history(state, &aggregate_health, ctx);
    }

    fn reconcile_component_health(
        &self,
        state: &mut Rack,
        capabilities: Option<&model::rack_type::RackCapabilitiesSet>,
        component_counts: RackComponentCounts,
        pending_db_writes: &mut DbWriteBatch,
    ) {
        // An unresolved profile is not evidence that missing components recovered.
        let Some(capabilities) = capabilities else {
            return;
        };
        let previous = state
            .health_reports
            .merges
            .get(COMPONENT_INVENTORY_HEALTH_SOURCE);
        let mut desired = component_counts.missing_component_report(capabilities);

        if let Some(desired) = desired.as_mut() {
            desired.update_in_alert_since(previous);
        }

        match (previous, desired) {
            (Some(_), None) => {
                state
                    .health_reports
                    .merges
                    .remove(COMPONENT_INVENTORY_HEALTH_SOURCE);
                pending_db_writes.push(RemoveRackComponentHealth {
                    rack_id: state.id.clone(),
                    source: COMPONENT_INVENTORY_HEALTH_SOURCE,
                });
            }
            (Some(previous), Some(desired)) if previous.alerts == desired.alerts => {}
            (_, Some(desired)) => {
                state.health_reports.merges.insert(
                    COMPONENT_INVENTORY_HEALTH_SOURCE.to_string(),
                    desired.clone(),
                );
                pending_db_writes.push(PersistRackComponentHealth {
                    rack_id: state.id.clone(),
                    health_report: desired,
                });
            }
            (None, None) => {}
        }
    }

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
        capabilities: Option<&model::rack_type::RackCapabilitiesSet>,
        component_counts: RackComponentCounts,
        ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
        let rack_profile_id = state.rack_profile_id.clone();
        let config = state.config.clone();

        match controller_state {
            RackState::Created => handle_created(id, capabilities, component_counts),
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

    #[tracing::instrument(skip_all, fields(object_id = %id, state = %controller_state))]
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

        if state.deleted.is_some() {
            self.record_health(state, ctx);
            if matches!(controller_state, RackState::Deleting) {
                return handle_deleting().await;
            }
            tracing::info!(
                rack_id = %id,
                rack_state = %controller_state,
                "Rack is marked as deleted, transitioning to Deleting",
            );
            return Ok(StateHandlerOutcome::transition(RackState::Deleting));
        }

        let capabilities = resolve_capabilities(id, state.rack_profile_id.as_ref(), ctx).cloned();
        let component_counts = RackComponentCounts::load(&ctx.services.db_pool, id).await?;
        self.reconcile_component_health(
            state,
            capabilities.as_ref(),
            component_counts,
            ctx.pending_db_writes,
        );
        self.record_health(state, ctx);

        self.attempt_state_transition(
            id,
            state,
            controller_state,
            capabilities.as_ref(),
            component_counts,
            ctx,
        )
        .await
    }
}
