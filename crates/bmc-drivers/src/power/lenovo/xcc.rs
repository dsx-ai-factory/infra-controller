/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::time::Duration;

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power, Quirk};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::oem::lenovo::SystemResetType;
use nv_redfish::resource::{PowerState, ResetType};

use crate::power::standard::StandardPower;
use crate::power::support::current_power_state;

/// Lenovo XClarity Controller power behavior.
///
/// With [`Quirk::LenovoForceRestartHangs`], a ForceRestart powers the host off
/// and, after a wait, back on instead.
pub(crate) struct XccPower;

/// How long the host stays off before it is powered back on in place of a
/// hanging ForceRestart; the host must report off by then.
const FORCE_RESTART_OFF_TIME: Duration = Duration::from_secs(10);

async fn off_then_on<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    if current_power_state(cx).await? != Some(PowerState::Off) {
        StandardPower.set(cx, ResetType::ForceOff).await?;
    }
    tokio::time::sleep(FORCE_RESTART_OFF_TIME).await;
    let state = current_power_state(cx).await?;
    if state != Some(PowerState::Off) {
        return Err(PlatformError::Timeout {
            message: format!(
                "host reports power state {state:?} {}s after ForceOff",
                FORCE_RESTART_OFF_TIME.as_secs()
            ),
        });
    }
    StandardPower.set(cx, ResetType::On).await
}

#[async_trait]
impl<B: Bmc> Power<B> for XccPower
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Power<B> {
        &StandardPower
    }

    async fn ac_power_cycle_supported(&self, _cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        Ok(true)
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        match reset_type {
            ResetType::ForceRestart if cx.has_quirk(Quirk::LenovoForceRestartHangs) => {
                off_then_on(cx).await
            }
            ResetType::FullPowerCycle => cx
                .system()
                .await?
                .oem_lenovo_actions()
                .map_err(|error| cx.map_redfish_error(error))?
                .ok_or(PlatformError::Unsupported)?
                .system_reset(SystemResetType::AcPowerCycle)
                .await
                .map(DriverOutcome::from)
                .map_err(|error| cx.map_redfish_error(error)),
            other => self.standard().set(cx, other).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use tokio::time::Instant;

    use super::*;
    use crate::test_support::{Fixture, body};

    /// The host is powered back on only after it has stayed off for the off
    /// time; a host still on by then fails after that one wait, never on.
    #[tokio::test(start_paused = true)]
    async fn hanging_force_restart_powers_on_only_after_the_host_has_stayed_off() {
        const SYSTEM: &str = "/redfish/v1/Systems/1";
        for (power_state, powered_on, writes) in [
            ("Off", true, vec![json!({"ResetType": "On"})]),
            ("On", false, vec![json!({"ResetType": "ForceOff"})]),
        ] {
            let bmc = Fixture::new("Lenovo", "XCC", "1", "1")
                .document(
                    SYSTEM,
                    json!({
                        "@odata.id": SYSTEM,
                        "Id": "1",
                        "Name": "System",
                        "PowerState": power_state,
                        "Actions": {"#ComputerSystem.Reset": {
                            "target": "/redfish/v1/Systems/1/Actions/ComputerSystem.Reset"
                        }},
                    }),
                )
                .build()
                .await;
            let quirks = BTreeSet::from([Quirk::LenovoForceRestartHangs]);
            let cx = bmc.cx().await.with_quirks(&quirks);
            let started = Instant::now();

            let result = XccPower.set(&cx, ResetType::ForceRestart).await;
            if powered_on {
                assert_eq!(result, Ok(DriverOutcome::complete()), "{power_state}");
            } else {
                assert!(
                    matches!(result, Err(PlatformError::Timeout { .. })),
                    "{power_state}: {result:?}"
                );
            }
            assert_eq!(started.elapsed(), FORCE_RESTART_OFF_TIME, "{power_state}");
            assert_eq!(
                bmc.writes().iter().map(body).collect::<Vec<_>>(),
                writes,
                "{power_state}"
            );
        }
    }
}
