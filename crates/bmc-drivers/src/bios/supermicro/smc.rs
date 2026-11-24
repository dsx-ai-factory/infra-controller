/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;

use async_trait::async_trait;
use bmc_platform::{
    Bios, BiosSettings, BiosStatus, BootInterfaceSelector, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::{Value, json};

use crate::bios::attributes::AttributeValue::{self, Bool, String as Text};
use crate::bios::standard::StandardBios;
use crate::bios::support::{RedfishBiosExt as _, compare, with_profile};
use crate::resources::{RedfishResourcesExt as _, bios_attributes, bios_update};

/// Supermicro hosts: the BIOS appends a registry suffix to every attribute
/// name, as in `IPv4HTTPSupport_009F`, and a TPM clear is a pending operation
/// written to the current BIOS resource.
pub(crate) struct SmcBios;

/// Expected values by attribute-name prefix. `Re_tryBoot` is left as the
/// host has it: some BIOS versions offer only `Disabled` and `Enabled`.
const ATTRIBUTES: &[(&str, AttributeValue)] = &[
    ("QuietBoot", Bool(false)),
    ("CSMSupport", Text("Disabled")),
    ("SecureBootEnable", Bool(false)),
    ("TXTSupport", Text("Enabled")),
    ("DeviceSelect", Text("TPM 2.0")),
    ("IntelVTforDirectedI_O_VT_d", Text("Enable")),
    ("SR-IOVSupport", Text("Enabled")),
    ("SR_IOVSupport", Text("Enabled")),
    ("IPv4HTTPSupport", Text("Enabled")),
    ("IPv4PXESupport", Text("Disabled")),
    ("IPv6HTTPSupport", Text("Disabled")),
    ("IPv6PXESupport", Text("Disabled")),
];

/// Boards spell these attributes' values either `Enabled`/`Disabled` or
/// `Enable`/`Disable`; each is enabled in the spelling the board uses.
const ENABLED_IN_BOARD_SPELLING: &[&str] =
    &["SecurityDeviceSupport", "IntelVirtualizationTechnology"];

/// The UEFI administrator password's `PasswordName`, before any registry
/// suffix.
const UEFI_PASSWORD_NAME: &str = "AdministratorPassword";

/// The `PasswordName` the board takes among the BIOS `attributes` names: the
/// attribute carrying the password, with the registry suffix some firmware
/// appends and requires (as in `AdministratorPassword_00B7`), or the plain
/// name when none is reported. Password attributes always read null, so the
/// names come from the BIOS resource rather than its non-null settings.
fn uefi_password_name<'a>(attributes: impl IntoIterator<Item = &'a String>) -> String {
    attributes
        .into_iter()
        .find(|name| name.starts_with(UEFI_PASSWORD_NAME))
        .cloned()
        .unwrap_or_else(|| UEFI_PASSWORD_NAME.to_string())
}

/// The expected value of every reported attribute the table names.
fn expected_settings(current: &BiosSettings) -> Result<BiosSettings, PlatformError> {
    let mut expected = BiosSettings::default();
    for (name, reported) in &current.attributes {
        let value = if ENABLED_IN_BOARD_SPELLING
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            enabled_in_board_spelling(name, reported)?
        } else if let Some((_, value)) = ATTRIBUTES
            .iter()
            .find(|(prefix, _)| name.starts_with(prefix))
        {
            Value::from(*value)
        } else {
            continue;
        };
        expected.attributes.insert(name.clone(), value);
    }
    Ok(expected)
}

fn enabled_in_board_spelling(name: &str, reported: &Value) -> Result<Value, PlatformError> {
    match reported.as_str() {
        Some("Enabled" | "Disabled") => Ok(json!("Enabled")),
        Some("Enable" | "Disable") => Ok(json!("Enable")),
        _ => Err(PlatformError::InvalidResponse {
            message: format!("{name} reports {reported}, which is neither spelling"),
        }),
    }
}

