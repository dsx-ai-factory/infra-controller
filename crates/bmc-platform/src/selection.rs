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

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A BMC operation capability with independently selectable behavior.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Power,
    BmcControl,
    Bios,
    BootOrder,
    SecureBoot,
    Lockdown,
    Accounts,
    Firmware,
    Storage,
    Dpu,
    Attestation,
    Console,
}

impl Capability {
    /// Every capability in stable driver-map order.
    pub const ALL: [Self; 12] = [
        Self::Power,
        Self::BmcControl,
        Self::Bios,
        Self::BootOrder,
        Self::SecureBoot,
        Self::Lockdown,
        Self::Accounts,
        Self::Firmware,
        Self::Storage,
        Self::Dpu,
        Self::Attestation,
        Self::Console,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Power => "power",
            Self::BmcControl => "bmc_control",
            Self::Bios => "bios",
            Self::BootOrder => "boot_order",
            Self::SecureBoot => "secure_boot",
            Self::Lockdown => "lockdown",
            Self::Accounts => "accounts",
            Self::Firmware => "firmware",
            Self::Storage => "storage",
            Self::Dpu => "dpu",
            Self::Attestation => "attestation",
            Self::Console => "console",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A validated, stable identifier for a compiled-in driver.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DriverId(String);

impl DriverId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DriverId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for DriverId {
    type Err = DriverIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(DriverIdError::Empty);
        }
        if matches!(value, "standard" | "unsupported") {
            return Err(DriverIdError::Reserved);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(DriverIdError::InvalidCharacter);
        }
        if value.starts_with('-') || value.ends_with('-') || value.contains("--") {
            return Err(DriverIdError::InvalidSeparator);
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for DriverId {
    type Error = DriverIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl Serialize for DriverId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DriverId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DriverIdError {
    #[error("driver id is empty")]
    Empty,
    #[error("driver id is reserved for a capability-selection sentinel")]
    Reserved,
    #[error("driver id must contain only lowercase ASCII letters, digits, and hyphens")]
    InvalidCharacter,
    #[error("driver id must use single hyphens between nonempty segments")]
    InvalidSeparator,
}

/// Selection for one capability in a complete driver map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilitySelection {
    Standard,
    Unsupported,
    Driver(DriverId),
}

impl Serialize for CapabilitySelection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Standard => serializer.serialize_str("standard"),
            Self::Unsupported => serializer.serialize_str("unsupported"),
            Self::Driver(driver) => driver.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for CapabilitySelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "standard" => Ok(Self::Standard),
            "unsupported" => Ok(Self::Unsupported),
            _ => value
                .parse()
                .map(Self::Driver)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// Persisted selection for every BMC operation capability.
///
/// Deserialization requires all fields so a newly introduced capability cannot
/// silently acquire a fallback driver.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DriverMap {
    pub power: CapabilitySelection,
    pub bmc_control: CapabilitySelection,
    pub bios: CapabilitySelection,
    pub boot_order: CapabilitySelection,
    pub secure_boot: CapabilitySelection,
    pub lockdown: CapabilitySelection,
    pub accounts: CapabilitySelection,
    pub firmware: CapabilitySelection,
    pub storage: CapabilitySelection,
    pub dpu: CapabilitySelection,
    pub attestation: CapabilitySelection,
    pub console: CapabilitySelection,
}

impl DriverMap {
    pub fn get(&self, capability: Capability) -> &CapabilitySelection {
        match capability {
            Capability::Power => &self.power,
            Capability::BmcControl => &self.bmc_control,
            Capability::Bios => &self.bios,
            Capability::BootOrder => &self.boot_order,
            Capability::SecureBoot => &self.secure_boot,
            Capability::Lockdown => &self.lockdown,
            Capability::Accounts => &self.accounts,
            Capability::Firmware => &self.firmware,
            Capability::Storage => &self.storage,
            Capability::Dpu => &self.dpu,
            Capability::Attestation => &self.attestation,
            Capability::Console => &self.console,
        }
    }

    pub fn set(&mut self, capability: Capability, selection: CapabilitySelection) {
        let target = match capability {
            Capability::Power => &mut self.power,
            Capability::BmcControl => &mut self.bmc_control,
            Capability::Bios => &mut self.bios,
            Capability::BootOrder => &mut self.boot_order,
            Capability::SecureBoot => &mut self.secure_boot,
            Capability::Lockdown => &mut self.lockdown,
            Capability::Accounts => &mut self.accounts,
            Capability::Firmware => &mut self.firmware,
            Capability::Storage => &mut self.storage,
            Capability::Dpu => &mut self.dpu,
            Capability::Attestation => &mut self.attestation,
            Capability::Console => &mut self.console,
        };
        *target = selection;
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Capability, &CapabilitySelection)> {
        Capability::ALL
            .into_iter()
            .map(|capability| (capability, self.get(capability)))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{scenarios, value_scenarios};
    use serde_json::json;

    use super::*;

    fn complete_map() -> DriverMap {
        DriverMap {
            power: CapabilitySelection::Standard,
            bmc_control: CapabilitySelection::Standard,
            bios: CapabilitySelection::Standard,
            boot_order: CapabilitySelection::Standard,
            secure_boot: CapabilitySelection::Standard,
            lockdown: CapabilitySelection::Standard,
            accounts: CapabilitySelection::Standard,
            firmware: CapabilitySelection::Standard,
            storage: CapabilitySelection::Unsupported,
            dpu: CapabilitySelection::Unsupported,
            attestation: CapabilitySelection::Standard,
            console: CapabilitySelection::Driver(
                "xcc-console".parse().expect("fixture driver id is valid"),
            ),
        }
    }

    #[test]
    fn driver_ids_enforce_canonical_form() {
        scenarios!(run = |value: &str| value.parse::<DriverId>().map(|id| id.to_string());
            "valid ids" {
                "redfish-standard" => Yields("redfish-standard".to_string()),
                "sr650v4-power" => Yields("sr650v4-power".to_string()),
                "driver2" => Yields("driver2".to_string()),
            }
            "invalid ids" {
                "" => Fails,
                "standard" => Fails,
                "unsupported" => Fails,
                "XCC" => Fails,
                "xcc_power" => Fails,
                "-xcc" => Fails,
                "xcc-" => Fails,
                "xcc--power" => Fails,
            }
        );
    }

    #[test]
    fn driver_selection_cannot_serialize_as_a_sentinel() {
        fn serialize_driver(value: &str) -> Result<serde_json::Value, DriverIdError> {
            let selection = CapabilitySelection::Driver(value.parse()?);
            Ok(serde_json::to_value(selection).expect("valid driver selection serializes"))
        }

        for sentinel in ["standard", "unsupported"] {
            assert_eq!(serialize_driver(sentinel), Err(DriverIdError::Reserved));
            assert!(!matches!(
                serde_json::from_value::<CapabilitySelection>(json!(sentinel))
                    .expect("sentinel selection deserializes"),
                CapabilitySelection::Driver(_)
            ));
        }
    }

    #[test]
    fn capabilities_have_stable_wire_names_and_order() {
        let names = Capability::ALL.map(|capability| capability.to_string());
        assert_eq!(
            names,
            [
                "power",
                "bmc_control",
                "bios",
                "boot_order",
                "secure_boot",
                "lockdown",
                "accounts",
                "firmware",
                "storage",
                "dpu",
                "attestation",
                "console",
            ]
        );
        for capability in Capability::ALL {
            let encoded = serde_json::to_string(&capability).expect("capability serializes");
            let decoded: Capability =
                serde_json::from_str(&encoded).expect("capability deserializes");
            assert_eq!(decoded, capability);
        }
    }

    #[test]
    fn capability_selections_use_plain_strings() {
        value_scenarios!(run = |selection: CapabilitySelection| serde_json::to_value(selection)
            .expect("selection serializes");
            "stable representation" {
                CapabilitySelection::Standard => json!("standard"),
                CapabilitySelection::Unsupported => json!("unsupported"),
                CapabilitySelection::Driver("xcc-console".parse().expect("valid id")) =>
                    json!("xcc-console"),
            }
        );
    }

    #[test]
    fn driver_map_requires_every_capability_and_round_trips() {
        let map = complete_map();
        let encoded = serde_json::to_value(&map).expect("driver map serializes");
        assert_eq!(
            serde_json::from_value::<DriverMap>(encoded.clone())
                .expect("complete map deserializes"),
            map
        );

        let mut missing = encoded;
        missing
            .as_object_mut()
            .expect("driver map is an object")
            .remove("console");
        assert!(serde_json::from_value::<DriverMap>(missing).is_err());
    }

    #[test]
    fn driver_map_access_is_complete_and_deterministic() {
        let mut map = complete_map();
        map.set(
            Capability::Storage,
            CapabilitySelection::Driver("xcc-storage".parse().expect("valid id")),
        );
        assert_eq!(
            map.get(Capability::Storage),
            &CapabilitySelection::Driver("xcc-storage".parse().expect("valid id"))
        );
        assert_eq!(
            map.iter()
                .map(|(capability, _)| capability)
                .collect::<Vec<_>>(),
            Capability::ALL
        );
    }
}
