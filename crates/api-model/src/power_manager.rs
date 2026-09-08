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

use carbide_uuid::machine::HostMachineId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::machine::{DpuInitState, ManagedHostState, ManagedHostStateSnapshot};

/// Representing DPU state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "host_power_state_t")]
pub enum PowerState {
    On,
    Off,
    PowerManagerDisabled,
}

pub struct PowerHandlingOutcome {
    pub power_options: Option<PowerOptions>,
    pub continue_state_machine: bool,
    pub msg: Option<String>,
}

impl PowerHandlingOutcome {
    pub fn new(
        power_options: Option<PowerOptions>,
        continue_state_machine: bool,
        msg: Option<String>,
    ) -> Self {
        Self {
            power_options,
            continue_state_machine,
            msg,
        }
    }
}

/// Represents the power management options for a specific host, including
/// details about the last fetched power information, the desired power state,
/// and the status of triggering power-on operations.
/// Carbide will poll for the actual power state of the machine, once in a 5 mins.
/// `next_try_at` will be now()+5 mins if power state is On. If machine is Off, next_try will be
/// now()+2 mins, if desired state is On. If machine remains off for 2 cycles (2+2 mins), carbide
/// would take the next decision.
/// If power manager tried to power on the host, wait until DPUs are up or wait_expiry_time is
/// expired (which is around 15 mins). If DPUs come up by this time, reboot the host, else ignore
/// the handling and move to the state handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerOptions {
    pub host_id: HostMachineId,
    pub last_fetched_updated_at: DateTime<Utc>,
    pub last_fetched_next_try_at: DateTime<Utc>,
    pub last_fetched_power_state: PowerState,
    /// Once counter is incremented >= 2, the machine will be assumed off.
    /// This is needed to avoid power off done by state machine for the recovery mechanism.
    pub last_fetched_off_counter: i32,
    pub desired_power_state_version: ConfigVersion,
    /// Tenant/SRE can set the desired power option.
    /// If there is some operation is being performed on any host, make the desired state
    /// off. Carbide won't try to turn on the machine and process any event in state machine.
    /// If desired state is On and machines state is Off, carbide will try to turn-on the machine.
    pub desired_power_state: PowerState,
    /// In the case if state machine decides to power on the host, state machine must wait until
    /// the DPUs come up and again reboot the host to force it to boot via pxe.
    pub wait_until_time_before_performing_next_power_action: DateTime<Utc>,
    /// If tried_triggering_on_at is some and last_fetched.power_state is not On and
    /// tried_triggering_on_at < last_fetched.updated_at, try powering on again.
    /// Reset it when host's power state is detected as On.
    pub tried_triggering_on_at: Option<DateTime<Utc>>,
    /// Increment it every time you try to power-on the host.
    /// Reset it when host's power state is detected as On.
    pub tried_triggering_on_counter: i32,
}

/// This function returns updated power options and boolean value which indicates if a power on is
/// needed or not.
pub fn get_updated_power_options_for_desired_on_state_off(
    mut updated_power_options: PowerOptions,
    next_try_duration_on_failure: chrono::TimeDelta,
    wait_duration_until_host_reboot: chrono::TimeDelta,
    now: DateTime<Utc>,
    last_fetched_off_counter: i32,
) -> (PowerHandlingOutcome, bool) {
    let mut try_power_on = false;
    updated_power_options.last_fetched_power_state = PowerState::Off;
    // In case of mismatch, next try can be soon to avoid delay.
    updated_power_options.last_fetched_next_try_at = now + next_try_duration_on_failure;
    // Carbide found the host OFF for at least 2 cycles.
    if last_fetched_off_counter >= 2 && updated_power_options.tried_triggering_on_counter < 3 {
        // Try power on here.
        try_power_on = true;
        updated_power_options.tried_triggering_on_at = Some(now);
        updated_power_options.wait_until_time_before_performing_next_power_action =
            now + wait_duration_until_host_reboot;
        updated_power_options.tried_triggering_on_counter += 1;
    }
    updated_power_options.last_fetched_off_counter += 1;
    let cause =
        "Since desired state is On, but actual state is Off, skipping state machine.".to_string();
    tracing::warn!(reason = %cause, "Skipping state machine");
    (
        PowerHandlingOutcome::new(Some(updated_power_options), false, Some(cause)),
        try_power_on,
    )
}

