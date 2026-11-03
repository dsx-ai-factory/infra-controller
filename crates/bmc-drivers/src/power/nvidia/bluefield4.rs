/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::oem::nvidia::NvidiaChassisResetType;
use nv_redfish::resource::{PowerState, ResetType};

use crate::power::standard::StandardPower;
use crate::power::support::current_power_state;
use crate::resources::RedfishResourcesExt as _;

/// BlueField-4 has no `ComputerSystem.Reset`; it shuts down and resets its
/// Arm cores through the card chassis's NVIDIA reset action. Powering off and
/// on are skipped when the DPU already reports the requested state.
pub(crate) struct BlueField4Power;

/// The card chassis whose OEM action resets the Arm cores.
const CARD_CHASSIS_ID: &str = "BlueField_0";

async fn arm_reset<B: Bmc>(
    cx: &OpCx<'_, B>,
    reset_type: NvidiaChassisResetType,
) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    cx.chassis_by_id(CARD_CHASSIS_ID)
        .await?
        .ok_or(PlatformError::Unsupported)?
        .oem_nvidia_actions()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .reset(reset_type)
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

#[async_trait]
impl<B: Bmc> Power<B> for BlueField4Power
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Power<B> {
        &StandardPower
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        match reset_type {
            ResetType::ForceOff | ResetType::GracefulShutdown => {
                if current_power_state(cx).await? == Some(PowerState::Off) {
                    return Ok(DriverOutcome::complete());
                }
                arm_reset(cx, NvidiaChassisResetType::ArmShutdown).await
            }
            ResetType::On | ResetType::ForceOn => {
                if current_power_state(cx).await? != Some(PowerState::Off) {
                    return Ok(DriverOutcome::complete());
                }
                arm_reset(cx, NvidiaChassisResetType::ArmReset).await
            }
            ResetType::ForceRestart | ResetType::GracefulRestart | ResetType::PowerCycle => {
                arm_reset(cx, NvidiaChassisResetType::ArmReset).await
            }
            _ => Err(PlatformError::Unsupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/BlueField_0";
    const CHASSIS: &str = "/redfish/v1/Chassis/BlueField_0";
    const ARM_RESET: &str = "/redfish/v1/Chassis/BlueField_0/Actions/Oem/NvidiaChassis.Reset";

    /// Each request becomes the Arm reset it maps to on the unadvertised
    /// NVIDIA chassis action, and powering off or on a DPU already in that
    /// state, or an AC power cycle, sends nothing.
    #[tokio::test]
    async fn power_requests_reset_the_arm_cores_through_the_card_chassis() {
        for (reset_type, power_state, expected) in [
            (ResetType::ForceOff, "On", Ok(Some("ArmShutdown"))),
            (ResetType::GracefulShutdown, "On", Ok(Some("ArmShutdown"))),
            (ResetType::ForceOff, "Off", Ok(None)),
            (ResetType::On, "Off", Ok(Some("ArmReset"))),
            (ResetType::On, "On", Ok(None)),
            (ResetType::ForceRestart, "On", Ok(Some("ArmReset"))),
            (ResetType::GracefulRestart, "On", Ok(Some("ArmReset"))),
            (ResetType::PowerCycle, "Off", Ok(Some("ArmReset"))),
            (
                ResetType::FullPowerCycle,
                "On",
                Err(PlatformError::Unsupported),
            ),
        ] {
            let bmc = Fixture::new("Nvidia", "BlueField-4", "BlueField_0", "Bluefield_BMC")
                .document(
                    SYSTEM,
                    json!({
                        "@odata.id": SYSTEM, "Id": "BlueField_0", "Name": "BlueField_0",
                        "PowerState": power_state, "Actions": null,
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
                        "@odata.id": CHASSIS,
                        "@odata.type": "#Chassis.v1_23_0.Chassis",
                        "Id": "BlueField_0", "Name": "Card1", "ChassisType": "Card",
                        "Actions": {"#Chassis.Reset": {
                            "@Redfish.ActionInfo": "/redfish/v1/Chassis/BlueField_0/ResetActionInfo",
                            "target": "/redfish/v1/Chassis/BlueField_0/Actions/Chassis.Reset",
                        }},
                    }),
                )
                .build()
                .await;
            let cx = bmc.cx().await;

            let result = BlueField4Power.set(&cx, reset_type).await;
            let writes = bmc.writes();
            let posted = writes
                .iter()
                .map(|write| (path(write).to_string(), body(write)))
                .collect::<Vec<_>>();
            match expected {
                Ok(arm_reset) => {
                    assert_eq!(result, Ok(DriverOutcome::complete()), "{reset_type:?}");
                    assert_eq!(
                        posted,
                        arm_reset
                            .map(|value| (ARM_RESET.to_string(), json!({"ResetType": value})))
                            .into_iter()
                            .collect::<Vec<_>>(),
                        "{reset_type:?} from {power_state}"
                    );
                }
                Err(error) => {
                    assert_eq!(result, Err(error), "{reset_type:?}");
                    assert!(posted.is_empty(), "{reset_type:?}: {posted:?}");
                }
            }
        }
    }
}
