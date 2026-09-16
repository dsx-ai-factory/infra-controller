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

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use carbide_libmlx_model::nvconfig::DpuNvConfigProfile;
use carbide_utils::config::as_std_duration;
use duration_str::deserialize_duration;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::hardware_info::HardwareInfo;

/// RackHardwareType identifies the hardware type of a rack.
/// This is a flexible string-based type to allow new hardware types
/// without code changes. The special value "any" indicates firmware
/// that is compatible with any rack hardware type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(transparent)]
#[serde(transparent)]
pub struct RackHardwareType(pub String);

impl RackHardwareType {
    /// Returns a RackHardwareType that matches any rack hardware.
    pub fn any() -> Self {
        Self("any".to_string())
    }

    /// Returns true if this is the wildcard "any" type.
    pub fn is_any(&self) -> bool {
        self.0 == "any"
    }
}

impl Default for RackHardwareType {
    fn default() -> Self {
        Self::any()
    }
}

impl fmt::Display for RackHardwareType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RackHardwareType {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RackHardwareType {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Identifies the product family shared by rack components.
///
/// String parsing trims outer whitespace, recognizes the lowercase
/// `gb200` and `gb300` values, and preserves other non-empty identifiers in
/// [`RackProductFamily::Other`]. Named variants retain existing API behavior;
/// the open-ended variant lets descriptor-based backends accept new product
/// families without a NICo release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RackProductFamily {
    /// GB200 rack hardware.
    Gb200,

    /// GB300 rack hardware.
    Gb300,

    /// A non-empty product-family identifier not represented by a named variant.
    ///
    /// The original spelling is preserved after outer whitespace is removed.
    Other(String),
}

/// Selects the fixed DPU NVConfig profile supported by a product family and DPU
/// identity.
///
/// A profile is selected only for the GB200 product family and an exact
/// supported DPU part number. Missing identity does not select a profile.
pub fn select_dpu_nvconfig_profile(
    product_family: Option<&RackProductFamily>,
    hardware_info: Option<&HardwareInfo>,
) -> Option<DpuNvConfigProfile> {
    if product_family != Some(&RackProductFamily::Gb200) {
        return None;
    }

    let part_number = &hardware_info?.dpu_info.as_ref()?.part_number;
    DpuNvConfigProfile::for_gb200_b3240_part_number(part_number)
}

impl RackProductFamily {
    /// Returns `GB200` or `GB300` when a hardware model reported by Redfish
    /// contains exactly one of those product family tokens.
    ///
    /// Matching ignores ASCII case and requires whole tokens separated by ASCII
    /// whitespace. Unknown models, concatenated names, and models naming both
    /// families return `None`.
    pub fn from_hardware_model(model: &str) -> Option<Self> {
        let mut has_gb200 = false;
        let mut has_gb300 = false;

        for token in model.split_ascii_whitespace() {
            has_gb200 |= token.eq_ignore_ascii_case("gb200");
            has_gb300 |= token.eq_ignore_ascii_case("gb300");
        }

        match (has_gb200, has_gb300) {
            (true, false) => Some(Self::Gb200),
            (false, true) => Some(Self::Gb300),
            (false, false) | (true, true) => None,
        }
    }

    /// Returns the product-family identifier sent to descriptor-based backends.
    ///
    /// Named variants use their canonical lowercase value. Values stored in
    /// [`RackProductFamily::Other`] retain their original spelling with outer
    /// whitespace removed.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Gb200 => "gb200",
            Self::Gb300 => "gb300",
            Self::Other(value) => value.trim(),
        }
    }
}

impl fmt::Display for RackProductFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned for an empty rack product-family identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("rack product family must not be empty")]
pub struct RackProductFamilyParseError;

impl FromStr for RackProductFamily {
    type Err = RackProductFamilyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();

        if value.is_empty() {
            return Err(RackProductFamilyParseError);
        }

        Ok(match value {
            "gb200" => Self::Gb200,
            "gb300" => Self::Gb300,
            value => Self::Other(value.to_string()),
        })
    }
}

