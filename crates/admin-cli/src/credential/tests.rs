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

// The intent of the tests.rs file is to test the integrity of the
// command, including things like basic structure parsing, enum
// translations, and any external input validators that are
// configured. Specific "categories" are:
//
// Command Structure - Baseline debug_assert() of the entire command.
// Argument Parsing  - Ensure required/optional arg combinations parse correctly.
// Enum Conversions  - Test From implementations for proto <-> non-proto mapping.
// ValueEnum Parsing - Test string parsing for types deriving claps ValueEnum.
// Custom Validators - Test external input validation functions.

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use clap::{CommandFactory, Parser};
use mac_address::MacAddress;

use super::common::{
    BmcCredentialType, RotationCredentialKind, UefiCredentialType, map_ufm_credential_api_error,
    password_validator, url_validator,
};
use super::*;
use crate::test_support::{parse_with_leaf_matches, raw_value};

// verify_cmd_structure runs a baseline clap debug_assert()
// to do basic command configuration checking and validation,
// ensuring things like unique argument definitions, group
// configurations, argument references, etc. Things that would
// otherwise be missed until runtime.
#[test]
fn verify_cmd_structure() {
    Cmd::command().debug_assert();
}

#[test]
fn ufm_failed_precondition_has_operator_specific_error() {
    let error = map_ufm_credential_api_error(tonic::Status::failed_precondition(
        "UFM fabric \"default\" is supplied by a watched-file credential; update that file",
    ));

    assert_eq!(
        error.to_string(),
        "ufm credential command is unavailable: UFM fabric \"default\" is supplied by a \
         watched-file credential; update that file"
    );

    assert!(matches!(
        map_ufm_credential_api_error(tonic::Status::unavailable("server unavailable")),
        crate::errors::CarbideCliError::ApiInvocationError(_)
    ));
}

/////////////////////////////////////////////////////////////////////////////
// Argument Parsing
//
// This section contains tests specific to argument parsing,
// including testing required arguments, as well as optional
// flag-specific checking.

// add-ufm routes to the AddUFM variant and carries its url plus token, where the
// token defaults to the empty string when its optional flag is omitted.
#[test]
fn parse_add_ufm_fields() {
    scenarios!(
        run = |argv| {
            parse_with_leaf_matches::<Cmd>(argv, &["add-ufm"])
                .map(|(cmd, matches)| {
                    assert!(matches!(cmd, Cmd::AddUFM(_)));
                    (
                        matches
                            .get_one::<String>("url")
                            .expect("url is required")
                            .clone(),
                        matches
                            .get_one::<String>("token")
                            .expect("token has an empty-string default")
                            .clone(),
                    )
                })
                .map_err(drop)
        };
        "required args only -- token defaults to empty" {
            &["credential", "add-ufm", "--url", "https://ufm.example.com"][..] => Yields(("https://ufm.example.com".to_string(), String::new())),
        }

        "with optional --token" {
            &[
                "credential",
                "add-ufm",
                "--url",
                "https://ufm.example.com",
                "--token",
                "my-secret-token",
            ][..] => Yields((
                "https://ufm.example.com".to_string(),
                "my-secret-token".to_string(),
            )),
        }
    );
}

// The retained NMX-M commands accept both argument-free invocations and the
// legacy credential flags before returning their unsupported-operation error.
#[test]
fn parse_nmx_m_compatibility_commands() {
    scenarios!(
        run = |argv| {
            let subcommand = argv[1];
            parse_with_leaf_matches::<Cmd>(argv, &[subcommand])
                .map(|(cmd, matches)| {
                    match cmd {
                        Cmd::AddNmxM(_) => (
                            "add",
                            raw_value(&matches, "username"),
                            raw_value(&matches, "password"),
                        ),
                        Cmd::DeleteNmxM(_) => {
                            ("delete", raw_value(&matches, "username"), None)
                        }
                        _ => panic!("expected an NMX-M compatibility command"),
                    }
                })
                .map_err(drop)
        };
        "add without legacy arguments" {
            &["credential", "add-nmx-m"][..] => Yields(("add", None, None)),
        }

        "add with legacy arguments" {
            &[
                "credential",
                "add-nmx-m",
                "--username",
                "admin",
                "--password",
                "mypassword",
            ][..] => Yields((
                "add",
                Some("admin".to_string()),
                Some("mypassword".to_string()),
            )),
        }

        "delete without legacy arguments" {
            &["credential", "delete-nmx-m"][..] => Yields(("delete", None, None)),
        }

        "delete with legacy arguments" {
            &["credential", "delete-nmx-m", "--username", "admin"][..] => Yields((
                "delete",
                Some("admin".to_string()),
                None,
            )),
        }
    );
}

