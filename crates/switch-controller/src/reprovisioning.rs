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

//! Handler for SwitchControllerState::ReProvisioning.

use carbide_uuid::switch::SwitchId;
use db::db_read::PgPoolReader;
use db::{ObjectColumnFilter, rack as db_rack, switch as db_switch};
use model::rack::{MaintenanceActivity, RackState};
use model::switch::{ReProvisioningState, Switch, SwitchControllerState, SwitchReprovisionRequest};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;

/// Returns true if the switch reprovisioning request was initiated by a
/// rack-level service (i.e. the rack firmware upgrade flow).
fn is_rack_level_reprovisioning(state: &Switch) -> bool {
    state
        .switch_reprovisioning_requested
        .as_ref()
        .is_some_and(|req| req.initiator.starts_with("rack-"))
}

fn should_run(activities: &[MaintenanceActivity], activity: &MaintenanceActivity) -> bool {
    activities.is_empty() || activities.iter().any(|a| a.same_kind(activity))
}

fn nvos_update_requested(activities: &[MaintenanceActivity]) -> bool {
    should_run(
        activities,
        &MaintenanceActivity::NvosUpdate {
            config_json: String::new(),
        },
    )
}

fn configure_nmx_cluster_requested(activities: &[MaintenanceActivity]) -> bool {
    should_run(activities, &MaintenanceActivity::ConfigureNmxCluster)
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
/// activities list has no switch-relevant wait phases.
pub(crate) fn first_reprovisioning_state(
    request: &SwitchReprovisionRequest,
) -> Option<ReProvisioningState> {
    let activities = &request.activities;
    if firmware_upgrade_requested(activities) {
        return Some(ReProvisioningState::WaitingForRackFirmwareUpgrade);
    }
    if nvos_update_requested(activities) {
        return Some(ReProvisioningState::WaitingForNVOSUpgrade);
    }
    if configure_nmx_cluster_requested(activities) {
        return Some(ReProvisioningState::WaitingForNMXCConfigure);
    }
    None
}

/// Next ReProvisioning sub-state after firmware completes.
fn next_state_after_firmware(request: &SwitchReprovisionRequest) -> Option<ReProvisioningState> {
    let activities = &request.activities;
    if nvos_update_requested(activities) {
        return Some(ReProvisioningState::WaitingForNVOSUpgrade);
    }
    if configure_nmx_cluster_requested(activities) {
        return Some(ReProvisioningState::WaitingForNMXCConfigure);
    }
    None
}

/// Next ReProvisioning sub-state after NVOS completes.
fn next_state_after_nvos(request: &SwitchReprovisionRequest) -> Option<ReProvisioningState> {
    configure_nmx_cluster_requested(&request.activities)
        .then_some(ReProvisioningState::WaitingForNMXCConfigure)
}

/// If the parent rack is in `RackState::Error`, clear
/// `switch_reprovisioning_requested` and short-circuit to `Ready`. The
/// rack will never advance the remaining `ReProvisioning` sub-states once
/// it has bailed out, so waiting on them would leave the switch stuck.
///
/// Only applies to rack-level reprovisioning requests; non-rack-initiated
/// reprovisions are independent of the rack's lifecycle.
async fn rack_failed_abort_outcome(
    switch_id: &SwitchId,
    state: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<Option<StateHandlerOutcome<SwitchControllerState>>, StateHandlerError> {
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
        switch_id = %switch_id,
        rack_id = %rack_id,
        "Rack is in Error; aborting switch ReProvisioning and returning to Ready",
    );

    let mut txn = ctx.services.db_pool.begin().await?;
    db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id).await?;
    Ok(Some(
        StateHandlerOutcome::transition(SwitchControllerState::Ready).with_txn(txn),
    ))
}

