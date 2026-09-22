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
// Command Structure   - Baseline debug_assert() of the entire command.
// Argument Parsing    - Ensure required/optional arg combinations parse correctly.
// Validation Logic    - Test business logic validators on parsed arguments.

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use clap::{CommandFactory, Parser};
use mac_address::MacAddress;

use super::common::ExpectedMachineJson;
use super::*;
use crate::expected_machines::common::HostDpuPolicy;
use crate::test_support::{parse_leaf, raw_value};

// verify_cmd_structure runs a baseline clap debug_assert()
// to do basic command configuration checking and validation,
// ensuring things like unique argument definitions, group
// configurations, argument references, etc. Things that would
// otherwise be missed until runtime.
#[test]
fn verify_cmd_structure() {
    Cmd::command().debug_assert();
}

/////////////////////////////////////////////////////////////////////////////
// Argument Parsing
//
// This section contains tests specific to argument parsing,
// including testing required arguments, as well as optional
// flag-specific checking.

// parse_show_no_args ensures show parses with no
// arguments (all machines).
#[test]
fn parse_show_no_args() {
    let matches =
        parse_leaf::<Cmd>(&["expected-machine", "show"], &["show"]).expect("should parse show");

    assert!(matches.get_one::<MacAddress>("bmc_mac_address").is_none());
}

// parse_show_with_mac ensures show parses with MAC address.
#[test]
fn parse_show_with_mac() {
    let matches = parse_leaf::<Cmd>(
        &["expected-machine", "show", "1a:2b:3c:4d:5e:6f"],
        &["show"],
    )
    .expect("should parse show with MAC");

    assert_eq!(
        matches.get_one::<MacAddress>("bmc_mac_address").copied(),
        Some("1a:2b:3c:4d:5e:6f".parse::<MacAddress>().unwrap())
    );
}

// parse_add ensures add parses with required arguments.
#[test]
fn parse_add() {
    let matches = parse_leaf::<Cmd>(
        &[
            "expected-machine",
            "add",
            "--bmc-mac-address",
            "1a:2b:3c:4d:5e:6f",
            "--bmc-username",
            "admin",
            "--bmc-password",
            "secret",
            "--chassis-serial-number",
            "SN12345",
        ],
        &["add"],
    )
    .expect("should parse add");

    assert_eq!(
        raw_value(&matches, "bmc_username").as_deref(),
        Some("admin")
    );
    assert_eq!(
        raw_value(&matches, "chassis_serial_number").as_deref(),
        Some("SN12345")
    );
}

/// Canonical and legacy interface flags build the same protobuf request.
#[test]
fn parse_add_with_new_and_legacy_interface_flags() {
    let value = r#"[{"mac_address":"00:11:22:33:44:55","primary":true}]"#;
    let mut parsed_interfaces = Vec::new();

    for flag in ["--interfaces", "--host_nics"] {
        let cmd = Cmd::try_parse_from([
            "expected-machine",
            "add",
            "--bmc-mac-address",
            "1a:2b:3c:4d:5e:6f",
            "--bmc-username",
            "admin",
            "--bmc-password",
            "secret",
            "--chassis-serial-number",
            "SN12345",
            flag,
            value,
        ])
        .unwrap_or_else(|error| panic!("{flag} should parse: {error}"));

        let Cmd::Add(args) = cmd else {
            panic!("expected Add variant");
        };
        let machine = rpc::forge::ExpectedMachine::try_from(args)
            .unwrap_or_else(|error| panic!("{flag} should build a request: {error}"));
        parsed_interfaces.push(machine.host_nics);
    }

    assert_eq!(parsed_interfaces[0], parsed_interfaces[1]);
    assert_eq!(parsed_interfaces[0][0].mac_address, "00:11:22:33:44:55");
}

