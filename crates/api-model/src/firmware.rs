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
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fmt::{Debug, Display};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use regex::Regex;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::site_explorer::{EndpointExplorationReport, Service};

/// Firmware versions this carbide instance wants to install onto hosts
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct DesiredFirmwareVersions {
    /// Parsed versions, serializtion override means it will always be sorted
    #[serde(default, serialize_with = "carbide_utils::ordered_map")]
    pub versions: HashMap<FirmwareComponentType, String>,
}

impl From<Firmware> for DesiredFirmwareVersions {
    fn from(value: Firmware) -> Self {
        // Using a BTreeMap instead of a hash means that this will be sorted by the key
        let mut versions: DesiredFirmwareVersions = Default::default();
        for (component_type, component) in value.components {
            for firmware in component.known_firmware {
                if firmware.default {
                    versions.versions.insert(component_type, firmware.version);
                    break;
                }
            }
        }
        versions
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Firmware {
    pub vendor: bmc_vendor::BMCVendor,
    pub model: String,

    pub components: HashMap<FirmwareComponentType, FirmwareComponent>,

    #[serde(default)]
    pub explicit_start_needed: bool,

    #[serde(default)]
    pub ordering: Vec<FirmwareComponentType>,
}

/// Runtime host firmware config stored by the API and overlaid onto the static
/// firmware catalog.
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct HostFirmwareConfig {
    pub vendor: bmc_vendor::BMCVendor,
    pub model: String,

    pub components: HashMap<FirmwareComponentType, FirmwareComponent>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_start_needed: Option<bool>,

    #[serde(default)]
    pub ordering: Vec<FirmwareComponentType>,
}

impl From<HostFirmwareConfig> for Firmware {
    fn from(config: HostFirmwareConfig) -> Self {
        Firmware {
            vendor: config.vendor,
            model: config.model,
            components: config.components,
            explicit_start_needed: config.explicit_start_needed.unwrap_or(false),
            ordering: config.ordering,
        }
    }
}

impl From<Firmware> for HostFirmwareConfig {
    fn from(firmware: Firmware) -> Self {
        HostFirmwareConfig {
            vendor: firmware.vendor,
            model: firmware.model,
            components: firmware.components,
            explicit_start_needed: Some(firmware.explicit_start_needed),
            ordering: firmware.ordering,
        }
    }
}

impl Firmware {
    pub fn matching_version_id(
        &self,
        redfish_id: &str,
        firmware_type: FirmwareComponentType,
    ) -> bool {
        // This searches for the regex we've recorded for what this vendor + model + firmware_type gets reported as in the list of firmware versions
        self.components
            .get(&firmware_type)
            .unwrap_or(&FirmwareComponent::default()) // Will trigger the unwrap_or below
            .current_version_reported_as
            .as_ref()
            .map(|regex| regex.captures(redfish_id).is_some())
            .unwrap_or(false)
    }
    pub fn ordering(&self) -> Vec<FirmwareComponentType> {
        let mut ordering = self.ordering.clone();
        if ordering.is_empty() {
            const ORDERING: [FirmwareComponentType; 2] =
                [FirmwareComponentType::Bmc, FirmwareComponentType::Uefi];
            ordering = ORDERING.to_vec();
        }
        ordering
    }

    /// Every installed inventory entry matching each configured component, as
    /// the updater matches them. A configured component without a match gets
    /// an empty group. Groups are sorted by component slug, then by drive
    /// model, and entries by service, inventory and version. `None` when a
    /// configured component is unknown.
    pub fn installed_inventory(&self, services: &[Service]) -> Option<Vec<ComponentInventory>> {
        let mut groups = self
            .components
            .keys()
            .map(|&component| {
                let mut entries: Vec<_> = services
                    .iter()
                    .flat_map(|service| {
                        service
                            .inventories
                            .iter()
                            .filter(move |inventory| {
                                self.matching_version_id(&inventory.id, component)
                            })
                            .map(|inventory| InstalledFirmwareEntry {
                                service_id: service.id.clone(),
                                inventory_id: inventory.id.clone(),
                                version: inventory.version.clone(),
                            })
                    })
                    .collect();
                entries.sort();
                (component != FirmwareComponentType::Unknown)
                    .then_some(ComponentInventory { component, entries })
            })
            .collect::<Option<Vec<_>>>()?;
        groups.sort_by_key(|group| (group.component.slug(), group.component));
        Some(groups)
    }

    /// find_version will locate a version number within an EndpointExplorationReport
    pub fn find_version(
        &self,
        report: &EndpointExplorationReport,
        firmware_type: FirmwareComponentType,
    ) -> Option<String> {
        for service in report.service.iter() {
            if let Some(matching_inventory) = service
                .inventories
                .iter()
                .find(|&x| self.matching_version_id(&x.id, firmware_type))
            {
                tracing::debug!(
                    machine_id = ?report.machine_id,
                    ?firmware_type,
                    version = ?matching_inventory.version,
                    "Found matching firmware version",
                );
                return matching_inventory.version.clone();
            };
        }
        None
    }
}

/// A host firmware component.
///
/// Serialized as its lowercase [`slug`](Self::slug) (`bmc`, `cx7`,
/// `combinedbmcuefi`, ...), or as `drive-<model>` for a drive model. These
/// names are stored in firmware configuration, machine state and exploration
/// reports, so they never change. A name that is not recognized, including a
/// malformed drive name, reads as [`Unknown`](Self::Unknown).
#[derive(Debug, Default, Eq, PartialEq, Hash, Copy, Clone, Ord, PartialOrd)]
pub enum FirmwareComponentType {
    Bmc,
    Cec,
    Uefi,
    Nic,
    CpldMb,
    CpldPdb,
    HGXBmc,
    CombinedBmcUefi,
    Gpu,
    Cx7,
    /// The firmware of one drive model, named `drive-<model>`. `model` is the
    /// drive's Redfish `Model` exactly as the BMC reports it, such as
    /// `KCD8DPUG15T3` or `Dell Ent NVMe CM6 RI 1.92TB`: non-empty, without
    /// leading or trailing whitespace.
    Drive {
        model: DriveModel,
    },
    #[default]
    Unknown,
}

/// A drive's Redfish `Model`, interned so that [`FirmwareComponentType`] stays
/// `Copy`.
///
/// Each distinct model is stored once and kept for the life of the process.
/// Models come only from firmware configuration and the machine state and
/// reports derived from it, so the set stays as small as the number of drive
/// models configured.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DriveModel(&'static str);

impl DriveModel {
    /// Returns the interned model, storing it on first use, or `None` when
    /// `model` is empty or has leading or trailing whitespace: such a model
    /// would not read back from its `drive-<model>` name.
    pub fn new(model: &str) -> Option<Self> {
        if model.is_empty() || model != model.trim() {
            return None;
        }
        static MODELS: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
        let mut models = MODELS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(&stored) = models.get(model) {
            return Some(Self(stored));
        }
        let stored: &'static str = Box::leak(model.into());
        models.insert(stored);
        Some(Self(stored))
    }

    /// The model as the BMC reports it.
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for DriveModel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl fmt::Debug for DriveModel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self.0, f)
    }
}

