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
    RedfishBootOrderExt as _, boot_order, display_name, listed_options, persistent_device,
};

/// NVIDIA GB200 and GB300 tray boot behavior.
///
/// Overrides and the boot order go through the system's `Settings` object.
/// The DPU's HTTP option is matched by its UEFI device path, since newer
/// firmware renames it, as in `(S22/B1/D0/F0) UEFI HTTP IPv4 ... - <MAC>`.
pub(crate) struct Gbx00BootOrder;

/// Whether `option` is the UEFI HTTP IPv4 boot option of the interface with
/// `mac`: its device path has the interface's `MAC(...)` node and ends in an
/// `IPv4(...)` node followed by a `Uri(...)` node, which PXE options lack.
/// Firmware that reports no device path is matched by the start of its name.
fn is_http_option_for<B: Bmc>(option: &BootOption<B>, mac: &str) -> bool {
    let mac = mac.replace([':', '-'], "");
    if mac.len() != 12 || !mac.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let path: Option<&str> = option.uefi_device_path().map(|path| *path.inner());
    let Some(path) = path.filter(|path| !path.is_empty()) else {
        return display_name(option).starts_with(&http_option_name(&mac));
    };
    // Split before the URI so slashes in an explicit URL do not split nodes.
    let Some((network, uri)) = path.split_once("/Uri(") else {
        return false;
    };
    let ipv4 = network
        .rsplit('/')
        .next()
        .is_some_and(|node| node.starts_with("IPv4(") && node.ends_with(')'));
    let mac_node = network.split('/').any(|node| {
        node.strip_prefix("MAC(")
            .and_then(|node| node.strip_suffix(')'))
            .and_then(|node| node.split_once(','))
            .is_some_and(|(address, _)| address.eq_ignore_ascii_case(&mac))
    });
    ipv4 && mac_node && uri.ends_with(')')
}

/// Writes the boot order with `option` moved to the front.
async fn option_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    option: &BootOption<B>,
) -> Result<DriverOutcome, PlatformError> {
    let id = option.raw().id.clone();
    let mut order = boot_order(cx.system().await?);
    order.retain(|entry| *entry != id);
    order.insert(0, id);
    write_settings_boot_order(cx, order).await
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for Gbx00BootOrder {
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
        let listed = listed_options(&order, &options)?;
        let expected = listed
            .iter()
            .find(|option| is_http_option_for(option, &mac));
        Ok(BootOrderStatus {
            boot_interface_first: expected
                .zip(listed.first())
                .is_some_and(|(expected, first)| expected.raw().id == first.raw().id),
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
        let mac = cx.boot_interface_mac(selector).await?;
        let options = cx.boot_options().await?;
        let option = options
            .iter()
            .find(|option| is_http_option_for(option, &mac))
            .ok_or_else(|| PlatformError::MissingBootOption {
                description: format!("UEFI HTTP IPv4 boot option for MAC {mac}"),
            })?;
        option_first(cx, option).await
    }
}
