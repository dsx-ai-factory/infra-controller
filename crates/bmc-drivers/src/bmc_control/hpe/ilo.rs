/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{BmcControl, DriverOutcome, OpCx, PlatformError, TimeStatus};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::oem::hpe::HpeManager;
use nv_redfish::oem::hpe::date_time::HpeiLoDateTimeUpdate;

use crate::bmc_control::standard::StandardBmcControl;

/// HPE iLO manager-control behavior.
///
/// iLO resets to defaults through its OEM action and takes exactly two static
/// NTP servers on its `DateTime` service rather than NetworkProtocol.
pub(crate) struct IloBmcControl;

async fn hpe_manager<B: Bmc>(cx: &OpCx<'_, B>) -> Result<HpeManager<B>, PlatformError> {
    cx.manager()
        .await?
        .oem_hpe()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)
}

#[async_trait]
impl<B: Bmc> BmcControl<B> for IloBmcControl
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
        hpe_manager(cx)
            .await?
            .reset_to_factory_defaults()
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    /// An empty list leaves the NTP configuration unchanged; the first two
    /// servers are written, padding a single server with an empty entry.
    /// `Unsupported` while iLO takes its NTP servers from DHCP, which then
    /// owns them.
    async fn set_ntp_servers(
        &self,
        cx: &OpCx<'_, B>,
        servers: &[String],
    ) -> Result<DriverOutcome, PlatformError> {
        if servers.is_empty() {
            return Ok(DriverOutcome::complete());
        }
        let static_ntp_servers = vec![
            servers.first().cloned().unwrap_or_default(),
            servers.get(1).cloned().unwrap_or_default(),
        ];
        hpe_manager(cx)
            .await?
            .date_time()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .update(
                &HpeiLoDateTimeUpdate::builder()
                    .with_static_ntp_servers(static_ntp_servers)
                    .build(),
            )
            .await
            .map(DriverOutcome::from)
            .map_err(|error| match cx.map_redfish_error(error) {
                PlatformError::Bmc {
                    message_id: Some(id),
                    ..
                } if id.ends_with(".SNTPConfigurationManagedByDHCPAndIsReadOnly") => {
                    PlatformError::Unsupported
                }
                error => error,
            })
    }

    /// The servers come from the `DateTime` service, which has no NTP switch.
    async fn time_status(&self, cx: &OpCx<'_, B>) -> Result<TimeStatus, PlatformError> {
        let servers = hpe_manager(cx)
            .await?
            .date_time()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .and_then(|date_time| date_time.raw().static_ntp_servers.clone())
            .unwrap_or_default();
        Ok(TimeStatus {
            ntp_enabled: None,
            ntp_servers: servers
                .into_iter()
                .filter(|server| !server.is_empty())
                .collect(),
            ..self.standard().time_status(cx).await?
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    use super::*;
    use crate::test_support::Fixture;

    /// iLO 5 v2.72 refuses static NTP servers while DHCP supplies them, which
    /// is unsupported rather than a failure; another refusal stays an error.
    #[tokio::test]
    async fn ntp_servers_from_dhcp_are_unsupported() {
        const DATE_TIME: &str = "/redfish/v1/Managers/1/DateTime";
        let refusal = |message_id: &str| {
            json!({"error": {"code": "iLO.0.10.ExtendedInfo",
                             "message": "See @Message.ExtendedInfo for more information.",
                             "@Message.ExtendedInfo": [{"MessageId": message_id}]}})
        };
        for (scenario, message_id, unsupported) in [
            (
                "managed by DHCP",
                "iLO.2.15.SNTPConfigurationManagedByDHCPAndIsReadOnly",
                true,
            ),
            (
                "another refusal",
                "iLO.2.15.PropertyNotWritableOrUnknown",
                false,
            ),
        ] {
            let bmc = Fixture::new("HPE", "", "1", "1")
                .document(
                    "/redfish/v1/Managers/1",
                    json!({"@odata.id": "/redfish/v1/Managers/1", "Id": "1", "Name": "Manager",
                           "Oem": {"Hpe": {"@odata.type": "#HpeiLO.v2_11_0.HpeiLO",
                                           "Links": {"DateTimeService": {"@odata.id": DATE_TIME}}}}}),
                )
                .document(
                    DATE_TIME,
                    json!({"@odata.id": DATE_TIME, "@odata.type": "#HpeiLODateTime.v2_0_0.HpeiLODateTime",
                           "Id": "DateTime", "Name": "Date Time",
                           "StaticNTPServers": ["0.0.0.0", "0.0.0.0"]}),
                )
                .respond(
                    Method::PATCH,
                    DATE_TIME,
                    StatusCode::BAD_REQUEST,
                    Some(refusal(message_id)),
                )
                .build()
                .await;
            let cx = bmc.cx().await;
            let result = IloBmcControl
                .set_ntp_servers(&cx, &["10.0.0.1".to_string()])
                .await;
            assert_eq!(
                matches!(result, Err(PlatformError::Unsupported)),
                unsupported,
                "{scenario}: {:?}",
                result.err()
            );
        }
    }

    /// iLO lists its NTP servers on its `DateTime` service, empty slots
    /// included, and has no NTP switch.
    #[tokio::test]
    async fn time_status_lists_the_date_time_servers() {
        const DATE_TIME: &str = "/redfish/v1/Managers/1/DateTime";
        let bmc = Fixture::new("HPE", "", "1", "1")
            .document(
                "/redfish/v1/Managers/1",
                json!({"@odata.id": "/redfish/v1/Managers/1", "Id": "1", "Name": "Manager",
                       "DateTimeLocalOffset": "+00:00",
                       "Oem": {"Hpe": {"@odata.type": "#HpeiLO.v2_11_0.HpeiLO",
                                       "Links": {"DateTimeService": {"@odata.id": DATE_TIME}}}}}),
            )
            .document(
                DATE_TIME,
                json!({"@odata.id": DATE_TIME, "@odata.type": "#HpeiLODateTime.v2_0_0.HpeiLODateTime",
                       "Id": "DateTime", "Name": "Date Time",
                       "StaticNTPServers": ["10.0.0.1", ""]}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;
        let status = IloBmcControl
            .time_status(&cx)
            .await
            .expect("time status reads");
        assert_eq!(
            (status.ntp_enabled, status.ntp_servers, status.utc_offset),
            (
                None,
                vec!["10.0.0.1".to_string()],
                Some("+00:00".to_string())
            )
        );
    }
}
