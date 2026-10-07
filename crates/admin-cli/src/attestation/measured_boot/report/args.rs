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

/*!
 *  Measured Boot CLI arguments for the `measurement report` subcommand.
 *
 * This provides the CLI subcommands and arguments for:
 *  - `report create`: Create a new machine measurement report.
 *  - `report delete`: Delete an existing machine measurement report.
 *  - `report promote`: Promote a machine measurement report to a bundle.
 *  - `report revoke`: Create a revoked measurement bundle from a report.
 *  - `report show all`: Show all info about all measurement reports.
 *  - `report show id`: Show all info about a specific report.
 *  - `report show machine`: Show all info about reports for a given machine.
 *  - `report list all`: List high level info about all reports.
 *  - `report list machine`: List all reports for a given machine.
 *  - `report match``
 */

use ::rpc::protos::measured_boot::{
    CreateMeasurementReportRequest, DeleteMeasurementReportRequest, ListMeasurementReportRequest,
    MatchMeasurementReportRequest, PromoteMeasurementReportRequest, RevokeMeasurementReportRequest,
    ShowMeasurementReportForIdRequest, ShowMeasurementReportsForMachineRequest,
    list_measurement_report_request,
};
use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::MeasurementReportId;
use clap::Parser;
use measured_boot::pcr::{PcrRegisterValue, PcrSet, parse_pcr_index_input};

use crate::cfg::dispatch::Dispatch;
use crate::cfg::measurement::parse_pcr_register_values;

// CmdReport provides a container for the `report`
// subcommand, which itself contains other subcommands
// for working with reports.
#[derive(Parser, Debug, Dispatch)]
pub(crate) enum CmdReport {
    #[clap(
        about = "Create a new report with a given config.",
        visible_alias = "c"
    )]
    Create(Create),

    #[clap(about = "Delete a report by ID.", visible_alias = "d")]
    Delete(Delete),

    #[clap(
        about = "Promote a specific journal entry to an active bundle",
        visible_alias = "p"
    )]
    Promote(Promote),

    #[clap(
        about = "Mark a specific journal entry as a revoked bundle.",
        visible_alias = "r"
    )]
    Revoke(Revoke),

    #[clap(
        subcommand,
        about = "Show reports in different ways.",
        visible_alias = "s"
    )]
    #[dispatch]
    Show(ShowFor),

    #[clap(
        subcommand,
        about = "List reports by various ways.",
        visible_alias = "l"
    )]
    #[dispatch]
    List(List),

    #[clap(
        about = "Match reports with the provided PCR register values.",
        visible_alias = "m"
    )]
    Match(Match),
}

/// Create is used for creating reports, which really
/// should be happening during machine attestation.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create a measurement report for a machine with two PCR values:
    $ nico-admin-cli attestation measured-boot report create \
    12345678-1234-5678-90ab-cdef01234567 0:abc123,7:def456

")]
pub(crate) struct Create {
    #[clap(help = "The machine ID of the machine to associate this report with.")]
    machine_id: MachineId,

    #[clap(
        required = true,
        use_value_delimiter = true,
        value_delimiter = ',',
        help = "Comma-separated list of {pcr_register:value,...} to associate with this report."
    )]
    #[arg(value_parser = parse_pcr_register_values)]
    values: Vec<PcrRegisterValue>,
}

/// Delete a profile by ID.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Delete a report by ID:
    $ nico-admin-cli attestation measured-boot report delete \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Delete {
    #[clap(help = "The report ID.")]
    report_id: MeasurementReportId,
}

/// Promote is used to promote a report to a measurement bundle,
/// with the ability to select which PCR registers to select from the
/// report to use for creating the new bundle.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Promote a report into an active bundle:
    $ nico-admin-cli attestation measured-boot report promote \
    12345678-1234-5678-90ab-cdef01234567

Promote a report using only specific PCR registers:
    $ nico-admin-cli attestation measured-boot report promote \
    12345678-1234-5678-90ab-cdef01234567 --pcr-registers 0,7,11-14

")]
pub(crate) struct Promote {
    #[clap(help = "The report ID to promote.")]
    pub(in crate::attestation::measured_boot) report_id: MeasurementReportId,

    #[clap(
        long,
        help = "Select a specific PCR range to use for the promoted bundle."
    )]
    #[arg(value_parser = parse_pcr_index_input)]
    pub(in crate::attestation::measured_boot) pcr_registers: Option<PcrSet>,
}

/// Revoke is used to mark a report as a revoked measurement bundle,
/// with the ability to select which PCR registers to select from the
/// report to use for creating the new (and revoked) bundle.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create a revoked bundle from a report:
    $ nico-admin-cli attestation measured-boot report revoke \
    12345678-1234-5678-90ab-cdef01234567

Revoke using only specific PCR registers:
    $ nico-admin-cli attestation measured-boot report revoke \
    12345678-1234-5678-90ab-cdef01234567 --pcr-registers 0,7,11-14

