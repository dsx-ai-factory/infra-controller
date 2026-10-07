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

use chrono::Utc;
use libredfish::SystemPowerControl;
use model::machine::ManagedHostStateSnapshot;
use model::power_manager::{
    PowerHandlingOutcome, PowerOptions, PowerState, are_all_dpus_up_after_power_operation,
    get_updated_power_options_for_desired_on_state_off,
    update_power_options_for_desired_on_state_on,
};
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::MachineStateHandlerContextObjects;
use crate::handler::{PowerOptionConfig, handler_host_power_control, host_power_state};

// If power state is Paused and Reset, state machine can't take any decision on it.
// Ignore power manager with a log and moved to state machine.
enum UsablePowerState {
    Usable(PowerState),
    NotUsable(libredfish::PowerState),
}

// Handle power related stuff and return updated power options.
pub(super) async fn handle_power(
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    power_options_config: &PowerOptionConfig,
) -> Result<PowerHandlingOutcome, StateHandlerError> {
    if let Some(power_options) = &mh_snapshot.host_snapshot.status.power_options {
        match power_options.desired_power_state {
            model::power_manager::PowerState::On => {
                handle_power_desired_on(power_options, mh_snapshot, ctx, power_options_config).await
            }
            model::power_manager::PowerState::Off => {
                get_updated_power_options_desired_off(
                    power_options,
                    mh_snapshot,
                    ctx,
                    power_options_config,
                )
                .await
            }
            model::power_manager::PowerState::PowerManagerDisabled => {
                // Nothing to do
                Ok(PowerHandlingOutcome::new(None, true, None))
            }
        }
    } else {
        tracing::warn!(
            machine_id = %mh_snapshot.host_snapshot.id,
            "Power options are not available for host"
        );
        Ok(PowerHandlingOutcome::new(None, true, None))
    }
}

pub(super) async fn handle_power_desired_on(
    current_power_options: &PowerOptions,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    power_options_config: &PowerOptionConfig,
) -> Result<PowerHandlingOutcome, StateHandlerError> {
    let mut update_done = false;
    let mut updated_power_options = current_power_options.clone();
    let now = Utc::now();
    if now > current_power_options.last_fetched_next_try_at {
        // Time to fetch the next power state.
        let power_state = get_power_state(mh_snapshot, ctx).await?;

        // Update the power options.
        updated_power_options.last_fetched_updated_at = now;
        updated_power_options.last_fetched_next_try_at =
            now + power_options_config.next_try_duration_on_success;
        match power_state {
            UsablePowerState::Usable(PowerState::Off) => {
                let (ret_val, try_power_on) = get_updated_power_options_for_desired_on_state_off(
                    updated_power_options,
                    power_options_config.next_try_duration_on_failure,
                    power_options_config.wait_duration_until_host_reboot,
                    now,
                    current_power_options.last_fetched_off_counter,
                );
                if try_power_on {
                    // Try power on here.
                    handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::On).await?;
                }

                return Ok(ret_val);
            }
            UsablePowerState::Usable(PowerState::On) => {
                update_power_options_for_desired_on_state_on(
                    &mut updated_power_options,
                    power_options_config.next_try_duration_on_success,
                    now,
                );
                update_done = true;
            }
            UsablePowerState::Usable(PowerState::PowerManagerDisabled) => {
                tracing::warn!("Unexpected PowerManagerDisabled state from BMC poll");
            }
            UsablePowerState::NotUsable(s) => {
                tracing::warn!(
                    power_state = %s,
                    "Not usable power state. Since desired state is On, continuing state machine. Will check in next cycle."
                );
                return Ok(PowerHandlingOutcome::new(
                    Some(updated_power_options),
                    true,
                    None,
                ));
            }
        }
    };

    let new_power_options = if update_done {
        Some(updated_power_options.clone())
    } else {
        None
    };

    if now < current_power_options.wait_until_time_before_performing_next_power_action {
        let ret = are_all_dpus_up_after_power_operation(
            mh_snapshot,
            new_power_options,
            current_power_options,
        );

        if let Some(handled_power_options) = ret {
            return Ok(handled_power_options);
        }

        // all DPUs are UP or don't wait for the DPUs. Reboot the host;
        handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart).await?;

        updated_power_options.wait_until_time_before_performing_next_power_action = now;
        return Ok(PowerHandlingOutcome::new(
            Some(updated_power_options),
            false,
            Some("Carbide will reboot host after DPU came up.".to_string()),
        ));
    }

    // Should we prevent state machine to continue until actual power state is On?
    Ok(PowerHandlingOutcome::new(new_power_options, true, None))
}

pub(super) async fn get_updated_power_options_desired_off(
    current_power_options: &PowerOptions,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    power_options_config: &PowerOptionConfig,
) -> Result<PowerHandlingOutcome, StateHandlerError> {
    let now = Utc::now();
    if now > current_power_options.last_fetched_next_try_at {
        // Time to fetch the next power state.
        let power_state = get_power_state(mh_snapshot, ctx).await?;
        // In phase 1, let's not power off the host but leave it as such without processing any
        // event. State machine assumes that SRE has manually powered-off the host.

        // Update the power options.
        let mut updated_power_options = current_power_options.clone();
        let now = Utc::now();
        updated_power_options.last_fetched_updated_at = now;
        updated_power_options.last_fetched_next_try_at =
            now + power_options_config.next_try_duration_on_success;
        match power_state {
            UsablePowerState::Usable(power_state) => {
                updated_power_options.last_fetched_power_state = power_state;
                let cause = if let PowerState::On = power_state {
                    "Power state is On while expected is Off. Since desired state is Off, not processing any event.".to_string()
                } else {
                    "Desired state is Off and actual state is Off.".to_string()
                };
                if let PowerState::On = power_state {
                    tracing::warn!(reason = %cause, "Skipping host power handling");
                }
                return Ok(PowerHandlingOutcome::new(
                    Some(updated_power_options),
                    false,
                    Some(cause),
                ));
            }
            UsablePowerState::NotUsable(s) => {
                let cause = format!(
                    "Not usable power state {s}. Since desired state is Off, not processing any event."
                );
                tracing::warn!(reason = %cause, "Skipping host power handling");
                return Ok(PowerHandlingOutcome::new(
                    Some(updated_power_options),
                    false,
                    Some(cause),
                ));
            }
        }
    };

    Ok(PowerHandlingOutcome::new(
        None,
        false,
        Some("Desired state is Off.".to_string()),
    ))
}

// Fetch actual power state.
async fn get_power_state(
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<UsablePowerState, StateHandlerError> {
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;
    let power_state = host_power_state(redfish_client.as_ref()).await?;
    Ok(match power_state {
        libredfish::PowerState::Off | libredfish::PowerState::PoweringOff => {
            UsablePowerState::Usable(PowerState::Off)
        }
        libredfish::PowerState::On | libredfish::PowerState::PoweringOn => {
            UsablePowerState::Usable(PowerState::On)
        }
        libredfish::PowerState::Paused
        | libredfish::PowerState::Reset
        | libredfish::PowerState::Unknown => UsablePowerState::NotUsable(power_state),
    })
}