// parse_add_without_password ensures add parses when --bmc-password is omitted.
#[test]
fn parse_add_without_password() {
    let matches = parse_leaf::<Cmd>(
        &[
            "expected-machine",
            "add",
            "--bmc-mac-address",
            "1a:2b:3c:4d:5e:6f",
            "--bmc-username",
            "admin",
            "--chassis-serial-number",
            "SN12345",
        ],
        &["add"],
    )
    .expect("should parse add without password");

    assert!(raw_value(&matches, "bmc_password").is_none());
    assert_eq!(
        raw_value(&matches, "bmc_username").as_deref(),
        Some("admin")
    );
}

// parse_add_with_options ensures add parses with
// all options.
#[test]
fn parse_add_with_options() {
    let matches = parse_leaf::<Cmd>(
        &[
            "expected-machine",
            "add",
            "--bmc-mac-address",
            "1a:2b:3c:4d:5e:6f",
            "--bmc-username",
            "admin",
            "--bmc-password",
            "secret",
            "--chassis-serial-number",
            "SN12345",
            "--meta-name",
            "MyMachine",
            "--label",
            "env:prod",
            "--sku-id",
            "sku123",
        ],
        &["add"],
    )
    .expect("should parse add with options");

    assert_eq!(
        raw_value(&matches, "meta_name").as_deref(),
        Some("MyMachine")
    );
    assert_eq!(raw_value(&matches, "sku_id").as_deref(), Some("sku123"));
}

// parse_delete ensures delete parses with MAC address.
#[test]
fn parse_delete() {
    let cmd = Cmd::try_parse_from(["expected-machine", "delete", "1a:2b:3c:4d:5e:6f"])
        .expect("should parse delete");

    assert!(matches!(cmd, Cmd::Delete(_)));
}

// parse_patch ensures patch parses with required arguments.
#[test]
fn parse_patch() {
    let matches = parse_leaf::<Cmd>(
        &[
            "expected-machine",
            "patch",
            "--bmc-mac-address",
            "1a:2b:3c:4d:5e:6f",
            "--sku-id",
            "new_sku",
        ],
        &["patch"],
    )
    .expect("should parse patch");

    assert_eq!(raw_value(&matches, "sku_id").as_deref(), Some("new_sku"));
}

// parse_update ensures update parses with filename.
#[test]
fn parse_update() {
    let matches = parse_leaf::<Cmd>(
        &["expected-machine", "update", "--filename", "machine.json"],
        &["update"],
    )
    .expect("should parse update");

    assert_eq!(
        raw_value(&matches, "filename").as_deref(),
        Some("machine.json")
    );
}

// parse_replace_all ensures replace-all parses with
// filename.
#[test]
fn parse_replace_all() {
    let matches = parse_leaf::<Cmd>(
        &[
            "expected-machine",
            "replace-all",
            "--filename",
            "machines.json",
        ],
        &["replace-all"],
    )
    .expect("should parse replace-all");

    assert_eq!(
        raw_value(&matches, "filename").as_deref(),
        Some("machines.json")
    );
}

#[test]
fn expected_machine_json_accepts_missing_id() {
    let machine: ExpectedMachineJson = serde_json::from_str(
        r#"{
            "bmc_mac_address": "00:11:22:33:44:55",
            "bmc_username": "admin",
            "bmc_password": "secret",
            "chassis_serial_number": "SN123"
        }"#,
    )
    .expect("expected machine without id should parse");

    assert_eq!(machine.id, None);
}

#[test]
fn expected_machine_json_accepts_string_id() {
    let machine: ExpectedMachineJson = serde_json::from_str(
        r#"{
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "bmc_mac_address": "00:11:22:33:44:55",
            "bmc_username": "admin",
            "bmc_password": "secret",
            "chassis_serial_number": "SN123"
        }"#,
    )
    .expect("expected machine with string id should parse");

    assert_eq!(
        machine.id.as_deref(),
        Some("123e4567-e89b-12d3-a456-426614174000")
    );
}

