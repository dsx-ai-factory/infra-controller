/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Declarative identity matching for driver selection rules.

use mac_address::MacAddress;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use version_compare::{Cmp, Version};

use crate::PlatformIdentity;

/// An identity value available to declarative selection rules.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityField {
    /// ServiceRoot `Vendor`.
    ServiceRootVendor,
    /// ServiceRoot `Product`.
    ServiceRootProduct,
    /// Any ServiceRoot OEM key.
    ServiceRootOemKey,
    /// Selected Manager model.
    ManagerModel,
    /// Selected Manager firmware version.
    ManagerFirmware,
    /// Selected Manager identifier.
    ManagerId,
    /// Selected ComputerSystem identifier.
    SystemId,
    /// Selected ComputerSystem manufacturer.
    SystemManufacturer,
    /// Selected ComputerSystem model.
    SystemModel,
    /// Selected ComputerSystem SKU.
    SystemSku,
    /// Selected ComputerSystem part number.
    SystemPartNumber,
    /// Selected ComputerSystem BIOS version.
    SystemBiosVersion,
    /// Any Chassis identifier.
    ChassisId,
    /// Any Chassis manufacturer.
    ChassisManufacturer,
    /// Any Chassis model.
    ChassisModel,
    /// Any Chassis part number.
    ChassisPartNumber,
    /// The version of the firmware inventory entry with this id.
    FirmwareInventory(String),
    /// Any MAC address of the selected Manager's network interfaces. It names
    /// one BMC, so only deployment overrides may match it.
    ManagerMacAddress,
}

impl IdentityField {
    /// Whether this field holds a firmware version, the only kind of value
    /// version patterns compare.
    pub const fn is_version(&self) -> bool {
        matches!(
            self,
            Self::ManagerFirmware | Self::SystemBiosVersion | Self::FirmwareInventory(_)
        )
    }

    /// Whether this field holds MAC addresses, the only kind of value MAC
    /// patterns compare and the only pattern it accepts.
    pub const fn is_mac_address(&self) -> bool {
        matches!(self, Self::ManagerMacAddress)
    }
}

/// Inclusive validated firmware-version bounds.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FirmwareVersionRange {
    minimum: String,
    maximum: String,
}

impl FirmwareVersionRange {
    /// Creates an inclusive range after validating both bounds and their order.
    pub fn new(minimum: String, maximum: String) -> Result<Self, FirmwareVersionRangeError> {
        let minimum_version =
            Version::from(&minimum).ok_or(FirmwareVersionRangeError::InvalidMinimum)?;
        let maximum_version =
            Version::from(&maximum).ok_or(FirmwareVersionRangeError::InvalidMaximum)?;
        if minimum_version.compare(maximum_version) == Cmp::Gt {
            return Err(FirmwareVersionRangeError::Reversed);
        }
        Ok(Self { minimum, maximum })
    }

    fn minimum(&self) -> &str {
        &self.minimum
    }

    /// Whether `candidate` parses and lies within the inclusive bounds.
    pub fn contains(&self, candidate: &str) -> bool {
        let Some(candidate) = Version::from(candidate) else {
            return false;
        };
        let (Some(minimum), Some(maximum)) =
            (Version::from(&self.minimum), Version::from(&self.maximum))
        else {
            return false;
        };
        candidate.compare(minimum) != Cmp::Lt && candidate.compare(maximum) != Cmp::Gt
    }
}

impl<'de> Deserialize<'de> for FirmwareVersionRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            minimum: String,
            maximum: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.minimum, wire.maximum).map_err(serde::de::Error::custom)
    }
}

/// Validation failure for inclusive firmware-version bounds.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FirmwareVersionRangeError {
    /// The minimum cannot be parsed by `version-compare`.
    #[error("minimum firmware version is invalid")]
    InvalidMinimum,
    /// The maximum cannot be parsed by `version-compare`.
    #[error("maximum firmware version is invalid")]
    InvalidMaximum,
    /// The minimum is greater than the maximum.
    #[error("minimum firmware version must not exceed maximum")]
    Reversed,
}

