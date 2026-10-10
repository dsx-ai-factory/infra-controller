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

use carbide_libmlx_model::firmware::FirmwareSpec;
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ConfigValidationError;

#[cfg(test)]
mod tests;

/// `NicFirmwareProfileId` is an immutable, case-sensitive catalog name.
/// Parsing rejects empty names, surrounding whitespace and control characters.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::Type)]
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

/// `NicFirmwareProfile` stores one definition and its optimistic edit version.
#[derive(Clone, Debug, sqlx::FromRow)]
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

/// `NicFirmwareSiteDefault` selects a live profile for one hardware pair.
/// Its version tracks assignment edits, independently of profile edits.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct NicFirmwareSiteDefault {
    /// Literal part-number/PSID pair identifying this assignment.
    #[sqlx(flatten)]
    pub hardware: NicFirmwareHardware,
    /// Live profile selected for this hardware, not a pinned revision.
    pub profile_id: NicFirmwareProfileId,
    /// Assignment edit token required to replace or clear this default.
    pub version: ConfigVersion,
}

/// `NicFirmwareBaseline` groups operator selections in order of precedence.
/// These are resolver inputs, not persisted machine or card overrides.
#[derive(Default)]
pub struct NicFirmwareBaseline<'a> {
    /// Operator override for this card, taking precedence over machine settings.
    pub card_override: Option<&'a NicFirmwareProfileId>,
    /// Operator override for this machine when no card override is selected.
    pub machine_override: Option<&'a NicFirmwareProfileId>,
    /// Hardware default used when neither an override nor an allocation selects a profile.
    pub site_default: Option<&'a NicFirmwareProfileId>,
}

/// `NicFirmwareSelectionSource` tells operators which setting selected a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NicFirmwareSelectionSource {
    /// The operator selected a profile for this card within its machine.
    CardOverride,
    /// The operator selected a machine profile, without a card override.
    MachineOverride,
    /// The allocation selected a profile, without an operator override.
    Allocation,
    /// The hardware default applied, without an override or allocation selection.
    SiteDefault,
}

/// `NicFirmwareResolution` preserves the selected profile revision and exact entry.
/// It is a read-only decision, not an admitted operation or permission to flash.
#[derive(Clone, Debug)]
pub struct NicFirmwareResolution {
    /// Setting that selected this profile under the resolver's precedence rules.
    pub source: NicFirmwareSelectionSource,
    /// Catalog identity of the selected profile.
    pub profile_id: NicFirmwareProfileId,
    /// Profile revision read for this resolution, not the assignment edit token.
    pub profile_version: ConfigVersion,
    /// Exact firmware target and artifacts matching the requested hardware pair.
    pub entry: NicFirmwareProfileEntry,
}

/// `resolve_nic_firmware` applies card > machine > allocation > site precedence.
/// No selection returns `None`. An invalid selected profile returns an error
/// without falling back. An operator pin conflicting with an allocation's
/// exact version or artifact digests also fails; source URLs do not change
/// firmware identity. Firmware versions are exact, with no ordering.
pub fn resolve_nic_firmware(
    hardware: &NicFirmwareHardware,
    profiles: &[NicFirmwareProfile],
    baseline: NicFirmwareBaseline<'_>,
    allocation: Option<&NicFirmwareProfileId>,
) -> Result<Option<NicFirmwareResolution>, ConfigValidationError> {
    use NicFirmwareSelectionSource::*;

    let selected = baseline
        .card_override
        .map(|id| (CardOverride, id))
        .or_else(|| baseline.machine_override.map(|id| (MachineOverride, id)))
        .or_else(|| allocation.map(|id| (Allocation, id)))
        .or_else(|| baseline.site_default.map(|id| (SiteDefault, id)));
    let Some((source, id)) = selected else {
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
    if matches!(source, CardOverride | MachineOverride)
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
                "operator NIC firmware override conflicts with the allocation requirement".into(),
            ));
        }
    }
    Ok(Some(NicFirmwareResolution {
        source,
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
