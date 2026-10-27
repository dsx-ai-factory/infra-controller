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

use std::str::FromStr;

use carbide_uuid::machine::MachineId;
use clap::{Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    about = "Configure maintenance mode for Spectrum-X planes on a machine",
    override_usage = "nico-admin-cli spx-maintenance --machine-id <MACHINE_ID> multiplane rail <0-3|START-END|all> plane <0-7|START-END|all> <on|off> [--reference <REFERENCE>]\n       nico-admin-cli spx-maintenance --machine-id <MACHINE_ID> show",
    long_about = "Configure maintenance mode for Spectrum-X planes on a machine. Each rail has \
        two SuperNICs: planes 0-3 belong to one, and planes 4-7 belong to the other. \
        Select a single value, an inclusive ascending START-END range, or all for each rail and plane selector. \
        A single-plane selection leaves the other planes unchanged. When partial-multiplane is enabled, the DPU flavor initializes \
        planes in maintenance mode. Until you send a CLI or REST request, the agent observes that state without changing it. Maintenance mode on disables a plane; maintenance mode off enables it.",
    after_long_help = "\
EXAMPLES:

Enable maintenance mode on plane 6 on rail 2 (eth_r2_p6):
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail 2 plane 6 on --reference SPX-42

Disable maintenance mode on plane 6 on rail 2:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail 2 plane 6 off

Enable maintenance mode on plane 6 on all four rails:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail all plane 6 on

Disable maintenance mode on all eight planes on rail 2:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail 2 plane all off

Disable maintenance mode on planes 1 through 4 on rails 0 through 2:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail 0-2 plane 1-4 off

Disable maintenance mode on all planes on all rails:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail all plane all off

Show host requests and per-DPU maintenance state for a machine:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 show

"
)]
pub(crate) struct Args {
    #[arg(long, value_name = "MACHINE_ID", help = "Machine ID of the host")]
    pub(super) machine_id: MachineId,
    #[command(subcommand)]
    pub(super) action: Action,
}

#[derive(Parser, Debug)]
pub(super) enum Action {
    #[command(
        about = "Turn maintenance mode on or off for the selected planes",
        override_usage = "nico-admin-cli spx-maintenance --machine-id <MACHINE_ID> multiplane rail <0-3|START-END|all> plane <0-7|START-END|all> <on|off> [--reference <REFERENCE>]",
        after_long_help = "\
EXAMPLES:

Enable maintenance mode on plane 6 on rail 2 with a ticket reference:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail 2 plane 6 on --reference SPX-42

Disable maintenance mode on plane 6 on rail 2:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail 2 plane 6 off

Enable maintenance mode on plane 6 on all four rails without changing plane 2:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail all plane 6 on

Disable maintenance mode on all eight planes on rail 2:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail 2 plane all off

Disable maintenance mode on planes 1 through 4 on rails 0 through 2:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail 0-2 plane 1-4 off

Disable maintenance mode on all planes on all rails:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    multiplane rail all plane all off

"
    )]
    Multiplane {
        #[arg(value_enum, value_name = "rail", help = "The literal keyword rail")]
        rail_keyword: RailKeyword,
        #[arg(
            value_name = "0-3|START-END|all",
            help = "Rail number (0-3), inclusive ascending range (for example, 0-2), or all four rails"
        )]
        rail: RailSelector,
        #[arg(value_enum, value_name = "plane", help = "The literal keyword plane")]
        plane_keyword: PlaneKeyword,
        #[arg(
            value_name = "0-7|START-END|all",
            help = "Plane number (0-7), inclusive ascending range (for example, 1-4), or all eight planes on each selected rail"
        )]
        plane: PlaneSelector,
        #[arg(
            value_enum,
            value_name = "on|off",
            help = "Maintenance mode on disables the plane; maintenance mode off enables it"
        )]
        mode: MaintenanceMode,
        #[arg(
            long,
            value_name = "REFERENCE",
            help = "Omit to preserve the saved reference; pass a value to replace it or an empty string to clear it"
        )]
        reference: Option<String>,
    },
    #[command(
        about = "Show host maintenance configuration and the status of each DPU",
        long_about = "Show host maintenance configuration and the status of each DPU. The Config updated at (UTC) column shows the last configuration change timestamp encoded in the desired revision. Unset maintenance modes show a dash because their revision only records initialization.",
        override_usage = "nico-admin-cli spx-maintenance --machine-id <MACHINE_ID> show",
        after_long_help = "\
EXAMPLES:

Show host requests and per-DPU maintenance state for a machine:
    $ nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 show

"
    )]
    Show,
}