#[test]
fn expected_machine_json_accepts_rpc_uuid_id() {
    let machine: ExpectedMachineJson = serde_json::from_str(
        r#"{
            "id": {
                "value": "123e4567-e89b-12d3-a456-426614174000"
            },
            "bmc_mac_address": "00:11:22:33:44:55",
            "bmc_username": "admin",
            "bmc_password": "secret",
            "chassis_serial_number": "SN123"
        }"#,
    )
    .expect("expected machine with RPC UUID id should parse");

    assert_eq!(
        machine.id.as_deref(),
        Some("123e4567-e89b-12d3-a456-426614174000")
    );
}

// parse_erase ensures erase parses with no arguments.
#[test]
fn parse_erase() {
    let cmd = Cmd::try_parse_from(["expected-machine", "erase"]).expect("should parse erase");

    assert!(matches!(cmd, Cmd::Erase(_)));
}

// Every malformed invocation is rejected at parse time -- a missing required
// argument, one half of a paired credential, or a flag left without its value.
#[test]
fn invalid_invocations_are_rejected() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "add without its required arguments" {
            &["expected-machine", "add"][..] => Fails,
        }

        "patch with a username but no password" {
            &[
                "expected-machine",
                "patch",
                "--bmc-mac-address",
                "00:00:00:00:00:00",
                "--bmc-username",
                "admin",
            ][..] => Fails,
        }

        "patch with a password but no username" {
            &[
                "expected-machine",
                "patch",
                "--bmc-mac-address",
                "00:00:00:00:00:00",
                "--bmc-password",
                "secret",
            ][..] => Fails,
        }

        "update without --filename" {
            &["expected-machine", "update"][..] => Fails,
        }

        "add with --fallback-dpu-serial-number missing its value" {
            &[
                "expected-machine",
                "add",
                "--bmc-mac-address",
                "0a:0b:0c:0d:0e:0f",
                "--bmc-username",
                "admin",
                "--bmc-password",
                "secret",
                "--chassis-serial-number",
                "SN12345",
                "--fallback-dpu-serial-number",
            ][..] => Fails,
        }
    );
}

/////////////////////////////////////////////////////////////////////////////
// Validation Logic
//
// This section tests business logic validators on parsed arguments,
// including custom validation methods like duplicate detection.

// has_duplicate_dpu_serials flags a repeated `-d` serial on an otherwise valid
// add: unique serials and the no-serials case are clean, a repeat is caught.
#[test]
fn has_duplicate_dpu_serials_flags_repeats() {
    scenarios!(
        run = |argv| {
            add::Args::try_parse_from(argv.iter().copied())
                .map(|m| m.has_duplicate_dpu_serials_for_test())
                .map_err(drop)
        };
        "three unique serials" {
            &[
                "ExpectedMachine",
                "--bmc-mac-address",
                "0a:0b:0c:0d:0e:0f",
                "--bmc-username",
                "admin",
                "--bmc-password",
                "secret",
                "--chassis-serial-number",
                "SN12345",
                "--fallback-dpu-serial-number",
                "dpu1",
                "-d",
                "dpu2",
                "-d",
                "dpu3",
            ][..] => Yields(false),
        }

        "a repeated serial is detected" {
            &[
                "ExpectedMachine",
                "--bmc-mac-address",
                "0a:0b:0c:0d:0e:0f",
                "--bmc-username",
                "admin",
                "--bmc-password",
                "secret",
                "--chassis-serial-number",
                "SN12345",
                "-d",
                "dpu1",
                "-d",
                "dpu2",
                "-d",
                "dpu3",
                "-d",
                "dpu1",
            ][..] => Yields(true),
        }

        "no serials at all" {
            &[
                "ExpectedMachine",
                "--bmc-mac-address",
                "0a:0b:0c:0d:0e:0f",
                "--bmc-username",
                "admin",
                "--bmc-password",
                "secret",
                "--chassis-serial-number",
                "SN12345",
            ][..] => Yields(false),
        }
    );
}

