/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use bmc_platform::{OpCx, PlatformError};
use nv_redfish::core::Bmc;

use crate::resources::RedfishResourcesExt as _;

pub(crate) mod gb300;
pub(crate) mod xcc;

/// Fails with [`PlatformError::LockedDown`] while XCC 3's `SettingsLockdown`
/// blocks BIOS settings writes and UEFI password changes. Firmware that does
/// not report the control is never locked.
pub(crate) async fn ensure_bios_settings_unlocked<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<(), PlatformError> {
    let locked = cx
        .bios()
        .await?
        .oem_lenovo()
        .map_err(|error| cx.map_redfish_error(error))?
        .and_then(|lenovo| lenovo.settings_lockdown());
    if locked == Some(true) {
        return Err(PlatformError::LockedDown);
    }
    Ok(())
}
