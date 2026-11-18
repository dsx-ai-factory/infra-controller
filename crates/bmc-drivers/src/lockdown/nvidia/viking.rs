/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Lockdown, LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus,
    OpCx, PlatformError, Quirk,
};
use nv_redfish::core::Bmc;

use crate::lockdown::support::{reported, signal, state_from_signals};
use crate::resources::RedfishResourcesExt as _;

/// NVIDIA Viking lockdown driver; both host and BMC lockdown are BIOS attributes.
///
/// Firmware generations differ in both the KCS attribute name and its value
/// vocabulary, so the write mirrors whichever encoding the BIOS reports. A
/// denied KCS alone reports lockdown as enabled; unlocked also needs
/// `RedfishEnable` enabled. Enabling lockdown needs
/// [`Quirk::VikingLockdownFirmware`].
pub(crate) struct VikingLockdown;

#[async_trait]
impl<B: Bmc> Lockdown<B> for VikingLockdown {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError> {
        let bios = cx.bios().await?;
        let kcs = bios
            .attribute("KcsInterfaceDisable")
            .or_else(|| bios.attribute("IPMIKCSInterfaceDisable"))
            .and_then(|value| value.str_value().map(str::to_owned));
        let redfish = bios
            .attribute("RedfishEnable")
            .and_then(|value| value.str_value().map(str::to_owned));
        let kcs_locked = matches!(kcs.as_deref(), Some("Deny All" | "Disabled"));
        let kcs_unlocked = matches!(kcs.as_deref(), Some("Allow All" | "Enabled"));
        let host = state_from_signals(&[(kcs_locked, kcs_unlocked)]);
        let bmc = state_from_signals(&[signal(redfish.as_deref(), "Disabled", "Enabled")]);
        let aggregate = match (kcs.as_deref(), redfish.as_deref()) {
            (None, None) => LockdownState::Unknown,
            (None, Some(_)) | (Some(_), None) => LockdownState::Partial,
            (Some(_), Some(_)) if kcs_locked => LockdownState::Enabled,
            (Some(_), Some(redfish)) if kcs_unlocked && redfish == "Enabled" => {
                LockdownState::Disabled
            }
            (Some(_), Some(_)) => LockdownState::Partial,
        };
        Ok(LockdownStatus {
            aggregate,
            message: format!(
                "kcs_interface_disable={}, redfish_enable={}",
                reported(&kcs),
                reported(&redfish)
            ),
            host,
            bmc,
        })
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<DriverOutcome, PlatformError> {
        // Viking has no BMC-side lock apart from full lockdown.
        if matches!(scope, LockdownScope::Bmc | LockdownScope::BmcSystemLockdown) {
            return Err(PlatformError::Unsupported);
        }
        let enabled = desired == LockdownDesiredState::Enabled;
        if enabled && !cx.has_quirk(Quirk::VikingLockdownFirmware) {
            return Err(PlatformError::Unsupported);
        }
        let bios = cx.bios().await?;
        let mut attributes = BTreeMap::new();
        if matches!(scope, LockdownScope::Host | LockdownScope::All) {
            let key = if bios.attribute("KcsInterfaceDisable").is_some() {
                "KcsInterfaceDisable"
            } else {
                "IPMIKCSInterfaceDisable"
            };
            let current = bios
                .attribute(key)
                .and_then(|value| value.str_value().map(str::to_owned));
            let kcs = match (current.as_deref(), enabled) {
                (Some("Enabled" | "Disabled"), true) => "Disabled",
                (Some("Enabled" | "Disabled"), false) => "Enabled",
                (_, true) => "Deny All",
                (_, false) => "Allow All",
            };
            attributes.insert(key.to_string(), kcs.into());
        }
        if scope == LockdownScope::All {
            attributes.insert(
                "RedfishEnable".to_string(),
                (if enabled { "Disabled" } else { "Enabled" }).into(),
            );
        }
        cx.stage_bios_attributes(&attributes).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::Fixture;

    #[tokio::test]
    async fn status_is_unknown_when_the_bios_reports_neither_signal() {
        const SYSTEM: &str = "/redfish/v1/Systems/DGX";
        const BIOS: &str = "/redfish/v1/Systems/DGX/Bios";
        let bmc = Fixture::new("AMI", "AMI Redfish Server", "DGX", "BMC")
            .document(
                SYSTEM,
                json!({"@odata.id": SYSTEM, "Id": "DGX", "Name": "System", "Bios": {"@odata.id": BIOS}}),
            )
            .document(
                BIOS,
                json!({"@odata.id": BIOS, "Id": "Bios", "Name": "BIOS", "Attributes": {}}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        assert_eq!(
            VikingLockdown
                .status(&cx)
                .await
                .map(|status| status.aggregate),
            Ok(LockdownState::Unknown)
        );
    }

    #[tokio::test]
    async fn lockdown_is_not_enabled_on_firmware_too_old_for_it() {
        let bmc = Fixture::new("AMI", "AMI Redfish Server", "DGX", "BMC")
            .build()
            .await;
        let cx = bmc.cx().await;

        assert_eq!(
            VikingLockdown
                .set(&cx, LockdownScope::All, LockdownDesiredState::Enabled)
                .await,
            Err(PlatformError::Unsupported)
        );
        assert!(bmc.writes().is_empty());
    }
}