// validate_patch_with_dpu_serials ensures patch validate()
// passes with unique DPU serials.
#[test]
fn validate_patch_with_dpu_serials() {
    let cmd = Cmd::try_parse_from([
        "expected-machine",
        "patch",
        "--bmc-mac-address",
        "00:00:00:00:00:00",
        "--fallback-dpu-serial-number",
        "dpu1",
        "-d",
        "dpu2",
    ])
    .expect("should parse");

    match cmd {
        Cmd::Patch(args) => {
            assert!(
                args.validate_for_test().is_ok(),
                "unique serials should validate"
            );
        }
        _ => panic!("expected Patch variant"),
    }
}

// validate_patch_duplicate_dpu_serials_fails ensures patch
// validate() fails with duplicate DPU serials.
#[test]
fn validate_patch_duplicate_dpu_serials_fails() {
    let cmd = Cmd::try_parse_from([
        "expected-machine",
        "patch",
        "--bmc-mac-address",
        "00:00:00:00:00:00",
        "--fallback-dpu-serial-number",
        "dpu1",
        "-d",
        "dpu2",
        "-d",
        "dpu3",
        "-d",
        "dpu2",
        "-d",
        "dpu4",
    ])
    .expect("should parse");

    match cmd {
        Cmd::Patch(args) => {
            assert!(
                args.validate_for_test().is_err(),
                "duplicate serials should fail validation"
            );
        }
        _ => panic!("expected Patch variant"),
    }
}

// validate_patch_with_credentials ensures patch validate()
// passes with username and password together.
#[test]
fn validate_patch_with_credentials() {
    let cmd = Cmd::try_parse_from([
        "expected-machine",
        "patch",
        "--bmc-mac-address",
        "00:00:00:00:00:00",
        "--bmc-username",
        "admin",
        "--bmc-password",
        "secret",
    ])
    .expect("should parse");

    match cmd {
        Cmd::Patch(args) => {
            assert!(
                args.validate_for_test().is_ok(),
                "credentials should validate"
            );
        }
        _ => panic!("expected Patch variant"),
    }
}

// validate_patch_all_fields ensures patch validate()
// passes with all fields provided.
#[test]
fn validate_patch_all_fields() {
    let cmd = Cmd::try_parse_from([
        "expected-machine",
        "patch",
        "--bmc-mac-address",
        "00:00:00:00:00:00",
        "--bmc-username",
        "admin",
        "--bmc-password",
        "secret",
        "--chassis-serial-number",
        "SN12345",
        "--fallback-dpu-serial-number",
        "dpu1",
    ])
    .expect("should parse");

    match cmd {
        Cmd::Patch(args) => {
            assert!(
                args.validate_for_test().is_ok(),
                "all fields should validate"
            );
        }
        _ => panic!("expected Patch variant"),
    }
}

// The DPU policy flag is optional. Downstream, unset defers to the site-wide
// `[site_explorer] dpu_policy` setting and ultimately defaults to `Manage`.
#[test]
fn parse_add_without_dpu_policy() {
    let matches = parse_leaf::<Cmd>(
        &[
            "expected-machine",
            "add",
            "--bmc-mac-address",
            "1a:2b:3c:4d:5e:6f",
            "--bmc-username",
            "admin",
            "--bmc-password",
            "secret",
            "--chassis-serial-number",
            "SN12345",
        ],
        &["add"],
    )
    .expect("should parse without --dpu-policy");

    assert!(
        matches.get_one::<HostDpuPolicy>("dpu_policy").is_none(),
        "--dpu-policy should be optional"
    );
}

// The protobuf sentinel remains accepted for backwards compatibility, but it
// is not part of the canonical three-value policy vocabulary shown to users.
#[test]
fn dpu_policy_help_only_lists_policy_values() {
    let mut command = Cmd::command();
    let add = command.find_subcommand_mut("add").unwrap();
    let dpu_policy = add
        .get_arguments()
        .find(|argument| argument.get_id() == "dpu_policy")
        .unwrap();
    let visible_values = dpu_policy
        .get_possible_values()
        .into_iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(visible_values, ["manage", "nic", "ignore"]);
}

