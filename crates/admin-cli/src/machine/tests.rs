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
// ValueEnum Parsing - Test string parsing for types deriving claps ValueEnum.

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use clap::{CommandFactory, Parser};
use metadata::args::Args as MachineMetadataCommand;
use network::args::Args as NetworkCommand;

use self::health_report::args::{Args as HealthReportCommand, HealthReportTemplates};
use super::*;

// Define a basic/working MachineId for testing.
const TEST_MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

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

// `show` parses with no arguments and with each of its flags, capturing the
// machine/all/dpus/hosts state in each case.
#[test]
fn parse_show_variants() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|cmd| match cmd {
                    Cmd::Show(args) => (args.machine.is_some(), args.all, args.dpus, args.hosts),
                    _ => panic!("expected Show variant"),
                })
                .map_err(drop)
        };
        "no arguments (all machines)" {
            &["machine", "show"][..] => Yields((false, false, false, false)),
        }

        "--dpus flag" {
            &["machine", "show", "--dpus"][..] => Yields((false, false, true, false)),
        }

        "--hosts flag" {
            &["machine", "show", "--hosts"][..] => Yields((false, false, false, true)),
        }
    );
}

// network status routes to the Status variant; network config parses with a
// machine ID and exposes it.
#[test]
fn parse_network_status() {
    let cmd =
        Cmd::try_parse_from(["machine", "network", "status"]).expect("should parse network status");

    assert!(matches!(cmd, Cmd::Network(NetworkCommand::Status)));
}

// parse_network_config ensures network config parses
// with machine ID.
#[test]
fn parse_network_config() {
    let cmd = Cmd::try_parse_from([
        "machine",
        "network",
        "config",
        "--machine-id",
        TEST_MACHINE_ID,
    ])
    .expect("should parse network config");

    match cmd {
        Cmd::Network(NetworkCommand::Config(args)) => {
            assert_eq!(args.machine_id.to_string(), TEST_MACHINE_ID);
        }
        _ => panic!("expected Network Config variant"),
    }
}

// health-report show parses, and the legacy health-override alias routes to the
// same Show variant; both expose the machine ID.
#[test]
fn parse_health_report_show_variants() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|cmd| match cmd {
                    Cmd::HealthReport(HealthReportCommand::Show { machine_id }) => {
                        machine_id.to_string()
                    }
                    _ => panic!("expected HealthReport Show variant"),
                })
                .map_err(drop)
        };
        "health-report show" {
            &["machine", "health-report", "show", TEST_MACHINE_ID][..] => Yields(TEST_MACHINE_ID.to_string()),
        }

        "legacy health-override show alias" {
            &["machine", "health-override", "show", TEST_MACHINE_ID][..] => Yields(TEST_MACHINE_ID.to_string()),
        }
    );
}

// parse_health_override_add_with_template ensures the
// legacy health-override alias still parses with template.
#[test]
fn parse_health_override_add_with_template() {
    let cmd = Cmd::try_parse_from([
        "machine",
        "health-override",
        "add",
        TEST_MACHINE_ID,
        "--template",
        "host-update",
    ])
    .expect("should parse health-override add with template");

    match cmd {
        Cmd::HealthReport(HealthReportCommand::Add(args)) => {
            assert!(args.template.is_some());
            assert!(args.health_report.is_none());
        }
        _ => panic!("expected HealthReport Add variant"),
    }
}

// parse_reboot ensures reboot parses with
// machine.
#[test]
fn parse_reboot() {
    let cmd = Cmd::try_parse_from(["machine", "reboot", "--machine", TEST_MACHINE_ID])
        .expect("should parse reboot");

    match cmd {
        Cmd::Reboot(args) => {
            assert_eq!(args.machine, TEST_MACHINE_ID);
        }
        _ => panic!("expected Reboot variant"),
    }
}

// parse_force_delete ensures force-delete parses with
// machine.
#[test]
fn parse_force_delete() {
    let cmd = Cmd::try_parse_from(["machine", "force-delete", "--machine", TEST_MACHINE_ID])
        .expect("should parse force-delete");

    match cmd {
        Cmd::ForceDelete(args) => {
            assert_eq!(args.machine, TEST_MACHINE_ID);
            assert!(!args.delete_interfaces);
            assert!(!args.allow_delete_with_instance);
            assert!(!args.allow_delete_with_orphaned_dpf_crds);
        }
        _ => panic!("expected ForceDelete variant"),
    }
}

// parse_auto_update_enable ensures auto-update parses
// with enable flag.
#[test]
fn parse_auto_update_enable() {
    let cmd = Cmd::try_parse_from([
        "machine",
        "auto-update",
        "--machine",
        TEST_MACHINE_ID,
        "--enable",
    ])
    .expect("should parse auto-update --enable");

    match cmd {
        Cmd::AutoUpdate(args) => {
            assert!(args.enable);
            assert!(!args.disable);
            assert!(!args.clear);
        }
        _ => panic!("expected AutoUpdate variant"),
    }
}

