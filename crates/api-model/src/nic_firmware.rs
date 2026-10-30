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

//! Operator-owned NIC firmware definitions and read-only target selection.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use carbide_libmlx_model::device::identity::MlxDeviceSelectorError;
use carbide_libmlx_model::firmware::FirmwareSpec;
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ConfigValidationError;

#[cfg(test)]
mod tests;

/// `NicFirmwareProfileId` is an immutable, case-sensitive catalog name.
/// Serialized as a string. Parsing and deserialization reject empty names,
/// surrounding whitespace and control characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct NicFirmwareProfileId(String);

impl fmt::Display for NicFirmwareProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for NicFirmwareProfileId {
    type Err = ConfigValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
            return Err(ConfigValidationError::InvalidValue(
                "profile ID must be nonempty without surrounding whitespace or control characters"
                    .into(),
            ));
        }
        Ok(Self(value.to_string()))
    }
}

impl<'de> Deserialize<'de> for NicFirmwareProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// `NicFirmwareProfile` stores one definition and its optimistic edit version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct NicFirmwareProfile {
    /// Immutable operator-chosen name identifying this catalog definition.
    pub id: NicFirmwareProfileId,
    /// Validated firmware targets and artifact sources stored for this profile.
    #[sqlx(json)]
    pub config: NicFirmwareProfileConfig,
    /// Optimistic concurrency token required to replace or delete this definition.
    pub version: ConfigVersion,
}

/// `NicFirmwareProfileConfig` supplies one exact target per compatible PN/PSID pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicFirmwareProfileConfig {
    /// Nonempty, ordered entries with unique part-number/PSID pairs.
    pub entries: Vec<NicFirmwareProfileEntry>,
}

/// `NicFirmwareProfileEntry` defines the exact firmware target for one hardware pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicFirmwareProfileEntry {
    /// Literal part number, PSID and target version, without surrounding whitespace
    /// or control characters; an older firmware version is a valid target.
    pub firmware: FirmwareSpec,
    /// Required firmware image and its expected content digest.
    pub image: NicFirmwareArtifact,
    /// Optional device configuration required by this image, with its own digest.
    pub device_config: Option<NicFirmwareArtifact>,
    /// Requested update mechanism, independent of platform qualification.
    pub approach: NicFirmwareApproach,
}

/// `NicFirmwareApproach` identifies the requested firmware update mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwareApproach {
    /// Requests a Scout-managed firmware update.
    Scout,
}

/// `NicFirmwareArtifact` stores a canonical source URL and expected content digest.
/// Sources must be accessible without login; registration does not fetch bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicFirmwareArtifact {
    /// Parsed source URL; validation requires HTTP(S) without userinfo,
    /// query parameters or a fragment.
    pub url: Url,
    /// Required SHA-256 digest as 64 hexadecimal digits, lowercased during validation.
    pub sha256: String,
}

impl fmt::Debug for NicFirmwareArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Source paths can contain secrets even without URL userinfo or queries.
        f.debug_struct("NicFirmwareArtifact")
            .field("url", &"[REDACTED]")
            .field("sha256", &self.sha256)
            .finish()
    }
}

impl NicFirmwareProfileConfig {
    /// `entry_for` matches literal hardware identifiers, not device family names.
    /// A match selects firmware; it does not qualify the card for flashing.
    pub fn entry_for(&self, hardware: &NicFirmwareHardware) -> Option<&NicFirmwareProfileEntry> {
        self.entries.iter().find(|entry| {
            entry.firmware.part_number == hardware.part_number
                && entry.firmware.psid == hardware.psid
        })
    }

