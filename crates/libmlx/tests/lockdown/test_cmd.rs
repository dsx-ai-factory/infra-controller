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
use clap::Parser;
use libmlx::lockdown::cmd::args::{Cli, Commands, LockdownAction, OutputFormat};
use libmlx::lockdown::cmd::cmds::run_cli;

// Parse a fully-formed argv into its LockdownAction, panicking on a parse error
// (every caller below hands it a valid command line). The outer `Commands` enum
// has the one `Lockdown` variant, so the destructure is irrefutable.
fn action_of(args: &[&str]) -> LockdownAction {
    let cli = Cli::try_parse_from(args).unwrap();
    let Commands::Lockdown { action } = cli.command;
    action
}

// Name the parsed action's variant -- LockdownAction isn't PartialEq, so a table
// pins the variant by this discriminant rather than by equality.
fn action_kind(action: &LockdownAction) -> &'static str {
    match action {
        LockdownAction::Lock { .. } => "lock",
        LockdownAction::Unlock { .. } => "unlock",
        LockdownAction::Status { .. } => "status",
        LockdownAction::SetKey { .. } => "set-key",
    }
}

#[test]
fn test_cli_parsing() {
    // Test parsing various command line arguments
    let cli = Cli::try_parse_from(["mlxconfig-lockdown", "lockdown", "status", "04:00.0"]).unwrap();

    // Just ensure it parsed without errors
    assert!(matches!(cli.command, Commands::Lockdown { .. }));
}

#[test]
fn test_cli_help() {
    // Help exits with an error code, which clap surfaces as Err -- that's expected.
    let result = Cli::try_parse_from(["mlxconfig-lockdown", "--help"]);
    assert!(result.is_err());
}

// Each lockdown subcommand parses to its matching action variant. Folds the
// per-subcommand parse-and-`matches!` blocks into one table over `action_kind`.
#[test]
fn lockdown_subcommands_parse_to_their_action_variant() {
    value_scenarios!(
        run = |args| action_kind(&action_of(&args));
        "lock" {
            vec![
                "mlxconfig-lockdown",
                "lockdown",
                "lock",
                "04:00.0",
                "12345678",
            ] => "lock",
        }

        "unlock" {
            vec![
                "mlxconfig-lockdown",
                "lockdown",
                "unlock",
                "04:00.0",
                "12345678",
            ] => "unlock",
        }

        "set-key" {
            vec![
                "mlxconfig-lockdown",
                "lockdown",
                "set-key",
                "04:00.0",
                "12345678",
            ] => "set-key",
        }

        "status" {
            vec!["mlxconfig-lockdown", "lockdown", "status", "04:00.0"] => "status",
        }
    );
}

#[test]
fn test_dry_run_flag_parsing() {
    // Test that dry-run flag is parsed correctly
    let LockdownAction::Status { dry_run, .. } = action_of(&[
        "mlxconfig-lockdown",
        "lockdown",
        "status",
        "04:00.0",
        "--dry-run",
    ]) else {
        panic!("expected a Status action");
    };
    assert!(dry_run);
}

// `--format json` / `--format yaml` parse to the matching OutputFormat. The format
// enum isn't PartialEq, so each row pins it by a discriminant name.
#[test]
fn output_format_flag_parses_to_the_named_format() {
    fn status_format(args: &[&str]) -> &'static str {
        let LockdownAction::Status { format, .. } = action_of(args) else {
            panic!("expected a Status action");
        };
        match format {
            OutputFormat::Text => "text",
            OutputFormat::Json => "json",
            OutputFormat::Yaml => "yaml",
        }
    }

    value_scenarios!(
        run = |args| status_format(&args);
        "--format json" {
            vec![
                "mlxconfig-lockdown",
                "lockdown",
                "status",
                "04:00.0",
                "--format",
                "json",
            ] => "json",
        }

        "--format yaml" {
            vec![
                "mlxconfig-lockdown",
                "lockdown",
                "status",
                "04:00.0",
                "--format",
                "yaml",
            ] => "yaml",
        }
    );
}

// device_id and key are parsed as positional arguments, in order. The (device_id,
// key) pair is PartialEq, so the table pins both at once.
#[test]
fn positional_arguments_become_device_id_and_key() {
    fn device_and_key(args: &[&str]) -> (String, String) {
        match action_of(args) {
            LockdownAction::Lock { device_id, key, .. }
            | LockdownAction::Unlock { device_id, key, .. }
            | LockdownAction::SetKey { device_id, key, .. } => (device_id, key),
            LockdownAction::Status { .. } => panic!("expected a keyed action"),
        }
    }

    value_scenarios!(
        run = |args| device_and_key(&args);
        "lock keeps an mst-style device id" {
            vec![
                "mlxconfig-lockdown",
                "lockdown",
                "lock",
                "test:device:id",
                "12345678",
            ] => ("test:device:id".to_string(), "12345678".to_string()),
        }

        "unlock keeps a PCI device id" {
            vec![
                "mlxconfig-lockdown",
                "lockdown",
                "unlock",
                "04:00.0",
                "abcdef01",
            ] => ("04:00.0".to_string(), "abcdef01".to_string()),
        }
    );
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    // run_cli over a parsed command line, dropping the (non-PartialEq) error so the
    // outcome rows only assert succeeds/fails.
    fn run(args: &[&str]) -> Result<(), ()> {
        run_cli(Cli::try_parse_from(args).unwrap()).map_err(drop)
    }

    // Every dry-run command succeeds end-to-end (it just prints what it would run),
    // while a real status against a device flint can't reach fails. Folds the
    // single-command dry-run/fake-device tests and the per-subcommand dry-run loop
    // into one outcome table.
    #[test]
    fn run_cli_dry_runs_succeed_and_real_lookups_fail() {
        scenarios!(
            run = |args| run(&args);
            "status dry-run prints and succeeds" {
                vec![
                    "mlxconfig-lockdown",
                    "lockdown",
                    "status",
                    "fake_device",
                    "--dry-run",
                ] => Yields(()),
            }

            "lock dry-run prints and succeeds" {
                vec![
                    "mlxconfig-lockdown",
                    "lockdown",
                    "lock",
                    "fake_device",
                    "12345678",
                    "--dry-run",
                ] => Yields(()),
            }

            "unlock dry-run prints and succeeds" {
                vec![
                    "mlxconfig-lockdown",
                    "lockdown",
                    "unlock",
                    "fake_device",
                    "12345678",
                    "--dry-run",
                ] => Yields(()),
            }

            "set-key dry-run prints and succeeds" {
                vec![
                    "mlxconfig-lockdown",
                    "lockdown",
                    "set-key",
                    "fake_device",
                    "12345678",
                    "--dry-run",
                ] => Yields(()),
            }

            "real status against a fake device fails (no flint)" {
                vec!["mlxconfig-lockdown", "lockdown", "status", "fake_device"] => Fails,
            }
        );
    }
}
