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
//! The rack stays in Created until the expected device counts (looked up
//! from the config-file RackProfile via the rack's `rack_profile_id`
//! column) match the actual device counts (machines, switches,
//! power shelves with `rack_id` FK).

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::{RackId, RackProfileId};
use db::{machine as db_machine, power_shelf as db_power_shelf, switch as db_switch};
use model::machine::machine_search_config::MachineSearchConfig;
use model::rack::RackState;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;

pub async fn handle_created(
    id: &RackId,
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

    let compute_count = db_machine::find_machine_ids::<MachineId>(
        txn.as_mut(),
        MachineSearchConfig {
            rack_id: Some(id.clone()),
            ..Default::default()
        },
    )
    .await?
    .len() as u32;
    let switch_count = db_switch::find_ids(
        txn.as_mut(),
        model::switch::SwitchSearchFilter {
            rack_id: Some(id.clone()),
            ..Default::default()
        },
    )
    .await?
    .len() as u32;
    let power_shelf_count = db_power_shelf::find_ids(
        txn.as_mut(),
        model::power_shelf::PowerShelfSearchFilter {
            rack_id: Some(id.clone()),
            ..Default::default()
        },
    )
    .await?
    .len() as u32;

    if compute_count < capabilities.compute.count
        || switch_count < capabilities.switch.count
        || power_shelf_count < capabilities.power_shelf.count
    {
        return Ok(StateHandlerOutcome::wait(format!(
            "waiting for devices: compute={}/{}, switch={}/{}, power_shelf={}/{}",
            compute_count,
            capabilities.compute.count,
            switch_count,
            capabilities.switch.count,
            power_shelf_count,
            capabilities.power_shelf.count,
        )));
    }

    tracing::info!(
        rack_id = %id,
        compute_count,
        switch_count,
        power_shelf_count,
        "Rack has all expected devices (compute, switch, power_shelf). Transitioning to Discovering.",
    );
    Ok(StateHandlerOutcome::transition(RackState::Discovering).with_txn(txn))
}