// parse_add_bmc_with_all_args ensures add-bmc parses
// with all arguments.
#[test]
fn parse_add_bmc_with_all_args() {
    let (cmd, matches) = parse_with_leaf_matches::<Cmd>(
        &[
            "credential",
            "add-bmc",
            "--kind=site-wide-root",
            "--password",
            "secret123",
            "--username",
            "admin",
            "--mac-address",
            "00:11:22:33:44:55",
        ],
        &["add-bmc"],
    )
    .expect("should parse add-bmc");

    assert!(matches!(cmd, Cmd::AddBMC(_)));
    assert!(matches!(
        matches.get_one::<BmcCredentialType>("kind"),
        Some(BmcCredentialType::SiteWideRoot)
    ));
    assert_eq!(
        raw_value(&matches, "password").as_deref(),
        Some("secret123")
    );
    assert_eq!(raw_value(&matches, "username").as_deref(), Some("admin"));
    assert_eq!(
        matches
            .get_one::<MacAddress>("mac_address")
            .map(ToString::to_string)
            .as_deref(),
        Some("00:11:22:33:44:55")
    );
}

// parse_add_uefi ensures add-uefi parses correctly.
#[test]
fn parse_add_uefi() {
    let (cmd, matches) = parse_with_leaf_matches::<Cmd>(
        &[
            "credential",
            "add-uefi",
            "--kind=dpu",
            "--password=uefi-password",
        ],
        &["add-uefi"],
    )
    .expect("should parse add-uefi");

    assert!(matches!(cmd, Cmd::AddUefi(_)));
    assert!(matches!(
        matches.get_one::<UefiCredentialType>("kind"),
        Some(UefiCredentialType::Dpu)
    ));
    assert_eq!(
        raw_value(&matches, "password").as_deref(),
        Some("uefi-password")
    );
}

// parse_add_nic_lockdown_ikm ensures add-nic-lockdown-ikm parses with the
// required password.
#[test]
fn parse_add_nic_lockdown_ikm() {
    let (cmd, matches) = parse_with_leaf_matches::<Cmd>(
        &[
            "credential",
            "add-nic-lockdown-ikm",
            "--password",
            "ikm-secret",
        ],
        &["add-nic-lockdown-ikm"],
    )
    .expect("should parse add-nic-lockdown-ikm");

    assert!(matches!(cmd, Cmd::AddNicLockdownIkm(_)));
    assert_eq!(
        raw_value(&matches, "password").as_deref(),
        Some("ikm-secret")
    );
}

// Every malformed invocation is rejected at parse time -- a subcommand missing
// its required flag, or a --kind value passed without the required `=` separator.
#[test]
fn invalid_invocations_are_rejected() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "add-ufm without required --url" {
            &["credential", "add-ufm"][..] => Fails,
        }

        "add-bmc --kind without the = separator" {
            &[
                "credential",
                "add-bmc",
                "--kind",
                "site-wide-root",
                "--password",
                "secret",
            ][..] => Fails,
        }

        "add-nic-lockdown-ikm without required --password" {
            &["credential", "add-nic-lockdown-ikm"][..] => Fails,
        }
    );
}

// parse_rotate covers both shapes: an auto-generate rotation (password omitted)
// and an explicit-password rotation with a reason note.
#[test]
fn parse_rotate() {
    let (cmd, auto) =
        parse_with_leaf_matches::<Cmd>(&["credential", "rotate", "--type=bmc"], &["rotate"])
            .expect("should parse auto-generate rotate");
    assert!(matches!(cmd, Cmd::Rotate(_)));
    assert!(matches!(
        auto.get_one::<RotationCredentialKind>("credential_type"),
        Some(RotationCredentialKind::Bmc)
    ));
    assert!(auto.get_one::<String>("password").is_none());
    assert!(auto.get_one::<String>("reason").is_none());

    let (cmd, explicit) = parse_with_leaf_matches::<Cmd>(
        &[
            "credential",
            "rotate",
            "--type=host-uefi",
            "--password=mynewpassword",
            "--reason",
            "quarterly rotation",
        ],
        &["rotate"],
    )
    .expect("should parse explicit rotate");
    assert!(matches!(cmd, Cmd::Rotate(_)));
    assert!(matches!(
        explicit.get_one::<RotationCredentialKind>("credential_type"),
        Some(RotationCredentialKind::HostUefi)
    ));
    assert_eq!(
        raw_value(&explicit, "password").as_deref(),
        Some("mynewpassword")
    );
    assert_eq!(
        raw_value(&explicit, "reason").as_deref(),
        Some("quarterly rotation")
    );
}

