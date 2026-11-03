/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;
use std::num::NonZeroU16;

use bmc_platform::{
    ConsoleSpec, ConsoleState, ConsoleStatus, DriverOutcome, EscapeSeq, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use serde_json::Value;

use crate::resources::RedfishResourcesExt as _;

pub(super) const SSH_PORT: NonZeroU16 = NonZeroU16::new(22).expect("22 is nonzero");
pub(super) const DPU_SSH_PORT: NonZeroU16 = NonZeroU16::new(2200).expect("2200 is nonzero");
pub(super) const IPMI_PORT: NonZeroU16 = NonZeroU16::new(623).expect("623 is nonzero");

/// One attribute the console depends on.
///
/// `enabled[0]` is also the value `setup` writes, so the table is the single
/// description of the console configuration. Values spelled `true`/`false`
/// are written as booleans, which is how those BIOSes report them.
#[derive(Clone, Copy)]
pub(super) struct AttrExpectation {
    pub(super) key: &'static str,
    pub(super) enabled: &'static [&'static str],
    pub(super) disabled: &'static [&'static str],
    /// Written only when the BIOS reports the attribute.
    optional: bool,
    /// Status reads it; a write-only attribute is set but not reported back
    /// meaningfully.
    checked: bool,
}

impl AttrExpectation {
    /// Written only when the BIOS reports the attribute.
    pub(super) const fn optional(self) -> Self {
        Self {
            optional: true,
            ..self
        }
    }
}

/// An attribute status checks; an empty `disabled` accepts any value as
/// disabled.
pub(super) const fn attr(
    key: &'static str,
    enabled: &'static [&'static str],
    disabled: &'static [&'static str],
) -> AttrExpectation {
    AttrExpectation {
        key,
        enabled,
        disabled,
        optional: false,
        checked: true,
    }
}

/// An attribute `setup` writes as `value` but status does not check.
pub(super) const fn write_only(
    key: &'static str,
    value: &'static [&'static str],
) -> AttrExpectation {
    AttrExpectation {
        key,
        enabled: value,
        disabled: &[],
        optional: false,
        checked: false,
    }
}

fn attr_value(text: &str) -> Value {
    match text {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        other => Value::String(other.to_string()),
    }
}

/// Rates the checked attributes `attrs` reports: enabled when every one
/// holds an enabled value, disabled when every one holds a disabled value,
/// partial otherwise. Unreported attributes are skipped.
pub(super) fn attr_status(
    attrs: &BTreeMap<String, Value>,
    expected: &[AttrExpectation],
) -> ConsoleStatus {
    let mut enabled = true;
    let mut disabled = true;
    let mut message = Vec::new();
    for expectation in expected.iter().filter(|attr| attr.checked) {
        let value = match attrs.get(expectation.key) {
            Some(Value::String(value)) => value.clone(),
            Some(Value::Bool(value)) => value.to_string(),
            _ => continue,
        };
        message.push(format!("{}={value}", expectation.key));
        enabled &= expectation.enabled.contains(&value.as_str());
        disabled &=
            expectation.disabled.is_empty() || expectation.disabled.contains(&value.as_str());
    }
    let state = if enabled {
        ConsoleState::Enabled
    } else if disabled {
        ConsoleState::Disabled
    } else {
        ConsoleState::Partial
    };
    ConsoleStatus {
        state,
        message: message.join(", "),
    }
}

pub(super) fn ipmi_sol_spec() -> Result<ConsoleSpec, PlatformError> {
    Ok(ConsoleSpec::IpmiSol {
        port: IPMI_PORT,
        escape_filter: EscapeSeq::pair(b'~', vec![b'.', b'B', b'?', 0x1a, 0x18])
            .map_err(spec_error)?,
    })
}

pub(super) fn spec_error(error: impl std::fmt::Display) -> PlatformError {
    PlatformError::InvalidResponse {
        message: error.to_string(),
    }
}

/// BIOS console setup shared by console drivers.
pub(super) trait RedfishConsoleExt<B: Bmc> {
    /// Writes each attribute's first enabled value, skipping optional attributes
    /// the BIOS does not report.
    async fn setup_console_bios_attributes(
        &self,
        attrs: &[AttrExpectation],
    ) -> Result<DriverOutcome, PlatformError>;
}

impl<B: Bmc> RedfishConsoleExt<B> for OpCx<'_, B> {
    async fn setup_console_bios_attributes(
        &self,
        attrs: &[AttrExpectation],
    ) -> Result<DriverOutcome, PlatformError> {
        let current = self.current_bios_settings().await?.attributes;
        let attributes: BTreeMap<String, Value> = attrs
            .iter()
            .filter(|attr| !attr.optional || current.contains_key(attr.key))
            .map(|attr| (attr.key.to_string(), attr_value(attr.enabled[0])))
            .collect();
        self.stage_bios_attributes(&attributes).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn attrs(values: &[(&str, &str)]) -> BTreeMap<String, Value> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect()
    }

    #[test]
    fn status_rates_only_reported_checked_attributes() {
        let expected = [
            attr("a", &["on"], &["off"]),
            attr("b", &["on"], &["off"]),
            write_only("c", &["on"]),
        ];
        let cases = [
            (
                "all enabled",
                attrs(&[("a", "on"), ("b", "on")]),
                ConsoleState::Enabled,
            ),
            (
                "all disabled",
                attrs(&[("a", "off"), ("b", "off")]),
                ConsoleState::Disabled,
            ),
            (
                "mixed",
                attrs(&[("a", "on"), ("b", "off")]),
                ConsoleState::Partial,
            ),
            (
                "one unreported",
                attrs(&[("a", "on")]),
                ConsoleState::Enabled,
            ),
            ("nothing reported", attrs(&[]), ConsoleState::Enabled),
            (
                "write-only differs",
                attrs(&[("a", "on"), ("b", "on"), ("c", "off")]),
                ConsoleState::Enabled,
            ),
        ];
        for (name, reported, state) in cases {
            assert_eq!(attr_status(&reported, &expected).state, state, "{name}");
        }
    }

    #[test]
    fn boolean_spellings_are_written_as_booleans() {
        assert_eq!(attr_value("true"), Value::Bool(true));
        assert_eq!(attr_value("COM1"), Value::String("COM1".to_string()));
    }
}