/// Comparison used by an identity matcher.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MatchPattern {
    /// Requires an exact case-sensitive value.
    Exact(String),
    /// Requires an exact ASCII case-insensitive value.
    ExactAsciiCaseInsensitive(String),
    /// Requires a case-sensitive prefix.
    Prefix(String),
    /// Requires a case-sensitive substring.
    Contains(String),
    /// Requires an ASCII case-insensitive substring.
    ContainsAsciiCaseInsensitive(String),
    /// Requires an exact match against one of several canonical values.
    OneOf(Vec<String>),
    /// Requires a parseable version within inclusive validated bounds.
    FirmwareVersionRange(FirmwareVersionRange),
    /// Requires a parseable version equal to this one.
    VersionEqual(String),
    /// Requires a parseable version at or above this one.
    VersionAtLeast(String),
    /// Requires a parseable version below this one.
    VersionBelow(String),
    /// Requires this MAC address, however the value is written.
    #[serde(deserialize_with = "deserialize_mac_address")]
    MacAddress(MacAddress),
}

fn deserialize_mac_address<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<MacAddress, D::Error> {
    let value = String::deserialize(deserializer)?;
    value
        .parse()
        .map_err(|error| serde::de::Error::custom(format!("invalid MAC address {value}: {error}")))
}

impl MatchPattern {
    /// The first comparison value, used to reject empty patterns.
    pub fn first_value(&self) -> Option<&str> {
        match self {
            Self::Exact(value)
            | Self::ExactAsciiCaseInsensitive(value)
            | Self::Prefix(value)
            | Self::Contains(value)
            | Self::ContainsAsciiCaseInsensitive(value)
            | Self::VersionEqual(value)
            | Self::VersionAtLeast(value)
            | Self::VersionBelow(value) => Some(value),
            Self::OneOf(values) => values.first().map(String::as_str),
            Self::FirmwareVersionRange(range) => Some(range.minimum()),
            Self::MacAddress(_) => None,
        }
    }

    /// Whether this pattern compares MAC addresses, which only
    /// [`IdentityField::is_mac_address`] fields carry.
    pub const fn is_mac_address_pattern(&self) -> bool {
        matches!(self, Self::MacAddress(_))
    }

    /// Whether this pattern compares versions, which only
    /// [`IdentityField::is_version`] fields carry.
    pub const fn is_version_pattern(&self) -> bool {
        matches!(
            self,
            Self::FirmwareVersionRange(_)
                | Self::VersionEqual(_)
                | Self::VersionAtLeast(_)
                | Self::VersionBelow(_)
        )
    }

    /// Whether a single version bound parses; other patterns have no bound to check.
    pub fn has_parseable_bound(&self) -> bool {
        match self {
            Self::VersionEqual(version)
            | Self::VersionAtLeast(version)
            | Self::VersionBelow(version) => Version::from(version).is_some(),
            _ => true,
        }
    }

    /// Reports whether `candidate` satisfies this pattern.
    pub fn matches(&self, candidate: &str) -> bool {
        match self {
            Self::Exact(value) => candidate == value,
            Self::ExactAsciiCaseInsensitive(value) => candidate.eq_ignore_ascii_case(value),
            Self::Prefix(value) => candidate.starts_with(value),
            Self::Contains(value) => candidate.contains(value),
            Self::ContainsAsciiCaseInsensitive(value) => candidate
                .to_ascii_lowercase()
                .contains(&value.to_ascii_lowercase()),
            Self::OneOf(values) => values.iter().any(|value| candidate == value),
            Self::FirmwareVersionRange(range) => range.contains(candidate),
            Self::VersionEqual(version) => version_order(candidate, version) == Some(Cmp::Eq),
            Self::VersionAtLeast(minimum) => {
                version_order(candidate, minimum).is_some_and(|order| order != Cmp::Lt)
            }
            Self::VersionBelow(bound) => version_order(candidate, bound) == Some(Cmp::Lt),
            Self::MacAddress(_) => false,
        }
    }
}

