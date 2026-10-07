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

//! Handler for SwitchControllerState::Validating.

use carbide_uuid::switch::SwitchId;
use model::switch::{BomValidatingState, Switch, SwitchControllerState, ValidatingState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;

/// Handles the Validating state for a switch.
/// TODO: Implement Switch validation logic.
pub async fn handle_validating(
    switch_id: &SwitchId,
    state: &mut Switch,
    _ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    tracing::info!(
        switch_id = ?switch_id,
        "Validating switch",
    );
    let validating_state = match &state.controller_state.value {
        SwitchControllerState::Validating { validating_state } => validating_state,
        _ => unreachable!("handle_validating called with non-Validating state"),
    };

    match validating_state {
        ValidatingState::ValidationComplete => {
            tracing::info!("Validating Switch: ValidationComplete");
            Ok(StateHandlerOutcome::transition(
                SwitchControllerState::BomValidating {
                    bom_validating_state: BomValidatingState::BomValidationComplete,
                },
            ))
        }
    }
}
