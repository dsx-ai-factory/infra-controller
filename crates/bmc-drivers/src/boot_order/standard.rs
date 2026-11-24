/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Standard Redfish boot control.

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::schema::computer_system::BootUpdate;

use crate::boot_order::support::RedfishBootOrderExt as _;

/// DMTF boot override on the live system resource.
///
/// Ordering the boot interface first depends on how each platform names its
/// boot options, so only vendor drivers configure or check it.
pub(crate) struct StandardBootOrder;

#[async_trait]
impl<B: Bmc> BootOrder<B> for StandardBootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        self
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
        cx.set_system_boot_override(override_setting, false).await
    }

    async fn configure(
        &self,
        _cx: &OpCx<'_, B>,
        _selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}
