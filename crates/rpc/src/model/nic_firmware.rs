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

use model::ConfigValidationError;
use model::nic_firmware::{
    NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareHardware, NicFirmwareProfile,
    NicFirmwareProfileConfig, NicFirmwareProfileEntry, NicFirmwareResolution,
    NicFirmwareSelectionSource, NicFirmwareSiteDefault,
};

use crate::forge as rpc;

impl TryFrom<rpc::NicFirmwareProfileConfig> for NicFirmwareProfileConfig {
    type Error = ConfigValidationError;

    fn try_from(config: rpc::NicFirmwareProfileConfig) -> Result<Self, Self::Error> {
        let entries = config
            .entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let firmware = entry.firmware.ok_or_else(|| {
                    ConfigValidationError::InvalidValue(format!(
                        "entries[{index}].firmware is required",
                    ))
                })?;
                let image = entry.image.ok_or_else(|| {
                    ConfigValidationError::InvalidValue(format!(
                        "entries[{index}].image is required",
                    ))
                })?;
                let approach = match rpc::NicFirmwareApproach::try_from(entry.approach) {
                    Ok(rpc::NicFirmwareApproach::Scout) => NicFirmwareApproach::Scout,
                    Err(_) => {
                        return Err(ConfigValidationError::InvalidValue(format!(
                            "entries[{index}].approach is unsupported",
                        )));
                    }
                };
                Ok(NicFirmwareProfileEntry {
                    firmware: firmware.into(),
                    image: artifact_from_rpc(image, &format!("entries[{index}].image"))?,
                    device_config: entry
                        .device_config
                        .map(|config| {
                            artifact_from_rpc(config, &format!("entries[{index}].device_config"))
                        })
                        .transpose()?,
                    approach,
                })
            })
            .collect::<Result<_, ConfigValidationError>>()?;
        Self { entries }.validate_and_normalize()
    }
}

fn artifact_from_rpc(
    artifact: rpc::NicFirmwareArtifact,
    path: &str,
) -> Result<NicFirmwareArtifact, ConfigValidationError> {
    Ok(NicFirmwareArtifact {
        url: artifact.url.parse().map_err(|_| {
            ConfigValidationError::InvalidValue(format!("{path}.url must be absolute HTTP(S)"))
        })?,
        sha256: artifact.sha256,
    })
}

impl From<NicFirmwareArtifact> for rpc::NicFirmwareArtifact {
    fn from(artifact: NicFirmwareArtifact) -> Self {
        Self {
            url: artifact.url.into(),
            sha256: artifact.sha256,
        }
    }
}

