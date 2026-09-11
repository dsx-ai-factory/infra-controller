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

//! Handler for RackState::Ready.

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_rack_controller::maintenance::first_maintenance_state;
use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::RackId;
use db::{
    machine as db_machine, power_shelf as db_power_shelf, rack as db_rack, switch as db_switch,
};
use model::DeletedFilter;
use model::machine::machine_search_config::MachineSearchConfig;
use model::power_shelf::PowerShelfSearchFilter;
use model::rack::{FirmwareUpgradeState, Rack, RackConfig, RackMaintenanceState, RackState};
use model::switch::SwitchSearchFilter;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;

const COMPONENT_ERROR_STATE: &str = "error";
const MACHINE_FAILED_STATE: &str = "failed";
const COMPONENT_READY_STATE: &str = "ready";

pub async fn handle_ready(
    id: &RackId,
    state: &mut Rack,
    config: &RackConfig,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    if config.topology_changed {
        tracing::info!(
            rack_id = %id,
            "Rack topology changed, transitioning from Ready to Discovering",
        );
        state.config.topology_changed = false;
        let mut txn = ctx.services.db_pool.begin().await?;
        db_rack::update(txn.as_mut(), id, &state.config).await?;
        return Ok(StateHandlerOutcome::transition(RackState::Discovering).with_txn(txn));
    }

    if config.reprovision_requested {
        tracing::info!(
            rack_id = %id,
            "Rack reprovision requested, transitioning from Ready to Maintenance",
        );
        state.config.reprovision_requested = false;
        state.config.maintenance_requested = None;
        let mut txn = ctx.services.db_pool.begin().await?;
        db_rack::update(txn.as_mut(), id, &state.config).await?;
        return Ok(StateHandlerOutcome::transition(RackState::Maintenance {
            maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                rack_firmware_upgrade: FirmwareUpgradeState::Start,
            },
        })
        .with_txn(txn));
    }

    if let Some(scope) = &config.maintenance_requested {
        let activities_desc = if scope.activities.is_empty() {
            "all".to_string()
        } else {
            scope
                .activities
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        if scope.is_full_rack() {
            tracing::info!(
                rack_id = %id,
                activity_description = %activities_desc,
                "Rack on-demand maintenance requested (full rack, activities), transitioning to Maintenance",
            );
        } else {
            tracing::info!(
                rack_id = %id,
                requested_machine_count = scope.machine_ids.len(),
                requested_switch_count = scope.switch_ids.len(),
                requested_power_shelf_count = scope.power_shelf_ids.len(),
                activity_description = %activities_desc,
                "Rack on-demand maintenance requested (partial: machines, switches, power shelves, activities), transitioning to Maintenance",
            );
        }
        // Leave maintenance_requested set; the maintenance handler reads the
        // scope to decide which activities to run and clears it on Completed.
        let txn = ctx.services.db_pool.begin().await?;
        return Ok(StateHandlerOutcome::transition(RackState::Maintenance {
            maintenance_state: first_maintenance_state(scope),
        })
        .with_txn(txn));
    }

    if let Some(cause) = detect_component_failure(id, ctx).await? {
        tracing::warn!(
            rack_id = %id,
            cause = %cause,
            "Rack transitioning from Ready to Error",
        );
        let txn = ctx.services.db_pool.begin().await?;
        return Ok(StateHandlerOutcome::transition(RackState::Error { cause }).with_txn(txn));
    }

    Ok(StateHandlerOutcome::wait(
        "rack is ready, no maintenance requested".into(),
    ))
}

/// Returns a cause string if any switch, power shelf, or machine in the rack
/// is in its terminal failure state, or `None` if every component is healthy.
pub(super) async fn detect_component_failure(
    rack_id: &RackId,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<Option<String>, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;

    let failed_switches = db_switch::find_ids(
        txn.as_mut(),
        SwitchSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            controller_state: Some(COMPONENT_ERROR_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    let failed_power_shelves = db_power_shelf::find_ids(
        txn.as_mut(),
        PowerShelfSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            controller_state: Some(COMPONENT_ERROR_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    let failed_machines = db_machine::find_machine_ids::<MachineId>(
        txn.as_mut(),
        MachineSearchConfig {
            rack_id: Some(rack_id.clone()),
            controller_state: Some(MACHINE_FAILED_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    txn.commit().await?;

    if failed_switches.is_empty() && failed_power_shelves.is_empty() && failed_machines.is_empty() {
        return Ok(None);
    }

    let mut parts = Vec::new();
    if !failed_switches.is_empty() {
        parts.push(format!(
            "{} switch(es) in Error: [{}]",
            failed_switches.len(),
            failed_switches
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if !failed_power_shelves.is_empty() {
        parts.push(format!(
            "{} power shelf/shelves in Error: [{}]",
            failed_power_shelves.len(),
            failed_power_shelves
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if !failed_machines.is_empty() {
        parts.push(format!(
            "{} machine(s) in Failed: [{}]",
            failed_machines.len(),
            failed_machines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    Ok(Some(format!(
        "component(s) reported terminal failure: {}",
        parts.join("; ")
    )))
}

/// Returns `true` only when the rack has at least one component registered
/// and every switch, power shelf, and machine is in its `Ready` state.
pub(super) async fn all_components_ready(
    rack_id: &RackId,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<bool, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;

    let all_switches = db_switch::find_ids(
        txn.as_mut(),
        SwitchSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;
    let ready_switches = db_switch::find_ids(
        txn.as_mut(),
        SwitchSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            controller_state: Some(COMPONENT_READY_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    let all_power_shelves = db_power_shelf::find_ids(
        txn.as_mut(),
        PowerShelfSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;
    let ready_power_shelves = db_power_shelf::find_ids(
        txn.as_mut(),
        PowerShelfSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            controller_state: Some(COMPONENT_READY_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    let all_machines = db_machine::find_machine_ids::<MachineId>(
        txn.as_mut(),
        MachineSearchConfig {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        },
    )
    .await?;
    let ready_machines = db_machine::find_machine_ids::<MachineId>(
        txn.as_mut(),
        MachineSearchConfig {
            rack_id: Some(rack_id.clone()),
            controller_state: Some(COMPONENT_READY_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    txn.commit().await?;

    let total = all_switches.len() + all_power_shelves.len() + all_machines.len();
    if total == 0 {
        tracing::debug!(
            rack_id = %rack_id,
            "Rack has no components registered; not promoting out of Error",
        );
        return Ok(false);
    }

    let all_ready = ready_switches.len() == all_switches.len()
        && ready_power_shelves.len() == all_power_shelves.len()
        && ready_machines.len() == all_machines.len();

    if !all_ready {
        tracing::debug!(
            rack_id = %rack_id,
            ready_switch_count = ready_switches.len(),
            total_switch_count = all_switches.len(),
            ready_power_shelf_count = ready_power_shelves.len(),
            total_power_shelf_count = all_power_shelves.len(),
            ready_machine_count = ready_machines.len(),
            total_machine_count = all_machines.len(),
            "Rack components not yet all Ready: switches, power shelves, machines",
        );
    }

    Ok(all_ready)
}
