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
use serde_json::{Value, json};

use crate::bios::attributes::{BiosAttribute, desired_settings};
use crate::bios::standard::StandardBios;
use crate::bios::support::{RedfishBiosExt as _, compare, settings, with_profile};
use crate::dell;
use crate::resources::RedfishResourcesExt as _;

/// Dell iDRAC BIOS behavior.
///
/// iDRAC applies staged settings only through a configuration job, refuses
/// new jobs while any is queued, and clears pending settings by deleting the
/// job queue rather than by re-writing attributes. Each write therefore
/// replaces the job of any earlier write not yet applied by a reset.
pub(crate) struct IdracBios;

/// iDRAC exposes the UEFI administrator password as `SetupPassword`.
const UEFI_PASSWORD_NAME: &str = "SetupPassword";

// Serial redirection settings, which console status also checks. A BIOS
// whose `SerialPortAddress` starts with `Serial1` takes the newer values and
// redirects automatically on COM2.
pub(crate) const FAIL_SAFE_BAUD: BiosAttribute =
    BiosAttribute::string("FailSafeBaud", "115200").required();
pub(crate) const CON_TERM_TYPE: BiosAttribute =
    BiosAttribute::string("ConTermType", "Vt100Vt220").required();
pub(crate) const REDIR_AFTER_BOOT: BiosAttribute =
    BiosAttribute::string("RedirAfterBoot", "Enabled");
pub(crate) const NEWER_SERIAL_COMM: BiosAttribute =
    BiosAttribute::string("SerialComm", "OnConRedirAuto").required();
pub(crate) const OLDER_SERIAL_COMM: BiosAttribute =
    BiosAttribute::string("SerialComm", "OnConRedir").required();
pub(crate) const NEWER_SERIAL_PORT_ADDRESS: BiosAttribute =
    BiosAttribute::string("SerialPortAddress", "Serial1Com2Serial2Com1").required();
pub(crate) const OLDER_SERIAL_PORT_ADDRESS: BiosAttribute =
    BiosAttribute::string("SerialPortAddress", "Com1").required();

const ATTRIBUTES: &[BiosAttribute] = &[
    BiosAttribute::string("InBandManageabilityInterface", "Disabled").required(),
    BiosAttribute::string("UefiVariableAccess", "Standard").required(),
    FAIL_SAFE_BAUD,
    CON_TERM_TYPE,
    REDIR_AFTER_BOOT,
    EXT_SERIAL_CONNECTOR,
    BiosAttribute::string("SriovGlobalEnable", "Enabled").required(),
    BiosAttribute::string("TpmSecurity", "On").required(),
    BiosAttribute::string("Tpm2Hierarchy", "Enabled").required(),
    BiosAttribute::string("Tpm2Algorithm", "SHA256").required(),
    BiosAttribute::string("HttpDev1EnDis", "Enabled").required(),
    BiosAttribute::string("HttpDev1TlsMode", "None").required(),
    BiosAttribute::string("PxeDev1EnDis", "Disabled").required(),
    INFINITE_BOOT,
];

const INFINITE_BOOT: BiosAttribute = BiosAttribute::string("BootSeqRetry", "Enabled").required();

/// The serial connector the console uses, on BIOSes that have the setting.
pub(crate) const EXT_SERIAL_CONNECTOR: BiosAttribute =
    BiosAttribute::string("ExtSerialConnector", "Serial1");

const BOOT_MODE: &str = "BootMode";

/// The hierarchy reads back `Enabled` once the clear has run.
fn tpm_clear() -> BiosSettings {
    settings([
        ("TpmSecurity", json!("On")),
        ("Tpm2Hierarchy", json!("Clear")),
    ])
}

/// Serial redirection in the format the BIOS uses.
fn serial_redirection(current: &BiosSettings) -> [BiosAttribute; 2] {
    let newer = current
        .attributes
        .get(OLDER_SERIAL_PORT_ADDRESS.name)
        .and_then(Value::as_str)
        .is_some_and(|address| address.starts_with("Serial1"));
    if newer {
        [NEWER_SERIAL_COMM, NEWER_SERIAL_PORT_ADDRESS]
    } else {
        [OLDER_SERIAL_COMM, OLDER_SERIAL_PORT_ADDRESS]
    }
}

/// The settings `current` should hold, with HTTP Device 1 on the boot
/// interface's `nic_slot`.
fn expected_settings(
    current: &BiosSettings,
    profile: &BiosSettings,
    nic_slot: &str,
) -> BiosSettings {
    let attributes: Vec<BiosAttribute> = ATTRIBUTES
        .iter()
        .copied()
        .chain(serial_redirection(current))
        .collect();
    let mut expected = desired_settings(&attributes, current);
    expected
        .attributes
        .insert("HttpDev1Interface".to_string(), json!(nic_slot));
    with_profile(expected, profile)
}

/// Whether `BootMode` must be written: only when reported as something other
/// than `Uefi`, since iDRAC 10 reports it read-only as `Uefi`. Status does not
/// check it.
fn needs_uefi_boot_mode(current: &BiosSettings) -> bool {
    current
        .attributes
        .get(BOOT_MODE)
        .and_then(Value::as_str)
        .is_some_and(|mode| mode != "Uefi")
}

