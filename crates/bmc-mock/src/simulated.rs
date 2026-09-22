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

use std::sync::Mutex;

use tokio::time::Instant;

use crate::{ActionError, Callbacks, MockPowerState, POWER_CYCLE_DELAY, ResourceResetType};

/// Stateful callbacks for a generated BMC that is not connected to a real or
/// virtual machine. This is useful for modeling independently addressable
/// devices such as a DPU BMC.
#[derive(Debug, Default)]
pub struct SimulatedCallbacks {
    power_state: Mutex<MockPowerState>,
}

impl SimulatedCallbacks {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Callbacks for SimulatedCallbacks {
    fn get_power_state(&self) -> MockPowerState {
        let mut state = self.power_state.lock().unwrap();
        if matches!(
            *state,
            MockPowerState::PowerCycling { since } if since.elapsed() >= POWER_CYCLE_DELAY
        ) {
            *state = MockPowerState::On;
        }
        *state
    }

    async fn computer_system_reset(
        &self,
        reset_type: ResourceResetType,
    ) -> Result<(), ActionError> {
        self.get_power_state().validate_reset_type(reset_type)?;
        use ResourceResetType::*;

        let new_state = match reset_type {
            On | ForceOn | GracefulRestart | ForceRestart | PushPowerButton | Pause | Resume => {
                Some(MockPowerState::On)
            }
            GracefulShutdown | ForceOff | Nmi | Suspend | Sleep | Hibernate => {
                Some(MockPowerState::Off)
            }
            PowerCycle | FullPowerCycle => Some(MockPowerState::PowerCycling {
                since: Instant::now(),
            }),
            UnsupportedValue => None,
        };
        if let Some(new_state) = new_state {
            *self.power_state.lock().unwrap() = new_state;
        }
        Ok(())
    }

    fn state_refresh_indication(&self) {}
}
