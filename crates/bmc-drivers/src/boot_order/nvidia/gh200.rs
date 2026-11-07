/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::schema::computer_system::BootUpdate;

use crate::boot_order::nvidia::{boots, device_options_first, http_option_name, settings_override};
use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{
    RedfishBootOrderExt as _, boot_order, display_name, is_first, persistent_device, reference,
};

/// NVIDIA GH200 boot behavior.
///
/// Overrides and the boot order go through the system's advertised settings
/// object (`Settings`). The DPU's HTTP option is matched by its whole name,
/// in any case.
pub(crate) struct Gh200BootOrder;

/// The boot order, the reference of the selected interface's HTTP option, and
/// the option's expected name.
async fn http_option<B: Bmc>(
    cx: &OpCx<'_, B>,
    selector: &BootInterfaceSelector,
) -> Result<(Vec<String>, Option<String>, String), PlatformError> {
    let name = http_option_name(&cx.boot_interface_mac(selector).await?);
    let order = boot_order(cx.system().await?);
    let target = cx
        .boot_options()
        .await?
        .iter()
        .find(|option| display_name(option).eq_ignore_ascii_case(&name))
        .map(|option| reference(option).to_string());
    Ok((order, target, name))
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for Gh200BootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let (order, target, _) = http_option(cx, selector).await?;
        Ok(BootOrderStatus {
            boot_interface_first: target.is_some_and(|target| is_first(&order, &target)),
            disk_enabled: None,
            other_network_options_disabled: None,
        })
    }

    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        match persistent_device(override_setting) {
            Some(device) => device_options_first(cx, |option| boots(option, device)).await,
            None => settings_override(cx, override_setting, false).await,
        }
    }

    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let (order, target, name) = http_option(cx, selector).await?;
        let target = target.ok_or(PlatformError::MissingBootOption { description: name })?;
        cx.stage_boot_option_first(order, &target).await
    }
}
