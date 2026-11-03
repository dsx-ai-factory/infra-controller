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

use model::power_manager::{PowerOptions, PowerState};

use crate as rpc;

impl From<rpc::forge::PowerState> for PowerState {
    fn from(value: rpc::forge::PowerState) -> Self {
        match value {
            rpc::forge::PowerState::On => PowerState::On,
            rpc::forge::PowerState::Off => PowerState::Off,
            rpc::forge::PowerState::PowerManagerDisabled => PowerState::PowerManagerDisabled,
        }
    }
}

impl From<PowerState> for rpc::forge::PowerState {
    fn from(value: PowerState) -> Self {
        match value {
            PowerState::Off => rpc::forge::PowerState::Off,
            PowerState::On => rpc::forge::PowerState::On,
            PowerState::PowerManagerDisabled => rpc::forge::PowerState::PowerManagerDisabled,
        }
    }
}

impl From<PowerOptions> for rpc::forge::PowerOptions {
    fn from(value: PowerOptions) -> Self {
        Self {
            desired_state: rpc::forge::PowerState::from(value.desired_power_state) as i32,
            desired_state_updated_at: Some(value.desired_power_state_version.timestamp().into()),
            actual_state: rpc::forge::PowerState::from(
                if value.desired_power_state == PowerState::PowerManagerDisabled {
                    PowerState::PowerManagerDisabled
                } else {
                    value.last_fetched_power_state
                },
            ) as i32,
            actual_state_updated_at: Some(value.last_fetched_updated_at.into()),
            host_id: Some(value.host_id),
            desired_power_state_version: value.desired_power_state_version.to_string(),
            next_power_state_fetch_at: Some(value.last_fetched_next_try_at.into()),
            off_counter: value.last_fetched_off_counter,
            tried_triggering_on_at: value.tried_triggering_on_at.map(|x| x.into()),
            tried_triggering_on_counter: value.tried_triggering_on_counter,
            wait_until_time_before_performing_next_power_action: Some(
                value
                    .wait_until_time_before_performing_next_power_action
                    .into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use chrono::Utc;
    use config_version::ConfigVersion;

    use super::*;

    #[test]
    fn power_options_actual_state_respects_disabled_policy() {
        let now = Utc::now();
        value_scenarios!(
            run = |(desired_power_state, last_fetched_power_state)| {
                let options = PowerOptions {
                    host_id: "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg".parse().unwrap(),
                    last_fetched_updated_at: now,
                    last_fetched_next_try_at: now,
                    last_fetched_power_state,
                    last_fetched_off_counter: 0,
                    desired_power_state_version: ConfigVersion::initial(),
                    desired_power_state,
                    wait_until_time_before_performing_next_power_action: now,
                    tried_triggering_on_at: None,
                    tried_triggering_on_counter: 0,
                };
                let response = rpc::forge::PowerOptions::from(options);
                assert_eq!(response.desired_state(), rpc::forge::PowerState::from(desired_power_state));
                assert_eq!(response.actual_state_updated_at, Some(now.into()));
                response.actual_state()
            };
            "disabled policy masks the cached observation" {
                (PowerState::PowerManagerDisabled, PowerState::On) => rpc::forge::PowerState::PowerManagerDisabled,
            }
            "enabled policy reports the observation rather than the desired state" {
                (PowerState::On, PowerState::Off) => rpc::forge::PowerState::Off,
                (PowerState::Off, PowerState::On) => rpc::forge::PowerState::On,
            }
        );
    }
}
