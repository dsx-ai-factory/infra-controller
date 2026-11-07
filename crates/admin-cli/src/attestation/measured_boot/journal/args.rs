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
 *  Measured Boot CLI arguments for the `measurement journal` subcommand.
 *
 * This provides the CLI subcommands and arguments for:
 *  - `journal delete`: Delete a journal entry.
 *  - `journal show`: Show all info about journal entr(ies).
 *  - `journal list`: List all journal entries.
 *  - `journal promote`: Promote the report from a journal entry into a bundle.
 */

use ::rpc::protos::measured_boot::{
    DeleteMeasurementJournalRequest, ListMeasurementJournalRequest, ShowMeasurementJournalRequest,
    list_measurement_journal_request, show_measurement_journal_request,
};
use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::MeasurementJournalId;
use clap::Parser;
use measured_boot::pcr::{PcrSet, parse_pcr_index_input};

use crate::cfg::dispatch::Dispatch;
use crate::errors::CarbideCliError;

/// CmdJournal provides a container for the `journal` subcommand, which itself
/// contains other subcommands for working with journals.
#[derive(Parser, Debug, Dispatch)]
pub(crate) enum CmdJournal {
    #[clap(about = "Delete a journal entry.", visible_alias = "d")]
    Delete(Delete),

    #[clap(about = "Show a journal entry by ID, or all.", visible_alias = "s")]
    Show(Show),

    #[clap(about = "List all journal IDs and machines.", visible_alias = "l")]
    List(List),

    #[clap(
        about = "Promote a journal entry report to a bundle.",
        visible_alias = "p"
    )]
    Promote(Promote),
}

/// Delete is used to delete an existing journal entry.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Delete a journal entry by ID:
    $ nico-admin-cli attestation measured-boot journal delete \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Delete {
    #[clap(help = "The journal ID to delete.")]
    journal_id: MeasurementJournalId,
}

/// List is used to list all journal entry IDs.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all journal entries:
    $ nico-admin-cli attestation measured-boot journal list

List journal entries for a single machine:
    $ nico-admin-cli attestation measured-boot journal list \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct List {
    #[clap(help = "List journal entries for a machine ID.")]
    machine_id: Option<MachineId>,
}

/// Show is used to show a journal entry based on ID, or all entries
/// if no ID is provided.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show all journal entries:
    $ nico-admin-cli attestation measured-boot journal show

Show one journal entry by ID:
    $ nico-admin-cli attestation measured-boot journal show \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Show {
    #[clap(help = "The optional journal entry ID.")]
    pub(super) journal_id: Option<MeasurementJournalId>,
}

/// Promote is used to promote a journal entry's report
/// into a measurement bundle.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Promote a journal entry's report into a bundle:
    $ nico-admin-cli attestation measured-boot journal promote \
    12345678-1234-5678-90ab-cdef01234567

Promote only specific PCR registers (indices and ranges):
    $ nico-admin-cli attestation measured-boot journal promote \
    12345678-1234-5678-90ab-cdef01234567 --pcr-registers 0,7,11-14

")]
pub(crate) struct Promote {
    #[clap(help = "The journal entry ID to promote a report from.")]
    pub(super) journal_id: MeasurementJournalId,

    #[clap(
        long,
        help = "Select specific PCR range(s) to use for the promoted bundle."
    )]
    #[arg(value_parser = parse_pcr_index_input)]
    pub(super) pcr_registers: Option<PcrSet>,
}

impl From<Delete> for DeleteMeasurementJournalRequest {
    fn from(delete: Delete) -> Self {
        Self {
            journal_id: Some(delete.journal_id),
        }
    }
}

impl TryFrom<Show> for ShowMeasurementJournalRequest {
    type Error = CarbideCliError;
    fn try_from(show: Show) -> Result<Self, Self::Error> {
        let journal_id = show
            .journal_id
            .ok_or(CarbideCliError::GenericError(String::from(
                "journal_id must be set to get a journal",
            )))?;
        Ok(Self {
            selector: Some(show_measurement_journal_request::Selector::JournalId(
                journal_id,
            )),
        })
    }
}

impl From<List> for ListMeasurementJournalRequest {
    fn from(list: List) -> Self {
        Self {
            selector: list.machine_id.map(|machine_id| {
                list_measurement_journal_request::Selector::MachineId(machine_id.to_string())
            }),
        }
    }
}
