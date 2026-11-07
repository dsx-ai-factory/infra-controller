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

use carbide_test_support::Outcome::*;
use carbide_test_support::{scenarios, value_scenarios};
use libmlx::firmware::config::FirmwareFlasherProfile;

// Parse `toml` into a profile, panicking with the scenario label if it doesn't.
fn profile(scenario: &str, toml: &str) -> FirmwareFlasherProfile {
    FirmwareFlasherProfile::from_toml(toml).unwrap_or_else(|e| panic!("{scenario}: {e}"))
}

#[test]
fn test_minimal_config() {
    let toml = r#"
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000884"
version = "32.43.1014"
firmware_url = "/opt/firmware/prod.signed.bin"
"#;

    let profile = FirmwareFlasherProfile::from_toml(toml).unwrap();
    assert_eq!(profile.firmware_spec.part_number, "900-9D3B4-00CV-TA0");
    assert_eq!(profile.firmware_spec.psid, "MT_0000000884");
    assert_eq!(profile.firmware_spec.version, "32.43.1014");
    assert_eq!(
        profile.flash_spec.firmware_url,
        "/opt/firmware/prod.signed.bin"
    );
    assert!(profile.flash_spec.firmware_credentials.is_none());
    assert!(profile.flash_spec.device_conf_url.is_none());
    assert!(profile.flash_spec.device_conf_credentials.is_none());
}

#[test]
fn test_config_with_version_and_options() {
    let toml = r#"
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000884"
version = "32.43.1014"
firmware_url = "/opt/firmware/prod.signed.bin"
reset = true
verify_image = true
verify_version = true
"#;

    let profile = FirmwareFlasherProfile::from_toml(toml).unwrap();
    assert_eq!(profile.firmware_spec.version, "32.43.1014");
    assert!(profile.flash_options.reset);
    assert!(profile.flash_options.verify_image);
    assert!(profile.flash_options.verify_version);
    assert_eq!(profile.flash_options.reset_level, 3); // default
}

// Every credential block parses into a present `firmware_credentials`, regardless
// of auth scheme (bearer token, basic auth, SSH key, SSH agent).
#[test]
fn credentials_block_populates_firmware_credentials() {
    value_scenarios!(
        run = |toml| {
            profile("credentials", toml)
                .flash_spec
                .firmware_credentials
                .is_some()
        };
        "bearer token" {
            r#"
                part_number = "900-9D3B4-00CV-TA0"
                psid = "MT_0000000884"
                version = "32.43.1014"
                firmware_url = "https://artifacts.example.com/fw/prod.signed.bin"

                [firmware_credentials]
                type = "bearer_token"
                token = "my-secret-token"
                "# => true,
        }

        "basic auth" {
            r#"
                part_number = "900-9D3B4-00CV-TA0"
                psid = "MT_0000000884"
                version = "32.43.1014"
                firmware_url = "https://internal.example.com/fw/prod.signed.bin"

                [firmware_credentials]
                type = "basic_auth"
                username = "deploy"
                password = "s3cret"
                "# => true,
        }

        "ssh key" {
            r#"
                part_number = "900-9D3B4-00CV-TA0"
                psid = "MT_0000000884"
                version = "32.43.1014"
                firmware_url = "ssh://deploy@build-server.example.com:builds/fw/prod.signed.bin"

                [firmware_credentials]
                type = "ssh_key"
                path = "/home/deploy/.ssh/id_ed25519"
                "# => true,
        }

        "ssh agent" {
            r#"
                part_number = "900-9D3B4-00CV-TA0"
                psid = "MT_0000000884"
                version = "32.43.1014"
                firmware_url = "ssh://deploy@build-server.example.com:builds/fw/prod.signed.bin"

                [firmware_credentials]
                type = "ssh_agent"
                "# => true,
        }
    );
}

// The firmware source built from the flash spec describes the transport scheme of
// its `firmware_url` -- http for an https URL, ssh for an ssh URL. Each row pairs a
// config with the scheme its source description must contain.
#[test]
fn firmware_source_describes_its_transport_scheme() {
    value_scenarios!(
        run = |(toml, scheme)| {
            profile("transport scheme", toml)
                .flash_spec
                .build_firmware_source()
                .unwrap()
                .description()
                .contains(scheme)
        };
        "https url -> http source" {
            (
                                r#"
                part_number = "900-9D3B4-00CV-TA0"
                psid = "MT_0000000884"
                version = "32.43.1014"
                firmware_url = "https://artifacts.example.com/fw/prod.signed.bin"

                [firmware_credentials]
                type = "bearer_token"
                token = "my-secret-token"
                "#,
                                "http:",
                            ) => true,
        }

        "ssh url -> ssh source" {
            (
                                r#"
                part_number = "900-9D3B4-00CV-TA0"
                psid = "MT_0000000884"
                version = "32.43.1014"
                firmware_url = "ssh://deploy@build-server.example.com:builds/fw/prod.signed.bin"

                [firmware_credentials]
                type = "ssh_key"
                path = "/home/deploy/.ssh/id_ed25519"
                "#,
                                "ssh://",
                            ) => true,
        }
    );
}

#[test]
fn test_config_with_device_conf() {
    let toml = r#"
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000884"
version = "32.43.1014"
firmware_url = "https://artifacts.example.com/fw/debug.signed.bin"
device_conf_url = "ssh://deploy@build-server.example.com:builds/configs/debug.conf.bin"

[firmware_credentials]
type = "bearer_token"
token = "fw-token"

[device_conf_credentials]
type = "ssh_agent"
"#;

    let profile = FirmwareFlasherProfile::from_toml(toml).unwrap();

    assert_eq!(
        profile.flash_spec.device_conf_url.as_deref(),
        Some("ssh://deploy@build-server.example.com:builds/configs/debug.conf.bin")
    );
    assert!(profile.flash_spec.device_conf_credentials.is_some());

    let fw_source = profile.flash_spec.build_firmware_source().unwrap();
    assert!(fw_source.description().contains("http:"));

    let conf_source = profile.flash_spec.build_device_conf_source().unwrap();
    assert!(conf_source.is_some());
    assert!(conf_source.unwrap().description().contains("ssh://"));
}

#[test]
fn test_config_no_device_conf_returns_none() {
    let toml = r#"
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000884"
version = "32.43.1014"
firmware_url = "/opt/firmware/prod.signed.bin"
"#;

    let profile = FirmwareFlasherProfile::from_toml(toml).unwrap();
    let conf_source = profile.flash_spec.build_device_conf_source().unwrap();
    assert!(conf_source.is_none());
}

// Malformed or incomplete TOML is rejected by `from_toml`.
#[test]
fn from_toml_rejects_malformed_or_incomplete_input() {
    scenarios!(
        run = |toml| {
            FirmwareFlasherProfile::from_toml(toml)
                .map(|_| ())
                .map_err(drop)
        };
        "unterminated string literal" {
            r#"
                firmware_url = "missing closing quote
                "# => Fails,
        }

        "missing required firmware_url" {
            r#"
                part_number = "900-9D3B4-00CV-TA0"
                psid = "MT_0000000884"
                version = "32.43.1014"
                "# => Fails,
        }
    );
}