// Reset the counters and updated next_try_at counter.
pub fn update_power_options_for_desired_on_state_on(
    updated_power_options: &mut PowerOptions,
    next_try_duration_on_success: chrono::TimeDelta,
    now: DateTime<Utc>,
) {
    updated_power_options.last_fetched_power_state = PowerState::On;
    updated_power_options.last_fetched_next_try_at = now + next_try_duration_on_success;
    updated_power_options.tried_triggering_on_counter = 0;
    updated_power_options.last_fetched_off_counter = 0;
}

// Check if DPU sent network observation after power on.
pub fn are_all_dpus_up_after_power_operation(
    mh_snapshot: &ManagedHostStateSnapshot,
    new_power_options: Option<PowerOptions>,
    current_power_options: &PowerOptions,
) -> Option<PowerHandlingOutcome> {
    if let ManagedHostState::DPUInit { dpu_states } = &mh_snapshot.host_snapshot.state.value {
        // DPU arm OS installation is not done yet. Shouldn't wait for DPUs to come up.
        if dpu_states.states.values().any(|x| *x <= DpuInitState::Init) {
            return None;
        }
    }

    // Waiting for DPU to come up.
    for dpu_snapshot in &mh_snapshot.dpu_snapshots {
        let observation_time = dpu_snapshot
            .network_status_observation
            .as_ref()
            .map(|o| o.observed_at)
            .unwrap_or(DateTime::<Utc>::MIN_UTC);

        let base_time =
            if let Some(last_tried_triggering_on) = current_power_options.tried_triggering_on_at {
                last_tried_triggering_on
            } else {
                current_power_options.last_fetched_updated_at
            };

        if observation_time < base_time {
            return Some(PowerHandlingOutcome::new(
                new_power_options,
                false,
                Some(format!(
                    "Waiting for all DPUs to come up. At least {} is not up.",
                    dpu_snapshot.id
                )),
            ));
        }
    }
    None
}

impl<'r> FromRow<'r, PgRow> for PowerOptions {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let host_id: HostMachineId = row.try_get("host_id")?;
        let last_fetched_updated_at = row.try_get("last_fetched_updated_at")?;
        let last_fetched_next_try_at = row.try_get("last_fetched_next_try_at")?;
        let last_fetched_power_state = row.try_get("last_fetched_power_state")?;
        let last_fetched_off_counter = row.try_get("last_fetched_off_counter")?;
        let desired_state_version: String = row.try_get("desired_power_state_version")?;
        let desired_power_state_version =
            desired_state_version
                .parse()
                .map_err(|e| sqlx::error::Error::ColumnDecode {
                    index: "version".to_string(),
                    source: Box::new(e),
                })?;
        let desired_power_state = row.try_get("desired_power_state")?;
        let wait_until_time_before_performing_next_power_action =
            row.try_get("wait_until_time_before_performing_next_power_action")?;
        let tried_triggering_on_at: Option<DateTime<Utc>> =
            row.try_get("tried_triggering_on_at").ok();
        let tried_triggering_on_counter = row.try_get("tried_triggering_on_counter")?;

        Ok(Self {
            host_id,
            last_fetched_updated_at,
            last_fetched_next_try_at,
            last_fetched_power_state,
            last_fetched_off_counter,
            desired_power_state_version,
            desired_power_state,
            wait_until_time_before_performing_next_power_action,
            tried_triggering_on_at,
            tried_triggering_on_counter,
        })
    }
}
