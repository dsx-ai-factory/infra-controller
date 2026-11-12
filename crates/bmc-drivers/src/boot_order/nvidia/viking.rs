/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::computer_system::{BootOption, ComputerSystemUpdate};
use nv_redfish::core::{Bmc, EntityTypeRef as _};
use nv_redfish::schema::computer_system::{BootSource, BootUpdate};

use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{
    RedfishBootOrderExt as _, alias, boot_order, boot_override, display_name, matching_first,
    persistent_device, reference,
};

/// NVIDIA DGX Viking boot behavior.
///
/// Overrides and the boot order go through the system's advertised settings
/// object (`SD`): an override with `If-Match` set to that object's ETag, the
/// boot order with `If-Match: *`. An override without a mode boots UEFI. The
/// DPU's HTTP option is the UEFI HTTP option whose device path names its MAC.
pub(crate) struct VikingBootOrder;

/// The selected interface's HTTP option.
async fn http_option<B: Bmc>(
    cx: &OpCx<'_, B>,
    selector: &BootInterfaceSelector,
) -> Result<Option<BootOption<B>>, PlatformError> {
    let mac = cx
        .boot_interface_mac(selector)
        .await?
        .replace(':', "")
        .to_uppercase();
    Ok(cx.boot_options().await?.into_iter().find(|option| {
        let path = option
            .uefi_device_path()
            .map(|path| path.inner().to_uppercase())
            .unwrap_or_default();
        path.contains(&mac) && path.contains("IPV4") && alias(option) == Some(BootSource::UefiHttp)
    }))
}

/// The boot order staged on the settings object the system advertises, or
/// the system's own when it advertises none.
async fn staged_boot_order<B: Bmc>(cx: &OpCx<'_, B>) -> Result<Vec<String>, PlatformError> {
    let system = cx.system().await?;
    let Some(settings) = system
        .settings()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
    else {
        return Ok(boot_order(system));
    };
    Ok(settings
        .raw()
        .boot
        .as_ref()
        .and_then(|boot| boot.boot_order.clone().flatten())
        .unwrap_or_default())
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for VikingBootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let target = http_option(cx, selector).await?;
        let order = boot_order(cx.system().await?);
        let options = cx.boot_options().await?;
        let first = order
            .first()
            .and_then(|first| options.iter().find(|option| reference(option) == first));
        Ok(BootOrderStatus {
            boot_interface_first: target
                .zip(first)
                .is_some_and(|(target, first)| display_name(&target) == display_name(first)),
            disk_enabled: None,
            other_network_options_disabled: None,
        })
    }

    /// A persistent device moves every boot option whose `Alias` names it
    /// ahead of the rest of the boot order.
    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        let Some(device) = persistent_device(override_setting) else {
            // The override needs the settings object's own ETag, which
            // nv-redfish's typed writes replace with `*` on AMI firmware.
            let system = cx.system().await?;
            let settings = system
                .settings()
                .await
                .map_err(|error| cx.map_redfish_error(error))?;
            let raw = settings.as_ref().unwrap_or(system).raw();
            return cx
                .patch_system(
                    &raw.odata_id,
                    &ComputerSystemUpdate::builder()
                        .with_boot(boot_override(override_setting, true)?)
                        .build(),
                    raw.etag(),
                )
                .await
                .map(DriverOutcome::from);
        };
        let order = boot_order(cx.system().await?);
        let options = cx.boot_options().await?;
        let ordered = matching_first(&order, &options, |option| alias(option) == Some(device))?
            .into_iter()
            .map(|option| format!("Boot{}", option.raw().id))
            .collect();
        cx.stage_boot_order(ordered).await
    }

    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let target =
            http_option(cx, selector)
                .await?
                .ok_or_else(|| PlatformError::MissingBootOption {
                    description: format!("IPv4 UEFI HTTP boot option for {selector:?}"),
                })?;
        let target = reference(&target).to_string();
        let mut order = staged_boot_order(cx).await?;
        let position = order
            .iter()
            .position(|entry| *entry == target)
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: format!("BootOrder does not list {target}"),
            })?;
        order.remove(position);
        order.insert(0, target);
        cx.stage_boot_order(order).await
    }
}
