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
use carbide_uuid::machine::{HostMachineId, MachineId, StableHostMachineId};
use clap::{CommandFactory, Parser};

use super::*;
use crate::test_support::{parse_leaf, raw_value};

// Define a basic/working MachineId for testing.
const TEST_MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
const TEST_DPU_ID: &str = "fm100ds3gfip02lfgleidqoitqgh8d8mdc4a3j2tdncbjrfjtvrrhn2kleg";
const TEST_INTERFACE_ID: &str = "00000000-0000-0000-0000-000000000001";

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

// show routes to the Show variant across its argument combinations: bare (all
// hosts), with a machine id, and with --fix. Each row yields the parsed
// (machine.is_some(), all, ips, more, fix) so every original assertion holds.
#[test]
fn parse_show_routes_to_show() {
    scenarios!(
        run = |argv| {
            let matches = parse_leaf::<Cmd>(argv, &["show"]).map_err(drop)?;
            Ok::<_, ()>((
                matches.get_one::<MachineId>("machine").is_some(),
                matches.get_flag("all"),
                matches.get_flag("ips"),
                matches.get_flag("more"),
                matches.get_flag("fix"),
            ))
        };
        "no args (all hosts)" {
            &["managed-host", "show"][..] => Yields((false, false, false, false, false)),
        }

        "with machine id" {
            &["managed-host", "show", TEST_MACHINE_ID][..] => Yields((true, false, false, false, false)),
        }

        "with --fix flag" {
            &["managed-host", "show", "--fix"][..] => Yields((false, false, false, false, true)),
        }
    );
}

// maintenance on/off route to the Maintenance variant with the expected host
// (and reference, for `on`). Each row yields (host, reference) -- reference is
// empty for the `off` case which carries none.
#[test]
fn parse_maintenance_routes_to_maintenance() {
    scenarios!(
        run = |argv| {
            let action = argv[2];
            let matches = parse_leaf::<Cmd>(argv, &["maintenance", action]).map_err(drop)?;
            Ok::<_, ()>((
                matches
                    .get_one::<MachineId>("host")
                    .copied()
                    .expect("host is required"),
                if action == "on" {
                    raw_value(&matches, "reference").expect("reference is required")
                } else {
                    String::new()
                },
            ))
        };
        "on with host and reference" {
            &[
                "managed-host",
                "maintenance",
                "on",
                "--host",
                TEST_MACHINE_ID,
                "--reference",
                "TICKET-123",
            ][..] => Yields((TEST_MACHINE_ID.parse::<MachineId>().unwrap(), "TICKET-123".to_string())),
        }

        "off with host" {
            &[
                "managed-host",
                "maintenance",
                "off",
                "--host",
                TEST_MACHINE_ID,
            ][..] => Yields((TEST_MACHINE_ID.parse::<MachineId>().unwrap(), String::new())),
        }
    );
}

// quarantine on/off route to the Quarantine variant with the expected host
// (and reason, for `on`). Each row yields (host, reason) -- reason is empty
// for the `off` case which carries none.
#[test]
fn parse_quarantine_routes_to_quarantine() {
    scenarios!(
        run = |argv| {
            let action = argv[2];
            let matches = parse_leaf::<Cmd>(argv, &["quarantine", action]).map_err(drop)?;
            Ok::<_, ()>((
                matches
                    .get_one::<HostMachineId>("host")
                    .copied()
                    .expect("host is required"),
                if action == "on" {
                    raw_value(&matches, "reason").expect("reason is required")
                } else {
                    String::new()
                },
            ))
        };
        "on with host and reason" {
            &[
                "managed-host",
                "quarantine",
                "on",
                "--host",
                TEST_MACHINE_ID,
                "--reason",
                "Security issue",
            ][..] => Yields((TEST_MACHINE_ID.parse::<HostMachineId>().unwrap(), "Security issue".to_string())),
        }

        "off with host" {
            &[
                "managed-host",
                "quarantine",
                "off",
                "--host",
                TEST_MACHINE_ID,
            ][..] => Yields((TEST_MACHINE_ID.parse::<HostMachineId>().unwrap(), String::new())),
        }
    );
}

// parse_reset_host_reprovisioning ensures
// reset-host-reprovisioning parses.
#[test]
fn parse_reset_host_reprovisioning() {
    let argv = [
        "managed-host",
        "reset-host-reprovisioning",
        "--machine",
        TEST_MACHINE_ID,
    ];
    let matches = parse_leaf::<Cmd>(&argv, &["reset-host-reprovisioning"])
        .expect("should parse reset-host-reprovisioning");

    assert_eq!(
        matches.get_one::<StableHostMachineId>("machine"),
        Some(&TEST_MACHINE_ID.parse().unwrap())
    );
}

