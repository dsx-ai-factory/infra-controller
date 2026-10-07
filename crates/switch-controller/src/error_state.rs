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

//! Handler for SwitchControllerState::Error.

use carbide_uuid::switch::SwitchId;
use model::switch::{Switch, SwitchControllerState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;

/// Handles the Error state for a switch.
///
/// Deletion takes precedence over a pending maintenance request so a stale
/// request cannot block deletion.
pub async fn handle_error(
    _switch_id: &SwitchId,
    state: &mut Switch,
    _ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    tracing::info!(
        switch_id = %_switch_id,
        "Switch is in error state",
    );
    if state.is_marked_as_deleted() {
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Deleting,
        ));
    }

    if let Some(req) = state.switch_maintenance_requested.as_ref() {
        tracing::info!(
            operation = ?req.operation,
            initiator = %req.initiator,
            "Switch maintenance requested from Error; transitioning to Maintenance"
        );
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::maintenance_for_request(req.clone()),
        ));
    }

    Ok(StateHandlerOutcome::do_nothing())
}
