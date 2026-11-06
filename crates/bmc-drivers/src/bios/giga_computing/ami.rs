/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Bios, BiosSettings, BiosStatus, BootInterfaceSelector, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::json;

use crate::bios::attributes::BiosAttribute;
use crate::bios::standard::StandardBios;
use crate::bios::support::{RedfishBiosExt as _, compare, expected, settings};
use crate::resources::RedfishResourcesExt as _;

/// Giga Computing AMI BIOS, as on R263-ZG0 (BIOS R23_F20): AMI's attribute
/// set without `LEM0001` and `FBO001`, which this BIOS does not define, and
/// vendor codes for virtualization, infinite boot, and the TPM operation.
pub(crate) struct GigaComputingAmiBios;

/// AMI firmware names the UEFI administrator password `SETUP001`.
const UEFI_PASSWORD_NAME: &str = "SETUP001";

/// The BIOS attribute that requests a TPM operation on the next boot;
/// AMI's `TCG006` is the TCM state on this BIOS.
const TPM_OPERATION: &str = "TCG008";

const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("CPU005", "Enabled").required(),
    BiosAttribute::string("PCIS007", "Enabled").required(),
    BiosAttribute::string("NWSK000", "Enabled").required(),
    BiosAttribute::string("NWSK001", "Disabled").required(),
    BiosAttribute::string("NWSK006", "Enabled").required(),
    BiosAttribute::string("NWSK002", "Disabled").required(),
    BiosAttribute::string("NWSK007", "Disabled").required(),
    INFINITE_BOOT,
];

/// "Endless Retry Boot"; this BIOS does not define AMI's `EndlessBoot`.
const INFINITE_BOOT: BiosAttribute = BiosAttribute::string("GBT0183", "Enabled").required();

fn tpm_clear() -> BiosSettings {
    settings([(TPM_OPERATION, json!("TPM Clear"))])
}

/// The BIOS attribute that resets NVRAM on the next boot.
const NVRAM_RESET: &str = "RECV000";

#[async_trait]
impl<B: Bmc> Bios<B> for GigaComputingAmiBios
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
        let current = cx.reported_bios_settings().await?;
        cx.stage_bios_attributes(&expected(ATTRIBUTES, &current, profile).attributes)
            .await
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        let current = cx.reported_bios_settings().await?;
        Ok(compare(&current, &expected(ATTRIBUTES, &current, profile)))
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

    async fn clear_tpm(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        cx.stage_bios_attributes(&tpm_clear().attributes).await
    }

    async fn clear_nvram(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        cx.stage_bios_attributes(&settings([(NVRAM_RESET, json!("Enabled"))]).attributes)
            .await
    }

    async fn infinite_boot_enabled(&self, cx: &OpCx<'_, B>) -> Result<Option<bool>, PlatformError> {
        Ok(cx
            .reported_bios_settings()
            .await?
            .attributes
            .get(INFINITE_BOOT.name)
            .map(|value| INFINITE_BOOT.value.matches(value)))
    }
}