// Literal positional keywords keep the public command flat while checking its syntax.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum RailKeyword {
    Rail,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum PlaneKeyword {
    Plane,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RailSelector {
    One(u8),
    Range { start: u8, end: u8 },
    All,
}

impl FromStr for RailSelector {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "all" {
            return Ok(Self::All);
        }
        if let Some((start, end)) = parse_range(value, 3) {
            return Ok(Self::Range { start, end });
        }
        match value.parse::<u8>() {
            Ok(rail @ 0..=3) => Ok(Self::One(rail)),
            _ => Err(
                "rail must be 0-3, an inclusive ascending range such as 0-2, or all".to_string(),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlaneSelector {
    One(u8),
    Range { start: u8, end: u8 },
    All,
}

impl FromStr for PlaneSelector {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "all" {
            return Ok(Self::All);
        }
        if let Some((start, end)) = parse_range(value, 7) {
            return Ok(Self::Range { start, end });
        }
        match value.parse::<u8>() {
            Ok(plane @ 0..=7) => Ok(Self::One(plane)),
            _ => Err(
                "plane must be 0-7, an inclusive ascending range such as 1-4, or all".to_string(),
            ),
        }
    }
}

fn parse_range(value: &str, maximum: u8) -> Option<(u8, u8)> {
    let (start, end) = value.split_once('-')?;
    let start = start.parse::<u8>().ok()?;
    let end = end.parse::<u8>().ok()?;
    (start <= end && end <= maximum).then_some((start, end))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum MaintenanceMode {
    On,
    Off,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases};
    use clap::Parser;
    use clap::error::ErrorKind;

    use super::*;
    use crate::cfg::cli_options::{CliCommand, CliOptions};

    const MACHINE: &str = "fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80";

    fn parse(arguments: &str) -> Result<Args, ErrorKind> {
        let options = CliOptions::try_parse_from(
            ["nico-admin-cli", "spx-maintenance"]
                .into_iter()
                .chain(arguments.split_whitespace()),
        )
        .map_err(|error| error.kind())?;
        match options.commands {
            Some(CliCommand::SpxMaintenance(args)) => Ok(args),
            _ => panic!("public CLI did not route to spx-maintenance"),
        }
    }

    #[test]
    fn public_command_preserves_selected_rail_plane_and_mode() {
        check_cases(
            [
                Case {
                    scenario: "first rail and plane, maintenance on",
                    input: "multiplane rail 0 plane 0 on",
                    expect: Yields((
                        RailSelector::One(0),
                        PlaneSelector::One(0),
                        MaintenanceMode::On,
                        None,
                    )),
                },
                Case {
                    scenario: "last rail and plane, maintenance off with reference",
                    input: "multiplane rail 3 plane 7 off --reference SPX-42",
                    expect: Yields((
                        RailSelector::One(3),
                        PlaneSelector::One(7),
                        MaintenanceMode::Off,
                        Some("SPX-42".to_string()),
                    )),
                },
                Case {
                    scenario: "all rails, only specified plane",
                    input: "multiplane rail all plane 6 on",
                    expect: Yields((
                        RailSelector::All,
                        PlaneSelector::One(6),
                        MaintenanceMode::On,
                        None,
                    )),
                },
                Case {
                    scenario: "all planes on one rail",
                    input: "multiplane rail 2 plane all off",
                    expect: Yields((
                        RailSelector::One(2),
                        PlaneSelector::All,
                        MaintenanceMode::Off,
                        None,
                    )),
                },
                Case {
                    scenario: "all planes on all rails",
                    input: "multiplane rail all plane all on",
                    expect: Yields((
                        RailSelector::All,
                        PlaneSelector::All,
                        MaintenanceMode::On,
                        None,
                    )),
                },
                Case {
                    scenario: "inclusive rail and plane ranges",
                    input: "multiplane rail 0-2 plane 1-4 off",
                    expect: Yields((
                        RailSelector::Range { start: 0, end: 2 },
                        PlaneSelector::Range { start: 1, end: 4 },
                        MaintenanceMode::Off,
                        None,
                    )),
                },
                Case {
                    scenario: "equal bounds select one value",
                    input: "multiplane rail 3-3 plane 7-7 on",
                    expect: Yields((
                        RailSelector::Range { start: 3, end: 3 },
                        PlaneSelector::Range { start: 7, end: 7 },
                        MaintenanceMode::On,
                        None,
                    )),
                },
            ],
            |selection| {
                let args = parse(&format!("--machine-id {MACHINE} {selection}"))?;
                assert_eq!(args.machine_id, MACHINE.parse::<MachineId>().unwrap());
                match args.action {
                    Action::Multiplane {
                        rail,
                        plane,
                        mode,
                        reference,
                        ..
                    } => Ok::<_, ErrorKind>((rail, plane, mode, reference)),
                    Action::Show => panic!("maintenance invocation routed to show"),
                }
            },
        );
    }

    #[test]
    fn public_command_rejects_invalid_selectors_and_incomplete_invocations() {
        check_cases(
            [
                Case {
                    scenario: "rail outside 0-3",
                    input: format!("--machine-id {MACHINE} multiplane rail 4 plane 6 on"),
                    expect: Yields(ErrorKind::ValueValidation),
                },
                Case {
                    scenario: "plane outside 0-7",
                    input: format!("--machine-id {MACHINE} multiplane rail 2 plane 8 on"),
                    expect: Yields(ErrorKind::ValueValidation),
                },
                Case {
                    scenario: "invalid maintenance mode",
                    input: format!("--machine-id {MACHINE} multiplane rail 2 plane 6 true"),
                    expect: Yields(ErrorKind::InvalidValue),
                },
                Case {
                    scenario: "descending rail range",
                    input: format!("--machine-id {MACHINE} multiplane rail 2-0 plane 1-4 off"),
                    expect: Yields(ErrorKind::ValueValidation),
                },
                Case {
                    scenario: "out of bounds rail range",
                    input: format!("--machine-id {MACHINE} multiplane rail 0-4 plane all off"),
                    expect: Yields(ErrorKind::ValueValidation),
                },
                Case {
                    scenario: "out of bounds plane range",
                    input: format!("--machine-id {MACHINE} multiplane rail all plane 0-8 off"),
                    expect: Yields(ErrorKind::ValueValidation),
                },
                Case {
                    scenario: "missing range endpoint",
                    input: format!("--machine-id {MACHINE} multiplane rail 0- plane 0 off"),
                    expect: Yields(ErrorKind::ValueValidation),
                },
                Case {
                    scenario: "malformed plane range",
                    input: format!("--machine-id {MACHINE} multiplane rail all plane 0-2-4 off"),
                    expect: Yields(ErrorKind::ValueValidation),
                },
                Case {
                    scenario: "invalid rail keyword",
                    input: format!("--machine-id {MACHINE} multiplane rails 2 plane 6 on"),
                    expect: Yields(ErrorKind::InvalidValue),
                },
                Case {
                    scenario: "invalid plane keyword",
                    input: format!("--machine-id {MACHINE} multiplane rail 2 planes 6 on"),
                    expect: Yields(ErrorKind::InvalidValue),
                },
                Case {
                    scenario: "missing machine",
                    input: "multiplane rail 2 plane 6 on".to_string(),
                    expect: Yields(ErrorKind::MissingRequiredArgument),
                },
                Case {
                    scenario: "missing maintenance mode",
                    input: format!("--machine-id {MACHINE} multiplane rail 2 plane 6"),
                    expect: Yields(ErrorKind::MissingRequiredArgument),
                },
                Case {
                    scenario: "missing rail selector",
                    input: format!("--machine-id {MACHINE} multiplane rail plane 6 on"),
                    expect: Yields(ErrorKind::ValueValidation),
                },
                Case {
                    scenario: "missing plane selector",
                    input: format!("--machine-id {MACHINE} multiplane rail 2 plane"),
                    expect: Yields(ErrorKind::MissingRequiredArgument),
                },
                Case {
                    scenario: "invalid machine ID",
                    input: "--machine-id not-a-machine multiplane rail 2 plane 6 on".to_string(),
                    expect: Yields(ErrorKind::ValueValidation),
                },
            ],
            |arguments| {
                Ok::<_, ()>(parse(&arguments).expect_err("invalid invocation was accepted"))
            },
        );
    }

    #[test]
    fn maintenance_help_includes_machine_and_both_selectors() {
        let error = CliOptions::try_parse_from([
            "nico-admin-cli",
            "spx-maintenance",
            "--machine-id",
            MACHINE,
            "multiplane",
            "--help",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(help.contains(
            "--machine-id <MACHINE_ID> multiplane rail <0-3|START-END|all> plane <0-7|START-END|all> <on|off>"
        ));
        assert!(help.contains("EXAMPLES:"));
    }

    #[test]
    fn show_parses_for_selected_machine() {
        let args = parse(&format!("--machine-id {MACHINE} show")).unwrap();
        assert!(matches!(args.action, Action::Show));
        assert_eq!(args.machine_id, MACHINE.parse::<MachineId>().unwrap());
    }
}