// decommission accepts exactly one managed-host id.
#[test]
fn parse_decommission_lifecycle_commands() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|cmd| match cmd {
                    Cmd::Decommission(args) => ("decommission", args.machine_id.to_string()),
                    _ => panic!("expected managed-host decommission lifecycle command"),
                })
                .map_err(drop)
        };
        "start decommissioning" {
            &["managed-host", "decommission", TEST_MACHINE_ID][..] =>
                Yields(("decommission", TEST_MACHINE_ID.to_string())),
        }
    );
}

// power-options show/update route to the PowerOptions variant. Each row yields
// the converted request's (machine_id, power_state); show has no request.
#[test]
fn parse_power_options_routes_to_power_options() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|cmd| match cmd {
                    Cmd::PowerOptions(power_options::Args::Show(_)) => (None, None),
                    Cmd::PowerOptions(power_options::Args::Update(args)) => {
                        let request: rpc::forge::PowerOptionUpdateRequest = args.into();
                        (request.machine_id, Some(request.power_state))
                    }
                    _ => panic!("expected PowerOptions variant"),
                })
                .map_err(drop)
        };
        "show with no machine" {
            &["managed-host", "power-options", "show"][..] => Yields((None, None)),
        }

        "update with machine and desired power state" {
            &[
                "managed-host",
                "power-options",
                "update",
                TEST_MACHINE_ID,
                "--desired-power-state",
                "on",
            ][..] => Yields((
                Some(TEST_MACHINE_ID.parse::<HostMachineId>().unwrap()),
                Some(rpc::forge::PowerState::On as i32),
            )),
        }
    );
}

#[test]
#[allow(deprecated)]
fn parse_primary_interface_reconciliation_controls() {
    scenarios!(
        run = |(subcommand, target, flag): (&str, &str, Option<&str>)| {
            let mut argv = vec!["managed-host", subcommand, TEST_MACHINE_ID, target];
            argv.extend(flag);
            Cmd::try_parse_from(argv)
                .map(|cmd| match cmd {
                    Cmd::SetPrimaryDpu(args) => {
                        let request: rpc::forge::SetPrimaryDpuRequest = args.into();
                        (request.force_reconcile, request.reboot)
                    }
                    Cmd::SetPrimaryInterface(args) => {
                        let request: rpc::forge::SetPrimaryInterfaceRequest = args.into();
                        (request.force_reconcile, request.reboot)
                    }
                    _ => panic!("expected a primary-interface command"),
                })
                .map_err(|error| error.to_string())
        };
        "DPU default" {
            ("set-primary-dpu", TEST_DPU_ID, None) => Yields((false, false)),
        }
        "DPU force reconcile" {
            ("set-primary-dpu", TEST_DPU_ID, Some("--force-reconcile"))
                => Yields((true, false)),
        }
        "DPU legacy reboot" {
            ("set-primary-dpu", TEST_DPU_ID, Some("--reboot"))
                => Yields((true, true)),
        }
        "interface default" {
            ("set-primary-interface", TEST_INTERFACE_ID, None)
                => Yields((false, false)),
        }
        "interface force reconcile" {
            (
                "set-primary-interface",
                TEST_INTERFACE_ID,
                Some("--force-reconcile"),
            ) => Yields((true, false)),
        }
        "interface legacy reboot" {
            (
                "set-primary-interface",
                TEST_INTERFACE_ID,
                Some("--reboot"),
            ) => Yields((true, true)),
        }
    );
}

// parse_debug_bundle ensures debug-bundle parses with
// required args.
#[test]
fn parse_debug_bundle() {
    let argv = [
        "managed-host",
        "debug-bundle",
        TEST_MACHINE_ID,
        "--start-time",
        "2025-01-01 00:00:00",
    ];
    let matches = parse_leaf::<Cmd>(&argv, &["debug-bundle"]).expect("should parse debug-bundle");

    assert_eq!(
        raw_value(&matches, "host_id").as_deref(),
        Some(TEST_MACHINE_ID)
    );
    assert_eq!(
        raw_value(&matches, "start_time").as_deref(),
        Some("2025-01-01 00:00:00"),
    );
    assert!(!matches.get_flag("utc"));
}

// Every malformed invocation is rejected at parse time.
#[test]
fn invalid_invocations_are_rejected() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "maintenance on without --host and --reference" {
            &["managed-host", "maintenance", "on"][..] => Fails,
        }
    );
}
