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

//! State Handler implementation for PowerShelves (mirrors Switch state handler structure).

use carbide_uuid::power_shelf::PowerShelfId;
use model::power_shelf::{
    PowerShelf, PowerShelfControllerState, derive_power_shelf_aggregate_health,
};
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use tracing::instrument;

use crate::configuring::handle_configuring;
use crate::context::PowerShelfStateHandlerContextObjects;
use crate::decommissioning::handle_decommissioning;
use crate::deleting::handle_deleting;
use crate::error_state::handle_error;
use crate::fetching_data::handle_fetching_data;
use crate::initializing::handle_initializing;
use crate::maintenance::handle_maintenance;
use crate::ready::handle_ready;
use crate::reprovisioning::handle_reprovisioning;
use crate::rotating_bmc::handle_rotating_bmc;
use crate::write_ops::PersistPowerShelfHealthHistory;

/// The actual PowerShelf State handler (structure mirrors SwitchStateHandler).
#[derive(Debug, Default, Clone)]
pub struct PowerShelfStateHandler {}

impl PowerShelfStateHandler {
    fn record_metrics(
        &self,
        state: &PowerShelf,
        aggregate_health: &health_report::HealthReport,
        ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
    ) {
        ctx.metrics.health.populate(
            state.id.to_string(),
            aggregate_health,
            &state.health_reports,
        );
        ctx.services.per_object_metrics_registry.record(
            "power_shelf",
            &state.id.to_string(),
            &ctx.metrics.health.health_alert_classifications,
            vec![],
        );
    }

    /// Persists a snapshot of the power shelf's aggregate health so it appears in
    /// the health history timeline. Deduplication of unchanged observations is
    /// handled in the database layer.
    fn record_health_history(
        &self,
        state: &PowerShelf,
        aggregate_health: &health_report::HealthReport,
        ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
    ) {
        ctx.pending_db_writes.push(PersistPowerShelfHealthHistory {
            power_shelf_id: state.id,
            health_report: aggregate_health.clone(),
        });
    }

    /// Attempts a state transition by delegating to the appropriate state handler.
    async fn attempt_state_transition(
        &self,
        power_shelf_id: &PowerShelfId,
        state: &mut PowerShelf,
        ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
        let controller_state = &state.controller_state.value;

        match controller_state {
            PowerShelfControllerState::Initializing => {
                handle_initializing(power_shelf_id, state, ctx).await
            }
            PowerShelfControllerState::FetchingData => {
                handle_fetching_data(power_shelf_id, state, ctx).await
            }
            PowerShelfControllerState::Configuring => {
                handle_configuring(power_shelf_id, state, ctx).await
            }
            PowerShelfControllerState::Ready => handle_ready(power_shelf_id, state, ctx).await,
            PowerShelfControllerState::RotatingBmc { retry_count } => {
                handle_rotating_bmc(power_shelf_id, state, *retry_count, ctx).await
            }
            PowerShelfControllerState::Maintenance { .. } => {
                handle_maintenance(power_shelf_id, state, ctx).await
            }
            PowerShelfControllerState::ReProvisioning { .. } => {
                handle_reprovisioning(power_shelf_id, state, ctx).await
            }
            PowerShelfControllerState::Decommissioning {
                decommissioning_state,
            } => handle_decommissioning(power_shelf_id, state, decommissioning_state, ctx).await,
            PowerShelfControllerState::Deleting => {
                handle_deleting(power_shelf_id, state, ctx).await
            }
            PowerShelfControllerState::Error { .. } => {
                handle_error(power_shelf_id, state, ctx).await
            }
        }
    }
}

#[async_trait::async_trait]
impl StateHandler for PowerShelfStateHandler {
    type ObjectId = PowerShelfId;
    type State = PowerShelf;
    type ControllerState = PowerShelfControllerState;
    type ContextObjects = PowerShelfStateHandlerContextObjects;

    #[instrument(skip_all, fields(object_id=%power_shelf_id))]
    async fn handle_object_state(
        &self,
        power_shelf_id: &PowerShelfId,
        state: &mut PowerShelf,
        _controller_state: &PowerShelfControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
        let aggregate_health = derive_power_shelf_aggregate_health(&state.health_reports);
        self.record_metrics(state, &aggregate_health, ctx);
        self.record_health_history(state, &aggregate_health, ctx);
        self.attempt_state_transition(power_shelf_id, state, ctx)
            .await
    }
}
