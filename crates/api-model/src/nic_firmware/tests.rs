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

use carbide_test_support::Outcome::Fails;
use carbide_test_support::{Case, check_cases, value_scenarios};

use super::*;

fn artifact() -> NicFirmwareArtifact {
    NicFirmwareArtifact {
        url: "https://firmware.example/image.bin".parse().unwrap(),
        sha256: "AB".repeat(32),
    }
}

fn config() -> NicFirmwareProfileConfig {
    NicFirmwareProfileConfig {
        entries: vec![NicFirmwareProfileEntry {
            firmware: FirmwareSpec {
                part_number: "PN1".into(),
                psid: "PSID1".into(),
                version: "older-exact-target".into(),
            },
            image: artifact(),
            device_config: None,
            approach: NicFirmwareApproach::Scout,
        }],
    }
}

#[test]
fn hardware_selectors_validate_indexed_identifiers() {
    let mut unselectable = config();
    unselectable.entries[0].firmware.part_number = "P".repeat(257);
    assert!(unselectable.validate_and_normalize().is_ok());
    value_scenarios!(
        run = |(part_number, psid): (String, String)| NicFirmwareHardware { part_number, psid }.validate().is_ok();
        "maximum UTF-8 byte length" { ("é".repeat(128), "P".repeat(256)) => true, }
        "oversized part number" { ("P".repeat(257), "PSID1".into()) => false, }
        "oversized PSID" { ("PN1".into(), "é".repeat(129)) => false, }
        "missing identifier" { ("PN1".into(), String::new()) => false, }
        "not silently trimmed" { (" PN1".into(), "PSID1".into()) => false, }
        "control character" { ("PN1".into(), "PSID\n1".into()) => false, }
    );
}

