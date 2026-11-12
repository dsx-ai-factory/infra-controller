/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

pub(crate) mod bluefield;
pub(crate) mod gbx00;
pub(crate) mod gh200;
pub(crate) mod switch;
pub(crate) mod vera_rubin;
pub(crate) mod viking;

use bmc_platform::{DriverOutcome, OpCx, PlatformError};
use nv_redfish::computer_system::BootOption;
use nv_redfish::core::Bmc;
use nv_redfish::schema::computer_system::{BootSource, BootUpdate};

use crate::boot_order::support::{
    RedfishBootOrderExt as _, boot_order, display_name, matching_first,
};

/// The display name prefix of NVIDIA UEFI HTTP boot options.
const HTTP: &str = "UEFI HTTPv4";
/// The display name prefix of NVIDIA UEFI PXE boot options.
const PXE: &str = "UEFI PXEv4";
/// The UEFI device path prefix of disk boot options, as in
/// `HD(1,GPT,A04D0F1E-...,0x800,0x100000)/\EFI\ubuntu\shimaa64.efi`.
const DISK_PATH: &str = "HD(";

/// The HTTP boot option NVIDIA firmware names after the interface's MAC, as
/// in `UEFI HTTPv4 (MAC:B8E924176D72)`.
fn http_option_name(mac: &str) -> String {
    format!("{HTTP} (MAC:{})", mac.replace(':', "").to_uppercase())
}

/// Whether `option` is a boot option for `device`: by display name for
/// network and HTTP boot, by UEFI device path for disks.
fn boots<B: Bmc>(option: &BootOption<B>, device: BootSource) -> bool {
    match device {
        BootSource::Pxe => display_name(option).starts_with(PXE),
        BootSource::UefiHttp => display_name(option).starts_with(HTTP),
        _ => option
            .uefi_device_path()
            .is_some_and(|path| path.inner().starts_with(DISK_PATH)),
    }
}

/// Stages the override fields of `setting` on the system's settings object.
async fn settings_override<B: Bmc>(
    cx: &OpCx<'_, B>,
    setting: &BootUpdate,
    uefi_by_default: bool,
) -> Result<DriverOutcome, PlatformError> {
    cx.stage_boot_override(setting, uefi_by_default).await
}

/// Stages `order` as the boot order of the system's settings object.
async fn write_settings_boot_order<B: Bmc>(
    cx: &OpCx<'_, B>,
    order: Vec<String>,
) -> Result<DriverOutcome, PlatformError> {
    cx.stage_boot_order(order).await
}

/// Moves every option accepted by `boots` ahead of the rest of the boot
/// order, staged on the advertised settings object.
async fn device_options_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    boots: impl Fn(&BootOption<B>) -> bool,
) -> Result<DriverOutcome, PlatformError> {
    let order = boot_order(cx.system().await?);
    let options = cx.boot_options().await?;
    let ordered = matching_first(&order, &options, boots)?
        .into_iter()
        .map(|option| option.raw().id.clone())
        .collect();
    write_settings_boot_order(cx, ordered).await
}
