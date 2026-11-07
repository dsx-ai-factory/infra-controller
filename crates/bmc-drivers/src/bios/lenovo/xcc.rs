/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Bios, BiosDiff, BiosSettings, BiosStatus, BootInterfaceSelector, DriverOutcome, OpCx,
    PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::{Value, json};

use crate::bios::attributes::BiosAttribute;
use crate::bios::lenovo::ensure_bios_settings_unlocked;
use crate::bios::standard::StandardBios;
use crate::bios::support::{RedfishBiosExt as _, compare, expected, settings};
use crate::boot_order::lenovo::{NETWORK, first_group, network_group_first};
use crate::resources::RedfishResourcesExt as _;

/// Lenovo XCC names the UEFI administrator password `UefiAdminPassword`, and
/// clears it when both the old and new passwords are empty, so a clear needs
/// no current password and works whatever the password was.
pub(crate) struct XccBios;

const UEFI_PASSWORD_NAME: &str = "UefiAdminPassword";

/// Both CPU vendors' virtualization attributes are optional, since each CPU
/// reports only its own; [`require_virtualization_attribute`] fails when
/// neither is reported. Serial console attributes belong to the console
/// capability.
const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("Processors_IntelVirtualizationTechnology", "Enabled"),
    BiosAttribute::string("Processors_SVMMode", "Enabled"),
    BiosAttribute::string("BootModes_SystemBootMode", "UEFIMode").required(),
    BiosAttribute::string("NetworkStackSettings_IPv4HTTPSupport", "Enabled").required(),
    BiosAttribute::string("NetworkStackSettings_IPv4PXESupport", "Disabled").required(),
    BiosAttribute::string("NetworkStackSettings_IPv6PXESupport", "Disabled").required(),
    INFINITE_BOOT,
    BiosAttribute::string("BootModes_PreventOSChangesToBootOrder", "Enabled").required(),
    // Only older systems still have a legacy BIOS mode.
    BiosAttribute::string("LegacyBIOS_NonOnboardPXE", "Disabled"),
    BiosAttribute::string("LegacyBIOS_LegacyBIOS", "Disabled"),
];

const INFINITE_BOOT: BiosAttribute =
    BiosAttribute::string("BootModes_InfiniteBootRetry", "Enabled").required();

fn tpm_clear() -> BiosSettings {
    settings([("TrustedComputingGroup_DeviceOperation", json!("Clear"))])
}

/// XCC names CPU virtualization after the CPU vendor.
const VIRTUALIZATION_ATTRIBUTES: [&str; 2] = [
    "Processors_IntelVirtualizationTechnology",
    "Processors_SVMMode",
];

/// Fails when the BIOS reports neither CPU vendor's virtualization attribute,
/// which leaves virtualization impossible to enable.
fn require_virtualization_attribute(current: &BiosSettings) -> Result<(), PlatformError> {
    if VIRTUALIZATION_ATTRIBUTES
        .iter()
        .any(|name| current.attributes.contains_key(*name))
    {
        return Ok(());
    }
    Err(PlatformError::InvalidResponse {
        message: format!(
            "BIOS reports neither {}",
            VIRTUALIZATION_ATTRIBUTES.join(" nor ")
        ),
    })
}

fn expected_settings(
    current: &BiosSettings,
    profile: &BiosSettings,
) -> Result<BiosSettings, PlatformError> {
    require_virtualization_attribute(current)?;
    Ok(expected(ATTRIBUTES, current, profile))
}

/// The status difference reporting which device group boots first.
const FIRST_BOOT_GROUP: &str = "boot_first_type";

