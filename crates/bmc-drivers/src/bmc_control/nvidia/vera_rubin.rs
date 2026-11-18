/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BmcControl, DriverOutcome, ManagerSettings, ManagerSettingsDiff, ManagerSettingsStatus, OpCx,
    PlatformError,
};
use nv_redfish::chassis::ChassisUpdate;
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::oem::nvidia::{NvidiaChassisUpdate, NvidiaChassisUpdateExt as _};
use nv_redfish::resource::PowerState;
use serde_json::{Value, json};

use crate::bmc_control::standard::StandardBmcControl;
use crate::resources::RedfishResourcesExt as _;

/// NVIDIA Vera Rubin: the host's onboard NICs reach the management network
/// unless the SMM chassis disables their access, which provisioning requires.
pub(crate) struct VeraRubinBmcControl;

/// The chassis that owns host management network access.
const SMM_CHASSIS_ID: &str = "SMM_0";
const HOST_MANAGEMENT_NETWORK_ACCESS: &str = "HostManagementNetworkAccess";

#[async_trait]
impl<B: Bmc> BmcControl<B> for VeraRubinBmcControl
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn BmcControl<B> {
        &StandardBmcControl
    }

    /// Disables host management network access. The SMM takes the change
    /// only while the host is on, and the stored value can disagree with the
    /// NICs after an AUX power cycle, so it is written every time.
    async fn apply_settings(
        &self,
        cx: &OpCx<'_, B>,
        profile: &ManagerSettings,
    ) -> Result<DriverOutcome, PlatformError> {
        if !profile.attributes.is_empty() {
            return Err(PlatformError::Unsupported);
        }
        if cx.system().await?.power_state() != Some(PowerState::On) {
            return Err(PlatformError::NotReady {
                message: "host management network access changes only while the host is on"
                    .to_string(),
            });
        }
        let chassis = cx
            .chassis_by_id(SMM_CHASSIS_ID)
            .await?
            .ok_or(PlatformError::Unsupported)?;
        let update = ChassisUpdate::builder()
            .build()
            .with_oem_nvidia(
                NvidiaChassisUpdate::builder()
                    .with_host_management_network_access(false)
                    .build(),
            )
            .map_err(|error| PlatformError::InvalidResponse {
                message: format!("failed to build the NVIDIA chassis update: {error}"),
            })?;
        chassis
            .update(&update)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    /// A missing value counts as a difference: access is applied only when
    /// the SMM reports it disabled.
    async fn settings_status(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<ManagerSettingsStatus, PlatformError> {
        let chassis = cx
            .chassis_by_id(SMM_CHASSIS_ID)
            .await?
            .ok_or(PlatformError::Unsupported)?;
        let actual = chassis
            .oem_nvidia()
            .map_err(|error| cx.map_redfish_error(error))?
            .and_then(|nvidia| nvidia.host_management_network_access())
            .map(Value::Bool);
        let differences: Vec<ManagerSettingsDiff> = (actual != Some(json!(false)))
            .then(|| ManagerSettingsDiff {
                key: HOST_MANAGEMENT_NETWORK_ACCESS.to_string(),
                expected: json!(false),
                actual,
            })
            .into_iter()
            .collect();
        Ok(ManagerSettingsStatus {
            is_applied: differences.is_empty(),
            differences,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/System_0";
    const SMM: &str = "/redfish/v1/Chassis/SMM_0";

    /// Applying disables access on the SMM chassis's NVIDIA extension, and
    /// status reports the access the SMM holds.
    #[tokio::test]
    async fn host_management_network_access_is_disabled_on_the_smm_chassis() {
        let bmc = Fixture::new("NVIDIA", "VR NVL72", "System_0", "BMC_0")
            .document(
                SYSTEM,
                json!({"@odata.id": SYSTEM, "Id": "System_0", "Name": "System_0",
                       "PowerState": "On"}),
            )
            .document(
                "/redfish/v1/Chassis",
                json!({"@odata.id": "/redfish/v1/Chassis",
                       "@odata.type": "#ChassisCollection.ChassisCollection",
                       "Name": "Chassis Collection", "Members": [{"@odata.id": SMM}]}),
            )
            .document(
                SMM,
                json!({"@odata.id": SMM, "@odata.type": "#Chassis.v1_23_0.Chassis",
                "Id": "SMM_0", "Name": "SMM_0", "ChassisType": "Module",
                "Oem": {"Nvidia": {
                    "@odata.type": "#NvidiaChassis.v1_14_0.NvidiaChassis",
                    "HostManagementNetworkAccess": true,
                }}}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        VeraRubinBmcControl
            .apply_settings(&cx, &ManagerSettings::default())
            .await
            .expect("apply succeeds");
        let writes: Vec<_> = bmc
            .writes()
            .iter()
            .map(|request| (path(request).to_string(), body(request)))
            .collect();
        assert_eq!(
            writes,
            [(
                SMM.to_string(),
                json!({"Oem": {"Nvidia": {"HostManagementNetworkAccess": false}}}),
            )]
        );

        assert_eq!(
            VeraRubinBmcControl.settings_status(&cx).await,
            Ok(ManagerSettingsStatus {
                is_applied: false,
                differences: vec![ManagerSettingsDiff {
                    key: HOST_MANAGEMENT_NETWORK_ACCESS.to_string(),
                    expected: json!(false),
                    actual: Some(json!(true)),
                }],
            })
        );
    }
}
