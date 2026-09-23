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

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use carbide_uuid::machine::StableHostMachineId;
use clap::{CommandFactory, Parser};
use rpc::forge::HostReprovisioningRequest;
use rpc::forge::host_reprovisioning_request::Mode;

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

// Define a basic/working MachineId for testing.
const TEST_MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

/////////////////////////////////////////////////////////////////////////////
// Argument Parsing
//
// This section contains tests specific to argument parsing,
// including testing required arguments, as well as optional
// flag-specific checking.

// The UEFI-password subcommands each route to their own top-level variant:
// set/clear take a machine query, generate takes no args.
#[test]
fn uefi_password_subcommands_route_to_their_variant() {
    scenarios!(
        run = |argv| {
            let subcommand = argv[1];
            parse_with_leaf_matches::<Cmd>(argv, &[subcommand])
                .map(|(cmd, _)| match cmd {
                    Cmd::SetUefiPassword(_) => "set-uefi-password",
                    Cmd::ClearUefiPassword(_) => "clear-uefi-password",
                    Cmd::GenerateHostUefiPassword(_) => "generate-host-uefi-password",
                    _ => panic!("expected a UEFI-password command"),
                })
                .map_err(drop)
        };
        "set-uefi-password with machine query" {
            &["host", "set-uefi-password", "--query", "machine-123"][..] => Yields("set-uefi-password"),
        }

        "clear-uefi-password with machine query" {
            &["host", "clear-uefi-password", "--query", "machine-123"][..] => Yields("clear-uefi-password"),
        }

        "generate-host-uefi-password with no args" {
            &["host", "generate-host-uefi-password"][..] => Yields("generate-host-uefi-password"),
        }
    );
}

// reprovision set parses to the Set variant. With only --id the optional
// flags stay at their defaults; with --update-firmware and --update-message
// supplied they carry through. The asserted tuple is
// (id, update_firmware, update_message).
#[test]
fn reprovision_set_parses_fields() {
    scenarios!(
        run = |argv| {
            parse_with_leaf_matches::<Cmd>(argv, &["reprovision", "set"])
                .map(|(cmd, matches)| {
                    let request = match cmd {
                        Cmd::Reprovision(reprovision::Args::Set(args)) => {
                            HostReprovisioningRequest::from(&args)
                        }
                        _ => panic!("expected Reprovision Set variant"),
                    };
                    assert_eq!(request.mode, Mode::Set as i32);
                    (
                        request
                            .machine_id
                            .expect("machine ID is required")
                            .to_string(),
                        matches.get_flag("update_firmware"),
                        raw_value(&matches, "update_message"),
                    )
                })
                .map_err(drop)
        };
        "set with only required --id" {
            &["host", "reprovision", "set", "--id", TEST_MACHINE_ID][..] => Yields((TEST_MACHINE_ID.to_string(), false, None)),
        }

        "set with all options" {
            &[
                "host",
                "reprovision",
                "set",
                "--id",
                TEST_MACHINE_ID,
                "--update-firmware",
                "--update-message",
                "Maintenance in progress",
            ][..] => Yields((
                TEST_MACHINE_ID.to_string(),
                true,
                Some("Maintenance in progress".to_string()),
            )),
        }
    );
}

// reprovision clear parses to the Clear variant with required --id; the
// update_firmware flag defaults off. The asserted tuple is
// (id, update_firmware).
#[test]
fn reprovision_clear_parses_fields() {
    scenarios!(
        run = |argv| {
            parse_with_leaf_matches::<Cmd>(argv, &["reprovision", "clear"])
                .map(|(cmd, matches)| {
                    let request = match cmd {
                        Cmd::Reprovision(reprovision::Args::Clear(args)) => {
                            HostReprovisioningRequest::from(args)
                        }
                        _ => panic!("expected Reprovision Clear variant"),
                    };
                    assert_eq!(request.mode, Mode::Clear as i32);
                    (
                        request
                            .machine_id
                            .expect("machine ID is required")
                            .to_string(),
                        matches.get_flag("update_firmware"),
                    )
                })
                .map_err(drop)
        };
        "clear with required --id" {
            &["host", "reprovision", "clear", "--id", TEST_MACHINE_ID][..] => Yields((TEST_MACHINE_ID.to_string(), false)),
        }
    );
}

// reprovision mark-manual-upgrade-complete parses to its variant with the
// required --id; the asserted value is the id.
#[test]
fn reprovision_mark_manual_upgrade_complete_parses_fields() {
    scenarios!(
        run = |argv| {
            parse_with_leaf_matches::<Cmd>(argv, &["reprovision", "mark-manual-upgrade-complete"])
                .map(|(cmd, matches)| {
                    assert!(matches!(
                        cmd,
                        Cmd::Reprovision(reprovision::Args::MarkManualUpgradeComplete(_))
                    ));
                    matches
                        .get_one::<StableHostMachineId>("id")
                        .expect("machine ID is required")
                        .to_string()
                })
                .map_err(drop)
        };
        "mark-manual-upgrade-complete with required --id" {
            &[
                "host",
                "reprovision",
                "mark-manual-upgrade-complete",
                "--id",
                TEST_MACHINE_ID,
            ][..] => Yields(TEST_MACHINE_ID.to_string()),
        }
    );
}

// reprovision list parses to the List variant with no args.
#[test]
fn reprovision_list_parses() {
    scenarios!(
        run = |argv| {
            parse_with_leaf_matches::<Cmd>(argv, &["reprovision", "list"])
                .map(|(cmd, _)| match cmd {
                    Cmd::Reprovision(reprovision::Args::List) => "list",
                    _ => panic!("expected Reprovision List variant"),
                })
                .map_err(drop)
        };
        "list with no args" {
            &["host", "reprovision", "list"][..] => Yields("list"),
        }
    );
}

// Each malformed reprovision invocation is rejected at parse time: the
// subcommands that need --id refuse to parse without it.
#[test]
fn invalid_invocations_are_rejected() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "reprovision set without --id" {
            &["host", "reprovision", "set"][..] => Fails,
        }

        "reprovision mark-manual-upgrade-complete without --id" {
            &["host", "reprovision", "mark-manual-upgrade-complete"][..] => Fails,
        }
    );
}