// parse_rotation_status ensures rotation-status parses with its required --type
// and that the optional --mac-address defaults to None (site-wide) or is parsed
// into a MAC for a device-scoped query.
#[test]
fn parse_rotation_status() {
    let (cmd, site_wide) = parse_with_leaf_matches::<Cmd>(
        &["credential", "rotation-status", "--type=lockdown-ikm"],
        &["rotation-status"],
    )
    .expect("should parse site-wide rotation-status");
    assert!(matches!(cmd, Cmd::RotationStatus(_)));
    assert!(matches!(
        site_wide.get_one::<RotationCredentialKind>("credential_type"),
        Some(RotationCredentialKind::LockdownIkm)
    ));
    assert!(
        site_wide.get_one::<MacAddress>("mac_address").is_none(),
        "omitting --mac-address means a site-wide query"
    );

    let (cmd, per_device) = parse_with_leaf_matches::<Cmd>(
        &[
            "credential",
            "rotation-status",
            "--type=bmc",
            "--mac-address",
            "00:11:22:33:44:55",
        ],
        &["rotation-status"],
    )
    .expect("should parse device-scoped rotation-status");
    assert!(matches!(cmd, Cmd::RotationStatus(_)));
    assert!(matches!(
        per_device.get_one::<RotationCredentialKind>("credential_type"),
        Some(RotationCredentialKind::Bmc)
    ));
    assert_eq!(
        per_device
            .get_one::<MacAddress>("mac_address")
            .map(ToString::to_string)
            .as_deref(),
        Some("00:11:22:33:44:55")
    );
}

// rotate without its required --type, and --type without the = separator, are
// both rejected at parse time.
#[test]
fn invalid_rotate_invocations_are_rejected() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "rotate without required --type" {
            &["credential", "rotate"][..] => Fails,
        }

        "rotate --type without the = separator" {
            &["credential", "rotate", "--type", "bmc"][..] => Fails,
        }

        "rotation-status without required --type" {
            &["credential", "rotation-status"][..] => Fails,
        }

        "rotation-status with a malformed --mac-address" {
            &["credential", "rotation-status", "--type=bmc", "--mac-address", "nope"][..] => Fails,
        }
    );
}

/////////////////////////////////////////////////////////////////////////////
// Enum Conversions
//
// This section is for testing the proto <-> non-proto enum
// From implementations that exist, ensuring enums translate
// from -> into their expected variants.

// bmc_credential_type_to_proto ensures BmcCredentialType
// converts to protobuf CredentialType.
#[test]
fn bmc_credential_type_to_proto() {
    use rpc::forge::CredentialType;

    assert!(matches!(
        CredentialType::from(BmcCredentialType::SiteWideRoot),
        CredentialType::SiteWideBmcRoot
    ));
    assert!(matches!(
        CredentialType::from(BmcCredentialType::BmcRoot),
        CredentialType::RootBmcByMacAddress
    ));
    assert!(matches!(
        CredentialType::from(BmcCredentialType::BmcForgeAdmin),
        CredentialType::BmcForgeAdminByMacAddress
    ));
}

// uefi_credential_type_to_proto ensures
// UefiCredentialType converts to protobuf CredentialType.
#[test]
fn uefi_credential_type_to_proto() {
    use rpc::forge::CredentialType;

    assert!(matches!(
        CredentialType::from(UefiCredentialType::Dpu),
        CredentialType::DpuUefi
    ));
    assert!(matches!(
        CredentialType::from(UefiCredentialType::Host),
        CredentialType::HostUefi
    ));
}

// rotation_credential_kind_to_proto ensures RotationCredentialKind converts to
// the supported arm of the protobuf RotationCredentialType.
#[test]
fn rotation_credential_kind_to_proto() {
    use rpc::forge::RotationCredentialType;

    assert!(matches!(
        RotationCredentialType::from(RotationCredentialKind::Bmc),
        RotationCredentialType::RotationBmc
    ));
    assert!(matches!(
        RotationCredentialType::from(RotationCredentialKind::HostUefi),
        RotationCredentialType::RotationHostUefi
    ));
    assert!(matches!(
        RotationCredentialType::from(RotationCredentialKind::DpuUefi),
        RotationCredentialType::RotationDpuUefi
    ));

    assert!(matches!(
        RotationCredentialType::from(RotationCredentialKind::Nvos),
        RotationCredentialType::RotationNvos
    ));
    assert!(matches!(
        RotationCredentialType::from(RotationCredentialKind::LockdownIkm),
        RotationCredentialType::RotationLockdownIkm
    ));
    assert!(matches!(
        RotationCredentialType::from(RotationCredentialKind::DpuBmcService),
        RotationCredentialType::RotationDpuBmcService
    ));
}

