/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::computer_system::BootOption;
use nv_redfish::core::Bmc;
use nv_redfish::schema::computer_system::BootUpdate;

use crate::boot_order::nvidia::{
    boots, http_option_name, settings_override, write_settings_boot_order,
};
use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{
    RedfishBootOrderExt as _, boot_order, display_name, listed_options, persistent_device, promote,
    reference,
};

/// NVIDIA Vera Rubin tray boot behavior.
///
/// Overrides and the boot order go through the system's advertised settings
/// object (`Settings`), whose boot order entries may read `Boot0001: <name>`.
/// The firmware can list duplicate HTTP options such as
/// `UEFI HTTPv4 (MAC:…) 2`, so the DPU's option is matched by its whole name.
pub(crate) struct VeraRubinBootOrder;

/// Writes the boot order with `option`'s entry moved to the front; fails when
/// the boot order does not list it.
async fn option_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    option: &BootOption<B>,
) -> Result<DriverOutcome, PlatformError> {
    let mut order = boot_order(cx.system().await?);
    if !promote(&mut order, reference(option)) {
        return Err(PlatformError::InvalidResponse {
            message: format!("BootOrder does not list {}", reference(option)),
        });
    }
    write_settings_boot_order(cx, order).await
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for VeraRubinBootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let name = http_option_name(&cx.boot_interface_mac(selector).await?);
        let order = boot_order(cx.system().await?);
        let options = cx.boot_options().await?;
        let listed = listed_options(&order, &options)?;
        Ok(BootOrderStatus {
            boot_interface_first: listed
                .first()
                .is_some_and(|first| display_name(first) == name),
            disk_enabled: None,
            other_network_options_disabled: None,
        })
    }

    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        let Some(device) = persistent_device(override_setting) else {
            return settings_override(cx, override_setting, false).await;
        };
        let options = cx.boot_options().await?;
        let option = options
            .iter()
            .find(|option| boots(option, device))
            .ok_or_else(|| PlatformError::MissingBootOption {
                description: format!("{device:?} boot option"),
            })?;
        option_first(cx, option).await
    }

    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let name = http_option_name(&cx.boot_interface_mac(selector).await?);
        let options = cx.boot_options().await?;
        let option = options
            .iter()
            .find(|option| display_name(option) == name)
            .ok_or(PlatformError::MissingBootOption { description: name })?;
        option_first(cx, option).await
    }
}
