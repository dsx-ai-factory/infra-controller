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

use carbide_state_controller_common::config::StateControllerConfig;
use carbide_utils::config::as_duration;
use chrono::Duration;
use duration_str::deserialize_duration_chrono;
use serde::{Deserialize, Serialize};

/// Deserializes a recurring interval and rejects zero or negative durations.
fn deserialize_positive_duration_chrono<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let duration = deserialize_duration_chrono(deserializer)?;
    if duration <= Duration::zero() {
        return Err(serde::de::Error::custom(
            "duration must be greater than zero",
        ));
    }

    Ok(duration)
}

/// MachineStateController related config.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MachineStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,

    /// How long should we wait before a DPU goes down for sure.
    #[serde(
        default = "MachineStateControllerConfig::dpu_wait_time_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub dpu_wait_time: Duration,
    /// How long to wait for after power down before power on the machine.
    #[serde(
        default = "MachineStateControllerConfig::power_down_wait_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub power_down_wait: Duration,
    /// After how much time, state machine should retrigger reboot if machine does not call back.
    #[serde(
        default = "MachineStateControllerConfig::failure_retry_time_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub failure_retry_time: Duration,
    /// How long to wait for a health report from the DPU before we assume it's down
    #[serde(
        default = "MachineStateControllerConfig::dpu_up_threshold_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub dpu_up_threshold: Duration,
    /// Duration after which a host is considered unhealthy if scout hasn't reported back
    #[serde(
        default = "MachineStateControllerConfig::scout_reporting_timeout_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub scout_reporting_timeout: Duration,
    /// How long a host may remain in WaitingForMeasurements before being
    /// escalated to Failed. A healthy fleet completes this state in minutes,
    /// so a generous ceiling still catches hosts that are silently stuck.
    #[serde(
        default = "MachineStateControllerConfig::waiting_for_measurements_timeout_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub waiting_for_measurements_timeout: Duration,
    /// How long to wait for UEFI boot to complete after rebooting a host
    #[serde(
        default = "MachineStateControllerConfig::uefi_boot_wait_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub uefi_boot_wait: Duration,
    /// Retry budget for automated host boot-configuration convergence, shared
    /// across BIOS recovery and boot-order verification. The field name is
    /// retained for configuration compatibility.
    #[serde(default = "MachineStateControllerConfig::max_bios_config_retries_default")]
    pub max_bios_config_retries: u32,
    /// Allow new Lenovo GB300 full-lockdown recovery states. Enable only after
    /// all state readers support RestoreFullLockdown. Disabling new entries
    /// does not stop recovery already in progress. See the host firmware guide
    /// for the additional state-drain requirements before an older-code rollback.
    #[serde(default)]
    pub full_lockdown_recovery_enabled: bool,
    /// How long PollingBiosSetup may sit on Ok(false) before escalating into
    /// HandleBiosJobFailure recovery.
    #[serde(
        default = "MachineStateControllerConfig::polling_bios_setup_stuck_threshold_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub polling_bios_setup_stuck_threshold: Duration,
    /// How long a verified desired boot interface may go before the controller
    /// compares it with Redfish again.
    #[serde(
        default = "MachineStateControllerConfig::boot_interface_observation_interval_default",
        deserialize_with = "deserialize_positive_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub boot_interface_observation_interval: Duration,
}

impl MachineStateControllerConfig {
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_default() -> Self {
        Self {
            dpu_wait_time: Duration::seconds(1),
            power_down_wait: Duration::seconds(1),
            failure_retry_time: Duration::seconds(1),
            dpu_up_threshold: Duration::weeks(52),
            controller: StateControllerConfig::default(),
            scout_reporting_timeout: Duration::weeks(52),
            waiting_for_measurements_timeout: Duration::weeks(52),
            uefi_boot_wait: Duration::seconds(0),
            max_bios_config_retries: MachineStateControllerConfig::max_bios_config_retries_default(
            ),
            full_lockdown_recovery_enabled: false,
            polling_bios_setup_stuck_threshold:
                MachineStateControllerConfig::polling_bios_setup_stuck_threshold_default(),
            // Keep periodic Redfish reads out of unrelated controller tests.
            // Focused tests explicitly age the observation they exercise.
            boot_interface_observation_interval: Duration::weeks(52),
        }
    }

    pub fn dpu_wait_time_default() -> Duration {
        Duration::minutes(5)
    }

    pub fn power_down_wait_default() -> Duration {
        Duration::minutes(2)
    }

    pub fn failure_retry_time_default() -> Duration {
        Duration::minutes(90)
    }

    pub fn dpu_up_threshold_default() -> Duration {
        Duration::minutes(5)
    }

    fn scout_reporting_timeout_default() -> Duration {
        Duration::minutes(5)
    }

    pub fn waiting_for_measurements_timeout_default() -> Duration {
        Duration::hours(4)
    }

    pub fn uefi_boot_wait_default() -> Duration {
        Duration::minutes(5)
    }

    pub fn max_bios_config_retries_default() -> u32 {
        3
    }

    pub fn polling_bios_setup_stuck_threshold_default() -> Duration {
        Duration::minutes(15)
    }

    /// Default cadence for rechecking an already-verified boot interface.
    pub fn boot_interface_observation_interval_default() -> Duration {
        Duration::minutes(10)
    }
}

impl Default for MachineStateControllerConfig {
    fn default() -> Self {
        Self {
            controller: StateControllerConfig::default(),
            dpu_wait_time: MachineStateControllerConfig::dpu_wait_time_default(),
            power_down_wait: MachineStateControllerConfig::power_down_wait_default(),
            failure_retry_time: MachineStateControllerConfig::failure_retry_time_default(),
            dpu_up_threshold: MachineStateControllerConfig::dpu_up_threshold_default(),
            scout_reporting_timeout: MachineStateControllerConfig::scout_reporting_timeout_default(
            ),
            waiting_for_measurements_timeout:
                MachineStateControllerConfig::waiting_for_measurements_timeout_default(),
            uefi_boot_wait: MachineStateControllerConfig::uefi_boot_wait_default(),
            max_bios_config_retries: MachineStateControllerConfig::max_bios_config_retries_default(
            ),
            full_lockdown_recovery_enabled: false,
            polling_bios_setup_stuck_threshold:
                MachineStateControllerConfig::polling_bios_setup_stuck_threshold_default(),
            boot_interface_observation_interval:
                MachineStateControllerConfig::boot_interface_observation_interval_default(),
        }
    }
}
