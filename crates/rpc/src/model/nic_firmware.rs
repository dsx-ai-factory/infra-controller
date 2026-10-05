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
    NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareProfile, NicFirmwareProfileConfig,
    NicFirmwareProfileEntry,
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
            entries: config
                .entries
                .into_iter()
                .map(|entry| rpc::NicFirmwareProfileEntry {
                    firmware: Some(entry.firmware.into()),
                    image: Some(entry.image.into()),
                    device_config: entry.device_config.map(Into::into),
                    approach: match entry.approach {
                        NicFirmwareApproach::Scout => rpc::NicFirmwareApproach::Scout.into(),
                    },
                })
                .collect(),
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