#[async_trait]
impl<B: Bmc> Bios<B> for SmcBios
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
        cx.stage_bios_attributes(&with_profile(expected_settings(&current)?, profile).attributes)
            .await
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        let current = cx.current_bios_settings().await?;
        Ok(compare(
            &current,
            &with_profile(expected_settings(&current)?, profile),
        ))
    }

    async fn change_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        let bios = cx.bios().await?.raw();
        let name = uefi_password_name(
            bios.attributes
                .iter()
                .flat_map(|attributes| attributes.dynamic_properties.keys()),
        );
        cx.change_bios_password(&name, current_password, new_password)
            .await
    }

    /// The board names its TPM pending operation `PendingOperation*`, and
    /// takes it on the current BIOS resource rather than the settings resource.
    async fn clear_tpm(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        let bios = cx.bios().await?;
        let operation = bios_attributes(&bios.raw())
            .into_keys()
            .find(|key| key.starts_with("PendingOperation"))
            .ok_or(PlatformError::Unsupported)?;
        let attributes = BTreeMap::from([(operation, json!("TPM Clear"))]);
        bios.update(&bios_update(&attributes)?)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use carbide_test_support::value_scenarios;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/1";
    const BIOS: &str = "/redfish/v1/Systems/1/Bios";

    /// Some firmware refuses the plain `AdministratorPassword` and takes only
    /// the suffixed attribute name, which reads null like every password.
    #[tokio::test]
    async fn the_uefi_password_name_carries_the_boards_suffix() {
        for (attributes, expected) in [
            (
                json!({"AdministratorPassword_00B7": null, "PasswordCheck_00B9": "Setup"}),
                "AdministratorPassword_00B7",
            ),
            (json!({"QuietBoot": false}), "AdministratorPassword"),
        ] {
            let bmc = Fixture::new("Supermicro", "", "1", "1")
                .document(
                    SYSTEM,
                    json!({"@odata.id": SYSTEM, "Id": "1", "Name": "System", "Bios": {"@odata.id": BIOS}}),
                )
                .document(
                    BIOS,
                    json!({"@odata.id": BIOS, "Id": "BIOS", "Name": "BIOS", "Attributes": attributes,
                           "Actions": {"#Bios.ChangePassword": {
                               "target": format!("{BIOS}/Actions/Bios.ChangePassword")}}}),
                )
                .build()
                .await;
            let cx = bmc.cx().await;
            SmcBios
                .change_uefi_password(&cx, "", "new")
                .await
                .expect("password change is accepted");
            let writes = bmc.writes();
            assert_eq!(writes.len(), 1, "{attributes}");
            assert_eq!(
                body(&writes[0])["PasswordName"],
                json!(expected),
                "{attributes}"
            );
        }
    }

    #[tokio::test]
    async fn tpm_clear_writes_the_boards_pending_operation_to_the_current_bios() {
        for (attributes, expected) in [
            (
                json!({"PendingOperation_7A3C": "None", "QuietBoot": "Enabled"}),
                Some(json!({"Attributes": {"PendingOperation_7A3C": "TPM Clear"}})),
            ),
            (json!({"QuietBoot": "Enabled"}), None),
        ] {
            let bmc = Fixture::new("Supermicro", "", "1", "1")
                .document(
                    SYSTEM,
                    json!({"@odata.id": SYSTEM, "Id": "1", "Name": "System", "Bios": {"@odata.id": BIOS}}),
                )
                .document(
                    BIOS,
                    json!({"@odata.id": BIOS, "Id": "BIOS", "Name": "BIOS", "Attributes": attributes}),
                )
                .build()
                .await;
            let cx = bmc.cx().await;

            let result = SmcBios.clear_tpm(&cx).await;
            let writes = bmc.writes();
            match expected {
                Some(expected) => {
                    assert_eq!(result, Ok(DriverOutcome::complete()));
                    assert_eq!(writes.len(), 1);
                    assert_eq!(writes[0].method, Method::PATCH);
                    assert_eq!(path(&writes[0]), BIOS);
                    assert_eq!(body(&writes[0]), expected);
                }
                None => {
                    assert_eq!(result, Err(PlatformError::Unsupported));
                    assert!(writes.is_empty());
                }
            }
        }
    }

    #[test]
    fn expected_values_follow_suffixed_names_and_the_boards_spelling() {
        value_scenarios!(run = |(name, reported): (&str, &str)| expected_settings(
            &serde_json::from_value(json!({"attributes": {name: reported, "Unrelated": "x"}}))
                .expect("settings")
        )
        .map(|expected| expected.attributes.into_iter().collect::<Vec<_>>());
            "suffixed name" {
                ("IPv4HTTPSupport_009F", "Disabled") => Ok(vec![("IPv4HTTPSupport_009F".to_string(), json!("Enabled"))]),
            }
            "spelling family" {
                ("SecurityDeviceSupport_0123", "Disabled") => Ok(vec![("SecurityDeviceSupport_0123".to_string(), json!("Enabled"))]),
                ("SecurityDeviceSupport_0123", "Disable") => Ok(vec![("SecurityDeviceSupport_0123".to_string(), json!("Enable"))]),
                ("SecurityDeviceSupport_0123", "Enable") => Ok(vec![("SecurityDeviceSupport_0123".to_string(), json!("Enable"))]),
                ("IntelVirtualizationTechnology_0042", "Disabled") => Ok(vec![("IntelVirtualizationTechnology_0042".to_string(), json!("Enabled"))]),
            }
            "unknown spelling" {
                ("SecurityDeviceSupport_0123", "Off") => Err(PlatformError::InvalidResponse {
                    message: "SecurityDeviceSupport_0123 reports \"Off\", which is neither spelling".to_string(),
                }),
            }
        );
    }
}
