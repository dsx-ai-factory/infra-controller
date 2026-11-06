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

use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{
    RedfishBootOrderExt as _, alias, boot_order, display_name, is_first, persistent_device,
    reference,
};

/// AMI MegaRAC boot behavior.
///
/// The override goes to the live system and the boot order to its advertised
/// settings object (`SD`), both with `If-Match: *`; an override without a mode boots
/// UEFI. A persistent device boots first by its boot option's `Alias`.
pub(crate) struct MegaRacBootOrder;

/// The HTTP IPv4 boot option of the interface with `mac`, named after the MAC
/// as in `[Slot2]UEFI: HTTP IPv4 Nvidia Network Adapter - B8:E9:24:17:6D:72 P1`.
pub(crate) fn http_option<'a, B: Bmc>(
    options: &'a [BootOption<B>],
    mac: &str,
) -> Option<&'a BootOption<B>> {
    let mac = mac.to_uppercase();
    options.iter().find(|option| {
        let name = display_name(option).to_uppercase();
        name.contains("HTTP") && name.contains("IPV4") && name.contains(&mac)
    })
}

pub(crate) fn missing_http_option(mac: &str) -> PlatformError {
    PlatformError::MissingBootOption {
        description: format!("HTTP IPv4 boot option for MAC {mac}"),
    }
}

/// Moves `target` first in the staged boot order unless `order` already
/// starts with it.
async fn put_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    order: Vec<String>,
    target: &BootOption<B>,
) -> Result<DriverOutcome, PlatformError> {
    cx.stage_boot_option_first(order, reference(target)).await
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for MegaRacBootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let mac = cx.boot_interface_mac(selector).await?;
        let order = boot_order(cx.system().await?);
        let options = cx.boot_options().await?;
        Ok(BootOrderStatus {
            boot_interface_first: http_option(&options, &mac)
                .is_some_and(|target| is_first(&order, reference(target))),
            disk_enabled: None,
            other_network_options_disabled: None,
        })
    }

    /// Boots a device persistently by moving its matching `Alias` first in the
    /// `SD` boot order; other requests use the live system's override.
    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        let Some(device) = persistent_device(override_setting) else {
            return cx.set_system_boot_override(override_setting, true).await;
        };
        let options = cx.boot_options().await?;
        let target = options
            .iter()
            .find(|option| alias(option) == Some(device))
            .ok_or_else(|| PlatformError::MissingBootOption {
                description: format!("boot option with alias {device:?}"),
            })?;
        put_first(cx, boot_order(cx.system().await?), target).await
    }

    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let mac = cx.boot_interface_mac(selector).await?;
        let options = cx.boot_options().await?;
        let target = http_option(&options, &mac).ok_or_else(|| missing_http_option(&mac))?;
        put_first(cx, boot_order(cx.system().await?), target).await
    }
}
