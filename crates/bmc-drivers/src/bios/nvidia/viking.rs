/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Bios, BiosSettings, BiosStatus, BootInterfaceSelector, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::{ActionError, Bmc, EntityTypeRef};
use serde_json::json;

use crate::bios::attributes::BiosAttribute;
use crate::bios::standard::StandardBios;
use crate::bios::support::{RedfishBiosExt as _, compare, expected, settings};
use crate::resources::RedfishResourcesExt as _;

/// NVIDIA DGX Viking: AMI firmware naming the password `AdminPassword`; BIOS
/// defaults are restored by clearing the host BIOS NVRAM through the
/// UpdateService OEM action.
pub(crate) struct VikingBios;

const UEFI_PASSWORD_NAME: &str = "AdminPassword";

/// Serial console attributes belong to the console capability.
const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("SriovSupport", "Enabled"),
    BiosAttribute::string("SRIOVEnable", "Enable"),
    BiosAttribute::string("VTdSupport", "Enable"),
    BiosAttribute::string("Ipv4Http", "Enabled"),
    BiosAttribute::string("Ipv4Pxe", "Disabled"),
    BiosAttribute::string("Ipv6Http", "Enabled"),
    BiosAttribute::string("Ipv6Pxe", "Disabled"),
    INFINITE_BOOT,
];

const INFINITE_BOOT: BiosAttribute = BiosAttribute::string("NvidiaInfiniteboot", "Enable");

fn tpm_clear() -> BiosSettings {
    settings([
        ("TpmOperation", json!("TPM Clear")),
        ("TpmSupport", json!("Enable")),
    ])
}

/// The firmware inventory entry whose NVRAM holds the host BIOS settings.
const HOST_BIOS_INVENTORY: &str = "HostBIOS_0";

/// Clears the host BIOS NVRAM through the NVIDIA UpdateService OEM action.
/// The target is named from the inventory collection's link: reading the
/// whole inventory can outlast the request timeout on Viking.
async fn clear_nvram<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    let service = cx
        .service_root()
        .update_service()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?;
    let host_bios = format!(
        "{}/{HOST_BIOS_INVENTORY}",
        service
            .raw()
            .firmware_inventory
            .as_ref()
            .ok_or(PlatformError::Unsupported)?
            .odata_id()
    );
    service
        .oem_nvidia_actions()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .clear_nvram(vec![host_bios])
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

#[async_trait]
impl<B: Bmc> Bios<B> for VikingBios
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

    async fn reset(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        clear_nvram(cx).await
    }

    async fn clear_nvram(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        clear_nvram(cx).await
    }

    /// Viking firmware does not support reverting staged settings.
    async fn clear_pending(&self, _cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        Ok(DriverOutcome::complete())
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
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const UPDATE_SERVICE: &str = "/redfish/v1/UpdateService";
    const CLEAR_NVRAM: &str =
        "/redfish/v1/UpdateService/Actions/Oem/NvidiaUpdateService.ClearNVRAM";

    /// The fixture serves no firmware inventory, so a reset that read it
    /// would fail instead of naming the host BIOS entry from the link.
    #[tokio::test]
    async fn reset_clears_host_bios_nvram_without_reading_the_inventory() {
        let bmc = Fixture::new("AMI", "AMI Redfish Server", "DGX", "BMC")
            .document(
                UPDATE_SERVICE,
                json!({
                    "@odata.id": UPDATE_SERVICE,
                    "Id": "UpdateService",
                    "Name": "Update Service",
                    "FirmwareInventory": {"@odata.id": "/redfish/v1/UpdateService/FirmwareInventory"},
                    "Actions": {"Oem": {"#NvidiaUpdateService.ClearNVRAM": {"target": CLEAR_NVRAM}}},
                }),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        assert_eq!(VikingBios.reset(&cx).await, Ok(DriverOutcome::complete()));
        let writes = bmc.writes();
        assert_eq!(writes.iter().map(path).collect::<Vec<_>>(), [CLEAR_NVRAM]);
        assert_eq!(
            body(&writes[0]),
            json!({"Targets": ["/redfish/v1/UpdateService/FirmwareInventory/HostBIOS_0"]})
        );
    }
}
