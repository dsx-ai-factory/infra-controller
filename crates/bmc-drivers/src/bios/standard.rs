/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Standard Redfish BIOS configuration.

use std::collections::BTreeMap;

use async_trait::async_trait;
use bmc_platform::{
    Bios, BiosSettings, BiosStatus, BootInterfaceSelector, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::Value;

use crate::bios::support::{RedfishBiosExt as _, compare};
use crate::resources::{RedfishResourcesExt as _, bios_attributes, bios_update};

/// DMTF names the UEFI administrator password `AdministratorPassword`.
const UEFI_PASSWORD_NAME: &str = "AdministratorPassword";

/// DMTF-standard BIOS behavior. A platform without its own settings expects
/// only the caller's profile.
pub(crate) struct StandardBios;

#[async_trait]
impl<B: Bmc> Bios<B> for StandardBios
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Bios<B> {
        self
    }

    async fn apply(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.stage_bios_attributes(&profile.attributes).await
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        Ok(compare(&cx.current_bios_settings().await?, profile))
    }

    /// Restores BIOS defaults through the advertised `Bios.ResetBios` action.
    async fn reset(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        cx.bios()
            .await?
            .reset()
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    /// Reverts only the staged attributes that differ, since re-sending every
    /// attribute trips read-only rejections on many BMCs.
    async fn clear_pending(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        let bios = cx.bios().await?;
        let current = bios_attributes(&bios.raw());
        let settings = bios
            .settings()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        let reverted: BTreeMap<String, Value> = bios_attributes(&settings.raw())
            .into_iter()
            .filter_map(|(key, pending)| {
                let current = current.get(&key)?;
                (*current != pending).then(|| (key, current.clone()))
            })
            .collect();
        if reverted.is_empty() {
            return Ok(DriverOutcome::complete());
        }
        settings
            .update(&bios_update(&reverted)?)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    async fn change_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.change_bios_password(UEFI_PASSWORD_NAME, current_password, new_password)
            .await
    }

    /// Redfish has no standard TPM clear.
    async fn clear_tpm(&self, _cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    /// Redfish has no standard NVRAM reset.
    async fn clear_nvram(&self, _cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    /// Redfish has no standard infinite-boot setting, so there is nothing to
    /// report.
    async fn infinite_boot_enabled(
        &self,
        _cx: &OpCx<'_, B>,
    ) -> Result<Option<bool>, PlatformError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/1";
    const BIOS: &str = "/redfish/v1/Systems/1/Bios";
    const SETTINGS: &str = "/redfish/v1/Systems/1/Bios/Settings";

    #[tokio::test]
    async fn status_reads_current_values_and_apply_stages_only_what_the_next_reset_lacks() {
        let bmc = Fixture::new("Contoso", "Server", "1", "1")
            .document(
                SYSTEM,
                json!({"@odata.id": SYSTEM, "Id": "1", "Name": "System", "Bios": {"@odata.id": BIOS}}),
            )
            .document(
                BIOS,
                json!({
                    "@odata.id": BIOS,
                    "@Redfish.Settings": {"SettingsObject": {"@odata.id": SETTINGS}},
                    "Id": "Bios",
                    "Name": "BIOS",
                    "Attributes": {"Kept": "on", "Staged": "old", "Changed": "old", "Echoed": "x"},
                }),
            )
            .document(
                SETTINGS,
                json!({
                    "@odata.id": SETTINGS,
                    "Id": "Settings",
                    "Name": "BIOS Settings",
                    "Attributes": {"Staged": "new", "Echoed": "x"},
                }),
            )
            .build()
            .await;
        let cx = bmc.cx().await;
        let profile = serde_json::from_value(json!({"attributes": {
            "Kept": "on",
            "Staged": "new",
            "Changed": "new",
            "Missing": true,
        }}))
        .expect("profile");

        let status = StandardBios
            .status(&cx, &profile, None)
            .await
            .expect("status");
        assert_eq!(
            status
                .differences
                .iter()
                .map(|difference| difference.key.as_str())
                .collect::<Vec<_>>(),
            ["Changed", "Missing", "Staged"]
        );
        assert_eq!(
            StandardBios.apply(&cx, &profile, None).await,
            Ok(DriverOutcome::complete())
        );
        let writes = bmc.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].method, Method::PATCH);
        assert_eq!(path(&writes[0]), SETTINGS);
        assert_eq!(
            body(&writes[0]),
            json!({"Attributes": {"Changed": "new", "Missing": true}})
        );
    }
}
