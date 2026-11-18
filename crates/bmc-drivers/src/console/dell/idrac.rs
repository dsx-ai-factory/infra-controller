/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;

use async_trait::async_trait;
use bmc_platform::{
    Console, ConsoleSpec, ConsoleState, ConsoleStatus, DriverOutcome, EscapeSeq, OpCx,
    PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::Value;

use crate::bios::dell::idrac::{
    CON_TERM_TYPE, EXT_SERIAL_CONNECTOR, FAIL_SAFE_BAUD, NEWER_SERIAL_COMM,
    NEWER_SERIAL_PORT_ADDRESS, OLDER_SERIAL_COMM, OLDER_SERIAL_PORT_ADDRESS, REDIR_AFTER_BOOT,
};
use crate::console::support::{AttrExpectation, SSH_PORT, attr, attr_status, spec_error};
use crate::dell;
use crate::resources::RedfishResourcesExt as _;

/// Dell iDRAC console driver.
///
/// Serial redirection is split between BIOS attributes and iDRAC manager
/// attributes, and the console is reached through the `racadm` SSH shell.
/// iDRAC holds one pending BIOS job, so BIOS setup stages the serial BIOS
/// attributes with the rest; console setup writes only the iDRAC attributes.
pub(crate) struct IdracConsole;

/// The serial BIOS settings BIOS setup stages, accepting either BIOS
/// generation's values and the COM-specific redirection modes.
const BIOS_ATTRS: &[AttrExpectation] = &[
    attr(
        OLDER_SERIAL_COMM.name,
        &[
            OLDER_SERIAL_COMM.text(),
            NEWER_SERIAL_COMM.text(),
            "OnConRedirCom1",
            "OnConRedirCom2",
        ],
        &["Off"],
    ),
    // Absent on iDRAC 10; BIOS setup writes it only where reported.
    attr(
        REDIR_AFTER_BOOT.name,
        &[REDIR_AFTER_BOOT.text()],
        &["Disabled"],
    )
    .optional(),
    attr(
        OLDER_SERIAL_PORT_ADDRESS.name,
        &[
            OLDER_SERIAL_PORT_ADDRESS.text(),
            NEWER_SERIAL_PORT_ADDRESS.text(),
        ],
        &[],
    ),
    attr(
        EXT_SERIAL_CONNECTOR.name,
        &[EXT_SERIAL_CONNECTOR.text()],
        &[],
    )
    .optional(),
    attr(FAIL_SAFE_BAUD.name, &[FAIL_SAFE_BAUD.text()], &[]),
    attr(CON_TERM_TYPE.name, &[CON_TERM_TYPE.text()], &[]),
];

const MANAGER_ATTRS: &[AttrExpectation] = &[
    attr("SerialRedirection.1.Enable", &["Enabled"], &["Disabled"]),
    attr("IPMISOL.1.BaudRate", &["115200"], &[]),
    attr("IPMISOL.1.Enable", &["Enabled"], &["Disabled"]),
    attr("IPMISOL.1.MinPrivilege", &["Administrator"], &[]),
    attr("SSH.1.Enable", &["Enabled"], &["Disabled"]),
    attr("IPMILan.1.Enable", &["Enabled"], &["Disabled"]),
];

fn dell_spec() -> Result<ConsoleSpec, PlatformError> {
    ConsoleSpec::ssh_shell(
        SSH_PORT,
        b"connect com2".to_vec(),
        None,
        b"\nracadm>>".to_vec(),
        EscapeSeq::Single(0x1c),
    )
    .map_err(spec_error)
}

/// The iDRAC attributes, every one of which iDRAC must report.
async fn manager_attributes<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<BTreeMap<String, Value>, PlatformError> {
    let names: Vec<&str> = MANAGER_ATTRS.iter().map(|attr| attr.key).collect();
    let attributes = dell::manager_attribute_values(cx, &names).await?;
    if let Some(missing) = names.iter().find(|name| !attributes.contains_key(**name)) {
        return Err(PlatformError::InvalidResponse {
            message: format!("iDRAC attributes do not report {missing}"),
        });
    }
    Ok(attributes)
}

/// Enabled when both halves are enabled, disabled when both are disabled.
fn combined(bmc: ConsoleStatus, bios: ConsoleStatus) -> ConsoleStatus {
    let state = match (bmc.state, bios.state) {
        (ConsoleState::Enabled, ConsoleState::Enabled) => ConsoleState::Enabled,
        (ConsoleState::Disabled, ConsoleState::Disabled) => ConsoleState::Disabled,
        _ => ConsoleState::Partial,
    };
    let message = [bmc.message, bios.message]
        .into_iter()
        .filter(|half| !half.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    ConsoleStatus { state, message }
}

#[async_trait]
impl<B: Bmc> Console<B> for IdracConsole
where
    B::Error: ActionError,
{
    async fn setup(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        let attributes = MANAGER_ATTRS
            .iter()
            .map(|attr| (attr.key.to_string(), Value::from(attr.enabled[0])))
            .collect();
        dell::patch_manager_attributes(cx, &attributes, None).await
    }

    async fn status(&self, cx: &OpCx<'_, B>) -> Result<ConsoleStatus, PlatformError> {
        let bmc = attr_status(&manager_attributes(cx).await?, MANAGER_ATTRS);
        let bios = attr_status(&cx.current_bios_settings().await?.attributes, BIOS_ATTRS);
        Ok(combined(bmc, bios))
    }

    async fn spec(&self, _cx: &OpCx<'_, B>) -> Result<ConsoleSpec, PlatformError> {
        dell_spec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_constructs_the_racadm_shell() {
        let spec = dell_spec().expect("Dell spec");
        let shell = spec.as_ssh_shell().expect("SSH shell");
        assert_eq!(shell.activate.as_slice(), b"connect com2");
        assert_eq!(shell.escape_filter, EscapeSeq::Single(0x1c));
    }

    /// A BIOS without the settings BIOS setup writes only where reported, as
    /// iDRAC 10 lacks `RedirAfterBoot`, still reads as enabled.
    #[test]
    fn bios_status_skips_the_settings_some_bioses_lack() {
        let attributes = [
            ("SerialComm", "OnConRedirAuto"),
            ("SerialPortAddress", "Serial1Com2Serial2Com1"),
            ("FailSafeBaud", "115200"),
            ("ConTermType", "Vt100Vt220"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), serde_json::json!(value)))
        .collect();
        assert_eq!(
            attr_status(&attributes, BIOS_ATTRS).state,
            ConsoleState::Enabled
        );
    }

    #[test]
    fn status_is_enabled_or_disabled_only_when_both_halves_agree() {
        let status = |state| ConsoleStatus {
            state,
            message: String::new(),
        };
        let cases = [
            (
                ConsoleState::Enabled,
                ConsoleState::Enabled,
                ConsoleState::Enabled,
            ),
            (
                ConsoleState::Disabled,
                ConsoleState::Disabled,
                ConsoleState::Disabled,
            ),
            (
                ConsoleState::Enabled,
                ConsoleState::Disabled,
                ConsoleState::Partial,
            ),
        ];
        for (bmc, bios, expected) in cases {
            assert_eq!(combined(status(bmc), status(bios)).state, expected);
        }
    }
}
