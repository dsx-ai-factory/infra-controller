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

//! Handler for PowerShelfControllerState::Error.

use carbide_uuid::power_shelf::PowerShelfId;
use model::power_shelf::{PowerShelf, PowerShelfControllerState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::PowerShelfStateHandlerContextObjects;

/// Handles the Error state for a power shelf.
///
/// Deletion takes precedence over a pending maintenance request so a stale
/// request cannot block deletion.
pub async fn handle_error(
    power_shelf_id: &PowerShelfId,
    state: &mut PowerShelf,
    _ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    if state.is_marked_as_deleted() {
        tracing::info!(
            power_shelf_id = %power_shelf_id,
            "PowerShelf in Error is marked for deletion; transitioning to Deleting"
        );
        return Ok(StateHandlerOutcome::transition(
            PowerShelfControllerState::Deleting,
        ));
    }

    if let Some(req) = state.power_shelf_maintenance_requested.as_ref() {
        tracing::info!(
            operation = ?req.operation,
            initiator = %req.initiator,
            "PowerShelf maintenance requested from Error; transitioning to Maintenance"
        );
        return Ok(StateHandlerOutcome::transition(
            PowerShelfControllerState::Maintenance {
                operation: req.operation,
                request: Some(req.clone()),
            },
        ));
    }

    Ok(StateHandlerOutcome::do_nothing())
}