#[async_trait]
impl<B: Bmc> Bios<B> for XccBios
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Bios<B> {
        &StandardBios
    }

    /// Also puts the network device group first so boot order setup, after
    /// the reset, finds the adapters it orders.
    async fn apply(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<DriverOutcome, PlatformError> {
        ensure_bios_settings_unlocked(cx).await?;
        let current = cx.current_bios_settings().await?;
        let staged = cx
            .stage_bios_attributes(&expected_settings(&current, profile)?.attributes)
            .await?;
        Ok(staged.merge(network_group_first(cx).await?))
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        let current = cx.current_bios_settings().await?;
        let mut status = compare(&current, &expected_settings(&current, profile)?);
        let group = first_group(cx).await?;
        if group.as_deref() != Some(NETWORK) {
            status.differences.push(BiosDiff {
                key: FIRST_BOOT_GROUP.to_string(),
                expected: json!(NETWORK),
                actual: group.map(Value::from),
            });
            status.is_applied = false;
        }
        Ok(status)
    }

    async fn change_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        ensure_bios_settings_unlocked(cx).await?;
        cx.change_bios_password(UEFI_PASSWORD_NAME, current_password, new_password)
            .await
    }

    async fn clear_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        _current_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        ensure_bios_settings_unlocked(cx).await?;
        cx.change_bios_password(UEFI_PASSWORD_NAME, "", "").await
    }

    async fn clear_tpm(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        ensure_bios_settings_unlocked(cx).await?;
        cx.stage_bios_attributes(&tpm_clear().attributes).await
    }

    async fn infinite_boot_enabled(&self, cx: &OpCx<'_, B>) -> Result<Option<bool>, PlatformError> {
        cx.bios_attribute_holds(INFINITE_BOOT).await
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body};

    /// XCC 2 refuses a clear naming an old password it does not accept with
    /// `ActionParameterValueFormatError`; XCC 2 and XCC 3 clear the password
    /// when both are empty.
    #[tokio::test]
    async fn a_uefi_password_clear_names_no_old_password() {
        const SYSTEM: &str = "/redfish/v1/Systems/1";
        const BIOS: &str = "/redfish/v1/Systems/1/Bios";
        let bmc = Fixture::new("Lenovo", "", "1", "1")
            .document(
                SYSTEM,
                json!({"@odata.id": SYSTEM, "Id": "1", "Name": "System", "Bios": {"@odata.id": BIOS}}),
            )
            .document(
                BIOS,
                json!({"@odata.id": BIOS, "Id": "Bios", "Name": "Bios", "Attributes": {},
                       "Actions": {"#Bios.ChangePassword": {
                           "target": format!("{BIOS}/Actions/Bios.ChangePassword")}}}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;
        XccBios
            .clear_uefi_password(&cx, "old-password")
            .await
            .expect("the clear is accepted");
        let writes = bmc.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(
            body(&writes[0]),
            json!({"PasswordName": UEFI_PASSWORD_NAME, "OldPassword": "", "NewPassword": ""})
        );
    }

    /// XCC 3's `SettingsLockdown` refuses UEFI password changes, so the driver
    /// reports `LockedDown` without sending one; firmware that does not report
    /// the control is never locked.
    #[tokio::test]
    async fn settings_lockdown_blocks_uefi_password_changes() {
        const SYSTEM: &str = "/redfish/v1/Systems/1";
        const BIOS: &str = "/redfish/v1/Systems/1/Bios";
        for (scenario, settings_lockdown, locked) in [
            ("locked", Some(true), true),
            ("unlocked", Some(false), false),
            ("firmware without the control", None, false),
        ] {
            let mut bios = json!({"@odata.id": BIOS, "Id": "Bios", "Name": "Bios", "Attributes": {},
                "Actions": {"#Bios.ChangePassword": {
                    "target": format!("{BIOS}/Actions/Bios.ChangePassword")}}});
            if let Some(settings_lockdown) = settings_lockdown {
                bios["Oem"] = json!({"Lenovo": {"SettingsLockdown": settings_lockdown}});
            }
            let bmc = Fixture::new("Lenovo", "", "1", "1")
                .document(
                    SYSTEM,
                    json!({"@odata.id": SYSTEM, "Id": "1", "Name": "System", "Bios": {"@odata.id": BIOS}}),
                )
                .document(BIOS, bios)
                .build()
                .await;
            let cx = bmc.cx().await;

            let result = XccBios.clear_uefi_password(&cx, "old-password").await;
            assert_eq!(
                matches!(result, Err(PlatformError::LockedDown)),
                locked,
                "{scenario}: {result:?}"
            );
            assert_eq!(bmc.writes().len(), usize::from(!locked), "{scenario}");
        }
    }

    #[test]
    fn virtualization_needs_either_cpu_vendors_attribute() {
        value_scenarios!(run = |name: &str| require_virtualization_attribute(
            &serde_json::from_value(json!({"attributes": {name: "Disabled"}})).expect("settings")
        )
        .is_ok();
            "present" {
                "Processors_IntelVirtualizationTechnology" => true,
                "Processors_SVMMode" => true,
            }
            "missing" {
                "Processors_Other" => false,
            }
        );
    }
}