// metadata show exposes the machine ID; metadata set exposes the parsed
// --name option.
#[test]
fn parse_metadata_show() {
    let cmd = Cmd::try_parse_from(["machine", "metadata", "show", TEST_MACHINE_ID])
        .expect("should parse metadata show");

    match cmd {
        Cmd::Metadata(MachineMetadataCommand::Show(args)) => {
            assert_eq!(args.machine.to_string(), TEST_MACHINE_ID);
        }
        _ => panic!("expected Metadata Show variant"),
    }
}

// parse_metadata_set ensures metadata set parses with
// machine ID and options.
#[test]
fn parse_metadata_set() {
    let cmd = Cmd::try_parse_from([
        "machine",
        "metadata",
        "set",
        TEST_MACHINE_ID,
        "--name",
        "MyMachine",
    ])
    .expect("should parse metadata set");

    match cmd {
        Cmd::Metadata(MachineMetadataCommand::Set(args)) => {
            assert_eq!(args.name, Some("MyMachine".to_string()));
        }
        _ => panic!("expected Metadata Set variant"),
    }
}

// parse_positions ensures positions parses with no
// arguments.
#[test]
fn parse_positions() {
    let cmd = Cmd::try_parse_from(["machine", "positions"]).expect("should parse positions");

    match cmd {
        Cmd::Positions(args) => {
            assert!(args.machine.is_empty());
        }
        _ => panic!("expected Positions variant"),
    }
}

/////////////////////////////////////////////////////////////////////////////
// ValueEnum Parsing
//
// These tests are for testing argument values which derive
// ValueEnum, ensuring the string representations of said
// values correctly convert back into their expected variant,
// or fail otherwise.

// Each HealthReportTemplates string round-trips to its variant, and an unknown
// string is rejected. The variant isn't PartialEq, so the closure projects the
// parsed variant through an EXHAUSTIVE `template_name` (no wildcard arm) and each
// row's expected is that same projection applied to the variant it names -- the
// variant, not a hand-copied string, is the source of truth. The exhaustive match
// also means a newly-added HealthReportTemplates variant fails to compile here
// rather than silently slipping past, so it has to be given a string and a row.
#[test]
fn health_override_templates_value_enum() {
    use HealthReportTemplates as T;
    use clap::ValueEnum;

    fn template_name(t: &HealthReportTemplates) -> &'static str {
        match t {
            T::HostUpdate => "host-update",
            T::InternalMaintenance => "internal-maintenance",
            T::OutForRepair => "out-for-repair",
            T::ComputeTrayDueForSwap => "compute-tray-due-for-swap",
            T::Degraded => "degraded",
            T::Validation => "validation",
            T::SuppressExternalAlerting => "suppress-external-alerting",
            T::MarkHealthy => "mark-healthy",
            T::StopRebootForAutomaticRecoveryFromStateMachine => {
                "stop-reboot-for-automatic-recovery-from-state-machine"
            }
            T::TenantReportedIssue => "tenant-reported-issue",
            T::RequestOnlineRepair => "request-online-repair",
            T::RequestRepair => "request-repair",
        }
    }

    scenarios!(
        run = |s| {
            HealthReportTemplates::from_str(s, false)
                .map(|t| template_name(&t))
                .map_err(drop)
        };
        "host-update" {
            "host-update" => Yields(template_name(&T::HostUpdate)),
        }

        "internal-maintenance" {
            "internal-maintenance" => Yields(template_name(&T::InternalMaintenance)),
        }

        "out-for-repair" {
            "out-for-repair" => Yields(template_name(&T::OutForRepair)),
        }

        "compute-tray-due-for-swap" {
            "compute-tray-due-for-swap" => Yields(template_name(&T::ComputeTrayDueForSwap)),
        }

        "degraded" {
            "degraded" => Yields(template_name(&T::Degraded)),
        }

        "validation" {
            "validation" => Yields(template_name(&T::Validation)),
        }

        "suppress-external-alerting" {
            "suppress-external-alerting" => Yields(template_name(&T::SuppressExternalAlerting)),
        }

        "mark-healthy" {
            "mark-healthy" => Yields(template_name(&T::MarkHealthy)),
        }

        "stop-reboot-for-automatic-recovery-from-state-machine" {
            "stop-reboot-for-automatic-recovery-from-state-machine" =>
                Yields(template_name(&T::StopRebootForAutomaticRecoveryFromStateMachine)),
        }

        "tenant-reported-issue" {
            "tenant-reported-issue" => Yields(template_name(&T::TenantReportedIssue)),
        }

        "request-online-repair" {
            "request-online-repair" => Yields(template_name(&T::RequestOnlineRepair)),
        }

        "request-repair" {
            "request-repair" => Yields(template_name(&T::RequestRepair)),
        }

        "invalid string is rejected" {
            "invalid" => Fails,
        }
    );
}
