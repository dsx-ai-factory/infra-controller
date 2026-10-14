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
    NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareCardSelection, NicFirmwareCardSelector,
    NicFirmwareHardware, NicFirmwareProfile, NicFirmwareProfileConfig, NicFirmwareProfileEntry,
    NicFirmwareResolution, NicFirmwareSelectionScope, NicFirmwareSiteSelection,
};

use crate::forge as rpc;

impl TryFrom<rpc::NicFirmwareCardSelector> for NicFirmwareCardSelector {
    type Error = ConfigValidationError;

    fn try_from(selector: rpc::NicFirmwareCardSelector) -> Result<Self, Self::Error> {
        use rpc::nic_firmware_card_selector::Identity;

        let selector = match selector.identity {
            Some(Identity::BaseMac(mac_address)) => {
                // `MacAddress` accepts signed octets and ignores extra split fields.
                // Reject those inputs before they become a typed selector.
                let bytes = mac_address.as_bytes();
                let valid_format = match bytes.len() {
                    12 => bytes.iter().all(u8::is_ascii_hexdigit),
                    17 => {
                        matches!(bytes[2], b':' | b'-')
                            && bytes.iter().enumerate().all(|(index, byte)| {
                                if index % 3 == 2 {
                                    *byte == bytes[2]
                                } else {
                                    byte.is_ascii_hexdigit()
                                }
                            })
                    }
                    _ => false,
                };
                if !valid_format {
                    return Err(ConfigValidationError::InvalidValue(
                        "card selector base MAC is malformed".into(),
                    ));
                }
                Self::Mac(mac_address.parse().map_err(|_| {
                    ConfigValidationError::InvalidValue(
                        "card selector base MAC is malformed".into(),
                    )
                })?)
            }
            Some(Identity::BaseGuid(guid)) => Self::Guid(guid),
            None => {
                return Err(ConfigValidationError::InvalidValue(
                    "card selector identity is required".into(),
                ));
            }
        };
        selector.validate()
    }
}

impl From<NicFirmwareCardSelector> for rpc::NicFirmwareCardSelector {
    fn from(selector: NicFirmwareCardSelector) -> Self {
        use rpc::nic_firmware_card_selector::Identity;

        Self {
            identity: Some(match selector {
                NicFirmwareCardSelector::Mac(mac_address) => {
                    Identity::BaseMac(mac_address.to_string())
                }
                NicFirmwareCardSelector::Guid(guid) => Identity::BaseGuid(guid),
            }),
        }
    }
}

impl TryFrom<rpc::NicFirmwareCardSelection> for NicFirmwareCardSelection {
    type Error = ConfigValidationError;

    fn try_from(card_selection: rpc::NicFirmwareCardSelection) -> Result<Self, Self::Error> {
        Ok(Self {
            selector: card_selection
                .selector
                .ok_or_else(|| {
                    ConfigValidationError::InvalidValue(
                        "card selection selector is required".into(),
                    )
                })?
                .try_into()?,
            profile_id: card_selection.profile_id.parse()?,
        })
    }
}

impl From<NicFirmwareCardSelection> for rpc::NicFirmwareCardSelection {
    fn from(card_selection: NicFirmwareCardSelection) -> Self {
        Self {
            selector: Some(card_selection.selector.into()),
            profile_id: card_selection.profile_id.to_string(),
        }
    }
}

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

impl From<NicFirmwareSiteSelection> for rpc::NicFirmwareSiteSelection {
    fn from(site_selection: NicFirmwareSiteSelection) -> Self {
        Self {
            hardware: Some(site_selection.hardware.into()),
            profile_id: site_selection.profile_id.to_string(),
            version: site_selection.version.to_string(),
        }
    }
}