impl From<NicFirmwareProfileConfig> for rpc::NicFirmwareProfileConfig {
    fn from(config: NicFirmwareProfileConfig) -> Self {
        Self {
            entries: config.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<NicFirmwareProfileEntry> for rpc::NicFirmwareProfileEntry {
    fn from(entry: NicFirmwareProfileEntry) -> Self {
        Self {
            firmware: Some(entry.firmware.into()),
            image: Some(entry.image.into()),
            device_config: entry.device_config.map(Into::into),
            approach: match entry.approach {
                NicFirmwareApproach::Scout => rpc::NicFirmwareApproach::Scout.into(),
            },
        }
    }
}

impl TryFrom<rpc::NicFirmwareHardware> for NicFirmwareHardware {
    type Error = ConfigValidationError;

    fn try_from(hardware: rpc::NicFirmwareHardware) -> Result<Self, Self::Error> {
        Self {
            part_number: hardware.part_number,
            psid: hardware.psid,
        }
        .validate()
    }
}

impl From<NicFirmwareHardware> for rpc::NicFirmwareHardware {
    fn from(hardware: NicFirmwareHardware) -> Self {
        Self {
            part_number: hardware.part_number,
            psid: hardware.psid,
        }
    }
}

impl From<NicFirmwareSiteDefault> for rpc::NicFirmwareSiteDefault {
    fn from(binding: NicFirmwareSiteDefault) -> Self {
        Self {
            hardware: Some(binding.hardware.into()),
            profile_id: binding.profile_id.to_string(),
            version: binding.version.to_string(),
        }
    }
}

impl From<NicFirmwareResolution> for rpc::NicFirmwareResolution {
    fn from(resolution: NicFirmwareResolution) -> Self {
        Self {
            source: match resolution.source {
                NicFirmwareSelectionSource::SiteDefault => {
                    rpc::NicFirmwareSelectionSource::SiteDefault
                }
                NicFirmwareSelectionSource::Allocation => {
                    rpc::NicFirmwareSelectionSource::Allocation
                }
                NicFirmwareSelectionSource::MachineOverride => {
                    rpc::NicFirmwareSelectionSource::MachineOverride
                }
                NicFirmwareSelectionSource::CardOverride => {
                    rpc::NicFirmwareSelectionSource::CardOverride
                }
            }
            .into(),
            profile_id: resolution.profile_id.to_string(),
            profile_version: resolution.profile_version.to_string(),
            entry: Some(resolution.entry.into()),
        }
    }
}

impl From<NicFirmwareProfile> for rpc::NicFirmwareProfile {
    fn from(profile: NicFirmwareProfile) -> Self {
        Self {
            id: profile.id.to_string(),
            config: Some(profile.config.into()),
            version: profile.version.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::protos::mlx_device::FirmwareSpec;

    fn config() -> rpc::NicFirmwareProfileConfig {
        rpc::NicFirmwareProfileConfig {
            entries: vec![rpc::NicFirmwareProfileEntry {
                firmware: Some(FirmwareSpec {
                    part_number: "PN1".into(),
                    psid: "PSID1".into(),
                    version: "older-exact-target".into(),
                }),
                image: Some(rpc::NicFirmwareArtifact {
                    url: "https://firmware.example/image.bin".into(),
                    sha256: "AB".repeat(32),
                }),
                device_config: Some(rpc::NicFirmwareArtifact {
                    url: "https://firmware.example/config.bin".into(),
                    sha256: "CD".repeat(32),
                }),
                approach: 0,
            }],
        }
    }

    #[test]
    fn profile_config_roundtrip_applies_domain_normalization() {
        let mut input = config();
        let model = NicFirmwareProfileConfig::try_from(input.clone()).unwrap();
        input.entries[0].image.as_mut().unwrap().sha256 = "ab".repeat(32);
        input.entries[0].device_config.as_mut().unwrap().sha256 = "cd".repeat(32);
        assert_eq!(rpc::NicFirmwareProfileConfig::from(model), input);
    }

    #[test]
    fn resolution_preserves_operator_override_sources() {
        let mut expected_entry = config().entries.remove(0);
        expected_entry.image.as_mut().unwrap().sha256 = "ab".repeat(32);
        expected_entry.device_config.as_mut().unwrap().sha256 = "cd".repeat(32);
        let entry = NicFirmwareProfileConfig::try_from(config())
            .unwrap()
            .entries
            .remove(0);

        check_values(
            [
                Check {
                    scenario: "machine override",
                    input: NicFirmwareSelectionSource::MachineOverride,
                    expect: rpc::NicFirmwareSelectionSource::MachineOverride as i32,
                },
                Check {
                    scenario: "card override",
                    input: NicFirmwareSelectionSource::CardOverride,
                    expect: rpc::NicFirmwareSelectionSource::CardOverride as i32,
                },
            ],
            |source| {
                let resolution = rpc::NicFirmwareResolution::from(NicFirmwareResolution {
                    source,
                    profile_id: "operator-override".parse().unwrap(),
                    profile_version: "V7-T123456".parse().unwrap(),
                    entry: entry.clone(),
                });
                assert_eq!(resolution.profile_id, "operator-override", "{source:?}");
                assert_eq!(resolution.profile_version, "V7-T123456", "{source:?}");
                assert_eq!(
                    resolution.entry.as_ref(),
                    Some(&expected_entry),
                    "{source:?}"
                );
                resolution.source
            },
        );
    }

    #[test]
    fn rejects_invalid_profile_messages() {
        check_values(
            [
                Check {
                    scenario: "missing specification",
                    input: {
                        let mut input = config();
                        input.entries[0].firmware = None;
                        input
                    },
                    expect: Some("entries[0].firmware is required".to_string()),
                },
                Check {
                    scenario: "missing image",
                    input: {
                        let mut input = config();
                        input.entries[0].image = None;
                        input
                    },
                    expect: Some("entries[0].image is required".to_string()),
                },
                Check {
                    scenario: "unsupported approach",
                    input: {
                        let mut input = config();
                        input.entries[0].approach = 99;
                        input
                    },
                    expect: Some("entries[0].approach is unsupported".to_string()),
                },
                Check {
                    scenario: "malformed image URL",
                    input: {
                        let mut input = config();
                        input.entries[0].image.as_mut().unwrap().url = "not an absolute URL".into();
                        input
                    },
                    expect: Some("entries[0].image.url must be absolute HTTP(S)".to_string()),
                },
                Check {
                    scenario: "malformed optional device config URL",
                    input: {
                        let mut input = config();
                        input.entries[0].device_config.as_mut().unwrap().url = String::new();
                        input
                    },
                    expect: Some(
                        "entries[0].device_config.url must be absolute HTTP(S)".to_string(),
                    ),
                },
            ],
            |input| {
                NicFirmwareProfileConfig::try_from(input)
                    .err()
                    .map(|error| match error {
                        ConfigValidationError::InvalidValue(message) => message,
                        error => panic!("unexpected validation error: {error}"),
                    })
            },
        );
    }
}
