/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;

use async_trait::async_trait;
use bmc_platform::{
    BmcControl, DriverOutcome, ManagerSettings, ManagerSettingsDiff, ManagerSettingsStatus, OpCx,
    PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::{Value, json};

use crate::bmc_control::standard::StandardBmcControl;
use crate::dell;
use crate::resources::attribute_map;

const HOST_HEADER_CHECK: &str = "WebServer.1.HostHeaderCheck";
const IPMI_LAN: &str = "IPMILan.1.Enable";
/// Reported only by iDRAC 9.
const OS_BMC_ADMIN_STATE: &str = "OS-BMC.1.AdminState";
const SETTINGS: [&str; 3] = [HOST_HEADER_CHECK, IPMI_LAN, OS_BMC_ADMIN_STATE];

/// Dell iDRAC manager-control behavior.
///
/// iDRAC configures NTP, time zone, and its provisioning settings through its
/// OEM manager attributes; the three NTP slots are always written so stale
/// servers are cleared, and an empty list leaves NTP unchanged.
pub(crate) struct IdracBmcControl;

#[async_trait]
impl<B: Bmc> BmcControl<B> for IdracBmcControl
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn BmcControl<B> {
        &StandardBmcControl
    }

    async fn set_ntp_servers(
        &self,
        cx: &OpCx<'_, B>,
        servers: &[String],
    ) -> Result<DriverOutcome, PlatformError> {
        if servers.is_empty() {
            return Ok(DriverOutcome::complete());
        }
        let slot = |index: usize| json!(servers.get(index).map_or("", String::as_str));
        let attributes = attribute_map([
            ("NTPConfigGroup.1.NTPEnable", json!("Enabled")),
            ("NTPConfigGroup.1.NTP1", slot(0)),
            ("NTPConfigGroup.1.NTP2", slot(1)),
            ("NTPConfigGroup.1.NTP3", slot(2)),
        ]);
        dell::patch_manager_attributes(cx, &attributes, None).await
    }

    async fn set_utc_timezone(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        let attributes = attribute_map([("Time.1.Timezone", json!("UTC"))]);
        dell::patch_manager_attributes(cx, &attributes, None).await
    }

    async fn apply_settings(
        &self,
        cx: &OpCx<'_, B>,
        profile: &ManagerSettings,
    ) -> Result<DriverOutcome, PlatformError> {
        let current = dell::manager_attribute_values(cx, &SETTINGS).await?;
        let mut attributes = expected_settings(&current);
        attributes.extend(profile.attributes.clone());
        dell::patch_manager_attributes(cx, &attributes, None).await
    }

    async fn settings_status(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<ManagerSettingsStatus, PlatformError> {
        let current = dell::manager_attribute_values(cx, &SETTINGS).await?;
        let differences: Vec<ManagerSettingsDiff> = expected_settings(&current)
            .into_iter()
            .filter(|(key, expected)| current.get(key) != Some(expected))
            .map(|(key, expected)| ManagerSettingsDiff {
                actual: current.get(&key).cloned(),
                key,
                expected,
            })
            .collect();
        Ok(ManagerSettingsStatus {
            is_applied: differences.is_empty(),
            differences,
        })
    }
}

/// Disables the web server's host-header check and OS-to-iDRAC pass-through,
/// and enables IPMI over LAN; pass-through is set only where `current`
/// reports it.
fn expected_settings(current: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut attributes = attribute_map([
        (HOST_HEADER_CHECK, json!("Disabled")),
        (IPMI_LAN, json!("Enabled")),
    ]);
    if current.contains_key(OS_BMC_ADMIN_STATE) {
        attributes.insert(OS_BMC_ADMIN_STATE.to_string(), json!("Disabled"));
    }
    attributes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, body};

    const MANAGER: &str = "/redfish/v1/Managers/iDRAC.Embedded.1";
    const ATTRIBUTES: &str =
        "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1";

    #[tokio::test]
    async fn settings_cover_os_bmc_only_where_reported_and_the_profile_wins() {
        let cases = [
            (
                "iDRAC 9 with a profile",
                json!({
                    "WebServer.1.HostHeaderCheck": "Enabled",
                    "IPMILan.1.Enable": "Enabled",
                    "OS-BMC.1.AdminState": "Enabled",
                }),
                attribute_map([
                    ("IPMILan.1.Enable", json!("Disabled")),
                    ("ServerPwr.1.PSRapidOn", json!("Disabled")),
                ]),
                vec![
                    ManagerSettingsDiff {
                        key: OS_BMC_ADMIN_STATE.to_string(),
                        expected: json!("Disabled"),
                        actual: Some(json!("Enabled")),
                    },
                    ManagerSettingsDiff {
                        key: HOST_HEADER_CHECK.to_string(),
                        expected: json!("Disabled"),
                        actual: Some(json!("Enabled")),
                    },
                ],
                json!({
                    "WebServer.1.HostHeaderCheck": "Disabled",
                    "IPMILan.1.Enable": "Disabled",
                    "OS-BMC.1.AdminState": "Disabled",
                    "ServerPwr.1.PSRapidOn": "Disabled",
                }),
            ),
            (
                "iDRAC 10 without a profile",
                json!({
                    "WebServer.1.HostHeaderCheck": "Disabled",
                    "IPMILan.1.Enable": "Enabled",
                }),
                attribute_map([]),
                vec![],
                json!({
                    "WebServer.1.HostHeaderCheck": "Disabled",
                    "IPMILan.1.Enable": "Enabled",
                }),
            ),
        ];
        for (name, reported, profile, differences, expected) in cases {
            let bmc = Fixture::new(
                "Dell",
                "Integrated Dell Remote Access Controller",
                "System.Embedded.1",
                "iDRAC.Embedded.1",
            )
            .document(
                MANAGER,
                json!({
                    "@odata.id": MANAGER,
                    "Id": "iDRAC.Embedded.1",
                    "Name": "Manager",
                    "Links": {"Oem": {"Dell": {
                        "DellAttributes": [{"@odata.id": ATTRIBUTES}],
                    }}},
                }),
            )
            .document(
                ATTRIBUTES,
                json!({
                    "@odata.id": ATTRIBUTES,
                    "Id": "iDRAC.Embedded.1",
                    "Name": "Manager Attributes",
                    "Attributes": reported,
                }),
            )
            .build()
            .await;
            let cx = bmc.cx().await;

            assert_eq!(
                BmcControl::settings_status(&IdracBmcControl, &cx).await,
                Ok(ManagerSettingsStatus {
                    is_applied: differences.is_empty(),
                    differences,
                }),
                "{name}"
            );
            let outcome = BmcControl::apply_settings(
                &IdracBmcControl,
                &cx,
                &ManagerSettings {
                    attributes: profile,
                },
            )
            .await;

            assert_eq!(outcome, Ok(DriverOutcome::complete()), "{name}");
            let writes = bmc.writes();
            assert_eq!(writes.len(), 1, "{name}");
            assert_eq!(
                body(&writes[0]).get("Attributes"),
                Some(&expected),
                "{name}"
            );
            assert!(writes[0].uri.ends_with(ATTRIBUTES), "{name}");
        }
    }
}