/// Prefix of a drive component's name: `drive-<model>`.
const DRIVE_PREFIX: &str = "drive-";

impl fmt::Display for FirmwareComponentType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FirmwareComponentType::Bmc => write!(f, "BMC"),
            FirmwareComponentType::Uefi => write!(f, "UEFI"),
            FirmwareComponentType::CombinedBmcUefi => write!(f, "BMC+UEFI"),
            FirmwareComponentType::Nic => write!(f, "NIC"),
            FirmwareComponentType::CpldMb => write!(f, "CPLD MB"),
            FirmwareComponentType::CpldPdb => write!(f, "CPLD PDB"),
            FirmwareComponentType::Cec => write!(f, "CEC"),
            FirmwareComponentType::Gpu => write!(f, "GPU"),
            FirmwareComponentType::HGXBmc => write!(f, "HGX BMC"),
            FirmwareComponentType::Cx7 => write!(f, "CX7"),
            // Scout passes this to drive scripts as COMPONENT_TYPE.
            FirmwareComponentType::Drive { model } => write!(f, "{DRIVE_PREFIX}{model}"),
            FirmwareComponentType::Unknown => write!(f, "Unknown"),
        }
    }
}

impl Serialize for FirmwareComponentType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // A drive's stored name is its Display text.
            FirmwareComponentType::Drive { .. } => serializer.collect_str(self),
            other => serializer.serialize_str(other.slug()),
        }
    }
}