impl Serialize for RackProductFamily {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RackProductFamily {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

impl sqlx::Type<sqlx::Postgres> for RackProductFamily {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for RackProductFamily {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<sqlx::Postgres>>::encode(self.as_str(), buf)
    }
}

impl sqlx::Decode<'_, sqlx::Postgres> for RackProductFamily {
    fn decode(value: sqlx::postgres::PgValueRef<'_>) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        value.parse().map_err(Into::into)
    }
}

/// RackHardwareTopology describes the hardware topology of a rack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // Topology suffix is part of the canonical config names
pub enum RackHardwareTopology {
    Gb200Nvl36r1C2g4Topology,
    Gb300Nvl36r1C2g4Topology,
    Gb200Nvl72r1C2g4Topology,
    Gb300Nvl72r1C2g4Topology,
    VrNvl8r1C2g4RtfTopology,
    VrNvl72r1C2g4Topology,
}

impl fmt::Display for RackHardwareTopology {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RackHardwareTopology::Gb200Nvl36r1C2g4Topology => {
                write!(f, "gb200_nvl36r1_c2g4_topology")
            }
            RackHardwareTopology::Gb300Nvl36r1C2g4Topology => {
                write!(f, "gb300_nvl36r1_c2g4_topology")
            }
            RackHardwareTopology::Gb200Nvl72r1C2g4Topology => {
                write!(f, "gb200_nvl72r1_c2g4_topology")
            }
            RackHardwareTopology::Gb300Nvl72r1C2g4Topology => {
                write!(f, "gb300_nvl72r1_c2g4_topology")
            }
            RackHardwareTopology::VrNvl8r1C2g4RtfTopology => {
                write!(f, "vr_nvl8r1_c2g4_rtf_topology")
            }
            RackHardwareTopology::VrNvl72r1C2g4Topology => {
                write!(f, "vr_nvl72r1_c2g4_topology")
            }
        }
    }
}

/// RackHardwareClass indicates whether a rack is a dev or production rack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum RackHardwareClass {
    Dev,
    Prod,
}

impl fmt::Display for RackHardwareClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RackHardwareClass::Dev => write!(f, "dev"),
            RackHardwareClass::Prod => write!(f, "prod"),
        }
    }
}

/* ********************************** */
/*        RackCapabilityType          */
/* ********************************** */

/// RackCapabilityType represents a category of rack component capability.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum RackCapabilityType {
    Compute,
    Switch,
    PowerShelf,
}

impl fmt::Display for RackCapabilityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RackCapabilityType::Compute => write!(f, "Compute"),
            RackCapabilityType::Switch => write!(f, "Switch"),
            RackCapabilityType::PowerShelf => write!(f, "PowerShelf"),
        }
    }
}

/* ********************************** */
/*       RackCapabilityCompute        */
/* ********************************** */

/// RackCapabilityCompute describes the expected compute tray capability
/// for a rack type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackCapabilityCompute {
    /// Model name of the compute tray (e.g. "GB200").
    #[serde(default)]
    pub name: Option<String>,

    /// Number of compute trays expected in the rack.
    pub count: u32,

    /// Vendor name (e.g. "NVIDIA").
    #[serde(default)]
    pub vendor: Option<String>,

    /// Slot IDs that compute trays are expected to occupy.
    #[serde(default)]
    pub slot_ids: Option<Vec<u32>>,

    /// Optional custom attributes for compute trays.
    ///
    /// Missing maps default to empty. Keys and values are preserved exactly,
    /// with case-sensitive key matching. These attributes override rack-level
    /// attributes with identical keys when a consumer combines both maps.
    #[serde(default)]
    pub attributes: HashMap<String, String>,
}

/* ********************************** */
/*        RackCapabilitySwitch        */
/* ********************************** */

/// RackCapabilitySwitch describes the expected switch capability
/// for a rack type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackCapabilitySwitch {
    /// Model name of the switch.
    #[serde(default)]
    pub name: Option<String>,

    /// Number of switches expected in the rack.
    pub count: u32,

    /// Vendor name.
    #[serde(default)]
    pub vendor: Option<String>,

    /// Slot IDs that switches are expected to occupy.
    #[serde(default)]
    pub slot_ids: Option<Vec<u32>>,

    /// Optional custom attributes for switches.
    ///
    /// Missing maps default to empty. Keys and values are preserved exactly,
    /// with case-sensitive key matching. These attributes override rack-level
    /// attributes with identical keys when a consumer combines both maps.
    #[serde(default)]
    pub attributes: HashMap<String, String>,
}

/* ********************************** */
/*      RackCapabilityPowerShelf      */
/* ********************************** */