    /// `validate_and_normalize` checks the hardware entries and artifact sources,
    /// and lowercases SHA-256 digests. Entry order and valid
    /// exact targets are preserved without trimming or version ordering.
    /// Invalid definitions return `ConfigValidationError` without fetching sources.
    pub fn validate_and_normalize(mut self) -> Result<Self, ConfigValidationError> {
        if self.entries.is_empty() {
            return Err(ConfigValidationError::InvalidValue(
                "profile must contain at least one firmware entry".into(),
            ));
        }
        let mut hardware = HashSet::new();
        for (index, entry) in self.entries.iter_mut().enumerate() {
            let firmware = &entry.firmware;
            if [&firmware.part_number, &firmware.psid, &firmware.version]
                .iter()
                .any(|value| {
                    value.is_empty()
                        || value.trim() != value.as_str()
                        || value.chars().any(char::is_control)
                })
            {
                return Err(ConfigValidationError::InvalidValue(format!(
                    "entries[{index}].firmware: part number, PSID and exact version must be nonempty without surrounding whitespace or control characters",
                )));
            }
            if !hardware.insert((firmware.part_number.clone(), firmware.psid.clone())) {
                return Err(ConfigValidationError::InvalidValue(format!(
                    "entries[{index}]: duplicate part number/PSID pair",
                )));
            }
            entry
                .image
                .validate_and_normalize(&format!("entries[{index}].image"))?;
            if let Some(device_config) = &mut entry.device_config {
                device_config.validate_and_normalize(&format!("entries[{index}].device_config"))?;
            }
        }
        Ok(self)
    }
}

/// `NicFirmwareHardware` identifies the exact profile entry a device needs.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct NicFirmwareHardware {
    /// Manufacturer part number, compared literally.
    pub part_number: String,
    /// Firmware parameter-set identifier, compared literally.
    pub psid: String,
}

impl NicFirmwareHardware {
    /// `validate` accepts identifiers up to 256 UTF-8 bytes each, without
    /// surrounding whitespace or controls. The bound keeps hardware selectors
    /// usable as an indexed assignment key rather than failing during a write.
    pub fn validate(self) -> Result<Self, ConfigValidationError> {
        if [&self.part_number, &self.psid].iter().any(|value| {
            value.is_empty()
                || value.len() > 256
                || value.trim() != value.as_str()
                || value.chars().any(char::is_control)
        }) {
            return Err(ConfigValidationError::InvalidValue(
                "part number and PSID must each contain 1-256 UTF-8 bytes without surrounding whitespace or control characters".into(),
            ));
        }
        Ok(self)
    }
}

/// `NicFirmwareSiteSelection` selects a live profile for one hardware pair.
/// Its version tracks assignment edits, independently of profile edits.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct NicFirmwareSiteSelection {
    /// Literal part-number/PSID pair identifying this assignment.
    #[sqlx(flatten)]
    pub hardware: NicFirmwareHardware,
    /// Live profile selected for this hardware, not a pinned revision.
    pub profile_id: NicFirmwareProfileId,
    /// Assignment edit token required to replace or clear this selection.
    pub version: ConfigVersion,
}

/// `NicFirmwareCardSelector` identifies a device in a stored MLX observation.
/// Matching stored inventory does not establish live card identity, a reset
/// boundary, or flash eligibility. The shared type preserves the stored JSON.
pub use carbide_libmlx_model::device::identity::MlxDeviceSelector as NicFirmwareCardSelector;

impl From<MlxDeviceSelectorError> for ConfigValidationError {
    fn from(error: MlxDeviceSelectorError) -> Self {
        Self::InvalidValue(error.to_string())
    }
}

/// `NicFirmwareCardSelection` selects a live profile for one observed device.
/// Its edits share the owning machine's configuration version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicFirmwareCardSelection {
    /// Explicit identity used to match one device within the machine's report.
    pub selector: NicFirmwareCardSelector,
    /// Live catalog profile, not a pinned profile revision.
    pub profile_id: NicFirmwareProfileId,
}

/// `NicFirmwareSelections` groups stored card, machine and site profile selections.
/// Callers supply the selected profile IDs; the resolver does not persist them.
/// Allocation is separate because it also constrains card and machine selections.
#[derive(Default)]
pub struct NicFirmwareSelections<'a> {
    /// Profile selected for this card, taking precedence over machine settings.
    pub card: Option<&'a NicFirmwareProfileId>,
    /// Profile selected for this machine when no card selection applies.
    pub machine: Option<&'a NicFirmwareProfileId>,
    /// Site profile used when no card, machine or allocation selection applies.
    pub site: Option<&'a NicFirmwareProfileId>,
}

