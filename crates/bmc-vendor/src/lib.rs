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

//! BMC Manufacturer ID

use std::fmt;

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Hash,
    Eq,
    PartialEq,
    clap::ValueEnum,
    clap::Parser,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum BMCVendor {
    Lenovo,
    LenovoAMI,
    Dell,
    Supermicro,
    Hpe,
    Nvidia, // DPU, Viking, Oberon
    Liteon,
    Delta,
    Sushy,
    #[serde(other)]
    #[default]
    Unknown,
}

impl fmt::Display for BMCVendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = format!("{self:?}").to_lowercase();
        write!(f, "{s}")
    }
}

impl From<&str> for BMCVendor {
    fn from(s: &str) -> BMCVendor {
        match s.to_lowercase().as_str() {
            "lenovo" => BMCVendor::Lenovo,
            "lenovoami" => BMCVendor::LenovoAMI,
            "dell" => BMCVendor::Dell,
            "supermicro" => BMCVendor::Supermicro,
            "hpe" => BMCVendor::Hpe,
            "nvidia" => BMCVendor::Nvidia,
            "liteon" => BMCVendor::Liteon,
            "delta" => BMCVendor::Delta,
            "sushy" => BMCVendor::Sushy,
            _ => BMCVendor::Unknown,
        }
    }
}

/// DPU generation / model identifier used to key per-model factory default credentials.
///
/// The `Display` impl produces the lowercase vault path segment ("bf3", "bf4", ...).
/// `Unknown` maps to "unknown" for new vault paths; existing deployments keep their
/// legacy entry at `machines/all_dpus/factory_default/bmc-metadata-items/root`, which
/// the credential key encoding maps `Unknown` to for backward compatibility.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Hash,
    Eq,
    PartialEq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum DpuModel {
    #[value(name = "bf2")]
    BlueField2,
    #[value(name = "bf3")]
    BlueField3,
    #[value(name = "bf4")]
    BlueField4,
    #[serde(other)]
    #[default]
    Unknown,
}

impl fmt::Display for DpuModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DpuModel::BlueField2 => "bf2",
            DpuModel::BlueField3 => "bf3",
            DpuModel::BlueField4 => "bf4",
            DpuModel::Unknown => "unknown",
        };
        write!(f, "{s}")
    }
}

impl From<&str> for DpuModel {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "bf2" => DpuModel::BlueField2,
            "bf3" => DpuModel::BlueField3,
            "bf4" => DpuModel::BlueField4,
            _ => DpuModel::Unknown,
        }
    }
}

impl DpuModel {
    /// Identify the DPU generation from the Redfish service root `Product` field,
    /// which BlueField BMCs set to a human-readable model string (e.g. "BlueField-3 DPU").
    /// Returns `Unknown` for unrecognized strings so callers can fall back gracefully.
    pub fn from_service_root_product(product: &str) -> Self {
        let lower = product.to_lowercase();
        if lower.contains("bluefield-2") || lower.contains("bluefield 2") {
            DpuModel::BlueField2
        } else if lower.contains("bluefield-3") || lower.contains("bluefield 3") {
            DpuModel::BlueField3
        } else if lower.contains("bluefield-4") || lower.contains("bluefield 4") {
            DpuModel::BlueField4
        } else {
            DpuModel::Unknown
        }
    }

    /// Publicly-documented factory-default BMC credentials `(username, password)`
    /// for this DPU generation.
    ///
    /// This is the single source of truth shared by site-explorer's last-resort
    /// credential fallback (used when no vault entry is configured) and the BMC
    /// mock's factory-default account, so the two cannot drift. BlueField-4 ships
    /// with a distinct default account (`admin`); earlier generations and
    /// unrecognized models use the legacy `root` default.
    pub fn default_factory_credentials(&self) -> (&'static str, &'static str) {
        match self {
            DpuModel::BlueField4 => ("admin", "0penBmc"),
            DpuModel::BlueField2 | DpuModel::BlueField3 | DpuModel::Unknown => ("root", "0penBmc"),
        }
    }
}

impl BMCVendor {
    /// From the string libudev returns querying the dmi subsystem
    pub fn from_udev_dmi(s: &str) -> BMCVendor {
        match s {
            "Lenovo" => BMCVendor::Lenovo,
            "Dell Inc." => BMCVendor::Dell,
            "https://www.mellanox.com" => BMCVendor::Nvidia,
            "NVIDIA" => BMCVendor::Nvidia,
            "Supermicro" => BMCVendor::Supermicro,
            "HPE" => BMCVendor::Hpe,
            _ => BMCVendor::Unknown,
        }
    }

    /// to_pascalcase converts to StringLikeThis to match serialization
    pub fn to_pascalcase(self) -> String {
        match self {
            BMCVendor::Lenovo => "Lenovo",
            BMCVendor::LenovoAMI => "LenovoAMI",
            BMCVendor::Dell => "Dell",
            BMCVendor::Supermicro => "Supermicro",
            BMCVendor::Hpe => "Hpe",
            BMCVendor::Nvidia => "Nvidia",
            BMCVendor::Liteon => "Liteon",
            BMCVendor::Delta => "Delta",
            BMCVendor::Sushy => "Sushy",
            BMCVendor::Unknown => "Unknown",
        }
        .to_string()
    }
    pub fn is_lenovo(&self) -> bool {
        *self == Self::Lenovo
    }