/// RackCapabilityPowerShelf describes the expected power shelf capability
/// for a rack type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackCapabilityPowerShelf {
    /// Model name of the power shelf.
    #[serde(default)]
    pub name: Option<String>,

    /// Number of power shelves expected in the rack.
    pub count: u32,

    /// Vendor name.
    #[serde(default)]
    pub vendor: Option<String>,

    /// Slot IDs that power shelves are expected to occupy.
    #[serde(default)]
    pub slot_ids: Option<Vec<u32>>,

    /// Optional custom attributes for power shelves.
    ///
    /// Missing maps default to empty. Keys and values are preserved exactly,
    /// with case-sensitive key matching. These attributes override rack-level
    /// attributes with identical keys when a consumer combines both maps.
    #[serde(default)]
    pub attributes: HashMap<String, String>,
}

/* ********************************** */
/*       RackCapabilitiesSet          */
/* ********************************** */

/// RackCapabilitiesSet is the combined set of all expected rack component
/// capabilities. It describes what a rack should contain in terms of
/// compute trays, switches, and power shelves.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackCapabilitiesSet {
    pub compute: RackCapabilityCompute,
    pub switch: RackCapabilitySwitch,
    pub power_shelf: RackCapabilityPowerShelf,
}

/* ********************************** */
/*           RackProfile              */
/* ********************************** */

/// Optional source for a rack-wide SOT firmware-object document.
///
/// When present on a [`RackProfile`], rack maintenance fetches this complete
/// document separately for the rack firmware and switch NVOS image phases.
/// RMS selects the matching artifacts from the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackFirmwareObjectConfig {
    /// URL from which automatic rack maintenance fetches the SOT JSON document.
    pub url: url::Url,

    /// Maximum duration for the complete HTTP request.
    ///
    /// Configuration that omits this field uses 30 seconds.
    #[serde(
        default = "RackFirmwareObjectConfig::default_fetch_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub fetch_timeout: std::time::Duration,
}

impl RackFirmwareObjectConfig {
    const fn default_fetch_timeout() -> std::time::Duration {
        std::time::Duration::from_secs(30)
    }
}

/// RackProfile describes the hardware identity and expected device
/// capabilities for a class of rack. The profile is referenced by name
/// (the map key in the config file) from expected racks and rack configs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackProfile {
    /// Product family used for product-level component behavior.
    #[serde(default)]
    pub product_family: Option<RackProductFamily>,

    /// Default firmware-object source for automatic rack maintenance.
    ///
    /// When absent, automatic firmware and NVOS updates are skipped unless an
    /// explicit maintenance request supplies a firmware object. If no firmware
    /// object is available while a switch in the maintenance scope is already
    /// waiting for an NVOS update, maintenance enters `Error` instead of
    /// skipping the NVOS phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_object: Option<RackFirmwareObjectConfig>,

    #[serde(default)]
    pub rack_hardware_type: Option<RackHardwareType>,

    #[serde(default)]
    pub rack_hardware_topology: Option<RackHardwareTopology>,

    #[serde(default)]
    pub rack_hardware_class: Option<RackHardwareClass>,

    /// Optional custom attributes inherited by each component role.
    ///
    /// Missing maps default to empty. Keys and values are preserved exactly,
    /// with case-sensitive key matching. Role-level attributes override
    /// rack-level attributes with identical keys.
    #[serde(default)]
    pub attributes: HashMap<String, String>,

    pub rack_capabilities: RackCapabilitiesSet,
}

/* ********************************** */
/*        RackProfileConfig           */
/* ********************************** */

/// RackProfileConfig contains all known rack profiles, keyed by profile id.
/// Loaded from the Carbide configuration file and used to validate that
/// the correct number of expected devices have been registered for a rack.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RackProfileConfig {
    /// Map of rack profile id to its profile.
    #[serde(default, flatten)]
    pub rack_profiles: HashMap<String, RackProfile>,
}

impl RackProfileConfig {
    /// get looks up a rack profile by the profile ID.
    pub fn get(&self, name: &str) -> Option<&RackProfile> {
        self.rack_profiles.get(name)
    }

    /// keys returns all known rack profile IDs.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.rack_profiles.keys()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};

    use super::*;
    use crate::hardware_info::DpuData;

