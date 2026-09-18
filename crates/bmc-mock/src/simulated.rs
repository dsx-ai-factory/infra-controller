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

use crate::{
    BootConfigPatch, CallbackError, Callbacks, InMemorySystemState, MockPowerState,
    POWER_CYCLE_DELAY, SetSystemPowerError, SystemPowerControl, SystemStateData, VirtualMediaState,
};

/// Stateful callbacks for a generated BMC that is not connected to a real or
/// virtual machine. This is useful for modeling independently addressable
/// devices such as a DPU BMC.
#[derive(Debug, Default)]
pub struct SimulatedCallbacks {
    system_state: InMemorySystemState,
    power_state: Mutex<MockPowerState>,
}

impl SimulatedCallbacks {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Callbacks for SimulatedCallbacks {
    fn initialize_system(&self, system_id: &str, initial: SystemStateData) {
        self.system_state.initialize_system(system_id, initial);
    }

    async fn get_system_state(&self, system_id: &str) -> Result<SystemStateData, CallbackError> {
        self.system_state.get_system_state(system_id)
    }

    async fn set_boot_config(
        &self,
        system_id: &str,
        patch: BootConfigPatch,
    ) -> Result<(), CallbackError> {
        self.system_state.set_boot_config(system_id, patch)
    }

    async fn set_virtual_media(
        &self,
        system_id: &str,
        desired: VirtualMediaState,
    ) -> Result<(), CallbackError> {
        self.system_state.set_virtual_media(system_id, desired)
    }

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

    fn send_power_command(
        &self,
        reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        use SystemPowerControl::*;

        let new_state = match reset_type {
            On | ForceOn | GracefulRestart | ForceRestart | PushPowerButton | Pause | Resume => {
                MockPowerState::On
            }
            GracefulShutdown | ForceOff | Nmi | Suspend => MockPowerState::Off,
            PowerCycle => MockPowerState::PowerCycling {
                since: Instant::now(),
            },
        };
        *self.power_state.lock().unwrap() = new_state;
        Ok(())
    }

    fn state_refresh_indication(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_power_state_without_an_external_machine() {
        let callbacks = SimulatedCallbacks::new();
        assert!(matches!(callbacks.get_power_state(), MockPowerState::On));

        callbacks
            .set_power_state(SystemPowerControl::ForceOff)
            .unwrap();
        assert!(matches!(callbacks.get_power_state(), MockPowerState::Off));

        callbacks.set_power_state(SystemPowerControl::On).unwrap();
        assert!(matches!(callbacks.get_power_state(), MockPowerState::On));
    }

    #[test]
    fn completes_a_power_cycle_after_the_delay() {
        let callbacks = SimulatedCallbacks::new();
        callbacks
            .set_power_state(SystemPowerControl::PowerCycle)
            .unwrap();
        assert!(matches!(
            callbacks.get_power_state(),
            MockPowerState::PowerCycling { .. }
        ));

        *callbacks.power_state.lock().unwrap() = MockPowerState::PowerCycling {
            since: Instant::now() - POWER_CYCLE_DELAY,
        };
        assert!(matches!(callbacks.get_power_state(), MockPowerState::On));
    }
}
