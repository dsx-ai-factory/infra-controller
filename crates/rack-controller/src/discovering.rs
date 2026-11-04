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

//! Handler for RackState::Discovering.
//!
//! The rack waits in Discovering until its ready switch count reaches the
//! profile minimum, then enters ingestion maintenance. Compute and power shelf
//! counts do not gate this transition.

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_uuid::rack::{RackId, RackProfileId};
use db::{rack as db_rack, switch as db_switch};
use model::rack::{FirmwareUpgradeState, Rack, RackMaintenanceState, RackState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;

const COMPONENT_READY_STATE: &str = "ready";

/// Waits for discovery readiness, then atomically marks the ingestion cycle
/// while entering maintenance.
pub async fn handle_discovering(
    id: &RackId,
    state: &mut Rack,
    rack_profile_id: Option<&RackProfileId>,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    let capabilities = match super::resolve_capabilities(id, rack_profile_id, ctx) {
        Some(caps) => caps,
        None => {
            return Ok(StateHandlerOutcome::wait(
                "no or unknown rack_profile_id".into(),
            ));
        }
    };

    let mut txn = ctx.services.db_pool.begin().await?;

    let ready_switches = db_switch::find_ids(
        txn.as_mut(),
        model::switch::SwitchSearchFilter {
            rack_id: Some(id.clone()),
            controller_state: Some(COMPONENT_READY_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?
    .len() as u32;
    if ready_switches < capabilities.switch.count {
        return Ok(StateHandlerOutcome::wait(format!(
            "waiting for switches ready: switch={}/{}",
            ready_switches, capabilities.switch.count,
        ))
        .with_txn(txn));
    }

    tracing::info!(
        rack_id = %id,
        "Rack has enough ready switches, transitioning to Maintenance",
    );
    state.config.ingestion_in_progress = true;
    db_rack::update(txn.as_mut(), id, &state.config).await?;
    Ok(StateHandlerOutcome::transition(RackState::Maintenance {
        maintenance_state: RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::Start,
        },
    })
    .with_txn(txn))
}
