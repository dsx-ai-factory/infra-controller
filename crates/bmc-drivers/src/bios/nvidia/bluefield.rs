/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Bios, BiosSettings, BiosStatus, BootInterfaceSelector, DriverOutcome, OpCx, PlatformError,
    Quirk,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::json;

use crate::bios::attributes::{BiosAttribute, desired_settings};
use crate::bios::standard::StandardBios;
use crate::bios::support::{compare, with_profile};
use crate::resources::{RedfishResourcesExt as _, attribute_map};

/// NVIDIA BlueField-3: the DPU BIOS exposes no `ResetBios` or `ChangePassword`
/// actions; both are write-only attributes on the pending settings.
///
/// BMC 24.10 dropped the spaces from some attribute names, so expected values
/// take whichever spelling the BIOS reports. Until the UEFI finishes POST the
/// BIOS reports neither, so the attributes show as missing and are written
/// under the spelling [`Quirk::BlueFieldSpacedBiosAttributeNames`] selects,
/// retrying with the other spelling when the BMC rejects it.
pub(crate) struct BlueFieldBios;

/// Attributes reported either without or with spaces, depending on firmware.
const RENAMED: [(&str, &str); 2] = [
    ("HostPrivilegeLevel", "Host Privilege Level"),
    ("InternalCPUModel", "Internal CPU Model"),
];

/// Declared under their unspaced names; [`reported_spellings`] renames them.
const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("HostPrivilegeLevel", "Restricted").required(),
    BiosAttribute::string("InternalCPUModel", "Embedded").required(),
];

/// `settings` with each renamed attribute under its unspaced name.
fn unspaced(mut settings: BiosSettings) -> BiosSettings {
    for (unspaced, spaced) in RENAMED {
        if let Some(value) = settings.attributes.remove(spaced) {
            settings.attributes.insert(unspaced.to_string(), value);
        }
    }
    settings
}

/// `expected` with each renamed attribute under the spaced name when `current`
/// reports that spelling, or reports neither on firmware that spaces them.
fn reported_spellings(
    current: &BiosSettings,
    mut expected: BiosSettings,
    firmware_spaced: bool,
) -> BiosSettings {
    for (unspaced, spaced) in RENAMED {
        let reports_spaced = current.attributes.contains_key(spaced)
            || (firmware_spaced && !current.attributes.contains_key(unspaced));
        if reports_spaced && let Some(value) = expected.attributes.remove(unspaced) {
            expected.attributes.insert(spaced.to_string(), value);
        }
    }
    expected
}

/// `settings` with each renamed attribute under its other spelling.
fn respelled(mut settings: BiosSettings) -> BiosSettings {
    for (unspaced, spaced) in RENAMED {
        if let Some(value) = settings.attributes.remove(unspaced) {
            settings.attributes.insert(spaced.to_string(), value);
        } else if let Some(value) = settings.attributes.remove(spaced) {
            settings.attributes.insert(unspaced.to_string(), value);
        }
    }
    settings
}

/// Whether the BMC's error names a renamed attribute `written` carries.
fn rejects_spelling(written: &BiosSettings, message: &str) -> bool {
    RENAMED
        .iter()
        .flat_map(|(unspaced, spaced)| [unspaced, spaced])
        .any(|key| written.attributes.contains_key(*key) && message.contains(key))
}

/// The table and `profile`, normalized to unspaced names, then renamed to the
/// spelling `current` reports.
fn expected_settings<B: Bmc>(
    cx: &OpCx<'_, B>,
    current: &BiosSettings,
    profile: &BiosSettings,
) -> BiosSettings {
    let expected = with_profile(
        desired_settings(ATTRIBUTES, &unspaced(current.clone())),
        &unspaced(profile.clone()),
    );
    reported_spellings(
        current,
        expected,
        cx.has_quirk(Quirk::BlueFieldSpacedBiosAttributeNames),
    )
}

