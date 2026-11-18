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

//! Handler for RackState::Created.
//!
//! The rack stays in Created until its switch count reaches the profile minimum.
//! Compute and power shelf shortages are reported through rack health without
//! blocking ingestion.

use carbide_uuid::rack::RackId;
use model::rack::RackState;
use model::rack_type::RackCapabilitiesSet;
use state_controller::state_handler::{StateHandlerError, StateHandlerOutcome};

use crate::component_inventory::RackComponentCounts;

pub(crate) fn handle_created(
    id: &RackId,
    capabilities: Option<&RackCapabilitiesSet>,
    component_counts: RackComponentCounts,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    let capabilities = match capabilities {
        Some(caps) => caps,
        None => {
            return Ok(StateHandlerOutcome::wait(
                "no or unknown rack_profile_id".into(),
            ));
        }
    };

    if component_counts.switch < capabilities.switch.count {
        return Ok(StateHandlerOutcome::wait(format!(
            "waiting for switches: switch={}/{}",
            component_counts.switch, capabilities.switch.count,
        )));
    }

    tracing::info!(
        rack_id = %id,
        compute_count = component_counts.compute,
        switch_count = component_counts.switch,
        power_shelf_count = component_counts.power_shelf,
        "Rack has enough switches. Transitioning to Discovering.",
    );
    Ok(StateHandlerOutcome::transition(RackState::Discovering))
}