/// Handles the ReProvisioning state for a switch.
pub async fn handle_reprovisioning(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let reprovisioning_state = match &state.controller_state.value {
        SwitchControllerState::ReProvisioning {
            reprovisioning_state,
        } => reprovisioning_state,
        _ => unreachable!("handle_reprovisioning called with non-ReProvisioning state"),
    };

    if let Some(outcome) = rack_failed_abort_outcome(switch_id, state, ctx).await? {
        return Ok(outcome);
    }

    match reprovisioning_state {
        ReProvisioningState::WaitingForRackFirmwareUpgrade => {
            let request = state
                .switch_reprovisioning_requested
                .as_ref()
                .expect("WaitingForRackFirmwareUpgrade requires a rack reprovision request");
            let requested_at = request.requested_at;
            let next_after_firmware = next_state_after_firmware(request);
            let Some(firmware_upgrade_status) = state.firmware_upgrade_status.as_ref() else {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for switch firmware upgrade status".into(),
                ));
            };
            if !firmware_upgrade_status.is_current_for(requested_at) {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for current rack firmware cycle".into(),
                ));
            }
            if !firmware_upgrade_status.is_terminal() {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for switch firmware completion".into(),
                ));
            }

            match &firmware_upgrade_status.status {
                model::rack::RackFirmwareUpgradeState::Completed => {
                    tracing::info!(
                        switch_id = %switch_id,
                        rack_id = ?state.rack_id,
                        backend_job_id = %firmware_upgrade_status.task_id,
                        next_reprovisioning_state = ?next_after_firmware,
                        returning_to_ready = next_after_firmware.is_none(),
                        "Switch rack firmware update completed"
                    );

                    if let Some(reprovisioning_state) = next_after_firmware {
                        return Ok(StateHandlerOutcome::transition(
                            SwitchControllerState::ReProvisioning {
                                reprovisioning_state,
                            },
                        ));
                    }

                    let mut txn = ctx.services.db_pool.begin().await?;
                    db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id)
                        .await?;
                    Ok(StateHandlerOutcome::transition(SwitchControllerState::Ready).with_txn(txn))
                }
                model::rack::RackFirmwareUpgradeState::Failed { cause } => {
                    tracing::warn!(
                        switch_id = %switch_id,
                        rack_id = ?state.rack_id,
                        backend_job_id = %firmware_upgrade_status.task_id,
                        %cause,
                        "Switch rack firmware update failed; transitioning to Error"
                    );

                    let mut txn = ctx.services.db_pool.begin().await?;
                    db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id)
                        .await?;
                    Ok(
                        StateHandlerOutcome::transition(SwitchControllerState::Error {
                            cause: cause.clone(),
                        })
                        .with_txn(txn),
                    )
                }
                model::rack::RackFirmwareUpgradeState::Started
                | model::rack::RackFirmwareUpgradeState::InProgress => Ok(
                    StateHandlerOutcome::wait("waiting for switch firmware completion".into()),
                ),
            }
        }
        ReProvisioningState::WaitingForNVOSUpgrade => {
            let request = state
                .switch_reprovisioning_requested
                .as_ref()
                .expect("WaitingForNVOSUpgrade requires a rack reprovision request");
            let requested_at = request.requested_at;
            let next_after_nvos = next_state_after_nvos(request);
            let Some(nvos_update_status) = state.nvos_update_status.as_ref() else {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for switch NVOS update status".into(),
                ));
            };
            if !nvos_update_status.is_current_for(requested_at) {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for current rack NVOS cycle".into(),
                ));
            }
            if !nvos_update_status.is_terminal() {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for switch NVOS completion".into(),
                ));
            }

            match &nvos_update_status.status {
                model::rack::SwitchNvosUpdateState::Completed => {
                    if let Some(reprovisioning_state) = next_after_nvos {
                        Ok(StateHandlerOutcome::transition(
                            SwitchControllerState::ReProvisioning {
                                reprovisioning_state,
                            },
                        ))
                    } else {
                        let mut txn = ctx.services.db_pool.begin().await?;
                        db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id)
                            .await?;
                        Ok(
                            StateHandlerOutcome::transition(SwitchControllerState::Ready)
                                .with_txn(txn),
                        )
                    }
                }
                model::rack::SwitchNvosUpdateState::Failed { cause } => {
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id)
                        .await?;
                    Ok(
                        StateHandlerOutcome::transition(SwitchControllerState::Error {
                            cause: cause.clone(),
                        })
                        .with_txn(txn),
                    )
                }
                model::rack::SwitchNvosUpdateState::Started
                | model::rack::SwitchNvosUpdateState::InProgress => Ok(StateHandlerOutcome::wait(
                    "waiting for switch NVOS completion".into(),
                )),
            }
        }
        ReProvisioningState::WaitingForNMXCConfigure => {
            let Some(fabric_manager_status) = state.fabric_manager_status.as_ref() else {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for switch fabric manager status".into(),
                ));
            };
            if fabric_manager_status.display_status() != "running" {
                if let Some(cause) = fabric_manager_status.error_message.as_ref() {
                    let mut txn = ctx.services.db_pool.begin().await?;
                    db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id)
                        .await?;
                    return Ok(
                        StateHandlerOutcome::transition(SwitchControllerState::Error {
                            cause: cause.clone(),
                        })
                        .with_txn(txn),
                    );
                }
                if let Some(reason) = fabric_manager_status.reason.as_ref() {
                    if !reason.is_empty() {
                        let mut txn = ctx.services.db_pool.begin().await?;
                        db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id)
                            .await?;
                        return Ok(
                            StateHandlerOutcome::transition(SwitchControllerState::Ready)
                                .with_txn(txn),
                        );
                    }
                    return Ok(StateHandlerOutcome::wait(reason.clone()));
                }
            }
            let mut txn = ctx.services.db_pool.begin().await?;
            db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id).await?;
            Ok(StateHandlerOutcome::transition(SwitchControllerState::Ready).with_txn(txn))
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use model::rack::MaintenanceActivity;
    use model::switch::{ReProvisioningState, SwitchReprovisionRequest};

    use super::{first_reprovisioning_state, next_state_after_firmware};

    fn request(activities: Vec<MaintenanceActivity>) -> SwitchReprovisionRequest {
        SwitchReprovisionRequest {
            // Timestamp is unused by activity selection helpers under test.
            requested_at: Default::default(),
            initiator: "rack-test".to_string(),
            activities,
        }
    }

    fn firmware() -> MaintenanceActivity {
        MaintenanceActivity::FirmwareUpgrade {
            firmware_version: None,
            components: vec![],
            force_update: false,
        }
    }

    fn nvos() -> MaintenanceActivity {
        MaintenanceActivity::NvosUpdate {
            config_json: String::new(),
        }
    }

    #[test]
    fn next_state_after_firmware_treats_empty_activities_as_all_phases() {
        value_scenarios!(
            run = |req| next_state_after_firmware(&req);
            "empty means all remaining phases" {
                request(vec![]) => Some(ReProvisioningState::WaitingForNVOSUpgrade),
            }

            "explicit NVOS still advances to NVOS" {
                request(vec![firmware(), nvos()]) => Some(ReProvisioningState::WaitingForNVOSUpgrade),
            }

            "firmware-only skips NVOS and later phases" {
                request(vec![firmware()]) => None,
            }

            "NMXC-only after firmware skips NVOS" {
                request(vec![MaintenanceActivity::ConfigureNmxCluster])
                    => Some(ReProvisioningState::WaitingForNMXCConfigure),
            }
        );
    }

    #[test]
    fn first_reprovisioning_state_treats_empty_activities_as_all_phases() {
        value_scenarios!(
            run = |req| first_reprovisioning_state(&req);
            "empty starts at firmware wait" {
                request(vec![]) => Some(ReProvisioningState::WaitingForRackFirmwareUpgrade),
            }

            "NVOS-only starts at NVOS wait" {
                request(vec![nvos()]) => Some(ReProvisioningState::WaitingForNVOSUpgrade),
            }

            "firmware-only starts at firmware wait" {
                request(vec![firmware()]) => Some(ReProvisioningState::WaitingForRackFirmwareUpgrade),
            }
        );
    }
}