#[test]
fn resolves_exact_targets_and_selection_precedence() {
    use NicFirmwareSelectionSource::*;
    let mut profiles: Vec<_> = [("site", 1), ("allocation", 2), ("machine", 3), ("card", 4)]
        .into_iter()
        .map(|(id, version)| NicFirmwareProfile {
            id: id.parse().unwrap(),
            config: config().validate_and_normalize().unwrap(),
            version: ConfigVersion::new(version),
        })
        .collect();
    profiles[1].config.entries[0].firmware.version = "allocation-target".into();
    profiles[2].config.entries[0].firmware.version = "machine-target".into();
    profiles[3].config.entries[0].firmware.version = "card-target".into();
    profiles[3].config.entries[0].device_config = Some(NicFirmwareArtifact {
        url: "https://firmware.example/config.bin".parse().unwrap(),
        sha256: "cd".repeat(32),
    });
    let mut mirror = profiles[3].clone();
    mirror.id = "mirror".parse().unwrap();
    mirror.version = ConfigVersion::new(5);
    mirror.config.entries[0].image.url = "https://mirror.example/image.bin".parse().unwrap();
    mirror.config.entries[0].device_config.as_mut().unwrap().url =
        "https://mirror.example/config.bin".parse().unwrap();
    profiles.push(mirror);
    let mut incompatible = profiles[0].clone();
    incompatible.id = "incompatible".parse().unwrap();
    incompatible.version = ConfigVersion::new(6);
    incompatible.config.entries[0].firmware.psid = "PSID2".into();
    profiles.push(incompatible);
    let hardware = NicFirmwareHardware {
        part_number: "PN1".into(),
        psid: "PSID1".into(),
    };
    let missing = "missing".parse().unwrap();
    let cases = [
        ("unmanaged", NicFirmwareBaseline::default(), None, Ok(None)),
        (
            "site exact target",
            NicFirmwareBaseline {
                site_default: Some(&profiles[0].id),
                ..Default::default()
            },
            None,
            Ok(Some((SiteDefault, &profiles[0]))),
        ),
        (
            "allocation overrides site",
            NicFirmwareBaseline {
                site_default: Some(&profiles[0].id),
                ..Default::default()
            },
            Some(&profiles[1].id),
            Ok(Some((Allocation, &profiles[1]))),
        ),
        (
            "machine overrides site",
            NicFirmwareBaseline {
                machine_override: Some(&profiles[2].id),
                site_default: Some(&profiles[0].id),
                ..Default::default()
            },
            None,
            Ok(Some((MachineOverride, &profiles[2]))),
        ),
        (
            "card overrides machine",
            NicFirmwareBaseline {
                card_override: Some(&profiles[3].id),
                machine_override: Some(&profiles[2].id),
                ..Default::default()
            },
            None,
            Ok(Some((CardOverride, &profiles[3]))),
        ),
        (
            "same firmware from different sources",
            NicFirmwareBaseline {
                card_override: Some(&profiles[3].id),
                ..Default::default()
            },
            Some(&profiles[4].id),
            Ok(Some((CardOverride, &profiles[3]))),
        ),
        (
            "conflicting allocation and pin",
            NicFirmwareBaseline {
                machine_override: Some(&profiles[2].id),
                ..Default::default()
            },
            Some(&profiles[1].id),
            Err("operator NIC firmware override conflicts with the allocation requirement"),
        ),
        (
            "missing selection cannot fall back",
            NicFirmwareBaseline {
                site_default: Some(&profiles[0].id),
                ..Default::default()
            },
            Some(&missing),
            Err("NIC firmware profile missing is missing"),
        ),
        (
            "incompatible allocation cannot fall back",
            NicFirmwareBaseline {
                site_default: Some(&profiles[0].id),
                ..Default::default()
            },
            Some(&profiles[5].id),
            Err("NIC firmware profile incompatible does not support this part number/PSID"),
        ),
        (
            "valid override still requires the allocation profile",
            NicFirmwareBaseline {
                card_override: Some(&profiles[3].id),
                ..Default::default()
            },
            Some(&missing),
            Err("NIC firmware profile missing is missing"),
        ),
    ];
    for (scenario, baseline, allocation, expected) in cases {
        let actual = resolve_nic_firmware(&hardware, &profiles, baseline, allocation);
        match expected {
            Ok(Some((source, profile))) => {
                let resolved = actual.unwrap().unwrap();
                assert_eq!(resolved.source, source, "{scenario}");
                assert_eq!(resolved.profile_id, profile.id, "{scenario}");
                assert_eq!(resolved.profile_version, profile.version, "{scenario}");
                assert_eq!(&resolved.entry, &profile.config.entries[0], "{scenario}");
            }
            Ok(None) => assert!(actual.unwrap().is_none(), "{scenario}"),
            Err(message) => assert_eq!(
                actual.unwrap_err().to_string(),
                format!("invalid value: {message}"),
                "{scenario}"
            ),
        }
    }
    for (part_number, psid) in [("pn1", "PSID1"), ("PN1", "psid1")] {
        let incompatible = NicFirmwareHardware {
            part_number: part_number.into(),
            psid: psid.into(),
        };
        assert!(
            resolve_nic_firmware(
                &incompatible,
                &profiles,
                NicFirmwareBaseline {
                    site_default: Some(&profiles[0].id),
                    ..Default::default()
                },
                None
            )
            .unwrap_err()
            .to_string()
            .contains("does not support")
        );
    }
    let device_config = profiles[4].config.entries[0]
        .device_config
        .as_ref()
        .unwrap();
    let different_device_config = NicFirmwareArtifact {
        sha256: "ef".repeat(32),
        ..device_config.clone()
    };
    for (scenario, image_digest, [override_config, required_config]) in [
        (
            "different image",
            "cd",
            [Some(device_config), Some(device_config)],
        ),
        (
            "required device configuration",
            "ab",
            [None, Some(device_config)],
        ),
        (
            "different device configuration",
            "ab",
            [Some(device_config), Some(&different_device_config)],
        ),
    ] {
        let mut conflicting = profiles.clone();
        conflicting[3].config.entries[0].device_config = override_config.cloned();
        conflicting[4].config.entries[0].image.sha256 = image_digest.repeat(32);
        conflicting[4].config.entries[0].device_config = required_config.cloned();
        assert!(
            resolve_nic_firmware(
                &hardware,
                &conflicting,
                NicFirmwareBaseline {
                    card_override: Some(&conflicting[3].id),
                    ..Default::default()
                },
                Some(&conflicting[4].id)
            )
            .unwrap_err()
            .to_string()
            .contains("conflicts"),
            "{scenario}"
        );
    }
}

#[test]
fn entry_match_requires_both_literal_identifiers_in_one_entry() {
    let mut input = config();
    let mut second = input.entries[0].clone();
    input.entries[0].firmware.psid = "*".into();
    second.firmware.part_number = "*".into();
    input.entries.push(second);

    assert!(
        input
            .entry_for(&NicFirmwareHardware {
                part_number: "PN1".into(),
                psid: "PSID1".into(),
            })
            .is_none()
    );
}

