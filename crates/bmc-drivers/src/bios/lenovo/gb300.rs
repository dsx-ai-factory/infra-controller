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

/// Lenovo GB300: AMI firmware whose Grace BIOS registry prefixes enum values
/// with their attribute id, and expresses infinite boot as a boot-retry count.
pub(crate) struct Gb300Bios;

/// AMI firmware names the UEFI administrator password `SETUP001`.
const UEFI_PASSWORD_NAME: &str = "SETUP001";

/// The BIOS attribute that requests a TPM operation on the next boot.
const TPM_OPERATION: &str = "TCG006";

const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("PCIS007", "PCIS007Enabled").required(),
    BiosAttribute::integer("LEM0001", 0).required(),
    BiosAttribute::string("NWSK000", "NWSK000Enabled").required(),
    BiosAttribute::string("NWSK001", "NWSK001Disabled").required(),
    BiosAttribute::string("NWSK006", "NWSK006Enabled").required(),
    BiosAttribute::string("NWSK002", "NWSK002Disabled").required(),
    BiosAttribute::string("NWSK007", "NWSK007Disabled").required(),
    INFINITE_BOOT,
];

/// A retry count of 50 is the BIOS's endless boot.
const INFINITE_BOOT: BiosAttribute = BiosAttribute::integer("LEM0003", 50).required();

fn tpm_clear() -> BiosSettings {
    settings([(TPM_OPERATION, json!("TCG006TPMClear"))])
}

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

    async fn infinite_boot_enabled(&self, cx: &OpCx<'_, B>) -> Result<Option<bool>, PlatformError> {
        cx.bios_attribute_holds(INFINITE_BOOT).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/System_0";
    const BIOS: &str = "/redfish/v1/Systems/System_0/Bios";
    const SETTINGS: &str = "/redfish/v1/Systems/System_0/Bios/SD";

    #[tokio::test]
    async fn tpm_clear_stages_the_attribute_prefixed_value() {
        let bmc = Fixture::new("AMI", "AMI Redfish Server", "System_0", "Self")
            .document(
                SYSTEM,
                json!({"@odata.id": SYSTEM, "Id": "System_0", "Name": "System", "Bios": {"@odata.id": BIOS}}),
            )
            .document(
                BIOS,
                json!({
                    "@odata.id": BIOS,
                    "@Redfish.Settings": {"SettingsObject": {"@odata.id": SETTINGS}},
                    "Id": "BIOS",
                    "Name": "BIOS",
                    "Attributes": {TPM_OPERATION: "TCG006None"},
                }),
            )
            .document(
                SETTINGS,
                json!({"@odata.id": SETTINGS, "Id": "SD", "Name": "BIOS Settings", "Attributes": {}}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        Gb300Bios.clear_tpm(&cx).await.expect("TPM clear is staged");
        let writes = bmc.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(path(&writes[0]), SETTINGS);
        assert_eq!(
            body(&writes[0]),
            json!({"Attributes": {TPM_OPERATION: "TCG006TPMClear"}})
        );
    }
}
