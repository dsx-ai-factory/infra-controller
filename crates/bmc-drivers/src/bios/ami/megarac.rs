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

/// AMI MegaRAC BIOS, including Lenovo HS350x-class systems.
pub(crate) struct MegaRacBios;

/// AMI firmware names the UEFI administrator password `SETUP001`.
const UEFI_PASSWORD_NAME: &str = "SETUP001";

/// The BIOS attribute that requests a TPM operation on the next boot.
const TPM_OPERATION: &str = "TCG006";

const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("VMXEN", "Enable").required(),
    BiosAttribute::string("PCIS007", "Enabled").required(),
    BiosAttribute::integer("LEM0001", 3).required(),
    BiosAttribute::string("NWSK000", "Enabled").required(),
    BiosAttribute::string("NWSK001", "Disabled").required(),
    BiosAttribute::string("NWSK006", "Enabled").required(),
    BiosAttribute::string("NWSK002", "Disabled").required(),
    BiosAttribute::string("NWSK007", "Disabled").required(),
    BiosAttribute::string("FBO001", "UEFI").required(),
    INFINITE_BOOT,
];

const INFINITE_BOOT: BiosAttribute = BiosAttribute::string("EndlessBoot", "Enabled").required();

fn tpm_clear() -> BiosSettings {
    settings([(TPM_OPERATION, json!("TPM Clear"))])
}

/// The BIOS attribute that resets NVRAM on the next boot.
const NVRAM_RESET: &str = "RECV000";

#[async_trait]
impl<B: Bmc> Bios<B> for MegaRacBios
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/Self";
    const BIOS: &str = "/redfish/v1/Systems/Self/Bios";
    const SETTINGS: &str = "/redfish/v1/Systems/Self/Bios/SD";

    /// A BMC restarted since the host last started has no BIOS attributes to
    /// report yet; that is a wait, not a platform without a BIOS.
    #[tokio::test]
    async fn missing_bios_attributes_are_not_ready_rather_than_unsupported() {
        let bios = |attributes: serde_json::Value| json!({"@odata.id": BIOS, "Id": "BIOS", "Name": "BIOS", "Attributes": attributes});
        let cases = [
            ("no Bios link", None, true),
            ("an empty Bios resource", Some(bios(json!({}))), true),
            (
                "reported attributes",
                Some(bios(json!({"VMXEN": "Enable"}))),
                false,
            ),
        ];
        for (scenario, document, not_ready) in cases {
            let system = match &document {
                Some(_) => json!({"@odata.id": SYSTEM, "Id": "Self", "Name": "System",
                                  "Bios": {"@odata.id": BIOS}}),
                None => json!({"@odata.id": SYSTEM, "Id": "Self", "Name": "System"}),
            };
            let mut fixture =
                Fixture::new("AMI", "AMI Redfish Server", "Self", "Self").document(SYSTEM, system);
            if let Some(document) = document {
                fixture = fixture.document(BIOS, document);
            }
            let bmc = fixture.build().await;
            let cx = bmc.cx().await;

            let status = MegaRacBios
                .status(&cx, &BiosSettings::default(), None)
                .await;
            assert_eq!(
                matches!(status, Err(PlatformError::NotReady { .. })),
                not_ready,
                "{scenario}: {status:?}"
            );
            assert!(status.is_ok() || not_ready, "{scenario}: {status:?}");
        }
    }

    #[tokio::test]
    async fn tpm_clear_stages_the_bare_enum_value() {
        let bmc = Fixture::new("AMI", "AMI Redfish Server", "Self", "Self")
            .document(
                SYSTEM,
                json!({"@odata.id": SYSTEM, "Id": "Self", "Name": "System", "Bios": {"@odata.id": BIOS}}),
            )
            .document(
                BIOS,
                json!({
                    "@odata.id": BIOS,
                    "@Redfish.Settings": {"SettingsObject": {"@odata.id": SETTINGS}},
                    "Id": "BIOS",
                    "Name": "BIOS",
                    "Attributes": {TPM_OPERATION: "None"},
                }),
            )
            .document(
                SETTINGS,
                json!({"@odata.id": SETTINGS, "Id": "SD", "Name": "BIOS Settings", "Attributes": {}}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        MegaRacBios
            .clear_tpm(&cx)
            .await
            .expect("TPM clear is staged");
        let writes = bmc.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(path(&writes[0]), SETTINGS);
        assert_eq!(
            body(&writes[0]),
            json!({"Attributes": {TPM_OPERATION: "TPM Clear"}})
        );
    }
}