/////////////////////////////////////////////////////////////////////////////
// ValueEnum Parsing
//
// These tests are for testing argument values which derive
// ValueEnum, ensuring the string representations of said
// values correctly convert back into their expected variant,
// or fail otherwise.

// bmc_credential_type_value_enum ensures
// BmcCredentialType parses from kebab-case strings.
#[test]
fn bmc_credential_type_value_enum() {
    use clap::ValueEnum;

    assert!(matches!(
        BmcCredentialType::from_str("site-wide-root", false),
        Ok(BmcCredentialType::SiteWideRoot)
    ));
    assert!(matches!(
        BmcCredentialType::from_str("bmc-root", false),
        Ok(BmcCredentialType::BmcRoot)
    ));
    assert!(matches!(
        BmcCredentialType::from_str("bmc-forge-admin", false),
        Ok(BmcCredentialType::BmcForgeAdmin)
    ));
    assert!(BmcCredentialType::from_str("invalid", false).is_err());
}

// uefi_credential_type_value_enum ensures UefiCredentialType
// parses from strings.
#[test]
fn uefi_credential_type_value_enum() {
    use clap::ValueEnum;

    assert!(matches!(
        UefiCredentialType::from_str("dpu", false),
        Ok(UefiCredentialType::Dpu)
    ));
    assert!(matches!(
        UefiCredentialType::from_str("host", false),
        Ok(UefiCredentialType::Host)
    ));
    assert!(UefiCredentialType::from_str("invalid", false).is_err());
}

// rotation_credential_kind_value_enum ensures RotationCredentialKind parses from
// kebab-case strings.
#[test]
fn rotation_credential_kind_value_enum() {
    use clap::ValueEnum;

    assert!(matches!(
        RotationCredentialKind::from_str("bmc", false),
        Ok(RotationCredentialKind::Bmc)
    ));
    assert!(matches!(
        RotationCredentialKind::from_str("host-uefi", false),
        Ok(RotationCredentialKind::HostUefi)
    ));
    assert!(matches!(
        RotationCredentialKind::from_str("dpu-uefi", false),
        Ok(RotationCredentialKind::DpuUefi)
    ));
    assert!(matches!(
        RotationCredentialKind::from_str("nvos", false),
        Ok(RotationCredentialKind::Nvos)
    ));
    assert!(matches!(
        RotationCredentialKind::from_str("lockdown-ikm", false),
        Ok(RotationCredentialKind::LockdownIkm)
    ));
    assert!(matches!(
        RotationCredentialKind::from_str("dpu-bmc-service", false),
        Ok(RotationCredentialKind::DpuBmcService)
    ));
    assert!(RotationCredentialKind::from_str("invalid", false).is_err());
}

/////////////////////////////////////////////////////////////////////////////
// Validators
//
// This section contains tests for testing argument values
// which are processed by custom/external validation
// functions. Here, we test that the functions work as expected.

// url_validator accepts well-formed http(s) URLs and rejects anything that does
// not parse as a URL (including the empty string).
#[test]
fn url_validator_accepts_only_valid_urls() {
    scenarios!(
        run = |url| url_validator(url.to_string()).map(|_| ()).map_err(drop);
        "https host" {
            "https://example.com" => Yields(()),
        }

        "http host with port" {
            "http://localhost:8080" => Yields(()),
        }

        "https host with path" {
            "https://ufm.corp.example.com/api" => Yields(()),
        }

        "not a url" {
            "not a url" => Fails,
        }

        "empty string" {
            "" => Fails,
        }
    );
}

// password_validator accepts any non-empty password and rejects only the empty
// string.
#[test]
fn password_validator_accepts_only_non_empty() {
    scenarios!(
        run = |pw| password_validator(pw.to_string()).map(|_| ()).map_err(drop);
        "ordinary password" {
            "secret123" => Yields(()),
        }

        "single character" {
            "a" => Yields(()),
        }

        "spaces are allowed" {
            "spaces are ok" => Yields(()),
        }

        "empty string is rejected" {
            "" => Fails,
        }
    );
}