/// `NicFirmwareSelectionScope` identifies the hierarchy level that selected a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NicFirmwareSelectionScope {
    /// The operator selected a profile for this card within its machine.
    Card,
    /// The operator selected a machine profile, without a card selection.
    Machine,
    /// The allocation selected a profile, without a card or machine selection.
    Allocation,
    /// The site's hardware selection applied, without a higher-level selection.
    Site,
}

/// `NicFirmwareResolution` preserves the selected profile revision and exact entry.
/// It is a read-only decision, not an admitted operation or permission to flash.
#[derive(Clone, Debug)]
pub struct NicFirmwareResolution {
    /// Hierarchy level that selected this profile under the resolver's precedence rules.
    pub scope: NicFirmwareSelectionScope,
    /// Catalog identity of the selected profile.
    pub profile_id: NicFirmwareProfileId,
    /// Profile revision read for this resolution, not the assignment edit token.
    pub profile_version: ConfigVersion,
    /// Exact firmware target and artifacts matching the requested hardware pair.
    pub entry: NicFirmwareProfileEntry,
}

/// `resolve_nic_firmware` applies card > machine > allocation > site precedence.
/// No selection returns `None`. An invalid selected profile returns an error
/// without falling back. An operator selection conflicting with an allocation's
/// exact version or artifact digests also fails; source URLs do not change
/// firmware identity. Firmware versions are exact, with no ordering.
pub fn resolve_nic_firmware(
    hardware: &NicFirmwareHardware,
    profiles: &[NicFirmwareProfile],
    selections: NicFirmwareSelections<'_>,
    allocation: Option<&NicFirmwareProfileId>,
) -> Result<Option<NicFirmwareResolution>, ConfigValidationError> {
    use NicFirmwareSelectionScope::*;

    let selected = selections
        .card
        .map(|id| (Card, id))
        .or_else(|| selections.machine.map(|id| (Machine, id)))
        .or_else(|| allocation.map(|id| (Allocation, id)))
        .or_else(|| selections.site.map(|id| (Site, id)));
    let Some((scope, id)) = selected else {
        return Ok(None);
    };
    let entry_for = |id: &NicFirmwareProfileId| {
        let profile = profiles
            .iter()
            .find(|profile| &profile.id == id)
            .ok_or_else(|| {
                ConfigValidationError::InvalidValue(format!("NIC firmware profile {id} is missing"))
            })?;
        let entry = profile.config.entry_for(hardware).ok_or_else(|| {
            ConfigValidationError::InvalidValue(format!(
                "NIC firmware profile {id} does not support this part number/PSID"
            ))
        })?;
        Ok::<_, ConfigValidationError>((profile, entry))
    };
    let (profile, entry) = entry_for(id)?;
    if matches!(scope, Card | Machine)
        && let Some(required_id) = allocation
    {
        let required = entry_for(required_id)?.1;
        if entry.firmware != required.firmware
            || entry.image.sha256 != required.image.sha256
            || entry
                .device_config
                .as_ref()
                .map(|artifact| &artifact.sha256)
                != required
                    .device_config
                    .as_ref()
                    .map(|artifact| &artifact.sha256)
        {
            return Err(ConfigValidationError::InvalidValue(
                "operator NIC firmware selection conflicts with the allocation requirement".into(),
            ));
        }
    }
    Ok(Some(NicFirmwareResolution {
        scope,
        profile_id: profile.id.clone(),
        profile_version: profile.version,
        entry: entry.clone(),
    }))
}

impl NicFirmwareArtifact {
    fn validate_and_normalize(&mut self, path: &str) -> Result<(), ConfigValidationError> {
        let url = &self.url;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ConfigValidationError::InvalidValue(format!(
                "{path}.url must be HTTP(S) without userinfo, query or fragment",
            )));
        }
        let mut digest = [0; 32];
        hex::decode_to_slice(&self.sha256, &mut digest).map_err(|_| {
            ConfigValidationError::InvalidValue(format!(
                "{path}.sha256 must be 64 hexadecimal digits",
            ))
        })?;
        self.sha256 = hex::encode(digest);
        Ok(())
    }
}
