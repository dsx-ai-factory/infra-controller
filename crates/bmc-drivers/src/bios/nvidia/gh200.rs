/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Bios, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::{ActionError, Bmc};

use crate::bios::standard::StandardBios;
use crate::bios::support::RedfishBiosExt as _;

/// NVIDIA GH200 trays: OpenBMC naming the UEFI administrator password
/// `AdminPassword`; machine setup expects nothing beyond the caller's profile.
pub(crate) struct Gh200Bios;

const UEFI_PASSWORD_NAME: &str = "AdminPassword";

#[async_trait]
impl<B: Bmc> Bios<B> for Gh200Bios
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Bios<B> {
        &StandardBios
    }

    async fn change_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.change_bios_password(UEFI_PASSWORD_NAME, current_password, new_password)
            .await
    }
}