#[async_trait]
impl<B: Bmc> Bios<B> for BlueFieldBios
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
        let expected = expected_settings(cx, &current, profile);
        match cx.stage_bios_attributes(&expected.attributes).await {
            Err(PlatformError::Bmc { message, .. }) if rejects_spelling(&expected, &message) => {
                cx.stage_bios_attributes(&respelled(expected).attributes)
                    .await
            }
            result => result,
        }
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        _boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        let current = cx.current_bios_settings().await?;
        Ok(compare(&current, &expected_settings(cx, &current, profile)))
    }

    async fn reset(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        cx.stage_bios_attributes(&attribute_map([("ResetEfiVars", json!(true))]))
            .await
    }

    /// Written unconditionally: the password attributes are write-only, so a
    /// value that happens to read back must still be sent.
    async fn change_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.write_bios_attributes(&attribute_map([
            ("CurrentUefiPassword", json!(current_password)),
            ("UefiPassword", json!(new_password)),
        ]))
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use axum::http::{Method, StatusCode};

    use super::*;
    use crate::test_support::{Fixture, FixtureBmc, body};

    const BIOS: &str = "/redfish/v1/Systems/Bluefield/Bios";
    const SETTINGS: &str = "/redfish/v1/Systems/Bluefield/Bios/Settings";

    async fn bluefield(attributes: serde_json::Value) -> FixtureBmc {
        bluefield_fixture(attributes).build().await
    }

    fn bluefield_fixture(attributes: serde_json::Value) -> Fixture {
        Fixture::new("Nvidia", "BlueField-3 DPU", "Bluefield", "Bluefield_BMC")
            .document(
                "/redfish/v1/Systems/Bluefield",
                json!({
                    "@odata.id": "/redfish/v1/Systems/Bluefield",
                    "Id": "Bluefield",
                    "Name": "Bluefield",
                    "Bios": {"@odata.id": BIOS},
                }),
            )
            .document(
                BIOS,
                json!({
                    "@odata.id": BIOS,
                    "@Redfish.Settings": {"SettingsObject": {"@odata.id": SETTINGS}},
                    "Id": "BIOS",
                    "Name": "BIOS Configuration Current Settings",
                    "Attributes": attributes,
                }),
            )
            .document(
                SETTINGS,
                json!({
                    "@odata.id": SETTINGS,
                    "Id": "BIOS_Settings",
                    "Name": "BIOS Configuration",
                    "Attributes": {},
                }),
            )
    }

    #[tokio::test]
    async fn apply_before_post_writes_the_firmware_spelling_and_retries_the_other() {
        let unspaced = json!({"Attributes": {
            "HostPrivilegeLevel": "Restricted",
            "InternalCPUModel": "Embedded",
        }});
        let spaced = json!({"Attributes": {
            "Host Privilege Level": "Restricted",
            "Internal CPU Model": "Embedded",
        }});
        let rejection = json!({"error": {"@Message.ExtendedInfo": [{
            "MessageId": "Base.1.15.PropertyUnknown",
            "Message": "The property HostPrivilegeLevel is not in the list of valid properties for the resource.",
        }]}});
        let no_quirks = BTreeSet::new();
        let spaced_firmware = BTreeSet::from([Quirk::BlueFieldSpacedBiosAttributeNames]);

        for (scenario, quirks, rejects, expected) in [
            (
                "spaced firmware",
                &spaced_firmware,
                false,
                vec![spaced.clone()],
            ),
            (
                "unspaced name rejected",
                &no_quirks,
                true,
                vec![unspaced.clone(), spaced.clone()],
            ),
        ] {
            let mut fixture = bluefield_fixture(json!({}));
            if rejects {
                fixture = fixture.respond(
                    Method::PATCH,
                    SETTINGS,
                    StatusCode::BAD_REQUEST,
                    Some(rejection.clone()),
                );
            }
            let bmc = fixture.build().await;
            let cx = bmc.cx().await.with_quirks(quirks);

            let result = BlueFieldBios
                .apply(&cx, &BiosSettings::default(), None)
                .await;
            assert_eq!(result.is_ok(), !rejects, "{scenario}");
            assert_eq!(
                bmc.writes().iter().map(body).collect::<Vec<_>>(),
                expected,
                "{scenario}"
            );
        }
    }

    #[tokio::test]
    async fn status_follows_the_reported_spelling_and_flags_a_bios_before_post() {
        struct Case {
            scenario: &'static str,
            reported: serde_json::Value,
            differences: Vec<&'static str>,
        }
        let cases = [
            Case {
                scenario: "unspaced firmware with one setting wrong",
                reported: json!({"HostPrivilegeLevel": "Restricted", "InternalCPUModel": "Separated"}),
                differences: vec!["InternalCPUModel"],
            },
            Case {
                scenario: "spaced firmware already set up",
                reported: json!({"Host Privilege Level": "Restricted", "Internal CPU Model": "Embedded"}),
                differences: vec![],
            },
            Case {
                scenario: "UEFI has not finished POST",
                reported: json!({}),
                differences: vec!["HostPrivilegeLevel", "InternalCPUModel"],
            },
        ];

        for case in cases {
            let bmc = bluefield(case.reported).await;
            let cx = bmc.cx().await;
            let status = BlueFieldBios
                .status(&cx, &BiosSettings::default(), None)
                .await
                .expect("status reads");
            assert_eq!(
                status
                    .differences
                    .iter()
                    .map(|difference| difference.key.as_str())
                    .collect::<Vec<_>>(),
                case.differences,
                "{}",
                case.scenario
            );
        }
    }
}
