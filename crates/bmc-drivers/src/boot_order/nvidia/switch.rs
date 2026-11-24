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

use crate::boot_order::nvidia::{boots, device_options_first, settings_override};
use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::persistent_device;

/// NVIDIA NVSwitch tray boot behavior.
///
/// Overrides and the boot order go through the system's advertised settings
/// object (`Settings`). A switch tray has no host interface to put first.
pub(crate) struct SwitchBootOrder;

#[async_trait]
impl<B: Bmc> BootOrder<B> for SwitchBootOrder {
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
        match persistent_device(override_setting) {
            Some(device) => device_options_first(cx, |option| boots(option, device)).await,
            None => settings_override(cx, override_setting, false).await,
        }
    }

    async fn configure(
        &self,
        _cx: &OpCx<'_, B>,
        _selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}
