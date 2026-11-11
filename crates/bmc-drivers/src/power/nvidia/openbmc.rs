/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::oem::nvidia::AuxPowerResetType;
use nv_redfish::resource::ResetType;

use crate::power::standard::StandardPower;
use crate::power::support::RedfishPowerExt as _;
use crate::resources::RedfishResourcesExt as _;

/// NVIDIA OpenBMC platforms cycle auxiliary power through the BMC chassis,
/// which GB200 accepts only while the host is off, so a request on a host
/// that is not yet off forces it off and answers `NotReady` until it is.
pub(crate) struct OpenBmcPower;

/// The chassis whose OEM action cycles auxiliary power.
const BMC_CHASSIS_ID: &str = "BMC_0";

/// Cycles auxiliary power through the BMC chassis OEM action.
async fn aux_power_cycle<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    cx.chassis_by_id(BMC_CHASSIS_ID)
        .await?
        .ok_or(PlatformError::Unsupported)?
        .oem_nvidia_actions()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .aux_power_reset(AuxPowerResetType::AuxPowerCycle)
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

#[async_trait]
impl<B: Bmc> Power<B> for OpenBmcPower
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
            ResetType::FullPowerCycle => {
                cx.ensure_host_off().await?;
                aux_power_cycle(cx).await
            }
            other => self.standard().set(cx, other).await,
        }
    }
}

/// NVIDIA GH200 trays: the auxiliary power cycle runs when requested, but is
/// not reported as an AC power cycle callers may plan around.
pub(crate) struct Gh200Power;

#[async_trait]
impl<B: Bmc> Power<B> for Gh200Power
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Power<B> {
        &StandardPower
    }

    async fn ac_power_cycle_supported(&self, _cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        Ok(false)
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        OpenBmcPower.set(cx, reset_type).await
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/System_0";
    const RESET: &str = "/redfish/v1/Systems/System_0/Actions/ComputerSystem.Reset";
    const CHASSIS: &str = "/redfish/v1/Chassis/BMC_0";
    const AUX_POWER_RESET: &str =
        "/redfish/v1/Chassis/BMC_0/Actions/Oem/NvidiaChassis.AuxPowerReset";

    /// The auxiliary power cycle goes out only once the host reports off: an
    /// off host gets it at once, and a host still on gets only the ForceOff
    /// and a `NotReady` answer to ask again.
    #[tokio::test]
    async fn an_ac_power_cycle_goes_out_only_once_the_host_is_off() {
        for (scenario, power_state, posted) in [
            (
                "host already off",
                "Off",
                (AUX_POWER_RESET, json!({"ResetType": "AuxPowerCycle"})),
            ),
            (
                "host still on",
                "On",
                (RESET, json!({"ResetType": "ForceOff"})),
            ),
        ] {
            let bmc = Fixture::new("NVIDIA", "GB BMC", "System_0", "BMC_0")
                .document(
                    SYSTEM,
                    json!({
                        "@odata.id": SYSTEM, "Id": "System_0", "Name": "System",
                        "PowerState": power_state,
                        "Actions": {"#ComputerSystem.Reset": {"target": RESET}},
                    }),
                )
                .document(
                    "/redfish/v1/Chassis",
                    json!({
                        "@odata.id": "/redfish/v1/Chassis",
                        "@odata.type": "#ChassisCollection.ChassisCollection",
                        "Name": "Chassis Collection",
                        "Members": [{"@odata.id": CHASSIS}],
                    }),
                )
                .document(
                    CHASSIS,
                    json!({
                        "@odata.id": CHASSIS, "Id": "BMC_0", "Name": "BMC_0",
                        "ChassisType": "Module",
                        "Actions": {"Oem": {
                            "#NvidiaChassis.AuxPowerReset": {"target": AUX_POWER_RESET},
                        }},
                    }),
                )
                .build()
                .await;
            let cx = bmc.cx().await;

            let result = OpenBmcPower.set(&cx, ResetType::FullPowerCycle).await;
            if power_state == "Off" {
                assert!(result.is_ok(), "{scenario}: {result:?}");
            } else {
                assert!(
                    matches!(result, Err(PlatformError::NotReady { .. })),
                    "{scenario}: {result:?}"
                );
            }
            let writes = bmc.writes();
            assert_eq!(writes.len(), 1, "{scenario}: {writes:?}");
            assert_eq!(writes[0].method, Method::POST, "{scenario}");
            assert_eq!(
                (path(&writes[0]), body(&writes[0])),
                (posted.0, posted.1),
                "{scenario}"
            );
        }
    }
}