/// Whether `current` already holds everything `apply` would stage. Every write
/// queues a job iDRAC runs on the next reset, so nothing is written then; boot
/// order setup puts the boot interface first on its own.
fn nothing_to_stage(current: &BiosSettings, expected: &BiosSettings) -> bool {
    compare(current, expected).is_applied && !needs_uefi_boot_mode(current)
}

/// `expected` plus the boot order iDRAC builds at the next reset, led by
/// `nic_slot`; boot order status checks the order that results.
fn staged_settings(
    current: &BiosSettings,
    expected: &BiosSettings,
    nic_slot: &str,
) -> BiosSettings {
    let mut writes = settings([
        ("SetBootOrderEn", json!(nic_slot)),
        ("SetBootOrderDis", json!("")),
    ]);
    if needs_uefi_boot_mode(current) {
        writes
            .attributes
            .insert(BOOT_MODE.to_string(), json!("Uefi"));
    }
    with_profile(writes, expected)
}

/// Changes the UEFI password inside a configuration job, which iDRAC requires
/// for the change to take effect.
async fn change_password_with_job<B: Bmc>(
    cx: &OpCx<'_, B>,
    current_password: &str,
    new_password: &str,
) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    dell::clear_job_queue(cx).await?;
    cx.change_bios_password(UEFI_PASSWORD_NAME, current_password, new_password)
        .await?;
    dell::create_bios_config_job(cx).await
}

