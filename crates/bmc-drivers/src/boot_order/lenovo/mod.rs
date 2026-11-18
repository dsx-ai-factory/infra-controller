/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Lenovo XCC boot device groups.
//!
//! XCC lists one boot option per device group (`Network`, `HardDisk`, ...)
//! and orders adapters within a group separately. BIOS setup puts the
//! network group first through [`network_group_first`].

pub(crate) mod gb300;
pub(crate) mod xcc;
pub(crate) mod xcc3;

use bmc_platform::{DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::Bmc;
use nv_redfish::oem::lenovo::boot_manager::{
    BootOrderKind, LenovoBootManager, LenovoBootManagerCollection, LenovoBootManagerUpdate,
};
use nv_redfish::schema::computer_system::{BootSource, BootUpdate};

use crate::boot_order::support::{
    RedfishBootOrderExt as _, boot_order, listed_option, one_time_device, persistent_device,
};

/// The name of the network device group's boot option.
pub(crate) const NETWORK: &str = "Network";
/// The name of the disk device group's boot option.
const HARD_DISK: &str = "HardDisk";

/// Puts the device group named `name` first in the system boot order, staged
/// on the advertised settings object (`Pending`); the other groups keep their
/// collection order.
async fn set_group_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    name: &str,
) -> Result<DriverOutcome, PlatformError> {
    let options = cx.boot_options().await?;
    let group = options
        .iter()
        .rfind(|option| option.raw().name == name)
        .ok_or_else(|| PlatformError::MissingBootOption {
            description: name.to_string(),
        })?;
    let order = std::iter::once(group)
        .chain(options.iter().filter(|option| option.raw().name != name))
        .map(|option| option.raw().id.clone())
        .collect();
    cx.stage_boot_order(order).await
}

/// Puts `Network` first in the OEM general boot order, for firmware that
/// lists no `Network` boot option until the group is in that order.
async fn set_general_network_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    settings: &LenovoBootManagerCollection<B>,
) -> Result<DriverOutcome, PlatformError> {
    const GENERAL_NETWORK: &str = "Network";
    let general = oem_boot_order(cx, settings, BootOrderKind::General).await?;
    let mut next = general.next().unwrap_or_default().to_vec();
    // XCC reports an empty order while the host POSTs; writing `Network`
    // alone then would drop every other group.
    if next.is_empty() {
        return Ok(DriverOutcome::complete());
    }
    match next.iter().position(|entry| entry == GENERAL_NETWORK) {
        Some(0) => return Ok(DriverOutcome::complete()),
        Some(position) => next.swap(0, position),
        None => next.insert(0, GENERAL_NETWORK.to_string()),
    }
    set_next(cx, &general, next).await
}

/// XCC boots the network or disk group once through the standard override,
/// sent without a mode, and from now on by putting the group first. It has
/// no HTTP boot override and takes no HTTP boot URI.
async fn set_override<B: Bmc>(
    cx: &OpCx<'_, B>,
    setting: &BootUpdate,
) -> Result<DriverOutcome, PlatformError> {
    if matches!(
        one_time_device(setting),
        Some(BootSource::Pxe | BootSource::Hdd)
    ) {
        return cx.set_system_boot_override(setting, false).await;
    }
    match persistent_device(setting) {
        Some(BootSource::Pxe) => set_group_first(cx, NETWORK).await,
        Some(BootSource::Hdd) => set_group_first(cx, HARD_DISK).await,
        _ => Err(PlatformError::Unsupported),
    }
}

/// Puts the network device group first in the boot order. Firmware that
/// lists no `Network` boot option gets it first in its OEM general order
/// instead; firmware without that order has nothing to change.
pub(crate) async fn network_group_first<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<DriverOutcome, PlatformError> {
    match set_group_first(cx, NETWORK).await {
        Err(PlatformError::MissingBootOption { .. }) => match boot_settings(cx).await? {
            Some(settings) => set_general_network_first(cx, &settings).await,
            None => Ok(DriverOutcome::complete()),
        },
        result => result,
    }
}

/// The name of the device group the system boot order lists first; `None`
/// when the order is empty.
pub(crate) async fn first_group<B: Bmc>(cx: &OpCx<'_, B>) -> Result<Option<String>, PlatformError> {
    let Some(first) = boot_order(cx.system().await?).into_iter().next() else {
        return Ok(None);
    };
    let options = cx.boot_options().await?;
    Ok(Some(listed_option(&first, &options)?.raw().name.clone()))
}

/// The OEM boot settings XCC 2 exposes; `None` on firmware that orders boot
/// devices through BIOS attributes instead.
async fn boot_settings<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<Option<LenovoBootManagerCollection<B>>, PlatformError> {
    let Some(lenovo) = cx
        .system()
        .await?
        .oem_lenovo()
        .map_err(|error| cx.map_redfish_error(error))?
    else {
        return Ok(None);
    };
    lenovo
        .boot_settings()
        .await
        .map_err(|error| cx.map_redfish_error(error))
}

/// The OEM boot order of `kind`.
async fn oem_boot_order<B: Bmc>(
    cx: &OpCx<'_, B>,
    settings: &LenovoBootManagerCollection<B>,
    kind: BootOrderKind,
) -> Result<LenovoBootManager<B>, PlatformError> {
    settings
        .boot_order(kind)
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("XCC boot settings do not list the {kind:?} boot order"),
        })
}

/// Stages `next` as the OEM boot order applied at the next boot.
async fn set_next<B: Bmc>(
    cx: &OpCx<'_, B>,
    order: &LenovoBootManager<B>,
    next: Vec<String>,
) -> Result<DriverOutcome, PlatformError> {
    order
        .update(
            &LenovoBootManagerUpdate::builder()
                .with_boot_order_next(next)
                .build(),
        )
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}
