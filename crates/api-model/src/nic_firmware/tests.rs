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
