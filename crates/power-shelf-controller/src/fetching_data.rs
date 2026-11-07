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

//! Handler for PowerShelfControllerState::FetchingData.

use carbide_uuid::power_shelf::PowerShelfId;
use model::power_shelf::{PowerShelf, PowerShelfControllerState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::PowerShelfStateHandlerContextObjects;

/// Handles the FetchingData state for a power shelf.
///
/// TODO: Implement real fetching logic. This would typically involve:
/// 1. Fetching data from the PowerShelf
/// 2. Updating the PowerShelf status
pub async fn handle_fetching_data(
    power_shelf_id: &PowerShelfId,
    _state: &mut PowerShelf,
    _ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    tracing::info!(
        %power_shelf_id,
        "Fetching PowerShelf data; transitioning to Configuring"
    );
    Ok(StateHandlerOutcome::transition(
        PowerShelfControllerState::Configuring,
    ))
}
