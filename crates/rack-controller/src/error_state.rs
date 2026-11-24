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

//! Handler for RackState::Error.

use std::fmt::Write;

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_rack_controller::maintenance::first_maintenance_state;
use carbide_rack_controller::ready::detect_component_failure;
use carbide_uuid::rack::RackId;
use model::rack::{Rack, RackConfig, RackErrorRecoveryPolicy, RackState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;

pub async fn handle_error(
    id: &RackId,
    _state: &mut Rack,
    config: &RackConfig,
    cause: &str,
    recovery_policy: RackErrorRecoveryPolicy,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    if let Some(scope) = &config.maintenance_requested {
        let activities_desc = if scope.activities.is_empty() {
            "all non-power activities".to_string()
        } else {
            let mut description = String::new();
            for (index, activity) in scope.activities.iter().enumerate() {
                if index > 0 {
                    description.push_str(", ");
                }
                write!(description, "{activity}").expect("writing to a String cannot fail");
            }
            description
        };
        if scope.is_full_rack() {
            tracing::info!(
                rack_id = %id,
                activity_description = %activities_desc,
                "Rack on-demand maintenance requested from Error state (full rack, activities), transitioning to Maintenance",
            );
        } else {
            tracing::info!(
                rack_id = %id,
                requested_machine_count = scope.machine_ids.len(),
                requested_switch_count = scope.switch_ids.len(),
                requested_power_shelf_count = scope.power_shelf_ids.len(),
                activity_description = %activities_desc,
                "Rack on-demand maintenance requested from Error state (partial: machines, switches, power shelves, activities), transitioning to Maintenance",
            );
        }
        return Ok(StateHandlerOutcome::transition(RackState::Maintenance {
            maintenance_state: first_maintenance_state(scope),
        }));
    }

    if recovery_policy == RackErrorRecoveryPolicy::MaintenanceRequestRequired {
        return Ok(StateHandlerOutcome::wait(format!(
            "rack maintenance failed: {cause}"
        )));
    }

    let Some(current_cause) = detect_component_failure(id, ctx).await? else {
        tracing::info!(
            rack_id = %id,
            "No terminal component failure remains, transitioning rack from Error back to Ready",
        );
        return Ok(StateHandlerOutcome::transition(RackState::Ready));
    };

    if current_cause != cause {
        return Ok(StateHandlerOutcome::transition(RackState::Error {
            cause: current_cause,
            certificate_failure: None,
            recovery_policy,
        }));
    }

    Ok(StateHandlerOutcome::wait(format!(
        "rack in error state: {}",
        cause
    )))
}