    fn dpu_hardware_info(part_number: &str) -> HardwareInfo {
        HardwareInfo {
            dpu_info: Some(DpuData {
                part_number: part_number.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn hardware_model_requires_one_known_product_family_token() {
        check_values(
            [
                Check {
                    scenario: "DGX GB200 compute tray",
                    input: "DGX GB200 Compute Tray",
                    expect: Some(RackProductFamily::Gb200),
                },
                Check {
                    scenario: "GB200 board",
                    input: "GB200 1CPU:2GPU Board PC",
                    expect: Some(RackProductFamily::Gb200),
                },
                Check {
                    scenario: "GB200 NVL",
                    input: "GB200 NVL",
                    expect: Some(RackProductFamily::Gb200),
                },
                Check {
                    scenario: "lowercase GB200 token",
                    input: "dgx gb200 compute tray",
                    expect: Some(RackProductFamily::Gb200),
                },
                Check {
                    scenario: "GB200 token separated by ASCII whitespace",
                    input: "DGX\tGB200\nCompute Tray",
                    expect: Some(RackProductFamily::Gb200),
                },
                Check {
                    scenario: "GB300 compute tray",
                    input: "DGX GB300 Compute Tray",
                    expect: Some(RackProductFamily::Gb300),
                },
                Check {
                    scenario: "concatenated GB200 name",
                    input: "GB200Nvl Compute Tray",
                    expect: None,
                },
                Check {
                    scenario: "model names multiple product families",
                    input: "GB200 GB300 Compute Tray",
                    expect: None,
                },
                Check {
                    scenario: "unknown model",
                    input: "PowerEdge R750",
                    expect: None,
                },
            ],
            RackProductFamily::from_hardware_model,
        );
    }

    #[test]
    fn dpu_nvconfig_profile_requires_matching_product_family_and_dpu_identity() {
        check_values(
            [
                Check {
                    scenario: "GB200 family with supported B3240",
                    input: (
                        Some(RackProductFamily::Gb200),
                        Some(dpu_hardware_info("900-9D3B6-00CN-PA0")),
                    ),
                    expect: Some(DpuNvConfigProfile::Gb200B3240V1),
                },
                Check {
                    scenario: "other product family with supported B3240",
                    input: (
                        Some(RackProductFamily::Gb300),
                        Some(dpu_hardware_info("900-9D3B6-00CN-PA0")),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "GB200 family with another BlueField 3 product",
                    input: (
                        Some(RackProductFamily::Gb200),
                        Some(dpu_hardware_info("900-9D3B6-00CV-AA0")),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "GB200 family without DPU hardware information",
                    input: (Some(RackProductFamily::Gb200), None),
                    expect: None,
                },
                Check {
                    scenario: "GB200 family without DPU identity",
                    input: (
                        Some(RackProductFamily::Gb200),
                        Some(HardwareInfo::default()),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "missing product family with supported B3240",
                    input: (None, Some(dpu_hardware_info("900-9D3B6-00CN-PA0"))),
                    expect: None,
                },
            ],
            |(product_family, hardware_info)| {
                select_dpu_nvconfig_profile(product_family.as_ref(), hardware_info.as_ref())
            },
        );
    }

    #[test]
    fn test_rack_profile_config_lookup() {
        let mut config = RackProfileConfig::default();
        config.rack_profiles.insert(
            "NVL72".to_string(),
            RackProfile {
                product_family: Some(RackProductFamily::Gb200),
                rack_capabilities: RackCapabilitiesSet {
                    compute: RackCapabilityCompute {
                        name: Some("GB200".to_string()),
                        count: 18,
                        vendor: Some("NVIDIA".to_string()),
                        slot_ids: None,
                        attributes: HashMap::new(),
                    },
                    switch: RackCapabilitySwitch {
                        name: None,
                        count: 9,
                        vendor: None,
                        slot_ids: None,
                        attributes: HashMap::new(),
                    },
                    power_shelf: RackCapabilityPowerShelf {
                        name: None,
                        count: 8,
                        vendor: None,
                        slot_ids: None,
                        attributes: HashMap::new(),
                    },
                },
                ..Default::default()
            },
        );

        let profile = config.get("NVL72").unwrap();
        assert_eq!(profile.rack_capabilities.compute.count, 18);
        assert_eq!(profile.rack_capabilities.switch.count, 9);
        assert_eq!(profile.rack_capabilities.power_shelf.count, 8);

        assert!(config.get("nonexistent").is_none());
    }

    #[test]
    fn test_rack_profile_config_toml_deserialization() {
        // Inline and expanded TOML tables produce the same attribute maps, and
        // deserialization does not normalize custom keys or values.
        let toml_str = r#"
[NVL72]
product_family = "gb200"
attributes = { attribute1 = "value1", additional_attribute2 = "value2" }

[NVL72.rack_capabilities.compute]
name = "GB200"
count = 18
vendor = "NVIDIA"
attributes = { attribute1 = "compute-value", additional_attribute2 = "  MiXeD-Value  " }

[NVL72.rack_capabilities.switch]
count = 9
attributes = { attribute1 = "switch-value" }

[NVL72.rack_capabilities.power_shelf]
count = 8
attributes = { additional_attribute2 = "power-shelf-value" }

[NVL36]
product_family = "test-product-family"

[NVL36.attributes]
attribute1 = "value1"

[NVL36.rack_capabilities.compute]
count = 9

[NVL36.rack_capabilities.compute.attributes]
additional_attribute2 = "compute-value"

[NVL36.rack_capabilities.switch]
count = 9

[NVL36.rack_capabilities.power_shelf]
count = 2
"#;
        let config: RackProfileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.rack_profiles.len(), 2);

        let nvl72 = config.get("NVL72").unwrap();
        assert_eq!(nvl72.product_family, Some(RackProductFamily::Gb200));
        assert_eq!(nvl72.rack_capabilities.compute.count, 18);
        assert_eq!(
            nvl72.rack_capabilities.compute.name.as_deref(),
            Some("GB200")
        );

        assert_eq!(
            nvl72.attributes,
            HashMap::from([
                ("attribute1".to_string(), "value1".to_string()),
                ("additional_attribute2".to_string(), "value2".to_string()),
            ])
        );

        assert_eq!(
            nvl72.rack_capabilities.compute.attributes,
            HashMap::from([
                ("attribute1".to_string(), "compute-value".to_string()),
                (
                    "additional_attribute2".to_string(),
                    "  MiXeD-Value  ".to_string(),
                ),
            ])
        );

        assert_eq!(
            nvl72.rack_capabilities.switch.attributes,
            HashMap::from([("attribute1".to_string(), "switch-value".to_string())])
        );

        assert_eq!(
            nvl72.rack_capabilities.power_shelf.attributes,
            HashMap::from([(
                "additional_attribute2".to_string(),
                "power-shelf-value".to_string(),
            )])
        );

        let nvl36 = config.get("NVL36").unwrap();

        assert_eq!(
            nvl36.product_family,
            Some(RackProductFamily::Other("test-product-family".to_string()))
        );

        assert_eq!(
            nvl36.attributes,
            HashMap::from([("attribute1".to_string(), "value1".to_string())])
        );

        assert_eq!(
            nvl36.rack_capabilities.compute.attributes,
            HashMap::from([(
                "additional_attribute2".to_string(),
                "compute-value".to_string(),
            )])
        );

        assert_eq!(nvl36.rack_capabilities.compute.count, 9);
        assert_eq!(nvl36.rack_capabilities.switch.count, 9);
        assert_eq!(nvl36.rack_capabilities.power_shelf.count, 2);
    }

    #[test]
    fn test_rack_profile_firmware_object_toml_deserialization() {
        const CAPABILITIES: &str = r#"
[Rack.rack_capabilities.compute]
count = 0
[Rack.rack_capabilities.switch]
count = 0
[Rack.rack_capabilities.power_shelf]
count = 0
"#;

        let cases = [
            (
                "configured timeout",
                r#"
[Rack.firmware_object]
url = "https://firmware.example.invalid/sot/rack.json"
fetch_timeout = "45s"
"#,
                Some((
                    "https://firmware.example.invalid/sot/rack.json",
                    std::time::Duration::from_secs(45),
                )),
            ),
            (
                "default timeout",
                r#"
[Rack.firmware_object]
url = "https://firmware.example.invalid/sot/rack.json"
"#,
                Some((
                    "https://firmware.example.invalid/sot/rack.json",
                    std::time::Duration::from_secs(30),
                )),
            ),
            ("not configured", "[Rack]\n", None),
        ];

        for (name, input, expected) in cases {
            let input = format!("{input}{CAPABILITIES}");
            let config: RackProfileConfig =
                toml::from_str(&input).unwrap_or_else(|error| panic!("{name}: {error}"));
            let actual =
                config
                    .get("Rack")
                    .unwrap()
                    .firmware_object
                    .as_ref()
                    .map(|firmware_object| {
                        (firmware_object.url.as_str(), firmware_object.fetch_timeout)
                    });

            assert_eq!(actual, expected, "{name}");
        }
    }

    #[test]
    fn test_rack_profile_config_toml_with_hardware_fields() {
        let toml_str = r#"
[NVL72]
product_family = "gb200"
rack_hardware_type = "dsx_gb200nvl_72x1"
rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"
rack_hardware_class = "prod"

[NVL72.rack_capabilities.compute]
name = "GB200"
count = 18
vendor = "NVIDIA"

[NVL72.rack_capabilities.switch]
count = 9

[NVL72.rack_capabilities.power_shelf]
count = 8
"#;
        let config: RackProfileConfig = toml::from_str(toml_str).unwrap();
        let nvl72 = config.get("NVL72").unwrap();

        assert_eq!(nvl72.product_family, Some(RackProductFamily::Gb200));
        assert_eq!(
            nvl72.rack_hardware_type,
            Some(RackHardwareType::from("dsx_gb200nvl_72x1"))
        );
        assert_eq!(
            nvl72.rack_hardware_topology,
            Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology)
        );
        assert_eq!(nvl72.rack_hardware_class, Some(RackHardwareClass::Prod));
        assert_eq!(nvl72.rack_capabilities.compute.count, 18);
    }

    #[test]
    fn test_rack_profile_config_toml_without_hardware_fields_defaults_to_none() {
        let toml_str = r#"
[NVL36]
product_family = "gb300"

[NVL36.rack_capabilities.compute]
count = 9
[NVL36.rack_capabilities.switch]
count = 9
[NVL36.rack_capabilities.power_shelf]
count = 2
"#;
        let config: RackProfileConfig = toml::from_str(toml_str).unwrap();
        let nvl36 = config.get("NVL36").unwrap();

        assert_eq!(nvl36.product_family, Some(RackProductFamily::Gb300));
        assert_eq!(nvl36.rack_hardware_type, None);
        assert_eq!(nvl36.rack_hardware_topology, None);
        assert_eq!(nvl36.rack_hardware_class, None);
        assert!(nvl36.attributes.is_empty());
        assert!(nvl36.rack_capabilities.compute.attributes.is_empty());
        assert!(nvl36.rack_capabilities.switch.attributes.is_empty());
        assert!(nvl36.rack_capabilities.power_shelf.attributes.is_empty());
    }

    #[test]
    fn test_rack_profile_config_rejects_duplicate_attribute_keys() {
        // TOML rejects repeated literal keys before Serde constructs the map.
        let toml_str = r#"
[NVL36]
attributes = { attribute1 = "value1", attribute1 = "value2" }

[NVL36.rack_capabilities.compute]
count = 9
[NVL36.rack_capabilities.switch]
count = 9
[NVL36.rack_capabilities.power_shelf]
count = 2
"#;

        assert!(toml::from_str::<RackProfileConfig>(toml_str).is_err());
    }

    #[test]
    fn test_rack_profile_config_without_product_family_defaults_to_none() {
        let toml_str = r#"
[NVL36.rack_capabilities.compute]
count = 9
[NVL36.rack_capabilities.switch]
count = 9
[NVL36.rack_capabilities.power_shelf]
count = 2
"#;

        let config: RackProfileConfig = toml::from_str(toml_str).unwrap();
        let nvl36 = config.get("NVL36").unwrap();

        assert_eq!(nvl36.product_family, None);
    }

    // RackHardwareType tests.

    // JSON round-trip: each variant serializes to its expected string and
    // deserializes back to itself. Projected to (json, value_back); the closure
    // discards the (non-PartialEq) serde_json error since every row succeeds.
    #[test]
    fn test_rack_hardware_type_serde_round_trip() {
        scenarios!(
            run = |hw_type| {
                let json = serde_json::to_string(&hw_type).map_err(drop)?;
                let back: RackHardwareType = serde_json::from_str(&json).map_err(drop)?;
                Ok::<_, ()>((json, back))
            };
            "dsx hardware type round-trips through json" {
                RackHardwareType::from("dsx_gb200nvl_72x1") => Yields((
                    "\"dsx_gb200nvl_72x1\"".to_string(),
                    RackHardwareType::from("dsx_gb200nvl_72x1"),
                )),
            }
        );
    }

    // Display forwards the inner string verbatim, including for the wildcard
    // and for empty/odd inputs.
    #[test]
    fn test_rack_hardware_type_display() {
        value_scenarios!(
            run = |hw_type| hw_type.to_string();
            "wildcard any" {
                RackHardwareType::any() => "any".to_string(),
            }

            "dsx hardware type" {
                RackHardwareType::from("dsx_gb200nvl_72x1") => "dsx_gb200nvl_72x1".to_string(),
            }

            "empty string" {
                RackHardwareType::from("") => String::new(),
            }

            "uppercase verbatim" {
                RackHardwareType::from("ANY") => "ANY".to_string(),
            }

            "whitespace preserved" {
                RackHardwareType::from("  spaced  ") => "  spaced  ".to_string(),
            }

            "default renders as any" {
                RackHardwareType::default() => "any".to_string(),
            }
        );
    }

    // is_any is an exact, case-sensitive match against the literal "any".
    #[test]
    fn test_rack_hardware_type_is_any() {
        value_scenarios!(
            run = |hw_type| hw_type.is_any();
            "constructed via any()" {
                RackHardwareType::any() => true,
            }

            "default is any" {
                RackHardwareType::default() => true,
            }

            "literal any from str" {
                RackHardwareType::from("any") => true,
            }

            "concrete hardware type" {
                RackHardwareType::from("dsx_gb200nvl_72x1") => false,
            }

            "empty is not any" {
                RackHardwareType::from("") => false,
            }

            "uppercase is not any" {
                RackHardwareType::from("ANY") => false,
            }

            "any with trailing space is not any" {
                RackHardwareType::from("any ") => false,
            }

            "substring is not any" {
                RackHardwareType::from("anything") => false,
            }
        );
    }

    #[test]
    fn test_rack_hardware_type_default_is_any() {
        assert_eq!(RackHardwareType::default(), RackHardwareType::any());
    }

    // Both From conversions wrap the input verbatim and agree with each other.
    #[test]
    fn test_rack_hardware_type_from_conversions() {
        value_scenarios!(
            run = |hw_type| hw_type;
            "from owned string" {
                RackHardwareType::from("dsx".to_string()) => RackHardwareType("dsx".to_string()),
            }

            "from str slice" {
                RackHardwareType::from("dsx") => RackHardwareType("dsx".to_string()),
            }

            "from empty str" {
                RackHardwareType::from("") => RackHardwareType(String::new()),
            }

            "owned and borrowed agree" {
                RackHardwareType::from("any".to_string()) => RackHardwareType::from("any"),
            }
        );
    }

    // RackProductFamily serde.

    #[test]
    fn test_rack_product_family_serde_round_trip() {
        scenarios!(
            run = |variant| {
                let json = serde_json::to_string(&variant).map_err(drop)?;
                let back: RackProductFamily = serde_json::from_str(&json).map_err(drop)?;
                Ok::<_, ()>((json, back))
            };
            "gb200 round-trips" {
                RackProductFamily::Gb200 => Yields(("\"gb200\"".to_string(), RackProductFamily::Gb200)),
            }

            "gb300 round-trips" {
                RackProductFamily::Gb300 => Yields(("\"gb300\"".to_string(), RackProductFamily::Gb300)),
            }

            "arbitrary family round-trips" {
                RackProductFamily::Other("test-product-family".to_string()) => Yields((
                    "\"test-product-family\"".to_string(),
                    RackProductFamily::Other("test-product-family".to_string()),
                )),
            }
        );
    }

    #[test]
    fn test_rack_product_family_display() {
        value_scenarios!(
            run = |variant| variant.to_string();
            "gb200" {
                RackProductFamily::Gb200 => "gb200".to_string(),
            }

            "gb300" {
                RackProductFamily::Gb300 => "gb300".to_string(),
            }

            "arbitrary family" {
                RackProductFamily::Other(" test-product-family ".to_string()) => "test-product-family".to_string(),
            }
        );
    }

    #[test]
    fn test_rack_product_family_deserialize() {
        scenarios!(
            run = |json| serde_json::from_str::<RackProductFamily>(json).map_err(drop);
            "valid gb200" {
                "\"gb200\"" => Yields(RackProductFamily::Gb200),
            }

            "valid gb300" {
                "\"gb300\"" => Yields(RackProductFamily::Gb300),
            }

            "arbitrary product family" {
                "\"test-product-family\"" => Yields(RackProductFamily::Other("test-product-family".to_string())),
            }

            "arbitrary product family preserves case" {
                "\"GB200\"" => Yields(RackProductFamily::Other("GB200".to_string())),
            }

            "empty product family" {
                "\"  \"" => Fails,
            }
        );
    }

    // Display covers every topology variant; the rendered string matches the
    // snake_case serde form.
    #[test]
    fn test_rack_hardware_topology_display() {
        value_scenarios!(
            run = |variant| variant.to_string();
            "gb200 nvl36" {
                RackHardwareTopology::Gb200Nvl36r1C2g4Topology => "gb200_nvl36r1_c2g4_topology".to_string(),
            }

            "gb300 nvl36" {
                RackHardwareTopology::Gb300Nvl36r1C2g4Topology => "gb300_nvl36r1_c2g4_topology".to_string(),
            }

            "gb200 nvl72" {
                RackHardwareTopology::Gb200Nvl72r1C2g4Topology => "gb200_nvl72r1_c2g4_topology".to_string(),
            }

            "gb300 nvl72" {
                RackHardwareTopology::Gb300Nvl72r1C2g4Topology => "gb300_nvl72r1_c2g4_topology".to_string(),
            }

            "vr nvl8 rtf" {
                RackHardwareTopology::VrNvl8r1C2g4RtfTopology => "vr_nvl8r1_c2g4_rtf_topology".to_string(),
            }

            "vr nvl72" {
                RackHardwareTopology::VrNvl72r1C2g4Topology => "vr_nvl72r1_c2g4_topology".to_string(),
            }
        );
    }

    // Deserialization accepts exactly the snake_case names and rejects anything
    // else (unknown names, the Display-only variant casing, empty). The
    // (non-PartialEq) serde error is discarded with map_err(drop).
    #[test]
    fn test_rack_hardware_topology_deserialize() {
        scenarios!(
            run = |json| serde_json::from_str::<RackHardwareTopology>(json).map_err(drop);
            "valid gb200 nvl36" {
                "\"gb200_nvl36r1_c2g4_topology\"" => Yields(RackHardwareTopology::Gb200Nvl36r1C2g4Topology),
            }

            "valid vr nvl72" {
                "\"vr_nvl72r1_c2g4_topology\"" => Yields(RackHardwareTopology::VrNvl72r1C2g4Topology),
            }

            "unknown topology name" {
                "\"gb500_nvl99_topology\"" => Fails,
            }

            "empty string" {
                "\"\"" => Fails,
            }

            "wrong json type" {
                "42" => Fails,
            }
        );
    }

    // RackHardwareClass serde.

    // JSON round-trip: each class variant serializes to its expected snake_case
    // string and deserializes back to itself. Projected to (json, value_back);
    // the (non-PartialEq) serde_json error is discarded.
    #[test]
    fn test_rack_hardware_class_serde_round_trip() {
        scenarios!(
            run = |variant| {
                let json = serde_json::to_string(&variant).map_err(drop)?;
                let back: RackHardwareClass = serde_json::from_str(&json).map_err(drop)?;
                Ok::<_, ()>((json, back))
            };
            "dev round-trips" {
                RackHardwareClass::Dev => Yields(("\"dev\"".to_string(), RackHardwareClass::Dev)),
            }

            "prod round-trips" {
                RackHardwareClass::Prod => Yields(("\"prod\"".to_string(), RackHardwareClass::Prod)),
            }
        );
    }

    #[test]
    fn test_rack_hardware_class_display() {
        value_scenarios!(
            run = |variant| variant.to_string();
            "dev" {
                RackHardwareClass::Dev => "dev".to_string(),
            }

            "prod" {
                RackHardwareClass::Prod => "prod".to_string(),
            }
        );
    }

    // Deserialization accepts the two snake_case names and rejects others.
    // The (non-PartialEq) serde error is discarded with map_err(drop).
    #[test]
    fn test_rack_hardware_class_deserialize() {
        scenarios!(
            run = |json| serde_json::from_str::<RackHardwareClass>(json).map_err(drop);
            "valid dev" {
                "\"dev\"" => Yields(RackHardwareClass::Dev),
            }

            "valid prod" {
                "\"prod\"" => Yields(RackHardwareClass::Prod),
            }

            "uppercase rejected" {
                "\"Dev\"" => Fails,
            }

            "unknown class" {
                "\"staging\"" => Fails,
            }

            "empty string" {
                "\"\"" => Fails,
            }
        );
    }

    // RackCapabilityType Display renders each variant with its canonical
    // PascalCase label.
    #[test]
    fn test_rack_capability_type_display() {
        value_scenarios!(
            run = |variant| variant.to_string();
            "compute" {
                RackCapabilityType::Compute => "Compute".to_string(),
            }

            "switch" {
                RackCapabilityType::Switch => "Switch".to_string(),
            }

            "power shelf" {
                RackCapabilityType::PowerShelf => "PowerShelf".to_string(),
            }
        );
    }
}