impl<'de> Deserialize<'de> for FirmwareComponentType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NameVisitor;

        impl de::Visitor<'_> for NameVisitor {
            type Value = FirmwareComponentType;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a firmware component name")
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<Self::Value, E> {
                Ok(FirmwareComponentType::from_name(name))
            }
        }

        deserializer.deserialize_str(NameVisitor)
    }
}

impl FirmwareComponentType {
    /// Parses a serialized component name; unrecognized names are `Unknown`.
    fn from_name(name: &str) -> Self {
        match name {
            "bmc" => FirmwareComponentType::Bmc,
            "cec" => FirmwareComponentType::Cec,
            "uefi" => FirmwareComponentType::Uefi,
            "nic" => FirmwareComponentType::Nic,
            "cpldmb" => FirmwareComponentType::CpldMb,
            "cpldpdb" => FirmwareComponentType::CpldPdb,
            "hgxbmc" => FirmwareComponentType::HGXBmc,
            "combinedbmcuefi" => FirmwareComponentType::CombinedBmcUefi,
            "gpu" => FirmwareComponentType::Gpu,
            "cx7" => FirmwareComponentType::Cx7,
            _ => name
                .strip_prefix(DRIVE_PREFIX)
                .and_then(DriveModel::new)
                .map_or(FirmwareComponentType::Unknown, |model| {
                    FirmwareComponentType::Drive { model }
                }),
        }
    }