// Clap rejects policy values that do not match the enum.
#[test]
fn parse_add_rejects_invalid_dpu_policy() {
    let result = Cmd::try_parse_from([
        "expected-machine",
        "add",
        "--bmc-mac-address",
        "1a:2b:3c:4d:5e:6f",
        "--bmc-username",
        "admin",
        "--bmc-password",
        "secret",
        "--chassis-serial-number",
        "SN12345",
        "--dpu-policy",
        "garbage",
    ]);
    assert!(
        result.is_err(),
        "clap should reject --dpu-policy with an invalid value"
    );
}

// `patch --dpu-policy nic`
// alone (no other patchable fields) satisfies clap's ArgGroup and the
// `Args::validate()` "at least one field" check.
#[test]
fn validate_patch_with_dpu_policy_only() {
    let cmd = Cmd::try_parse_from([
        "expected-machine",
        "patch",
        "--bmc-mac-address",
        "00:00:00:00:00:00",
        "--dpu-policy",
        "nic",
    ])
    .expect("patch --dpu-policy alone should parse (ArgGroup)");

    match cmd {
        Cmd::Patch(args) => {
            assert!(
                args.validate_for_test().is_ok(),
                "patch --dpu-policy alone should validate"
            );
        }
        _ => panic!("expected Patch variant"),
    }
}

// `--bmc-ip-allocation` is optional on `add`; unset is treated downstream as the
// server default (`auto`), which retains an auto-allocated BMC address.
#[test]
fn parse_add_without_bmc_ip_allocation() {
    let matches = parse_leaf::<Cmd>(
        &[
            "expected-machine",
            "add",
            "--bmc-mac-address",
            "1a:2b:3c:4d:5e:6f",
            "--bmc-username",
            "admin",
            "--bmc-password",
            "secret",
            "--chassis-serial-number",
            "SN12345",
        ],
        &["add"],
    )
    .expect("should parse without --bmc-ip-allocation");

    assert!(
        matches
            .get_one::<rpc::forge::BmcIpAllocationType>("bmc_ip_allocation")
            .is_none(),
        "--bmc-ip-allocation should be optional"
    );
}

// `--bmc-ip-allocation <value>` parses to the matching BmcIpAllocationType variant
// on both `add` and `patch`. The closure pulls bmc_ip_allocation off whichever
// variant parsed; each row pins the parsed `Some(variant)`.
#[test]
fn parse_bmc_ip_allocation_to_its_variant() {
    scenarios!(
        run = |argv| {
            let subcommand = argv[1];
            parse_leaf::<Cmd>(argv, &[subcommand])
                .map(|matches| {
                    matches
                        .get_one::<rpc::forge::BmcIpAllocationType>("bmc_ip_allocation")
                        .copied()
                })
                .map_err(drop)
        };
        "add --bmc-ip-allocation retained" {
            &[
                "expected-machine",
                "add",
                "--bmc-mac-address",
                "1a:2b:3c:4d:5e:6f",
                "--bmc-username",
                "admin",
                "--bmc-password",
                "secret",
                "--chassis-serial-number",
                "SN12345",
                "--bmc-ip-allocation",
                "retained",
            ][..] => Yields(Some(rpc::forge::BmcIpAllocationType::Retained)),
        }

        "add --bmc-ip-allocation dynamic" {
            &[
                "expected-machine",
                "add",
                "--bmc-mac-address",
                "1a:2b:3c:4d:5e:6f",
                "--bmc-username",
                "admin",
                "--bmc-password",
                "secret",
                "--chassis-serial-number",
                "SN12345",
                "--bmc-ip-allocation",
                "dynamic",
            ][..] => Yields(Some(rpc::forge::BmcIpAllocationType::Dynamic)),
        }

        "add --bmc-ip-allocation auto" {
            &[
                "expected-machine",
                "add",
                "--bmc-mac-address",
                "1a:2b:3c:4d:5e:6f",
                "--bmc-username",
                "admin",
                "--bmc-password",
                "secret",
                "--chassis-serial-number",
                "SN12345",
                "--bmc-ip-allocation",
                "auto",
            ][..] => Yields(Some(rpc::forge::BmcIpAllocationType::Auto)),
        }

        "patch --bmc-ip-allocation retained" {
            &[
                "expected-machine",
                "patch",
                "--bmc-mac-address",
                "1a:2b:3c:4d:5e:6f",
                "--bmc-ip-allocation",
                "retained",
            ][..] => Yields(Some(rpc::forge::BmcIpAllocationType::Retained)),
        }

        "patch --bmc-ip-allocation fixed" {
            &[
                "expected-machine",
                "patch",
                "--bmc-mac-address",
                "1a:2b:3c:4d:5e:6f",
                "--bmc-ip-allocation",
                "fixed",
            ][..] => Yields(Some(rpc::forge::BmcIpAllocationType::Fixed)),
        }
    );
}

