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

//! Handler for SwitchControllerState::Ready.

use carbide_uuid::switch::SwitchId;
use db::switch as db_switch;
use model::switch::{ConfiguringState, Switch, SwitchControllerState, SwitchDecommissioningState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;
use crate::nvos_password_rotation::needs_nvos_password_reconciliation;
use crate::reprovisioning::first_reprovisioning_state;
use crate::rotating_bmc::should_enter_bmc_rotation;

/// Handles the Ready state for a switch.
///
/// If the switch is marked for deletion, transitions to `Deleting`.
/// If a maintenance request has been posted via `switch_maintenance_requested`,
/// transitions to `Maintenance` with the requested operation. If rack-level
/// reprovisioning has been requested, transitions to `ReProvisioning`.
/// Otherwise, actionable NVOS convergence work transitions back to `Configuring`.
pub async fn handle_ready(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    if state.is_marked_as_deleted() {
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Deleting,
        ));
    }

    if state.decommission_requested {
        let mut txn = ctx.services.db_pool.begin().await?;
        db_switch::clear_decommission_requested(&mut txn, *switch_id).await?;
        return Ok(
            StateHandlerOutcome::transition(SwitchControllerState::Decommissioning {
                decommissioning_state: SwitchDecommissioningState::SuppressingSiteExplorer,
            })
            .with_txn(txn),
        );
    }

    if let Some(req) = state.switch_maintenance_requested.as_ref() {
        tracing::info!(
            operation = ?req.operation,
            initiator = %req.initiator,
            "Switch maintenance requested; transitioning to Maintenance"
        );
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::maintenance_for_request(req.clone()),
        ));
    }

    if let Some(req) = &state.switch_reprovisioning_requested {
        if !req.initiator.starts_with("rack-") {
            tracing::warn!(
                initiator = %req.initiator,
                "Unknown initiator for switch reprovisioning request",
            );
            let cause = format!(
                "unknown initiator for switch reprovisioning request: {}",
                req.initiator
            );
            let mut txn = ctx.services.db_pool.begin().await?;
            db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id).await?;
            return Ok(
                StateHandlerOutcome::transition(SwitchControllerState::Error { cause })
                    .with_txn(txn),
            );
        }

        let Some(reprovisioning_state) = first_reprovisioning_state(req) else {
            tracing::warn!(
                switch_id = %switch_id,
                initiator = %req.initiator,
                "Rack reprovision request has no switch-relevant activities; clearing request"
            );
            let mut txn = ctx.services.db_pool.begin().await?;
            db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id).await?;
            return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
        };

        tracing::info!(
            ?reprovisioning_state,
            "Rack-level reprovisioning requested — entering ReProvisioning"
        );
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::ReProvisioning {
                reprovisioning_state,
            },
        ));
    }

    if needs_nvos_password_reconciliation(switch_id, state, ctx).await? {
        tracing::info!(
            switch_id = ?switch_id,
            "Switch: NVOS password reconciliation pending; transitioning to Configuring",
        );

        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Configuring {
                config_state: ConfiguringState::RotateOsPassword,
            },
        ));
    }

    // Lowest precedence: only converge the BMC credential once the switch is
    // otherwise idle in Ready, so rotation never contends with NVOS
    // reconfiguration, maintenance, or reprovisioning. The site-flag gate and
    // the operator force-converge override live in `should_enter_bmc_rotation`.
    if should_enter_bmc_rotation(ctx.services, state).await? {
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::RotatingBmc { retry_count: 0 },
        ));
    }

    Ok(StateHandlerOutcome::do_nothing())
}
