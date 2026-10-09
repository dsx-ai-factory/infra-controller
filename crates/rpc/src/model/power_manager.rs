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
            actual_state: rpc::forge::PowerState::from(value.last_fetched_power_state) as i32,
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
