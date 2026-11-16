/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! BIOS operations shared by the BIOS drivers.
//!
//! A driver reads [`RedfishResourcesExt::current_bios_settings`], works out
//! the settings it expects, then either [`compare`]s or stages those with
//! [`RedfishResourcesExt::stage_bios_attributes`].

use bmc_platform::{BiosDiff, BiosSettings, BiosStatus, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::Value;

use crate::bios::attributes::{BiosAttribute, desired_settings};
use crate::resources::{RedfishResourcesExt, attribute_map};

/// The settings `attributes` call for on a BIOS reporting `current`, with the
/// caller's `profile` taking precedence.
pub(super) fn expected(
    attributes: &[BiosAttribute],
    current: &BiosSettings,
    profile: &BiosSettings,
) -> BiosSettings {
    with_profile(desired_settings(attributes, current), profile)
}

/// `settings` with every entry of the caller's `profile` added, replacing any
/// entry for the same attribute.
pub(super) fn with_profile(mut settings: BiosSettings, profile: &BiosSettings) -> BiosSettings {
    settings.attributes.extend(
        profile
            .attributes
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    settings
}

/// Settings holding exactly `entries`.
pub(super) fn settings<const N: usize>(entries: [(&str, Value); N]) -> BiosSettings {
    BiosSettings {
        attributes: attribute_map(entries),
    }
}

/// Whether `current` holds `expected`, with every expected attribute whose
/// current value differs.
pub(super) fn compare(current: &BiosSettings, expected: &BiosSettings) -> BiosStatus {
    let differences: Vec<BiosDiff> = expected
        .attributes
        .iter()
        .filter_map(|(key, expected)| {
            let actual = current.attributes.get(key);
            (actual != Some(expected)).then(|| BiosDiff {
                key: key.clone(),
                expected: expected.clone(),
                actual: actual.cloned(),
            })
        })
        .collect();
    BiosStatus {
        is_applied: differences.is_empty(),
        differences,
    }
}

/// BIOS attribute checks and password mechanics shared by BIOS drivers.
pub(super) trait RedfishBiosExt<B: Bmc> {
    /// The attributes the BIOS currently runs with, for a BIOS that reports
    /// them to the BMC as the host starts: a BMC restarted since then serves
    /// no `Bios` link, or an empty `Bios` resource, which is
    /// [`PlatformError::NotReady`] until the host starts again.
    async fn reported_bios_settings(&self) -> Result<BiosSettings, PlatformError>;

    /// Whether the current BIOS holds `attribute`'s expected value; `None` when
    /// the BIOS does not report the attribute.
    async fn bios_attribute_holds(
        &self,
        attribute: BiosAttribute,
    ) -> Result<Option<bool>, PlatformError>;

    /// Changes the UEFI password named `password_name` through the advertised
    /// `Bios.ChangePassword` action; an empty `new_password` clears it.
    async fn change_bios_password(
        &self,
        password_name: &str,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError>
    where
        B::Error: ActionError;
}

impl<B: Bmc> RedfishBiosExt<B> for OpCx<'_, B> {
    async fn reported_bios_settings(&self) -> Result<BiosSettings, PlatformError> {
        let not_ready = || PlatformError::NotReady {
            message: "the BIOS has not reported its attributes since the BMC restarted; \
                      they return once the host starts again"
                .to_string(),
        };
        match self.current_bios_settings().await {
            Ok(current) if current.attributes.is_empty() => Err(not_ready()),
            Err(PlatformError::Unsupported) => Err(not_ready()),
            result => result,
        }
    }

    async fn bios_attribute_holds(
        &self,
        attribute: BiosAttribute,
    ) -> Result<Option<bool>, PlatformError> {
        Ok(self
            .current_bios_settings()
            .await?
            .attributes
            .get(attribute.name)
            .map(|value| attribute.value.matches(value)))
    }

    async fn change_bios_password(
        &self,
        password_name: &str,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError>
    where
        B::Error: ActionError,
    {
        self.bios()
            .await?
            .change_password(
                password_name.to_string(),
                Some(current_password.to_string()),
                new_password.to_string(),
            )
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }
}

/// Asserts that `driver`'s apply of `profile`, on a BIOS already holding
/// `attributes`, `profile`, and the `idle` TPM attributes, stages exactly the
/// `staged` TPM clear request, and that status still reads as applied: the
/// request is one-shot and the BIOS resets it once performed.
#[cfg(test)]
pub(super) async fn assert_apply_requests_tpm_clear(
    driver: &dyn bmc_platform::Bios<bmc_mock::test_support::TestBmc>,
    attributes: &[BiosAttribute],
    profile: Value,
    idle: Value,
    staged: Value,
) {
    use std::collections::BTreeMap;

    use serde_json::json;

    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/1";
    const BIOS: &str = "/redfish/v1/Systems/1/Bios";
    const SETTINGS: &str = "/redfish/v1/Systems/1/Bios/SD";

    let profile = BiosSettings {
        attributes: serde_json::from_value(profile).expect("an object"),
    };
    let mut current = with_profile(
        desired_settings(attributes, &BiosSettings::default()),
        &profile,
    )
    .attributes;
    current.extend(serde_json::from_value::<BTreeMap<String, Value>>(idle).expect("an object"));
    let bmc = Fixture::new("Vendor", "", "1", "1")
        .document(
            SYSTEM,
            json!({"@odata.id": SYSTEM, "Id": "1", "Name": "System", "Bios": {"@odata.id": BIOS}}),
        )
        .document(
            BIOS,
            json!({
                "@odata.id": BIOS,
                "@Redfish.Settings": {"SettingsObject": {"@odata.id": SETTINGS}},
                "Id": "BIOS",
                "Name": "BIOS",
                "Attributes": current,
            }),
        )
        .document(
            SETTINGS,
            json!({"@odata.id": SETTINGS, "Id": "SD", "Name": "BIOS Settings", "Attributes": {}}),
        )
        .build()
        .await;
    let cx = bmc.cx().await;

    driver
        .apply(&cx, &profile, None)
        .await
        .expect("apply stages the settings");
    let writes = bmc.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(path(&writes[0]), SETTINGS);
    assert_eq!(body(&writes[0]), json!({ "Attributes": staged }));
    let status = driver
        .status(&cx, &profile, None)
        .await
        .expect("status reads");
    assert!(status.is_applied, "{status:?}");
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn from_json(attributes: serde_json::Value) -> BiosSettings {
        serde_json::from_value(json!({ "attributes": attributes })).expect("settings")
    }

    #[test]
    fn profile_wins_over_the_platform_expectations() {
        let table = [BiosAttribute::string("Virtualization", "Enabled")];
        assert_eq!(
            expected(
                &table,
                &from_json(json!({"Virtualization": "Disabled", "Other": "x"})),
                &from_json(json!({"Virtualization": "Profile", "PowerProfile": "Performance"})),
            ),
            from_json(json!({
                "Virtualization": "Profile",
                "PowerProfile": "Performance",
            }))
        );
    }
}
