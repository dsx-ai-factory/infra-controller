// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers used across component-manager backends and state controllers.

use std::net::IpAddr;

use carbide_secrets::credentials::Credentials;
use mac_address::MacAddress;
use model::machine::PowerState;

/// Management-controller endpoint for a compute tray, switch, or power shelf.
/// Carries controller credentials; switch backends receive NVOS details in `SwitchEndpoint`.
#[derive(Debug, Clone)]
pub struct BmcEndpoint {
    /// Vendor used to select the controller implementation.
    pub vendor: BmcVendor,
    /// Controller address used for the connection.
    pub bmc_ip: IpAddr,
    /// Controller MAC identifying the requested device.
    pub bmc_mac: MacAddress,
    /// Credentials for direct controller authentication.
    pub bmc_credentials: Credentials,
}

/// Controller vendor supported by the component backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmcVendor {
    /// Unknown or otherwise unlisted vendor.
    Unknown,
    /// Dell controller.
    Dell,
    /// Hewlett Packard Enterprise controller.
    Hpe,
    /// Lenovo controller.
    Lenovo,
    /// Supermicro controller.
    Supermicro,
    /// NVIDIA controller.
    Nvidia,
}

impl From<bmc_vendor::BMCVendor> for BmcVendor {
    fn from(vendor: bmc_vendor::BMCVendor) -> Self {
        match vendor {
            bmc_vendor::BMCVendor::Dell => Self::Dell,
            bmc_vendor::BMCVendor::Hpe => Self::Hpe,
            bmc_vendor::BMCVendor::Lenovo => Self::Lenovo,
            bmc_vendor::BMCVendor::Supermicro => Self::Supermicro,
            bmc_vendor::BMCVendor::Nvidia => Self::Nvidia,
            bmc_vendor::BMCVendor::LenovoAMI
            | bmc_vendor::BMCVendor::GigaComputing
            | bmc_vendor::BMCVendor::Liteon
            | bmc_vendor::BMCVendor::Delta
            | bmc_vendor::BMCVendor::Sushy
            | bmc_vendor::BMCVendor::Unknown => Self::Unknown,
        }
    }
}

/// Outcome of a command for one management-controller endpoint.
#[derive(Debug, Clone)]
pub struct BmcOperationResult {
    /// Controller address supplied with the request.
    pub bmc_ip: IpAddr,
    /// Controller MAC supplied with the request.
    pub bmc_mac: MacAddress,
    /// Whether the backend accepted the command.
    pub success: bool,
    /// Failure detail when the command was not accepted.
    pub error: Option<String>,

    /// Opaque backend job ID returned for an asynchronous operation.
    ///
    /// `None` means the backend returned no durable job handle. Presence does
    /// not imply success; a failed result may retain the ID for reconciliation.
    pub backend_job_id: Option<String>,
}

/// A managed component within a rack's compute trays, switches, or power shelves.
#[derive(Clone, Copy, Debug)]
pub enum RackComponent {
    /// Compute BMC
    ComputeBmc,
    /// Switch BMC
    SwitchBmc,
    /// Switch NVOS
    SwitchNvos,
    /// Powershelf PMC
    PowerShelfPmc,
}

/// Backend providing fresh power observations without registering or mutating devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerStateSource {
    /// Direct BMC Redfish reads.
    Redfish,
    /// Live reads through Rack Management Service.
    Rms,
}

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
