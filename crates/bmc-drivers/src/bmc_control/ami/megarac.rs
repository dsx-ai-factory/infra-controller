/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{BmcControl, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::oem::ami::update_service::{
    AmiUpdateServiceUpdate, AmiUpdateServiceUpdateExt, PreserveConfigurationUpdate,
};
use nv_redfish::resource::ResetType;
use nv_redfish::update_service::UpdateServiceUpdate;

use crate::bmc_control::standard::StandardBmcControl;
use crate::bmc_control::support::RedfishBmcControlExt as _;

/// AMI MegaRAC accepts at most two NTP servers, only restarts through
/// `ForceRestart`, and keeps whatever its preserve-configuration flags name
/// through a reset to factory defaults, so those are cleared first where the
/// BMC has them.
pub(crate) struct MegaRacBmcControl;

const MAX_NTP_SERVERS: usize = 2;

/// Clears every preserve-configuration flag; a BMC firmware flash sets them
/// all.
async fn clear_preserved_configuration<B: Bmc>(cx: &OpCx<'_, B>) -> Result<(), PlatformError> {
    let preserve = PreserveConfigurationUpdate::builder()
        .with_authentication(false)
        .with_extlog(false)
        .with_fru(false)
        .with_ipmi(false)
        .with_kvm(false)
        .with_ntp(false)
        .with_network(false)
        .with_redfish(false)
        .with_sdr(false)
        .with_sel(false)
        .with_snmp(false)
        .with_ssh(false)
        .with_syslog(false)
        .with_web(false)
        .build();
    let body = UpdateServiceUpdate::builder()
        .build()
        .with_oem_ami(
            AmiUpdateServiceUpdate::builder()
                .with_preserve_configuration(preserve)
                .build(),
        )
        .map_err(|error| PlatformError::InvalidResponse {
            message: format!("failed to build the AMI preserve-configuration update: {error}"),
        })?;
    let service = cx
        .service_root()
        .update_service()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?;
    service
        .update(&body)
        .await
        .map(drop)
        .map_err(|error| cx.map_redfish_error(error))
}

#[async_trait]
impl<B: Bmc> BmcControl<B> for MegaRacBmcControl
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn BmcControl<B> {
        &StandardBmcControl
    }

    async fn reset(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        cx.reset_manager(ResetType::ForceRestart).await
    }

    async fn reset_to_factory_defaults(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<DriverOutcome, PlatformError> {
        match clear_preserved_configuration(cx).await {
            Ok(())
            | Err(
                PlatformError::Unsupported
                | PlatformError::Bmc {
                    status: 400 | 405, ..
                },
            ) => {}
            Err(error) => return Err(error),
        }
        self.standard().reset_to_factory_defaults(cx).await
    }

    async fn set_ntp_servers(
        &self,
        cx: &OpCx<'_, B>,
        servers: &[String],
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard()
            .set_ntp_servers(cx, &servers[..servers.len().min(MAX_NTP_SERVERS)])
            .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const RESET_TO_DEFAULTS: &str = "/redfish/v1/Managers/Self/Actions/Manager.ResetToDefaults";

    #[tokio::test]
    async fn factory_reset_clears_preserved_configuration_before_resetting() {
        let bmc = Fixture::new("AMI", "AMI Redfish Server", "Self", "Self")
            .document(
                "/redfish/v1/Managers/Self",
                json!({"@odata.id": "/redfish/v1/Managers/Self", "Id": "Self", "Name": "Manager",
                       "Actions": {"#Manager.ResetToDefaults": {"target": RESET_TO_DEFAULTS}}}),
            )
            .document(
                "/redfish/v1/UpdateService",
                json!({"@odata.id": "/redfish/v1/UpdateService", "Id": "UpdateService",
                       "Name": "Update Service"}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        assert_eq!(
            MegaRacBmcControl.reset_to_factory_defaults(&cx).await,
            Ok(DriverOutcome::complete())
        );
        let writes = bmc.writes();
        assert_eq!(
            writes.iter().map(path).collect::<Vec<_>>(),
            ["/redfish/v1/UpdateService", RESET_TO_DEFAULTS]
        );
        let cleared = [
            "Authentication",
            "EXTLOG",
            "FRU",
            "IPMI",
            "KVM",
            "NTP",
            "Network",
            "REDFISH",
            "SDR",
            "SEL",
            "SNMP",
            "SSH",
            "Syslog",
            "WEB",
        ]
        .map(|flag| (flag.to_string(), json!(false)));
        assert_eq!(
            body(&writes[0]),
            json!({"Oem": {"AMIUpdateService": {"PreserveConfiguration":
                serde_json::Map::from_iter(cleared)}}})
        );
        assert_eq!(body(&writes[1]), json!({"ResetType": "ResetAll"}));
    }
}
