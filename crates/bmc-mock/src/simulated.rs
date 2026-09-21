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

use crate::{Callbacks, MockPowerState, POWER_CYCLE_DELAY, ResourceResetType, SetSystemPowerError};

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

    fn send_power_command(&self, reset_type: ResourceResetType) -> Result<(), SetSystemPowerError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_power_state_without_an_external_machine() {
        let callbacks = SimulatedCallbacks::new();
        assert!(matches!(callbacks.get_power_state(), MockPowerState::On));

        callbacks
            .set_power_state(ResourceResetType::ForceOff)
            .unwrap();
        assert!(matches!(callbacks.get_power_state(), MockPowerState::Off));

        callbacks.set_power_state(ResourceResetType::On).unwrap();
        assert!(matches!(callbacks.get_power_state(), MockPowerState::On));
    }

    #[test]
    fn power_on_is_rejected_while_a_transition_is_in_flight() {
        // `set_power_state` guards every backend: while the host is coming up or
        // going down, a further power-on is a 400, never a silently dropped request.
        for state in [MockPowerState::PoweringOn, MockPowerState::PoweringOff] {
            let callbacks = SimulatedCallbacks::new();
            *callbacks.power_state.lock().unwrap() = state;
            for control in [ResourceResetType::On, ResourceResetType::ForceOn] {
                assert!(
                    matches!(
                        callbacks.set_power_state(control),
                        Err(SetSystemPowerError::BadRequest(_))
                    ),
                    "{control:?} during {state:?} must be rejected"
                );
            }
            assert!(
                matches!(callbacks.get_power_state(), s if std::mem::discriminant(&s) == std::mem::discriminant(&state)),
                "a rejected request must not change the state"
            );
        }
    }

    #[test]
    fn completes_a_power_cycle_after_the_delay() {
        let callbacks = SimulatedCallbacks::new();
        callbacks
            .set_power_state(ResourceResetType::PowerCycle)
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