#[async_trait]
impl<B: Bmc> Bios<B> for IdracBios
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Bios<B> {
        &StandardBios
    }

    /// Stages every setting as one configuration job, unless all are
    /// already in effect.
    async fn apply(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<DriverOutcome, PlatformError> {
        let nic_slot = dell::nic_slot(cx, boot_interface).await?;
        let current = cx.current_bios_settings().await?;
        let expected = expected_settings(&current, profile, &nic_slot);
        if nothing_to_stage(&current, &expected) {
            return Ok(DriverOutcome::complete());
        }
        let writes = staged_settings(&current, &expected, &nic_slot);
        dell::stage_bios_attributes(cx, &writes.attributes).await
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        let nic_slot = dell::nic_slot(cx, boot_interface).await?;
        let current = cx.current_bios_settings().await?;
        Ok(compare(
            &current,
            &expected_settings(&current, profile, &nic_slot),
        ))
    }

    async fn clear_pending(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        dell::clear_job_queue(cx).await?;
        Ok(DriverOutcome::complete())
    }

    async fn change_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        change_password_with_job(cx, current_password, new_password).await
    }

    /// With [`Quirk::DellUefiPasswordClearJobFails`], the accepted
    /// `Bios.ChangePassword` request leaves nothing to fall back from, so the
    /// clear goes straight to the import, once the job queue that would
    /// refuse it is empty.
    async fn clear_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        if cx.has_quirk(Quirk::DellUefiPasswordClearJobFails) {
            dell::clear_job_queue(cx).await?;
            return dell::clear_uefi_password_via_import(cx, current_password).await;
        }
        match change_password_with_job(cx, current_password, "").await {
            Ok(outcome) => Ok(outcome),
            Err(_) => dell::clear_uefi_password_via_import(cx, current_password).await,
        }
    }

    async fn clear_tpm(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        dell::stage_bios_attributes(cx, &tpm_clear().attributes).await
    }

    async fn infinite_boot_enabled(&self, cx: &OpCx<'_, B>) -> Result<Option<bool>, PlatformError> {
        cx.bios_attribute_holds(INFINITE_BOOT).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use carbide_test_support::value_scenarios;

    use super::*;
    use crate::bios::attributes::AttributeValue;
    use crate::test_support::{Fixture, body};

    /// An XE9680's iDRAC accepts a `Bios.ChangePassword` clear and then fails
    /// its configuration job, so with the quirk a clear empties the job
    /// queue, which would refuse the import, and imports the old and empty
    /// setup passwords.
    #[tokio::test]
    async fn the_uefi_password_is_cleared_through_a_configuration_import() {
        const MANAGER: &str = "/redfish/v1/Managers/iDRAC.Embedded.1";
        const ATTRIBUTES: &str =
            "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1";
        const JOB_SERVICE: &str = "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService";
        const DELETE_JOB_QUEUE: &str = "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService/Actions/DellJobService.DeleteJobQueue";
        const IMPORT: &str = "/redfish/v1/Managers/iDRAC.Embedded.1/Actions/Oem/EID_674_Manager.ImportSystemConfiguration";
        let bmc = Fixture::new(
            "Dell",
            "Integrated Dell Remote Access Controller",
            "System.Embedded.1",
            "iDRAC.Embedded.1",
        )
        .document(
            MANAGER,
            json!({
                "@odata.id": MANAGER,
                "Id": "iDRAC.Embedded.1",
                "Name": "Manager",
                "Links": {"Oem": {"Dell": {
                    "DellAttributes": [{"@odata.id": ATTRIBUTES}],
                    "DellJobService": {"@odata.id": JOB_SERVICE},
                }}},
                "Actions": {"Oem": {
                    "#OemManager.ImportSystemConfiguration": {"target": IMPORT},
                }},
            }),
        )
        .document(
            ATTRIBUTES,
            json!({
                "@odata.id": ATTRIBUTES,
                "Id": "iDRAC.Embedded.1",
                "Name": "Manager Attributes",
                "Attributes": {"Lockdown.1.SystemLockdown": "Disabled"},
            }),
        )
        .document(
            JOB_SERVICE,
            json!({
                "@odata.id": JOB_SERVICE,
                "Id": "Job Service",
                "Name": "DellJobService",
                "Actions": {"#DellJobService.DeleteJobQueue": {"target": DELETE_JOB_QUEUE}},
            }),
        )
        .build()
        .await;
        let quirks = BTreeSet::from([Quirk::DellUefiPasswordClearJobFails]);
        let cx = bmc.lazy_cx().with_quirks(&quirks);

        IdracBios
            .clear_uefi_password(&cx, "old<password")
            .await
            .expect("the clear is requested");
        let writes = bmc.writes();
        let uris: Vec<_> = writes.iter().map(|write| write.uri.as_str()).collect();
        assert!(uris[0].ends_with(DELETE_JOB_QUEUE), "{uris:?}");
        assert!(uris[1].ends_with(IMPORT), "{uris:?}");
        assert_eq!(uris.len(), 2, "{uris:?}");
        assert_eq!(
            body(&writes[1])["ImportBuffer"],
            json!(
                r#"<SystemConfiguration><Component FQDD="BIOS.Setup.1-1"><Attribute Name="OldSetupPassword">old&lt;password</Attribute><Attribute Name="NewSetupPassword"></Attribute></Component></SystemConfiguration>"#
            )
        );
    }

    #[test]
    fn http_device_follows_the_boot_interface_and_only_the_interface_is_checked() {
        let current = BiosSettings::default();
        let profile = BiosSettings::default();
        let expected = expected_settings(&current, &profile, "NIC.Slot.7-1-1");
        let staged = staged_settings(&current, &expected, "NIC.Slot.7-1-1");

        assert_eq!(
            expected.attributes["HttpDev1Interface"],
            json!("NIC.Slot.7-1-1")
        );
        assert!(!expected.attributes.contains_key("SetBootOrderEn"));
        assert_eq!(staged.attributes["SetBootOrderEn"], json!("NIC.Slot.7-1-1"));
        assert_eq!(staged.attributes["SetBootOrderDis"], json!(""));
        assert_eq!(
            staged.attributes["HttpDev1Interface"],
            json!("NIC.Slot.7-1-1")
        );
    }

    #[test]
    fn boot_mode_is_written_only_when_it_is_not_already_uefi() {
        value_scenarios!(run = |boot_mode: Option<&str>| {
            let current: BiosSettings = serde_json::from_value(json!({
                "attributes": boot_mode.map_or_else(|| json!({}), |mode| json!({"BootMode": mode})),
            }))
            .expect("settings");
            staged_settings(&current, &BiosSettings::default(), "NIC.Slot.7-1-1")
                .attributes
                .get(BOOT_MODE)
                .cloned()
        };
            "iDRAC 9 in legacy mode" { Some("Bios") => Some(json!("Uefi")) }
            "already Uefi, read-only on iDRAC 10" { Some("Uefi") => None }
            "not reported" { None => None }
        );
    }

    /// Every write queues a job, so a BIOS already set up must not be written.
    #[test]
    fn only_a_bios_needing_changes_is_staged() {
        let in_effect = expected_settings(
            &BiosSettings::default(),
            &BiosSettings::default(),
            "NIC.Slot.7-1-1",
        );
        let with = |name: &str, value: &str| {
            let mut current = in_effect.clone();
            current.attributes.insert(name.to_string(), json!(value));
            current
        };
        value_scenarios!(run = |current: BiosSettings| {
            let expected = expected_settings(&current, &BiosSettings::default(), "NIC.Slot.7-1-1");
            nothing_to_stage(&current, &expected)
        };
            "every setting in effect" { in_effect.clone() => true }
            "a setting differs" { with("BootSeqRetry", "Disabled") => false }
            "in effect but legacy boot mode" { with(BOOT_MODE, "Bios") => false }
        );
    }

    #[test]
    fn serial_redirection_follows_the_port_address_format() {
        value_scenarios!(run = |address: &str| serial_redirection(
            &serde_json::from_value(json!({"attributes": {"SerialPortAddress": address}}))
                .expect("settings")
        )
        .map(|attribute| attribute.value);
            "newer BIOS" {
                "Serial1Com1Serial2Com2" => [
                    AttributeValue::String("OnConRedirAuto"),
                    AttributeValue::String("Serial1Com2Serial2Com1"),
                ],
            }
            "older BIOS" {
                "Com2" => [AttributeValue::String("OnConRedir"), AttributeValue::String("Com1")],
            }
        );
    }
}
