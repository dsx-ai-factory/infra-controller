/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Bios, BiosSettings, BiosStatus, BootInterfaceSelector, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};

use crate::bios::attributes::BiosAttribute;
use crate::bios::nvidia::gbx00::with_option_roms_enabled;
use crate::bios::standard::StandardBios;
use crate::bios::support::{RedfishBiosExt as _, compare, expected};
use crate::resources::RedfishResourcesExt as _;

/// Supermicro GB300 NVL compute trays: NVIDIA tray firmware naming the UEFI
/// administrator password `AdminPassword`, with the TPM behind an AMI BIOS
/// attribute.
pub(crate) struct Gb300Bios;

const UEFI_PASSWORD_NAME: &str = "AdminPassword";

const ATTRIBUTES: &[BiosAttribute] =
    &[BiosAttribute::string("SecurityDeviceSupport", "Enabled").required()];

#[async_trait]
impl<B: Bmc> Bios<B> for Gb300Bios
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Bios<B> {
        &StandardBios
    }

    async fn apply(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<DriverOutcome, PlatformError> {
        let current = cx.current_bios_settings().await?;
        let wanted = with_option_roms_enabled(expected(ATTRIBUTES, &current, profile), &current);
        cx.stage_bios_attributes(&wanted.attributes).await
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        let current = cx.current_bios_settings().await?;
        let wanted = with_option_roms_enabled(expected(ATTRIBUTES, &current, profile), &current);
        Ok(compare(&current, &wanted))
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
