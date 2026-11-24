/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Power mechanics shared by several vendor drivers.

use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power};
use nv_redfish::chassis::PowerSupply;
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::resource::{PowerState, ResetType};

use crate::power::standard::StandardPower;
use crate::resources::RedfishResourcesExt as _;

/// The selected system's power state as the BMC reports it now; the
/// context's resolved system keeps the state it was fetched with.
pub(super) async fn current_power_state<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<Option<PowerState>, PlatformError> {
    Ok(cx.fresh().system().await?.power_state())
}

/// Aggregates per-supply states into one shelf state; `None` when there are no
/// supplies or they disagree or do not report.
pub(super) fn power_state_from_supplies(states: &[Option<bool>]) -> Option<PowerState> {
    if states.is_empty() {
        None
    } else if states.iter().all(|state| *state == Some(true)) {
        Some(PowerState::On)
    } else if states.iter().all(|state| *state == Some(false)) {
        Some(PowerState::Off)
    } else {
        None
    }
}

/// Power resources and reset workflows shared by power drivers.
pub(super) trait RedfishPowerExt<B: Bmc> {
    /// Succeeds once the BMC reports the host off, for operations a platform
    /// accepts only on a powered-off host. Until then it forces the host off,
    /// unless it is already powering off, and answers
    /// [`PlatformError::NotReady`], so the caller asks again later rather than
    /// this call waiting for the host.
    async fn ensure_host_off(&self) -> Result<(), PlatformError>
    where
        B::Error: ActionError;

    /// Restarts the host over IPMI when the runtime attached it, for hosts whose
    /// Redfish restart cuts power to their DPUs.
    async fn ipmi_restart(&self) -> Result<DriverOutcome, PlatformError>;

    /// The power supplies of the first chassis that has any.
    async fn power_supplies(&self) -> Result<Vec<PowerSupply<B>>, PlatformError>;
}

impl<B: Bmc> RedfishPowerExt<B> for OpCx<'_, B> {
    async fn ensure_host_off(&self) -> Result<(), PlatformError>
    where
        B::Error: ActionError,
    {
        let state = current_power_state(self).await?;
        match state {
            Some(PowerState::Off) => return Ok(()),
            Some(PowerState::PoweringOff) => {}
            _ => {
                StandardPower.set(self, ResetType::ForceOff).await?;
            }
        }
        Err(PlatformError::NotReady {
            message: format!("the host reports power state {state:?}, not Off"),
        })
    }

    async fn ipmi_restart(&self) -> Result<DriverOutcome, PlatformError> {
        self.ipmi()
            .ok_or(PlatformError::Unsupported)?
            .chassis_power_reset()
            .await
            .map(|()| DriverOutcome::complete())
    }

    async fn power_supplies(&self) -> Result<Vec<PowerSupply<B>>, PlatformError> {
        for chassis in &self.all_chassis().await? {
            let supplies = chassis
                .power_supplies()
                .await
                .map_err(|error| self.map_redfish_error(error))?;
            if !supplies.is_empty() {
                return Ok(supplies);
            }
        }
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    /// An off host passes at once; any other host answers `NotReady` without
    /// waiting, after a ForceOff unless it is already powering off.
    #[tokio::test]
    async fn a_host_not_yet_off_is_forced_off_and_not_ready() {
        const SYSTEM: &str = "/redfish/v1/Systems/1";
        const RESET: &str = "/redfish/v1/Systems/1/Actions/ComputerSystem.Reset";
        for (power_state, ready, forced_off) in [
            ("Off", true, false),
            ("On", false, true),
            ("PoweringOn", false, true),
            ("PoweringOff", false, false),
        ] {
            let bmc = Fixture::new("Contoso", "Server", "1", "1")
                .document(
                    SYSTEM,
                    json!({
                        "@odata.id": SYSTEM,
                        "Id": "1",
                        "Name": "System",
                        "PowerState": power_state,
                        "Actions": {"#ComputerSystem.Reset": {"target": RESET}},
                    }),
                )
                .build()
                .await;
            let cx = bmc.cx().await;

            let result = cx.ensure_host_off().await;
            assert_eq!(result.is_ok(), ready, "{power_state}: {result:?}");
            if !ready {
                assert!(
                    matches!(result, Err(PlatformError::NotReady { .. })),
                    "{power_state}: {result:?}"
                );
            }
            let writes: Vec<_> = bmc
                .writes()
                .iter()
                .map(|write| (path(write).to_string(), body(write)))
                .collect();
            let expected = if forced_off {
                vec![(RESET.to_string(), json!({"ResetType": "ForceOff"}))]
            } else {
                Vec::new()
            };
            assert_eq!(writes, expected, "{power_state}");
        }
    }

    #[test]
    fn supplies_agree_on_a_state_or_report_it_unknown() {
        value_scenarios!(run = |states: Vec<Option<bool>>| power_state_from_supplies(&states);
            "agreeing supplies" {
                vec![Some(true), Some(true)] => Some(PowerState::On),
                vec![Some(false)] => Some(PowerState::Off),
            }
            "unknown" {
                Vec::new() => None,
                vec![Some(true), Some(false)] => None,
                vec![Some(true), None] => None,
            }
        );
    }
}
