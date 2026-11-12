/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Declarative BIOS attribute expectations and their resolution against the
//! attributes a BIOS reports.

use bmc_platform::BiosSettings;
use serde_json::Value;

/// The value expected for a BIOS attribute.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttributeValue {
    String(&'static str),
    Bool(bool),
    /// An integer, which some BIOSes report as a numeric string.
    Integer(i64),
}

impl AttributeValue {
    /// Reports whether a reported value satisfies this expectation.
    pub(super) fn matches(self, actual: &Value) -> bool {
        match self {
            Self::String(expected) => actual.as_str() == Some(expected),
            Self::Bool(expected) => actual.as_bool() == Some(expected),
            Self::Integer(expected) => {
                actual.as_i64().or_else(|| actual.as_str()?.parse().ok()) == Some(expected)
            }
        }
    }
}

impl From<AttributeValue> for Value {
    fn from(value: AttributeValue) -> Self {
        match value {
            AttributeValue::String(value) => Value::from(value),
            AttributeValue::Bool(value) => Value::from(value),
            AttributeValue::Integer(value) => Value::from(value),
        }
    }
}

/// One BIOS attribute NICo expects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BiosAttribute {
    pub(crate) name: &'static str,
    pub(super) value: AttributeValue,
    /// Whether a BIOS that does not report the attribute is missing a setting,
    /// rather than lacking it by design.
    required: bool,
}

impl BiosAttribute {
    pub(super) const fn string(name: &'static str, value: &'static str) -> Self {
        Self::new(name, AttributeValue::String(value))
    }

    pub(super) const fn bool(name: &'static str, value: bool) -> Self {
        Self::new(name, AttributeValue::Bool(value))
    }

    pub(super) const fn integer(name: &'static str, value: i64) -> Self {
        Self::new(name, AttributeValue::Integer(value))
    }

    /// This attribute is expected even when the BIOS does not report it, so
    /// its absence shows as a difference instead of being skipped.
    pub(super) const fn required(self) -> Self {
        Self {
            required: true,
            ..self
        }
    }

    /// The expected value of an attribute declared with [`Self::string`].
    pub(crate) const fn text(self) -> &'static str {
        match self.value {
            AttributeValue::String(value) => value,
            AttributeValue::Bool(_) | AttributeValue::Integer(_) => {
                panic!("BIOS attribute is not a string")
            }
        }
    }
    const fn new(name: &'static str, value: AttributeValue) -> Self {
        Self {
            name,
            value,
            required: false,
        }
    }
}

/// The settings `expected` calls for on a BIOS reporting `current`.
///
/// Unreported attributes are skipped unless [`BiosAttribute::required`], so
/// tables may list alternative names (HPE's per-CPU-vendor virtualization
/// keys, BlueField's renamed attributes) without writing keys a given firmware
/// lacks. A reported value that already satisfies its expectation is kept in
/// the encoding the BIOS uses.
pub(super) fn desired_settings(expected: &[BiosAttribute], current: &BiosSettings) -> BiosSettings {
    let mut desired = BiosSettings::default();
    for attribute in expected {
        let value = match current.attributes.get(attribute.name) {
            Some(reported) if attribute.value.matches(reported) => reported.clone(),
            Some(_) => attribute.value.into(),
            None if attribute.required => attribute.value.into(),
            None => continue,
        };
        desired.attributes.insert(attribute.name.to_string(), value);
    }
    desired
}

#[cfg(test)]
mod tests;