impl From<NicFirmwareResolution> for rpc::NicFirmwareResolution {
    fn from(resolution: NicFirmwareResolution) -> Self {
        Self {
            scope: match resolution.scope {
                NicFirmwareSelectionScope::Site => rpc::NicFirmwareSelectionScope::Site,
                NicFirmwareSelectionScope::Allocation => rpc::NicFirmwareSelectionScope::Allocation,
                NicFirmwareSelectionScope::Machine => rpc::NicFirmwareSelectionScope::Machine,
                NicFirmwareSelectionScope::Card => rpc::NicFirmwareSelectionScope::Card,
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
    use prost::Message;

    use super::*;
    use crate::protos::mlx_device::FirmwareSpec;

    /// MAC spelling follows the typed address formatter, while GUID text and the
    /// profile name must survive conversion unchanged for literal matching.
    #[test]
    fn card_selection_roundtrip_preserves_identity_and_profile() {
        use rpc::nic_firmware_card_selector::Identity;

        check_values(
            [
                Check {
                    scenario: "MAC uses typed address formatting",
                    input: Identity::BaseMac("02abcdef0123".into()),
                    expect: Identity::BaseMac("02:AB:CD:EF:01:23".into()),
                },
                Check {
                    scenario: "hyphen separators use typed address formatting",
                    input: Identity::BaseMac("02-ab-cd-ef-01-23".into()),
                    expect: Identity::BaseMac("02:AB:CD:EF:01:23".into()),
                },
                Check {
                    scenario: "GUID remains opaque and case sensitive",
                    input: Identity::BaseGuid("opaque-Base_Guid".into()),
                    expect: Identity::BaseGuid("opaque-Base_Guid".into()),
                },
            ],
            |identity| {
                let input = rpc::NicFirmwareCardSelection {
                    selector: Some(rpc::NicFirmwareCardSelector {
                        identity: Some(identity),
                    }),
                    profile_id: "Card-Profile".into(),
                };
                let model = NicFirmwareCardSelection::try_from(input).unwrap();
                let output = rpc::NicFirmwareCardSelection::from(model);
                assert_eq!(output.profile_id, "Card-Profile");
                output.selector.unwrap().identity.unwrap()
            },
        );
    }

    /// Conversion must enforce wire presence and domain validation before a
    /// transaction can store a partial or malformed card selection.
    #[test]
    fn card_selection_conversion_rejects_missing_and_invalid_fields() {
        use rpc::nic_firmware_card_selector::Identity;

        check_values(
            [
                Check {
                    scenario: "missing selector message",
                    input: (None, "profile"),
                    expect: "card selection selector is required".to_string(),
                },
                Check {
                    scenario: "missing selector identity",
                    input: (
                        Some(rpc::NicFirmwareCardSelector { identity: None }),
                        "profile",
                    ),
                    expect: "card selector identity is required".to_string(),
                },
                Check {
                    scenario: "malformed MAC",
                    input: (
                        Some(rpc::NicFirmwareCardSelector {
                            identity: Some(Identity::BaseMac("not-a-mac".into())),
                        }),
                        "profile",
                    ),
                    expect: "card selector base MAC is malformed".to_string(),
                },
                Check {
                    scenario: "extra MAC octet cannot be truncated",
                    input: (
                        Some(rpc::NicFirmwareCardSelector {
                            identity: Some(Identity::BaseMac("02:ab:cd:0:1:2:ff".into())),
                        }),
                        "profile",
                    ),
                    expect: "card selector base MAC is malformed".to_string(),
                },
                Check {
                    scenario: "bare MAC octet cannot have a plus prefix",
                    input: (
                        Some(rpc::NicFirmwareCardSelector {
                            identity: Some(Identity::BaseMac("+2abcdef0123".into())),
                        }),
                        "profile",
                    ),
                    expect: "card selector base MAC is malformed".to_string(),
                },
                Check {
                    scenario: "separated MAC octet cannot have a plus prefix",
                    input: (
                        Some(rpc::NicFirmwareCardSelector {
                            identity: Some(Identity::BaseMac("+2:ab:cd:01:02:03".into())),
                        }),
                        "profile",
                    ),
                    expect: "card selector base MAC is malformed".to_string(),
                },
                Check {
                    scenario: "MAC separators must be consistent",
                    input: (
                        Some(rpc::NicFirmwareCardSelector {
                            identity: Some(Identity::BaseMac("02-ab:cd-01-02-03".into())),
                        }),
                        "profile",
                    ),
                    expect: "card selector base MAC is malformed".to_string(),
                },
                Check {
                    scenario: "selector domain validation",
                    input: (
                        Some(rpc::NicFirmwareCardSelector {
                            identity: Some(Identity::BaseMac("00:00:00:00:00:00".into())),
                        }),
                        "profile",
                    ),
                    expect: "card selector base MAC must be nonzero and unicast".to_string(),
                },
                Check {
                    scenario: "GUID domain validation",
                    input: (
                        Some(rpc::NicFirmwareCardSelector {
                            identity: Some(Identity::BaseGuid("--".into())),
                        }),
                        "profile",
                    ),
                    expect: concat!(
                        "card selector base GUID must contain 1-256 UTF-8 bytes ",
                        "without surrounding whitespace or control characters, ",
                        "and cannot be -- or N/A",
                    )
                    .to_string(),
                },
                Check {
                    scenario: "profile domain validation",
                    input: (
                        Some(rpc::NicFirmwareCardSelector {
                            identity: Some(Identity::BaseGuid("opaque-Base_Guid".into())),
                        }),
                        "",
                    ),
                    expect: concat!(
                        "profile ID must be nonempty without surrounding ",
                        "whitespace or control characters",
                    )
                    .to_string(),
                },
            ],
            |(selector, profile_id)| match NicFirmwareCardSelection::try_from(
                rpc::NicFirmwareCardSelection {
                    selector,
                    profile_id: profile_id.into(),
                },
            )
            .unwrap_err()
            {
                ConfigValidationError::InvalidValue(message) => message,
                error => panic!("unexpected validation error: {error}"),
            },
        );
    }

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

    /// Public scope names and numbers must identify the selected hierarchy level,
    /// while conversion preserves the exact normalized target and profile revision.
    #[test]
    fn resolution_preserves_selection_scopes() {
        // Profile conversion normalizes digests without changing target versions.
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
                    scenario: "site selection",
                    input: NicFirmwareSelectionScope::Site,
                    expect: (0, "NicFirmwareSelectionScopeSite"),
                },
                Check {
                    scenario: "allocation selection",
                    input: NicFirmwareSelectionScope::Allocation,
                    expect: (1, "NicFirmwareSelectionScopeAllocation"),
                },
                Check {
                    scenario: "machine selection",
                    input: NicFirmwareSelectionScope::Machine,
                    expect: (2, "NicFirmwareSelectionScopeMachine"),
                },
                Check {
                    scenario: "card selection",
                    input: NicFirmwareSelectionScope::Card,
                    expect: (3, "NicFirmwareSelectionScopeCard"),
                },
            ],
            |scope| {
                let resolution = rpc::NicFirmwareResolution::from(NicFirmwareResolution {
                    scope,
                    profile_id: "operator-selection".parse().unwrap(),
                    profile_version: "V7-T123456".parse().unwrap(),
                    entry: entry.clone(),
                });
                assert_eq!(resolution.profile_id, "operator-selection", "{scope:?}");
                assert_eq!(resolution.profile_version, "V7-T123456", "{scope:?}");
                assert_eq!(
                    resolution.entry.as_ref(),
                    Some(&expected_entry),
                    "{scope:?}"
                );
                // Isolate the scope so exact bytes prove tag 5 replaces retired
                // tag 1. Proto3 omits the default site value from the wire.
                let scope_only = rpc::NicFirmwareResolution {
                    scope: resolution.scope,
                    ..Default::default()
                };
                let encoded = scope_only.encode_to_vec();
                let expected_bytes = if scope == NicFirmwareSelectionScope::Site {
                    vec![]
                } else {
                    vec![0x28, u8::try_from(resolution.scope).unwrap()]
                };
                assert_eq!(encoded, expected_bytes, "{scope:?}");
                assert_eq!(
                    rpc::NicFirmwareResolution::decode(encoded.as_slice()).unwrap(),
                    scope_only,
                    "{scope:?}"
                );
                (
                    resolution.scope,
                    rpc::NicFirmwareSelectionScope::try_from(resolution.scope)
                        .unwrap()
                        .as_str_name(),
                )
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
