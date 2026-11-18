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
use crate::bios::support::{compare, expected, settings};
use crate::resources::RedfishResourcesExt as _;

/// HPE iLO BIOS behavior: staged settings are left in place rather than
/// reverted, and the TPM is cleared through BIOS attributes.
pub(crate) struct IloBios;

/// The virtualization keys differ by CPU vendor, so each BIOS reports only
/// some of them.
const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("IntelProcVtd", "Enabled"),
    BiosAttribute::string("ProcAmdIoVt", "Enabled"),
    BiosAttribute::string("ProcVirtualization", "Enabled"),
    BiosAttribute::string("Dhcpv4", "Enabled"),
    BiosAttribute::string("HttpSupport", "Auto"),
];

fn tpm_clear() -> BiosSettings {
    settings([
        ("Tpm2Operation", json!("Clear")),
        ("TpmVisibility", json!("Visible")),
    ])
}

#[async_trait]
impl<B: Bmc> Bios<B> for IloBios
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
        cx.stage_bios_attributes(&expected(ATTRIBUTES, &current, profile).attributes)
            .await
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        let current = cx.current_bios_settings().await?;
        Ok(compare(&current, &expected(ATTRIBUTES, &current, profile)))
    }

    async fn clear_pending(&self, _cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        Ok(DriverOutcome::complete())
    }

    async fn clear_tpm(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        cx.stage_bios_attributes(&tpm_clear().attributes).await
    }
}
