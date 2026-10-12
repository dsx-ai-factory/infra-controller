// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers used across component-manager backends and state controllers.

use mac_address::MacAddress;
use model::machine::PowerState;

/// `ComponentPowerStateResult` identifies a component and its power observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentPowerStateResult {
    /// Management-controller MAC: BMC for compute trays/switches, PMC for shelves.
    pub mac_address: MacAddress,
    /// `Ok(Some(_))` is an observation, including an explicit `Unknown` state.
    /// `Ok(None)` means the backend supplied no state; `Err` describes a failed
    /// observation. Neither should replace a previously observed state.
    pub power_state: Result<Option<PowerState>, String>,
}

/// `PowerStatePollOutcome` describes a single-component `get_power_state` poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerStatePollOutcome {
    /// The backend reported a state, including an explicit `Unknown` value.
    Observed(PowerState),
    /// The backend failed to observe the state, with a description of the error.
    BackendError(String),
    /// The backend returned an entry without a power observation.
    NoPowerState,
    /// The backend returned no entries.
    NoResult,
}

/// `interpret_power_state_poll` reads the first entry from a component-manager
/// response. Callers polling one component use this to decide whether to update
/// its stored observation; any additional entries are ignored.
pub fn interpret_power_state_poll(
    results: Vec<ComponentPowerStateResult>,
) -> PowerStatePollOutcome {
    let Some(result) = results.into_iter().next() else {
        return PowerStatePollOutcome::NoResult;
    };

    match result.power_state {
        Ok(Some(power_state)) => PowerStatePollOutcome::Observed(power_state),
        Ok(None) => PowerStatePollOutcome::NoPowerState,
        Err(error) => PowerStatePollOutcome::BackendError(error),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    fn observation(power_state: Result<Option<PowerState>, String>) -> ComponentPowerStateResult {
        ComponentPowerStateResult {
            mac_address: MacAddress::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
            power_state,
        }
    }

    #[test]
    fn power_state_poll_preserves_observations_and_missing_states() {
        value_scenarios!(interpret_power_state_poll:
            "reported states" {
                vec![observation(Ok(Some(PowerState::Unknown)))]
                    => PowerStatePollOutcome::Observed(PowerState::Unknown),
            }
            "missing observations" {
                vec![observation(Ok(None))] => PowerStatePollOutcome::NoPowerState,
                vec![observation(Err("rms failed".into()))]
                    => PowerStatePollOutcome::BackendError("rms failed".into()),
                vec![] => PowerStatePollOutcome::NoResult,
            }
            "only the first entry is used" {
                vec![observation(Ok(None)), observation(Ok(Some(PowerState::On)))]
                    => PowerStatePollOutcome::NoPowerState,
            }
        );
    }
}
