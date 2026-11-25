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

use std::fmt::Write;

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_rack_controller::maintenance::first_maintenance_state;
use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::RackId;
use db::{
    TransactionVending as _, machine as db_machine, power_shelf as db_power_shelf, rack as db_rack,
    switch as db_switch,
};
use model::DeletedFilter;
use model::machine::machine_search_config::MachineSearchConfig;
use model::power_shelf::PowerShelfSearchFilter;
use model::rack::{
    FirmwareUpgradeState, Rack, RackConfig, RackErrorRecoveryPolicy, RackMaintenanceState,
    RackState,
};
use model::switch::SwitchSearchFilter;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;

const COMPONENT_ERROR_STATE: &str = "error";
const MACHINE_FAILED_STATE: &str = "failed";

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
        let mut txn = ctx
            .services
            .db_pool
            .txn_begin()
            .await
            .map_err(StateHandlerError::from_transaction)?;
        db_rack::update(txn.as_mut(), id, &state.config).await?;
        return Ok(StateHandlerOutcome::transition(RackState::Discovering).with_txn(txn));
    }

    if config.reprovision_requested {
        tracing::info!(
            rack_id = %id,
            "Rack reprovision requested, transitioning from Ready to Maintenance",
        );
        state.config.reprovision_requested = false;
        state.config.ingestion_in_progress = false;
        state.config.maintenance_requested = None;
        let mut txn = ctx
            .services
            .db_pool
            .txn_begin()
            .await
            .map_err(StateHandlerError::from_transaction)?;
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
        return Ok(StateHandlerOutcome::transition(RackState::Maintenance {
            maintenance_state: first_maintenance_state(scope),
        }));
    }

    if let Some(cause) = detect_component_failure(id, ctx).await? {
        tracing::warn!(
            rack_id = %id,
            cause = %cause,
            "Rack transitioning from Ready to Error",
        );
        return Ok(StateHandlerOutcome::transition(RackState::Error {
            certificate_failure: None,
            cause,
            recovery_policy: RackErrorRecoveryPolicy::ComponentsReady,
        }));
    }

    Ok(StateHandlerOutcome::wait(
        "rack is ready, no maintenance requested".into(),
    ))
}

/// Returns a cause string if any switch, power shelf, or machine in the rack
/// is in its terminal failure state, or `None` if no terminal component failure remains.
pub(super) async fn detect_component_failure(
    rack_id: &RackId,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<Option<String>, StateHandlerError> {
    let mut failed_switches = db_switch::find_ids(
        &ctx.services.db_pool,
        SwitchSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            controller_state: Some(COMPONENT_ERROR_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    let mut failed_power_shelves = db_power_shelf::find_ids(
        &ctx.services.db_pool,
        PowerShelfSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            controller_state: Some(COMPONENT_ERROR_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    let mut failed_machines = db_machine::find_machine_ids::<MachineId>(
        &ctx.services.db_pool,
        MachineSearchConfig {
            rack_id: Some(rack_id.clone()),
            controller_state: Some(MACHINE_FAILED_STATE.to_string()),
            ..Default::default()
        },
    )
    .await?;

    if failed_switches.is_empty() && failed_power_shelves.is_empty() && failed_machines.is_empty() {
        return Ok(None);
    }

    // Database lookup order is unspecified; keep unchanged failure sets stable.
    failed_switches.sort_unstable();
    failed_power_shelves.sort_unstable();
    failed_machines.sort_unstable();

    let mut cause = String::from("component(s) reported terminal failure: ");
    if !failed_switches.is_empty() {
        write!(cause, "{} switch(es) in Error: [", failed_switches.len())
            .expect("writing to a String cannot fail");
        for (index, id) in failed_switches.iter().enumerate() {
            if index > 0 {
                cause.push_str(", ");
            }
            write!(cause, "{id}").expect("writing to a String cannot fail");
        }
        cause.push(']');
    }
    if !failed_power_shelves.is_empty() {
        if !failed_switches.is_empty() {
            cause.push_str("; ");
        }
        write!(
            cause,
            "{} power shelf/shelves in Error: [",
            failed_power_shelves.len()
        )
        .expect("writing to a String cannot fail");
        for (index, id) in failed_power_shelves.iter().enumerate() {
            if index > 0 {
                cause.push_str(", ");
            }
            write!(cause, "{id}").expect("writing to a String cannot fail");
        }
        cause.push(']');
    }
    if !failed_machines.is_empty() {
        if !failed_switches.is_empty() || !failed_power_shelves.is_empty() {
            cause.push_str("; ");
        }
        write!(cause, "{} machine(s) in Failed: [", failed_machines.len())
            .expect("writing to a String cannot fail");
        for (index, id) in failed_machines.iter().enumerate() {
            if index > 0 {
                cause.push_str(", ");
            }
            write!(cause, "{id}").expect("writing to a String cannot fail");
        }
        cause.push(']');
    }

    Ok(Some(cause))
}