/// How `candidate` orders against `reference`; `None` when either does not parse.
fn version_order(candidate: &str, reference: &str) -> Option<Cmp> {
    Some(Version::from(candidate)?.compare(Version::from(reference)?))
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityMatcher {
    /// Identity field examined by this predicate.
    pub field: IdentityField,
    /// Comparison applied to every value for `field`.
    pub pattern: MatchPattern,
}

impl IdentityMatcher {
    /// Creates a field matcher.
    pub const fn new(field: IdentityField, pattern: MatchPattern) -> Self {
        Self { field, pattern }
    }

    /// The ServiceRoot vendor, ignoring ASCII case.
    pub fn vendor(value: &str) -> Self {
        Self::new(
            IdentityField::ServiceRootVendor,
            MatchPattern::ExactAsciiCaseInsensitive(value.to_string()),
        )
    }

    /// Any ServiceRoot OEM key, ignoring ASCII case.
    pub fn oem_key(value: &str) -> Self {
        Self::new(
            IdentityField::ServiceRootOemKey,
            MatchPattern::ExactAsciiCaseInsensitive(value.to_string()),
        )
    }

    /// The ServiceRoot product is exactly one of `values`.
    pub fn product(values: &[&str]) -> Self {
        Self::new(
            IdentityField::ServiceRootProduct,
            MatchPattern::OneOf(values.iter().map(|value| (*value).to_string()).collect()),
        )
    }

    /// Any chassis manufacturer contains `value`, ignoring ASCII case.
    pub fn chassis_manufacturer(value: &str) -> Self {
        Self::contains_any_case(IdentityField::ChassisManufacturer, value)
    }

    /// `field` is exactly `value`.
    pub fn exact(field: IdentityField, value: &str) -> Self {
        Self::new(field, MatchPattern::Exact(value.to_string()))
    }

    /// `field` contains `value`.
    pub fn contains(field: IdentityField, value: &str) -> Self {
        Self::new(field, MatchPattern::Contains(value.to_string()))
    }

    /// The version in `field` equals `version`.
    pub fn version_equal(field: IdentityField, version: &str) -> Self {
        Self::new(field, MatchPattern::VersionEqual(version.to_string()))
    }

    /// The version in `field` is at least `version`.
    pub fn version_at_least(field: IdentityField, version: &str) -> Self {
        Self::new(field, MatchPattern::VersionAtLeast(version.to_string()))
    }

    /// The version in `field` is below `version`.
    pub fn version_below(field: IdentityField, version: &str) -> Self {
        Self::new(field, MatchPattern::VersionBelow(version.to_string()))
    }

    /// `field` contains `value`, ignoring ASCII case.
    pub fn contains_any_case(field: IdentityField, value: &str) -> Self {
        Self::new(
            field,
            MatchPattern::ContainsAsciiCaseInsensitive(value.to_string()),
        )
    }

    /// Reports whether any value of this field satisfies the matcher.
    pub fn matches(&self, identity: &PlatformIdentity) -> bool {
        match (&self.field, &self.pattern) {
            (IdentityField::ManagerMacAddress, MatchPattern::MacAddress(mac_address)) => identity
                .manager
                .as_ref()
                .is_some_and(|manager| manager.mac_addresses.contains(mac_address)),
            (IdentityField::ManagerMacAddress, _) | (_, MatchPattern::MacAddress(_)) => false,
            (field, pattern) => field_values(identity, field)
                .into_iter()
                .any(|value| pattern.matches(value)),
        }
    }
}

/// Explicit selection-rule precedence from broad defaults to local overrides.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "snake_case")]
pub enum Precedence {
    /// Built-in standard/default behavior.
    StandardDefault,
    /// Vendor, manufacturer, or ServiceRoot OEM evidence.
    VendorManufacturer,
    /// BMC product or Manager identity evidence.
    BmcProductManager,
    /// Exact ComputerSystem model, SKU, or part-number evidence.
    ExactSystemIdentity,
    /// Deliberate deployment-local override.
    DeploymentOverride,
    /// Deployment-local override for specific BMCs, named by MAC address.
    DeploymentBmcOverride,
}

fn field_values<'a>(identity: &'a PlatformIdentity, field: &IdentityField) -> Vec<&'a str> {
    match field {
        IdentityField::ServiceRootVendor => optional(identity.service_root.vendor.as_deref()),
        IdentityField::ServiceRootProduct => optional(identity.service_root.product.as_deref()),
        IdentityField::ServiceRootOemKey => identity
            .service_root
            .oem_keys
            .iter()
            .map(String::as_str)
            .collect(),
        IdentityField::ManagerModel => optional(
            identity
                .manager
                .as_ref()
                .and_then(|value| value.model.as_deref()),
        ),
        IdentityField::ManagerFirmware => optional(
            identity
                .manager
                .as_ref()
                .and_then(|value| value.firmware.as_deref()),
        ),
        IdentityField::ManagerId => identity
            .manager
            .as_ref()
            .map(|value| vec![value.id.as_str()])
            .unwrap_or_default(),
        IdentityField::SystemId => identity
            .system
            .as_ref()
            .map(|value| vec![value.id.as_str()])
            .unwrap_or_default(),
        IdentityField::SystemManufacturer => optional(
            identity
                .system
                .as_ref()
                .and_then(|value| value.manufacturer.as_deref()),
        ),
        IdentityField::SystemModel => optional(
            identity
                .system
                .as_ref()
                .and_then(|value| value.model.as_deref()),
        ),
        IdentityField::SystemSku => optional(
            identity
                .system
                .as_ref()
                .and_then(|value| value.sku.as_deref()),
        ),
        IdentityField::SystemPartNumber => optional(
            identity
                .system
                .as_ref()
                .and_then(|value| value.part_number.as_deref()),
        ),
        IdentityField::SystemBiosVersion => optional(
            identity
                .system
                .as_ref()
                .and_then(|value| value.bios_version.as_deref()),
        ),
        IdentityField::ChassisId => identity
            .chassis
            .iter()
            .map(|value| value.id.as_str())
            .collect(),
        IdentityField::ChassisManufacturer => identity
            .chassis
            .iter()
            .filter_map(|value| value.manufacturer.as_deref())
            .collect(),
        IdentityField::ChassisModel => identity
            .chassis
            .iter()
            .filter_map(|value| value.model.as_deref())
            .collect(),
        IdentityField::ChassisPartNumber => identity
            .chassis
            .iter()
            .filter_map(|value| value.part_number.as_deref())
            .collect(),
        IdentityField::FirmwareInventory(id) => identity
            .firmware_inventory
            .iter()
            .filter(|entry| entry.id == *id)
            .filter_map(|entry| entry.version.as_deref())
            .collect(),
        IdentityField::ManagerMacAddress => Vec::new(),
    }
}