    pub fn is_supermicro(&self) -> bool {
        *self == Self::Supermicro
    }

    pub fn is_nvidia(&self) -> bool {
        *self == Self::Nvidia
    }

    pub fn is_dell(&self) -> bool {
        *self == Self::Dell
    }

    pub fn is_sushy(&self) -> bool {
        *self == Self::Sushy
    }

    pub fn is_unknown(&self) -> bool {
        *self == Self::Unknown
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Debug, Default, PartialEq, Eq)]
    struct VendorPredicates {
        is_lenovo: bool,
        is_supermicro: bool,
        is_nvidia: bool,
        is_dell: bool,
        is_sushy: bool,
        is_unknown: bool,
    }

    #[test]
    fn bmc_vendor_canonical_forms_cover_all_variants() {
        value_scenarios!(
            run = |input: &str| {
                let vendor = BMCVendor::from(input);
                (
                    vendor,
                    vendor.to_string(),
                    vendor.to_pascalcase(),
                    VendorPredicates {
                        is_lenovo: vendor.is_lenovo(),
                        is_supermicro: vendor.is_supermicro(),
                        is_nvidia: vendor.is_nvidia(),
                        is_dell: vendor.is_dell(),
                        is_sushy: vendor.is_sushy(),
                        is_unknown: vendor.is_unknown(),
                    },
                )
            };

            "canonical names are case insensitive" {
                "LeNoVo" => (
                    BMCVendor::Lenovo,
                    "lenovo".to_string(),
                    "Lenovo".to_string(),
                    VendorPredicates {
                        is_lenovo: true,
                        ..Default::default()
                    },
                ),
                "LeNoVoAmI" => (
                    BMCVendor::LenovoAMI,
                    "lenovoami".to_string(),
                    "LenovoAMI".to_string(),
                    VendorPredicates::default(),
                ),
                "DELL" => (
                    BMCVendor::Dell,
                    "dell".to_string(),
                    "Dell".to_string(),
                    VendorPredicates {
                        is_dell: true,
                        ..Default::default()
                    },
                ),
                "SuPeRmIcRo" => (
                    BMCVendor::Supermicro,
                    "supermicro".to_string(),
                    "Supermicro".to_string(),
                    VendorPredicates {
                        is_supermicro: true,
                        ..Default::default()
                    },
                ),
                "HPE" => (
                    BMCVendor::Hpe,
                    "hpe".to_string(),
                    "Hpe".to_string(),
                    VendorPredicates::default(),
                ),
                "NVIDIA" => (
                    BMCVendor::Nvidia,
                    "nvidia".to_string(),
                    "Nvidia".to_string(),
                    VendorPredicates {
                        is_nvidia: true,
                        ..Default::default()
                    },
                ),
                "LiTeOn" => (
                    BMCVendor::Liteon,
                    "liteon".to_string(),
                    "Liteon".to_string(),
                    VendorPredicates::default(),
                ),
                "DeLtA" => (
                    BMCVendor::Delta,
                    "delta".to_string(),
                    "Delta".to_string(),
                    VendorPredicates::default(),
                ),
                "SuShY" => (
                    BMCVendor::Sushy,
                    "sushy".to_string(),
                    "Sushy".to_string(),
                    VendorPredicates {
                        is_sushy: true,
                        ..Default::default()
                    },
                ),
                "unknown" => (
                    BMCVendor::Unknown,
                    "unknown".to_string(),
                    "Unknown".to_string(),
                    VendorPredicates {
                        is_unknown: true,
                        ..Default::default()
                    },
                ),
            }

            "noncanonical names are unknown" {
                "Acme" => (
                    BMCVendor::Unknown,
                    "unknown".to_string(),
                    "Unknown".to_string(),
                    VendorPredicates {
                        is_unknown: true,
                        ..Default::default()
                    },
                ),
                " dell " => (
                    BMCVendor::Unknown,
                    "unknown".to_string(),
                    "Unknown".to_string(),
                    VendorPredicates {
                        is_unknown: true,
                        ..Default::default()
                    },
                ),
            }
        );
    }

    #[test]
    fn bmc_vendor_from_udev_dmi_classifies_system_vendors() {
        value_scenarios!(BMCVendor::from_udev_dmi:
            "known system vendors" {
                "Lenovo" => BMCVendor::Lenovo,
                "Dell Inc." => BMCVendor::Dell,
                "https://www.mellanox.com" => BMCVendor::Nvidia,
                "NVIDIA" => BMCVendor::Nvidia,
                "Supermicro" => BMCVendor::Supermicro,
                "HPE" => BMCVendor::Hpe,
            }

            "unknown system vendors" {
                "Acme Corp" => BMCVendor::Unknown,
                "dell inc." => BMCVendor::Unknown,
            }
        );
    }
}
