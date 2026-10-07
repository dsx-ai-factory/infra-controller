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

//! Handler for PowerShelfControllerState::ReProvisioning.

use carbide_uuid::power_shelf::PowerShelfId;
use db::db_read::PgPoolReader;
use db::{ObjectColumnFilter, power_shelf as db_power_shelf, rack as db_rack};
use model::power_shelf::{
    PowerShelf, PowerShelfControllerState, PowerShelfReprovisionRequest, ReProvisioningState,
};
use model::rack::{MaintenanceActivity, RackState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::PowerShelfStateHandlerContextObjects;

fn is_rack_level_reprovisioning(state: &PowerShelf) -> bool {
    state
        .power_shelf_reprovisioning_requested
        .as_ref()
        .is_some_and(|req| req.initiator.starts_with("rack-"))
}

fn should_run(activities: &[MaintenanceActivity], activity: &MaintenanceActivity) -> bool {
    activities.is_empty() || activities.iter().any(|a| a.same_kind(activity))
}

fn firmware_upgrade_requested(activities: &[MaintenanceActivity]) -> bool {
    should_run(
        activities,
        &MaintenanceActivity::FirmwareUpgrade {
            firmware_version: None,
            components: vec![],
            force_update: false,
        },
    )
}

/// First ReProvisioning sub-state to enter from Ready, based on the request
/// activities. Empty activities means all phases. Returns `None` when the
/// activities list has no power-shelf-relevant wait phases.
pub(crate) fn first_reprovisioning_state(
    request: &PowerShelfReprovisionRequest,
) -> Option<ReProvisioningState> {
    if firmware_upgrade_requested(&request.activities) {
        return Some(ReProvisioningState::WaitingForRackFirmwareUpgrade);
    }
    None
}

/// If the parent rack is in `RackState::Error`, clear
/// `power_shelf_reprovisioning_requested` and short-circuit to `Ready`.
async fn rack_failed_abort_outcome(
    power_shelf_id: &PowerShelfId,
    state: &PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<Option<StateHandlerOutcome<PowerShelfControllerState>>, StateHandlerError> {
    if !is_rack_level_reprovisioning(state) {
        return Ok(None);
    }

    let Some(rack_id) = state.rack_id.as_ref() else {
        return Ok(None);
    };

    let mut reader: PgPoolReader = ctx.services.db_pool.clone().into();
    let racks = db_rack::find_by(
        reader.as_mut(),
        ObjectColumnFilter::One(db_rack::IdColumn, rack_id),
    )
    .await?;
    let Some(rack) = racks.into_iter().next() else {
        return Ok(None);
    };
    if !matches!(rack.controller_state.value, RackState::Error { .. }) {
        return Ok(None);
    }

    tracing::info!(
        power_shelf_id = %power_shelf_id,
        rack_id = %rack_id,
        "Rack is in Error; aborting power shelf ReProvisioning and returning to Ready",
    );

    let mut txn = ctx.services.db_pool.begin().await?;
    db_power_shelf::clear_power_shelf_reprovisioning_requested(txn.as_mut(), *power_shelf_id)
        .await?;
    Ok(Some(
        StateHandlerOutcome::transition(PowerShelfControllerState::Ready).with_txn(txn),
    ))
}

/// Handles the ReProvisioning state for a power shelf.
pub async fn handle_reprovisioning(
    power_shelf_id: &PowerShelfId,
    state: &mut PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let reprovisioning_state = match &state.controller_state.value {
        PowerShelfControllerState::ReProvisioning {
            reprovisioning_state,
        } => reprovisioning_state,
        _ => unreachable!("handle_reprovisioning called with non-ReProvisioning state"),
    };

    if let Some(outcome) = rack_failed_abort_outcome(power_shelf_id, state, ctx).await? {
        return Ok(outcome);
    }

    match reprovisioning_state {
        ReProvisioningState::WaitingForRackFirmwareUpgrade => {
            let request = state
                .power_shelf_reprovisioning_requested
                .as_ref()
                .expect("WaitingForRackFirmwareUpgrade requires a rack reprovision request");
            let requested_at = request.requested_at;
            let Some(firmware_upgrade_status) = state.firmware_upgrade_status.as_ref() else {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for power shelf firmware upgrade status".into(),
                ));
            };
            if !firmware_upgrade_status.is_current_for(requested_at) {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for current rack firmware cycle".into(),
                ));
            }
            if !firmware_upgrade_status.is_terminal() {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for power shelf firmware completion".into(),
                ));
            }

            match &firmware_upgrade_status.status {
                model::rack::RackFirmwareUpgradeState::Completed => {
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db_power_shelf::clear_power_shelf_reprovisioning_requested(
                        txn.as_mut(),
                        *power_shelf_id,
                    )
                    .await?;
                    Ok(
                        StateHandlerOutcome::transition(PowerShelfControllerState::Ready)
                            .with_txn(txn),
                    )
                }
                model::rack::RackFirmwareUpgradeState::Failed { cause } => {
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db_power_shelf::clear_power_shelf_reprovisioning_requested(
                        txn.as_mut(),
                        *power_shelf_id,
                    )
                    .await?;
                    Ok(
                        StateHandlerOutcome::transition(PowerShelfControllerState::Error {
                            cause: cause.clone(),
                        })
                        .with_txn(txn),
                    )
                }
                model::rack::RackFirmwareUpgradeState::Started
                | model::rack::RackFirmwareUpgradeState::InProgress => Ok(
                    StateHandlerOutcome::wait("waiting for power shelf firmware completion".into()),
                ),
            }
        }
    }
}