#[test]
fn preserves_exact_targets_and_canonical_artifacts() {
    let mut input = config();
    input.entries[0].device_config = Some(NicFirmwareArtifact {
        url: "HTTPS://FIRMWARE.EXAMPLE/config.bin".parse().unwrap(),
        ..artifact()
    });
    let mut second = input.entries[0].clone();
    second.firmware.psid = "PSID2".into();
    let mut third = input.entries[0].clone();
    third.firmware.part_number = "PN2".into();
    input.entries.extend([second, third]);
    let validated = input.clone().validate_and_normalize().unwrap();
    let json = serde_json::to_string(&validated).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["entries"][0],
        serde_json::json!({
            "firmware": {
                "part_number": "PN1",
                "psid": "PSID1",
                "version": "older-exact-target",
            },
            "image": {
                "url": "https://firmware.example/image.bin",
                "sha256": "ab".repeat(32),
            },
            "device_config": {
                "url": "https://firmware.example/config.bin",
                "sha256": "ab".repeat(32),
            },
            "approach": "Scout",
        })
    );
    let restored: NicFirmwareProfileConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, validated);
    for entry in &mut input.entries {
        entry.image.sha256 = "ab".repeat(32);
        let device_config = entry.device_config.as_mut().unwrap();
        device_config.sha256 = "ab".repeat(32);
    }
    assert_eq!(restored, input);
}

#[test]
fn rejects_invalid_profile_definitions() {
    check_cases(
        [
            Case {
                scenario: "empty catalog entry list",
                input: NicFirmwareProfileConfig { entries: vec![] },
                expect: Fails,
            },
            Case {
                scenario: "duplicate hardware pair",
                input: {
                    let mut input = config();
                    input.entries.push(input.entries[0].clone());
                    input
                },
                expect: Fails,
            },
            Case {
                scenario: "empty exact target",
                input: {
                    let mut input = config();
                    input.entries[0].firmware.version = String::new();
                    input
                },
                expect: Fails,
            },
            Case {
                scenario: "leading whitespace in part number",
                input: {
                    let mut input = config();
                    input.entries[0].firmware.part_number = " PN1".into();
                    input
                },
                expect: Fails,
            },
            Case {
                scenario: "trailing whitespace in PSID",
                input: {
                    let mut input = config();
                    input.entries[0].firmware.psid = "PSID1 ".into();
                    input
                },
                expect: Fails,
            },
            Case {
                scenario: "embedded control character in exact version",
                input: {
                    let mut input = config();
                    input.entries[0].firmware.version = "32.\n43".into();
                    input
                },
                expect: Fails,
            },
            Case {
                scenario: "invalid optional device config",
                input: {
                    let mut input = config();
                    input.entries[0].device_config = Some(NicFirmwareArtifact {
                        url: "file:///firmware/config.bin".parse().unwrap(),
                        ..artifact()
                    });
                    input
                },
                expect: Fails,
            },
        ],
        |input| input.validate_and_normalize().map_err(drop),
    );
}

#[test]
fn canonicalizes_sources_without_echoing_urls() {
    value_scenarios!(
        run = |url: &str| {
            let mut input = artifact();
            input.url = url.parse().unwrap();
            match input.validate_and_normalize("entries[0].image") {
                Ok(()) => { assert!(!format!("{input:?}").contains(input.url.as_str())); Some(input.url.to_string()) }
                Err(error) => { assert!(!error.to_string().contains(url)); None }
            }
        };
        "supported sources" {
            "https://firmware.example/image.bin" => Some("https://firmware.example/image.bin".to_string()),
            "http://192.0.2.1/image.bin" => Some("http://192.0.2.1/image.bin".to_string()),
        }
        "canonical sources" {
            "HTTPS://FIRMWARE.EXAMPLE/image.bin" => Some("https://firmware.example/image.bin".to_string()),
            " \thttps://firmware.example/image.bin\r\n" => Some("https://firmware.example/image.bin".to_string()),
            r"https:\\firmware.example\image.bin" => Some("https://firmware.example/image.bin".to_string()),
        }
        "unsupported source definitions" {
            "file:///firmware/image.bin" => None,
            "https://user:secret@firmware.example/image.bin" => None,
            "https://firmware.example/image.bin?token=secret" => None,
            "https://firmware.example/image.bin#fragment" => None,
        }
    );
}

#[test]
fn requires_valid_digests() {
    check_cases(
        [
            Case {
                scenario: "digest required",
                input: NicFirmwareArtifact {
                    sha256: String::new(),
                    ..artifact()
                },
                expect: Fails,
            },
            Case {
                scenario: "digest hexadecimal",
                input: NicFirmwareArtifact {
                    sha256: "xy".repeat(32),
                    ..artifact()
                },
                expect: Fails,
            },
        ],
        |mut input| {
            input
                .validate_and_normalize("entries[0].image")
                .map_err(drop)
        },
    );
}

#[test]
fn named_profile_ids_are_not_silently_normalized() {
    value_scenarios!(
        run = |id: &str| id.parse::<NicFirmwareProfileId>().ok().map(|id| id.to_string());
        "case-sensitive name" { "Baseline" => Some("Baseline".to_string()) }
        "invalid names" { "" => None, " baseline " => None, "base\nline" => None }
    );
}