fn optional(value: Option<&str>) -> Vec<&str> {
    value.into_iter().collect()
}

/// The rank a rule competes at, derived from the most specific identity
/// field its matchers read; no matchers means the standard default.
pub fn derived_precedence(matchers: &[IdentityMatcher]) -> Precedence {
    matchers
        .iter()
        .filter_map(|matcher| field_precedence(&matcher.field))
        .max()
        .unwrap_or(Precedence::StandardDefault)
}

const fn field_precedence(field: &IdentityField) -> Option<Precedence> {
    match field {
        IdentityField::ServiceRootVendor
        | IdentityField::ServiceRootOemKey
        | IdentityField::SystemManufacturer
        | IdentityField::ChassisManufacturer => Some(Precedence::VendorManufacturer),
        IdentityField::ServiceRootProduct | IdentityField::ManagerModel => {
            Some(Precedence::BmcProductManager)
        }
        IdentityField::SystemId
        | IdentityField::ManagerId
        | IdentityField::SystemModel
        | IdentityField::SystemSku
        | IdentityField::SystemPartNumber
        | IdentityField::ChassisId
        | IdentityField::ChassisModel
        | IdentityField::ChassisPartNumber => Some(Precedence::ExactSystemIdentity),
        IdentityField::ManagerFirmware
        | IdentityField::SystemBiosVersion
        | IdentityField::FirmwareInventory(_)
        | IdentityField::ManagerMacAddress => None,
    }
}

#[cfg(test)]
mod tests;