// clap rejects `--bmc-ip-allocation` values that don't match the enum.
#[test]
fn parse_add_rejects_invalid_bmc_ip_allocation() {
    let result = Cmd::try_parse_from([
        "expected-machine",
        "add",
        "--bmc-mac-address",
        "1a:2b:3c:4d:5e:6f",
        "--bmc-username",
        "admin",
        "--bmc-password",
        "secret",
        "--chassis-serial-number",
        "SN12345",
        "--bmc-ip-allocation",
        "garbage",
    ]);
    assert!(
        result.is_err(),
        "clap should reject --bmc-ip-allocation with an invalid value"
    );
}

// `patch --bmc-ip-allocation retained` alone (no other patchable fields) must
// satisfy clap's ArgGroup and `Args::validate()`'s "at least one field" check.
// A patch that sets only this field.
#[test]
fn validate_patch_with_bmc_ip_allocation_only() {
    let cmd = Cmd::try_parse_from([
        "expected-machine",
        "patch",
        "--bmc-mac-address",
        "00:00:00:00:00:00",
        "--bmc-ip-allocation",
        "retained",
    ])
    .expect("patch --bmc-ip-allocation alone should parse (ArgGroup)");

    match cmd {
        Cmd::Patch(args) => {
            assert!(
                args.validate_for_test().is_ok(),
                "patch --bmc-ip-allocation alone should validate"
            );
        }
        _ => panic!("expected Patch variant"),
    }
}

/// Canonical and legacy interface flags both satisfy the patch argument group
/// and produce the same value.
#[test]
fn validate_patch_with_new_and_legacy_interface_flags() {
    let value = r#"[{"mac_address":"00:11:22:33:44:55","primary":true}]"#;
    let mut parsed_values = Vec::new();

    for flag in ["--interfaces", "--host_nics"] {
        let argv = [
            "expected-machine",
            "patch",
            "--bmc-mac-address",
            "00:00:00:00:00:00",
            flag,
            value,
        ];
        let matches = parse_leaf::<Cmd>(&argv, &["patch"])
            .unwrap_or_else(|error| panic!("{flag} should parse: {error}"));
        let cmd = Cmd::try_parse_from(argv)
            .unwrap_or_else(|error| panic!("{flag} should parse into Cmd: {error}"));

        let Cmd::Patch(args) = cmd else {
            panic!("expected Patch variant");
        };
        assert!(args.validate_for_test().is_ok(), "{flag} should validate");
        parsed_values.push(raw_value(&matches, "interfaces"));
    }

    assert_eq!(
        parsed_values,
        [Some(value.to_string()), Some(value.to_string())]
    );
}
