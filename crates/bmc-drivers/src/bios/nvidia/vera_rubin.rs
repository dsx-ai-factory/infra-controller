/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Bios, BiosSettings, BiosStatus, BootInterfaceSelector, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::Value;

use crate::bios::attributes::BiosAttribute;
use crate::bios::standard::StandardBios;
use crate::bios::support::{RedfishBiosExt as _, compare, expected};
use crate::resources::RedfishResourcesExt as _;

/// NVIDIA Vera Rubin NVL compute trays: OpenBMC naming the UEFI administrator
/// password `AdminPassword`, with GPUs exposed as PCIe devices.
pub(crate) struct VeraRubinBios;

const UEFI_PASSWORD_NAME: &str = "AdminPassword";

const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("TPM", "Enabled").required(),
    INFINITE_BOOT,
    BiosAttribute::bool("GpuExposeAsPcie", true).required(),
];

/// Disabling the embedded UEFI shell keeps the BIOS retrying boot instead of
/// dropping into the shell.
const INFINITE_BOOT: BiosAttribute =
    BiosAttribute::string("EmbeddedUefiShell", "Disabled").required();

/// Devices whose option ROMs the BIOS skips, as space-separated vendor and
/// device IDs such as `0x15B3A2DF`. BlueField DPUs must keep theirs.
const OP_ROM_DISABLED_DEVICES: &str = "OpRomDisabledDevices";
const MELLANOX_VENDOR_ID: &str = "15B3";
/// BlueField-4, BlueField-3 Lx, BlueField-3, BlueField and BlueField-2.
const BLUEFIELD_DEVICE_IDS: &[&str] = &["A2DF", "A2D9", "A2DC", "A2D2", "A2D6"];

/// The expected settings, with BlueField DPUs dropped from the reported
/// `OpRomDisabledDevices` unless the caller's profile sets that list.
fn expected_settings(current: &BiosSettings, profile: &BiosSettings) -> BiosSettings {
    let mut settings = expected(ATTRIBUTES, current, profile);
    if let Some(Value::String(disabled)) = current.attributes.get(OP_ROM_DISABLED_DEVICES) {
        settings
            .attributes
            .entry(OP_ROM_DISABLED_DEVICES.to_string())
            .or_insert_with(|| Value::from(without_bluefield(disabled)));
    }
    settings
}

fn without_bluefield(devices: &str) -> String {
    devices
        .split_whitespace()
        .filter(|device| !is_bluefield(device))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_bluefield(device: &str) -> bool {
    let device = device.to_uppercase();
    device
        .strip_prefix("0X")
        .unwrap_or(&device)
        .strip_prefix(MELLANOX_VENDOR_ID)
        .is_some_and(|id| BLUEFIELD_DEVICE_IDS.contains(&id))
}

#[async_trait]
impl<B: Bmc> Bios<B> for VeraRubinBios
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
        cx.stage_bios_attributes(&expected_settings(&current, profile).attributes)
            .await
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        let current = cx.current_bios_settings().await?;
        Ok(compare(&current, &expected_settings(&current, profile)))
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

    async fn infinite_boot_enabled(&self, cx: &OpCx<'_, B>) -> Result<Option<bool>, PlatformError> {
        cx.bios_attribute_holds(INFINITE_BOOT).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn from_json(attributes: Value) -> BiosSettings {
        serde_json::from_value(json!({ "attributes": attributes })).expect("settings")
    }

    /// BlueField DPUs leave the list whatever their ID's case or prefix;
    /// ConnectX NICs stay, and a BIOS without the list is left alone.
    #[test]
    fn bluefield_option_roms_are_enabled() {
        let cases = [
            (
                "BlueField among ConnectX",
                json!({"OpRomDisabledDevices": "0x15B31023 0x15B31025 0x15B3A2DF 15b3a2dc"}),
                Some(json!("0x15B31023 0x15B31025")),
            ),
            ("list not reported", json!({}), None),
        ];
        for (scenario, current, expected) in cases {
            let settings = expected_settings(&from_json(current), &BiosSettings::default());
            assert_eq!(
                settings.attributes.get(OP_ROM_DISABLED_DEVICES),
                expected.as_ref(),
                "{scenario}"
            );
        }
    }
}
