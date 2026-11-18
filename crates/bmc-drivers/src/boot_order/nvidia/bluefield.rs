/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::schema::computer_system::{BootSource, BootUpdate};

use crate::boot_order::nvidia::{HTTP, PXE, device_options_first, settings_override};
use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{display_name, persistent_device};

/// NVIDIA BlueField boot behavior.
///
/// Overrides and the boot order go through the system's advertised settings
/// object (`Settings`), and an override without a mode boots UEFI. The DPU
/// boots itself, so there is no host interface to put first.
pub(crate) struct BlueFieldBootOrder;

/// The display name prefix of the DPU's disk boot option.
const DISK: &str = "UEFI Non-Block Boot Device";

#[async_trait]
impl<B: Bmc> BootOrder<B> for BlueFieldBootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        _cx: &OpCx<'_, B>,
        _selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        let Some(device) = persistent_device(override_setting) else {
            return settings_override(cx, override_setting, true).await;
        };
        let prefix = match device {
            BootSource::Pxe => PXE,
            BootSource::UefiHttp => HTTP,
            _ => DISK,
        };
        device_options_first(cx, |option| display_name(option).starts_with(prefix)).await
    }

    async fn configure(
        &self,
        _cx: &OpCx<'_, B>,
        _selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use axum::http::header::IF_MATCH;
    use nv_redfish::schema::computer_system::BootSourceOverrideEnabled;
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    #[tokio::test]
    async fn override_without_a_mode_boots_uefi_through_the_settings_object() {
        let bmc = Fixture::new("Nvidia", "BlueField-3 DPU", "Bluefield", "Bluefield_BMC")
            .document(
                "/redfish/v1/Systems/Bluefield",
                json!({
                    "@odata.id": "/redfish/v1/Systems/Bluefield",
                    "@odata.etag": "\"system-1\"",
                    "@Redfish.Settings": {"SettingsObject": {
                        "@odata.id": "/redfish/v1/Systems/Bluefield/Settings",
                    }},
                    "Id": "Bluefield",
                    "Name": "Bluefield",
                }),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        BlueFieldBootOrder
            .set_override(
                &cx,
                &BootUpdate::builder()
                    .with_boot_source_override_target(BootSource::UefiHttp)
                    .with_boot_source_override_enabled(BootSourceOverrideEnabled::Once)
                    .build(),
            )
            .await
            .expect("override succeeds");

        let writes = bmc.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(path(&writes[0]), "/redfish/v1/Systems/Bluefield/Settings");
        assert_eq!(writes[0].headers[IF_MATCH], "*");
        assert_eq!(
            body(&writes[0]),
            json!({"Boot": {
                "BootSourceOverrideTarget": "UefiHttp",
                "BootSourceOverrideEnabled": "Once",
                "BootSourceOverrideMode": "UEFI",
            }})
        );
    }
}
