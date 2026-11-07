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

//! State Handler implementation for Switches (mirrors Machine state handler structure).

use carbide_uuid::switch::SwitchId;
use model::switch::{Switch, SwitchControllerState, derive_switch_aggregate_health};
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use tracing::instrument;

use crate::bom_validating::handle_bom_validating;
use crate::configuring::handle_configuring;
use crate::context::SwitchStateHandlerContextObjects;
use crate::created::handle_created;
use crate::decommissioning::handle_decommissioning;
use crate::deleting::handle_deleting;
use crate::error_state::handle_error;
use crate::fetch_info::handle_fetch_info;
use crate::initializing::handle_initializing;
use crate::maintenance::handle_maintenance;
use crate::ready::handle_ready;
use crate::reprovisioning::handle_reprovisioning;
use crate::rotating_bmc::handle_rotating_bmc;
use crate::validating::handle_validating;
use crate::write_ops::PersistSwitchHealthHistory;

/// The actual Switch State handler (structure mirrors MachineStateHandler).
#[derive(Debug, Default, Clone)]
pub struct SwitchStateHandler {}

impl SwitchStateHandler {
    fn record_metrics(
        &self,
        state: &Switch,
        aggregate_health: &health_report::HealthReport,
        ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    ) {
        ctx.metrics.health.populate(
            state.id.to_string(),
            aggregate_health,
            &state.health_reports,
        );
        ctx.services.per_object_metrics_registry.record(
            "switch",
            &state.id.to_string(),
            &ctx.metrics.health.health_alert_classifications,
            vec![],
        );
    }

    /// Persists a snapshot of the switch's aggregate health so it appears in the
    /// health history timeline. Deduplication of unchanged observations is
    /// handled in the database layer.
    fn record_health_history(
        &self,
        state: &Switch,
        aggregate_health: &health_report::HealthReport,
        ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    ) {
        ctx.pending_db_writes.push(PersistSwitchHealthHistory {
            switch_id: state.id,
            health_report: aggregate_health.clone(),
        });
    }

    /// Attempts a state transition by delegating to the appropriate state handler.
    async fn attempt_state_transition(
        &self,
        switch_id: &SwitchId,
        state: &mut Switch,
        ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    ) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
        let controller_state = &state.controller_state.value;

        match controller_state {
            SwitchControllerState::Created => handle_created(switch_id, state, ctx).await,
            SwitchControllerState::Initializing { .. } => {
                handle_initializing(switch_id, state, ctx).await
            }
            SwitchControllerState::Configuring { .. } => {
                handle_configuring(switch_id, state, ctx).await
            }
            SwitchControllerState::FetchInfo => handle_fetch_info(switch_id, state, ctx).await,
            SwitchControllerState::Validating { .. } => {
                handle_validating(switch_id, state, ctx).await
            }
            SwitchControllerState::BomValidating { .. } => {
                handle_bom_validating(switch_id, state, ctx).await
            }
            SwitchControllerState::ReProvisioning { .. } => {
                handle_reprovisioning(switch_id, state, ctx).await
            }
            SwitchControllerState::Maintenance { .. } => {
                handle_maintenance(switch_id, state, ctx).await
            }
            SwitchControllerState::Ready => handle_ready(switch_id, state, ctx).await,
            SwitchControllerState::Decommissioning {
                decommissioning_state,
            } => handle_decommissioning(switch_id, state, decommissioning_state, ctx).await,
            SwitchControllerState::RotatingBmc { retry_count } => {
                handle_rotating_bmc(switch_id, state, *retry_count, ctx).await
            }
            SwitchControllerState::Deleting => handle_deleting(switch_id, state, ctx).await,
            SwitchControllerState::Error { .. } => handle_error(switch_id, state, ctx).await,
        }
    }
}

#[async_trait::async_trait]
impl StateHandler for SwitchStateHandler {
    type ObjectId = SwitchId;
    type State = Switch;
    type ControllerState = SwitchControllerState;
    type ContextObjects = SwitchStateHandlerContextObjects;

    #[instrument(skip_all, fields(object_id=%switch_id))]
    async fn handle_object_state(
        &self,
        switch_id: &SwitchId,
        state: &mut Switch,
        _controller_state: &SwitchControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
        let aggregate_health = derive_switch_aggregate_health(&state.health_reports);
        self.record_metrics(state, &aggregate_health, ctx);
        self.record_health_history(state, &aggregate_health, ctx);
        self.attempt_state_transition(switch_id, state, ctx).await
    }
}