")]
pub(crate) struct Revoke {
    #[clap(help = "The report ID to revoke.")]
    report_id: MeasurementReportId,

    #[clap(
        long,
        help = "Select a specific PCR range to use for the revoked bundle."
    )]
    #[arg(value_parser = parse_pcr_index_input)]
    pcr_registers: Option<PcrSet>,
}

/// Show a report for an ID, reports for a machine, or all reports.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

Show all reports:
    $ nico-admin-cli attestation measured-boot report show all

Show one report by ID:
    $ nico-admin-cli attestation measured-boot report show id \
    12345678-1234-5678-90ab-cdef01234567

Show all reports for a machine:
    $ nico-admin-cli attestation measured-boot report show machine \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum ShowFor {
    #[clap(about = "Show a report ID.")]
    Id(ShowForId),

    #[clap(about = "Show reports for a machine.")]
    Machine(ShowForMachine),

    #[clap(about = "Show all reports.")]
    All(ShowForAll),
}

/// ShowForAll shows all reports.
#[derive(Parser, Debug)]
pub(crate) struct ShowForAll {}

/// Show a report for the given ID.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show one report by ID:
    $ nico-admin-cli attestation measured-boot report show id \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct ShowForId {
    #[clap(help = "The report ID.")]
    report_id: MeasurementReportId,
}

/// Show all reports for a machine.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show all reports for a machine:
    $ nico-admin-cli attestation measured-boot report show machine \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct ShowForMachine {
    #[clap(help = "The profile name.")]
    machine_id: String,
}

/// List provides a few ways to list things.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

List all reports:
    $ nico-admin-cli attestation measured-boot report list all

List all reports for a machine:
    $ nico-admin-cli attestation measured-boot report list machines \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum List {
    #[clap(about = "List all reports", visible_alias = "a")]
    All(ListAll),

    #[clap(
        about = "List all reports for a given machine ID.",
        visible_alias = "m"
    )]
    Machines(ListMachines),
}

/// ListAll will list all profiles.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all reports:
    $ nico-admin-cli attestation measured-boot report list all

")]
pub(crate) struct ListAll {}

/// ListMachines will list all machines matching this report.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all reports for a machine:
    $ nico-admin-cli attestation measured-boot report list machines \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct ListMachines {
    #[clap(help = "The machine ID.")]
    machine_id: MachineId,
}

/// Match is used for finding reports matching the provided PCR pairs.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Find reports matching a set of PCR register values:
    $ nico-admin-cli attestation measured-boot report match 0:abc123,7:def456

")]
pub(crate) struct Match {
    #[clap(
        required = true,
        use_value_delimiter = true,
        value_delimiter = ',',
        help = "Comma-separated list of {pcr_register:value,...} to match on."
    )]
    #[arg(value_parser = parse_pcr_register_values)]
    values: Vec<PcrRegisterValue>,
}

impl From<Create> for CreateMeasurementReportRequest {
    fn from(create: Create) -> Self {
        Self {
            machine_id: create.machine_id.to_string(),
            pcr_values: create.values.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<Delete> for DeleteMeasurementReportRequest {
    fn from(delete: Delete) -> Self {
        Self {
            report_id: Some(delete.report_id),
        }
    }
}

impl From<Promote> for PromoteMeasurementReportRequest {
    fn from(promote: Promote) -> Self {
        Self {
            report_id: Some(promote.report_id),
            pcr_registers: match &promote.pcr_registers {
                None => "".to_string(),
                Some(pcr_set) => pcr_set.to_string(),
            },
        }
    }
}

impl From<Revoke> for RevokeMeasurementReportRequest {
    fn from(revoke: Revoke) -> Self {
        Self {
            report_id: Some(revoke.report_id),
            pcr_registers: match &revoke.pcr_registers {
                None => "".to_string(),
                Some(pcr_set) => pcr_set.to_string(),
            },
        }
    }
}

impl From<ShowForId> for ShowMeasurementReportForIdRequest {
    fn from(show_for_id: ShowForId) -> Self {
        Self {
            report_id: Some(show_for_id.report_id),
        }
    }
}

impl From<ShowForMachine> for ShowMeasurementReportsForMachineRequest {
    fn from(show_for_machine: ShowForMachine) -> Self {
        Self {
            machine_id: show_for_machine.machine_id,
        }
    }
}

impl From<ListMachines> for ListMeasurementReportRequest {
    fn from(list_machines: ListMachines) -> Self {
        Self {
            selector: Some(list_measurement_report_request::Selector::MachineId(
                list_machines.machine_id.to_string(),
            )),
        }
    }
}

impl From<Match> for MatchMeasurementReportRequest {
    fn from(match_args: Match) -> Self {
        Self {
            pcr_values: match_args.values.into_iter().map(Into::into).collect(),
        }
    }
}