    /// The lowercase name of the component type. Every drive model shares
    /// `drive`.
    pub fn slug(self) -> &'static str {
        match self {
            FirmwareComponentType::Bmc => "bmc",
            FirmwareComponentType::Cec => "cec",
            FirmwareComponentType::Uefi => "uefi",
            FirmwareComponentType::Nic => "nic",
            FirmwareComponentType::CpldMb => "cpldmb",
            FirmwareComponentType::CpldPdb => "cpldpdb",
            FirmwareComponentType::HGXBmc => "hgxbmc",
            FirmwareComponentType::CombinedBmcUefi => "combinedbmcuefi",
            FirmwareComponentType::Gpu => "gpu",
            FirmwareComponentType::Cx7 => "cx7",
            FirmwareComponentType::Drive { .. } => "drive",
            FirmwareComponentType::Unknown => "unknown",
        }
    }

    pub fn is_bmc(&self) -> bool {
        matches!(
            self,
            FirmwareComponentType::Bmc | FirmwareComponentType::CombinedBmcUefi
        )
    }
    pub fn is_uefi(&self) -> bool {
        matches!(
            self,
            FirmwareComponentType::Uefi | FirmwareComponentType::CombinedBmcUefi
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FirmwareComponent {
    #[serde(with = "serde_regex")]
    pub current_version_reported_as: Option<Regex>,
    pub preingest_upgrade_when_below: Option<String>,
    #[serde(default)]
    pub known_firmware: Vec<FirmwareEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FirmwareEntry {
    pub version: String,
    pub mandatory_upgrade_from_priority: Option<MandatoryUpgradeFromPriority>,
    #[serde(default)]
    pub default: bool,
    pub filename: Option<String>,
    #[serde(default)]
    pub filenames: Vec<String>,
    pub url: Option<String>,
    pub checksum: Option<String>,
    #[serde(default)]
    // If set, we will pass the firmware type to libredfish which for some platforms will install only one part of a multi-firmware package.
    pub install_only_specified: bool,
    pub power_drains_needed: Option<u32>,
    #[serde(default)]
    // this firmware entry is only applicable in preingestion.
    // BF3s are the only machine with multiple firmware entries for a given firmware compoanent type (BMC FWs).
    // This flag is used to mark the firmware entry for BMC preingestion on BF3s.
    pub preingestion_exclusive_config: bool,
    /// If true, we will need a series of resets before even trying to upgrade
    #[serde(default)]
    pub pre_update_resets: bool,
    #[serde(default)]
    pub script: Option<PathBuf>,
    #[serde(default)]
    pub files: Vec<FirmwareFileArtifact>,
    #[serde(default)]
    pub scout: Option<ScoutConfig>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct FirmwareFileArtifact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FirmwareFileArtifactWire {
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    url: Option<String>,
    sha256: String,
}

// Transitional validation while firmware metadata supports both local artifacts
// and URL-based artifacts. Once metadata is fully URL-based, `url` should
// become required and `filename` can be removed (and this impl block too).
impl<'de> Deserialize<'de> for FirmwareFileArtifact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FirmwareFileArtifactWire::deserialize(deserializer)?;
        if !firmware_file_artifact_location_is_set(&wire.filename)
            && !firmware_file_artifact_location_is_set(&wire.url)
        {
            return Err(de::Error::custom(
                "firmware files[] artifact must set filename or url",
            ));
        }

        Ok(Self {
            filename: wire.filename,
            url: wire.url,
            sha256: wire.sha256,
        })
    }
}

fn firmware_file_artifact_location_is_set(value: &Option<String>) -> bool {
    value
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScoutConfig {
    /// Legacy script metadata accepted for backwards-compatible config parsing.
    /// Scout script selection is inferred from the PXE script registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<FirmwareFileArtifact>,
    pub execution_timeout_seconds: u32,
    pub artifact_download_timeout_seconds: u32,
}

impl FirmwareEntry {
    /// Creates a FirmwareEntry with default parameters for tests
    pub fn standard(version: &str) -> Self {
        Self {
            version: version.to_string(),
            default: true,
            filename: Some("/dev/null".to_string()),
            filenames: vec![],
            url: Some("file://dev/null".to_string()),
            checksum: None,
            mandatory_upgrade_from_priority: None,
            install_only_specified: false,
            power_drains_needed: None,
            preingestion_exclusive_config: false,
            pre_update_resets: false,
            script: None,
            files: vec![],
            scout: None,
        }
    }
    pub fn standard_multiple_filenames(version: &str) -> Self {
        let mut ret = FirmwareEntry::standard(version);
        ret.filename = None;
        ret.filenames = vec!["/dev/null".to_string(), "/dev/null".to_string()];
        ret
    }
    pub fn standard_notdefault(version: &str) -> Self {
        let mut ret = FirmwareEntry::standard(version);
        ret.default = false;
        ret
    }
    pub fn standard_filename(version: &str, filename: &str) -> Self {
        let mut ret = FirmwareEntry::standard(version);
        ret.filename = Some(filename.to_string());
        ret.url = None;
        ret
    }
    pub fn standard_filename_notdefault(version: &str, filename: &str) -> Self {
        let mut ret = FirmwareEntry::standard_notdefault(version);
        ret.filename = Some(filename.to_string());
        ret.url = None;
        ret
    }
    pub fn standard_powerdrains(version: &str, powerdrains: u32) -> Self {
        let mut ret = FirmwareEntry::standard(version);
        ret.power_drains_needed = Some(powerdrains);
        ret.pre_update_resets = true;
        ret
    }
    pub fn standard_script(version: &str, script: &str) -> Self {
        let mut ret = FirmwareEntry::standard(version);
        ret.script = Some(script.into());
        ret
    }

    pub fn artifact_count(&self) -> usize {
        if !self.files.is_empty() {
            self.files.len()
        } else if !self.filenames.is_empty() {
            self.filenames.len()
        } else if self.filename.is_some() || self.url.is_some() {
            1
        } else {
            0
        }
    }

    pub fn get_filename(&self, pos: u32) -> PathBuf {
        let pos = pos.try_into().unwrap_or(usize::MAX);
        let filename = if self.filenames.is_empty() {
            &self.filename
        } else if pos < self.filenames.len() {
            let filename_clone = self.filenames[pos].clone();
            &Some(filename_clone)
        } else {
            &None
        };
        match filename {
            None => PathBuf::from("/dev/null"),
            Some(file_key) => PathBuf::from(file_key),
        }
    }
    pub fn get_url(&self) -> String {
        match &self.url {
            None => "file://dev/null".to_string(),
            Some(url) => url.to_owned(),
        }
    }
    pub fn get_checksum(&self) -> String {
        match &self.checksum {
            None => "".to_string(),
            Some(checksum) => checksum.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MandatoryUpgradeFromPriority {
    None,
    Security,
}

// Should match api/src/model/machine/upgrade_policy.rs DpuAgentUpgradePolicy
#[derive(Debug, Copy, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUpgradePolicyChoice {
    Off,
    UpOnly,
    UpDown,
}

impl Display for AgentUpgradePolicyChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self, f)
    }
}

/// Installed inventory entries matched to one configured component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentInventory {
    pub component: FirmwareComponentType,
    pub entries: Vec<InstalledFirmwareEntry>,
}

/// One installed firmware inventory entry of an explored report.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstalledFirmwareEntry {
    pub service_id: String,
    pub inventory_id: String,
    pub version: Option<String>,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;
    use crate::site_explorer::Inventory;

    #[test]
    fn inventory_keeps_every_match() {
        let firmware = |components: &[(FirmwareComponentType, &str)]| Firmware {
            components: components
                .iter()
                .map(|&(component, regex)| {
                    let reported_as = Some(Regex::new(regex).unwrap());
                    (
                        component,
                        FirmwareComponent {
                            current_version_reported_as: reported_as,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        let services = |inventories: &[(&str, Option<&str>)]| {
            let inventories = inventories.iter().map(|&(id, version)| Inventory {
                id: id.to_string(),
                version: version.map(str::to_string),
                ..Default::default()
            });
            let id = "FirmwareInventory".to_string();
            vec![Service {
                id,
                inventories: inventories.collect(),
            }]
        };
        let entry = |inventory_id: &str, version: Option<&str>| InstalledFirmwareEntry {
            service_id: "FirmwareInventory".to_string(),
            inventory_id: inventory_id.to_string(),
            version: version.map(str::to_string),
        };
        let cx7_and_bmc = firmware(&[
            (FirmwareComponentType::Cx7, "^CX7_"),
            (FirmwareComponentType::Bmc, "^BMC$"),
        ]);
        let both_cards = Some(vec![
            ("bmc", vec![]),
            (
                "cx7",
                vec![entry("CX7_0", Some("28.2")), entry("CX7_1", Some("28.1"))],
            ),
        ]);

        value_scenarios!(
            run = |(firmware, installed): (&Firmware, Vec<Service>)| {
                let groups = firmware.installed_inventory(&installed)?;
                Some(groups.into_iter().map(|group| (group.component.slug(), group.entries)).collect::<Vec<_>>())
            };
            "every match is kept in any report order, a component without one is empty" {
                (&cx7_and_bmc, services(&[("CX7_0", Some("28.2")), ("NIC_0", Some("1")), ("CX7_1", Some("28.1"))])) => both_cards.clone(),
                (&cx7_and_bmc, services(&[("CX7_1", Some("28.1")), ("CX7_0", Some("28.2"))])) => both_cards.clone(),
            }
            "an entry without a version is kept" {
                (&cx7_and_bmc, services(&[("BMC", None)])) => Some(vec![("bmc", vec![entry("BMC", None)]), ("cx7", vec![])]),
            }
            "an unknown configured component fails closed" {
                (&firmware(&[(FirmwareComponentType::Unknown, "^X$")]), services(&[("X", Some("1"))])) => None,
            }
        );
    }

    fn drive(model: &str) -> FirmwareComponentType {
        FirmwareComponentType::Drive {
            model: DriveModel::new(model).unwrap(),
        }
    }

    /// The stored name, the parsed value and the `Display` text agree.
    #[test]
    fn component_type_names() {
        let check = |name: &str| {
            let parsed: FirmwareComponentType =
                serde_json::from_value(serde_json::json!(name)).unwrap();
            let stored = serde_json::to_value(parsed).unwrap();
            (parsed, stored, parsed.to_string())
        };
        let unknown = || {
            (
                FirmwareComponentType::Unknown,
                "unknown".into(),
                "Unknown".to_string(),
            )
        };
        value_scenarios!(run = check;
            "existing names are unchanged" {
                "bmc" => (FirmwareComponentType::Bmc, "bmc".into(), "BMC".to_string()),
                "cec" => (FirmwareComponentType::Cec, "cec".into(), "CEC".to_string()),
                "uefi" => (FirmwareComponentType::Uefi, "uefi".into(), "UEFI".to_string()),
                "nic" => (FirmwareComponentType::Nic, "nic".into(), "NIC".to_string()),
                "cpldmb" => (FirmwareComponentType::CpldMb, "cpldmb".into(), "CPLD MB".to_string()),
                "cpldpdb" => (FirmwareComponentType::CpldPdb, "cpldpdb".into(), "CPLD PDB".to_string()),
                "hgxbmc" => (FirmwareComponentType::HGXBmc, "hgxbmc".into(), "HGX BMC".to_string()),
                "combinedbmcuefi" => (
                    FirmwareComponentType::CombinedBmcUefi,
                    "combinedbmcuefi".into(),
                    "BMC+UEFI".to_string(),
                ),
                "gpu" => (FirmwareComponentType::Gpu, "gpu".into(), "GPU".to_string()),
                "cx7" => (FirmwareComponentType::Cx7, "cx7".into(), "CX7".to_string()),
                "unknown" => unknown(),
            }

            "drive names keep the Redfish model as reported" {
                "drive-KCD8DPUG15T3" => (
                    drive("KCD8DPUG15T3"),
                    "drive-KCD8DPUG15T3".into(),
                    "drive-KCD8DPUG15T3".to_string(),
                ),
                "drive-MZVL6960HFLB-01AW7" => (
                    drive("MZVL6960HFLB-01AW7"),
                    "drive-MZVL6960HFLB-01AW7".into(),
                    "drive-MZVL6960HFLB-01AW7".to_string(),
                ),
                "drive-Dell Ent NVMe CM6 RI 1.92TB" => (
                    drive("Dell Ent NVMe CM6 RI 1.92TB"),
                    "drive-Dell Ent NVMe CM6 RI 1.92TB".into(),
                    "drive-Dell Ent NVMe CM6 RI 1.92TB".to_string(),
                ),
            }

            "unrecognized names read as Unknown" {
                "drive-" => unknown(),
                "drive- KCD8DPUG15T3" => unknown(),
                "drive-KCD8DPUG15T3 " => unknown(),
                "Drive-KCD8DPUG15T3" => unknown(),
                "drive" => unknown(),
                "BMC" => unknown(),
            }
        );
    }

    /// Parsing a model again reuses its stored copy, so repeated reads of the
    /// same configuration and state do not grow memory.
    #[test]
    fn drive_model_is_stored_once() {
        let first = DriveModel::new("Dell Ent NVMe CM7 FIPS E3.S MU 6.4TB").unwrap();
        let again = DriveModel::new(&String::from("Dell Ent NVMe CM7 FIPS E3.S MU 6.4TB")).unwrap();
        assert_eq!(first, again);
        assert!(std::ptr::eq(first.as_str(), again.as_str()));
        assert_ne!(
            first,
            DriveModel::new("Dell Ent NVMe CM7 FIPS E3.S MU 3.2TB").unwrap()
        );
    }

    /// A model that would not read back from its `drive-<model>` name is
    /// rejected.
    #[test]
    fn drive_model_must_be_trimmed_and_non_empty() {
        let check = |model: &str| DriveModel::new(model).map(DriveModel::as_str);
        value_scenarios!(run = check;
            "accepted" {
                "KCD8DPUG15T3" => Some("KCD8DPUG15T3"),
                "Dell Ent NVMe CM6 RI 1.92TB" => Some("Dell Ent NVMe CM6 RI 1.92TB"),
            }

            "rejected" {
                "" => None,
                " " => None,
                " KCD8DPUG15T3" => None,
                "KCD8DPUG15T3 " => None,
            }
        );
    }

    /// Component types are map keys in firmware configuration (TOML) and in
    /// stored `Versions` maps (JSON).
    #[test]
    fn drive_component_types_round_trip_as_map_keys() {
        let versions = HashMap::from([
            (FirmwareComponentType::Bmc, "1.0".to_string()),
            (drive("Dell Ent NVMe CM6 RI 1.92TB"), "2.2.1".to_string()),
        ]);
        let json = serde_json::to_string(&versions).unwrap();
        assert_eq!(
            serde_json::from_str::<HashMap<FirmwareComponentType, String>>(&json).unwrap(),
            versions
        );

        let config: HashMap<FirmwareComponentType, String> = toml::from_str(
            r#"
bmc = "a"
drive-KCD8DPUG15T3 = "b"
"drive-Dell Ent NVMe CM6 RI 1.92TB" = "c"
"#,
        )
        .unwrap();
        let mut keys = config.into_keys().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                FirmwareComponentType::Bmc,
                drive("Dell Ent NVMe CM6 RI 1.92TB"),
                drive("KCD8DPUG15T3"),
            ]
        );
    }

    #[test]
    fn artifact_count_prefers_files_then_legacy_fields() {
        let files_count = FirmwareEntry {
            files: vec![
                FirmwareFileArtifact {
                    filename: Some("first.bin".to_string()),
                    url: None,
                    sha256: "abc123".to_string(),
                },
                FirmwareFileArtifact {
                    filename: Some("second.bin".to_string()),
                    url: None,
                    sha256: "def456".to_string(),
                },
            ],
            filenames: vec!["legacy.bin".to_string()],
            filename: Some("single-legacy.bin".to_string()),
            ..FirmwareEntry::default()
        };
        assert_eq!(files_count.artifact_count(), 2);

        let filenames_count = FirmwareEntry {
            filenames: vec!["first.bin".to_string(), "second.bin".to_string()],
            filename: Some("single-legacy.bin".to_string()),
            ..FirmwareEntry::default()
        };
        assert_eq!(filenames_count.artifact_count(), 2);

        let filename_count = FirmwareEntry {
            filename: Some("single-legacy.bin".to_string()),
            ..FirmwareEntry::default()
        };
        assert_eq!(filename_count.artifact_count(), 1);

        let url_count = FirmwareEntry {
            url: Some("https://firmware.example.invalid/fw.bin".to_string()),
            ..FirmwareEntry::default()
        };
        assert_eq!(url_count.artifact_count(), 1);

        assert_eq!(FirmwareEntry::default().artifact_count(), 0);
    }

    #[test]
    fn firmware_file_artifact_deserializes_when_filename_or_url_is_set() {
        let cases = [
            (
                r#"
filename = "/opt/nico/firmware/fw.bin"
sha256 = "abc123"
"#,
                (Some("/opt/nico/firmware/fw.bin"), None),
            ),
            (
                r#"
url = "https://firmware.example.invalid/fw.bin"
sha256 = "def456"
"#,
                (None, Some("https://firmware.example.invalid/fw.bin")),
            ),
        ];

        for (input, (expected_filename, expected_url)) in cases {
            let artifact = toml::from_str::<FirmwareFileArtifact>(input).unwrap();

            assert_eq!(artifact.filename.as_deref(), expected_filename);
            assert_eq!(artifact.url.as_deref(), expected_url);
        }
    }

    #[test]
    fn firmware_file_artifact_deserialization_requires_filename_or_url() {
        let invalid_artifacts = [
            r#"
sha256 = "abc123"
"#,
            r#"
filename = ""
sha256 = "abc123"
"#,
            r#"
url = "  "
sha256 = "abc123"
"#,
            r#"
filename = " "
url = ""
sha256 = "abc123"
"#,
        ];

        for input in invalid_artifacts {
            let error = toml::from_str::<FirmwareFileArtifact>(input).unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("firmware files[] artifact must set filename or url")
            );
        }
    }

    #[test]
    fn firmware_deserialization_rejects_files_artifact_without_location() {
        let input = r#"
model = "DGXH100"
vendor = "Nvidia"

[components.cx7]
current_version_reported_as = "^CX7_[0-9]+$"

[[components.cx7.known_firmware]]
version = "28.47.2682"

[[components.cx7.known_firmware.files]]
sha256 = "abc123"
"#;

        let error = toml::from_str::<Firmware>(input).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("firmware files[] artifact must set filename or url")
        );
    }
}
