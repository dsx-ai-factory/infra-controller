/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{BmcControl, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::oem::supermicro::ResetOption;

use crate::bmc_control::standard::StandardBmcControl;

/// How many NTP servers the BMC keeps; it rejects a longer `NTPServers`.
const MAX_NTP_SERVERS: usize = 2;

/// Supermicro restores manager defaults through the OEM `SmcManagerConfig.Reset` action.
pub(crate) struct SmcBmcControl;

#[async_trait]
impl<B: Bmc> BmcControl<B> for SmcBmcControl
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn BmcControl<B> {
        &StandardBmcControl
    }

    async fn reset_to_factory_defaults(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.manager()
            .await?
            .oem_supermicro()
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .reset_configuration(ResetOption::ClearConfig)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    /// Writes the first [`MAX_NTP_SERVERS`] servers.
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
    use crate::test_support::{Fixture, body};

    /// X13 firmware 01.03.34 rejects three `NTPServers` as malformed.
    #[tokio::test]
    async fn only_the_first_two_ntp_servers_are_written() {
        const PROTOCOL: &str = "/redfish/v1/Managers/1/NetworkProtocol";
        let bmc = Fixture::new("Supermicro", "", "1", "1")
            .document(
                "/redfish/v1/Managers/1",
                json!({"@odata.id": "/redfish/v1/Managers/1", "Id": "1", "Name": "Manager",
                       "NetworkProtocol": {"@odata.id": PROTOCOL}}),
            )
            .document(
                PROTOCOL,
                json!({"@odata.id": PROTOCOL, "Id": "NetworkProtocol", "Name": "Network Protocol"}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;
        let servers = ["10.0.0.1", "10.0.0.2", "10.0.0.3"].map(String::from);
        SmcBmcControl
            .set_ntp_servers(&cx, &servers)
            .await
            .expect("NTP servers are written");
        let writes = bmc.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(
            body(&writes[0])["NTP"]["NTPServers"],
            json!(["10.0.0.1", "10.0.0.2"])
        );
    }
}
