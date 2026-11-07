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

//!
//! `measurement journal` subcommand dispatcher + backing functions.
use ::rpc::measured_boot::{FromGrpc, FromGrpcOpt};
use ::rpc::protos::measured_boot::ShowMeasurementJournalRequest;
use measured_boot::bundle::MeasurementBundle;
use measured_boot::journal::MeasurementJournal;
use measured_boot::records::MeasurementJournalRecord;
use measured_boot::{ToTable, just_print_summary};
use serde::Serialize;

use crate::attestation::measured_boot::journal::args::{Delete, List, Promote, Show};
use crate::attestation::measured_boot::report::args::Promote as ReportPromoteArgs;
use crate::attestation::measured_boot::report::cmds::promote as report_promote;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

impl Run for Delete {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            delete(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Show {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        if self.journal_id.is_some() {
            crate::cli_output(
                show_by_id(&ctx.api_client, self).await?,
                &ctx.config.format,
                crate::Destination::Stdout(),
            )
        } else {
            crate::cli_output(
                show_all(&ctx.api_client, self).await?,
                &ctx.config.format,
                crate::Destination::Stdout(),
            )
        }
    }
}

impl Run for List {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Promote {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            promote(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

/// delete deletes an existing journal entry.
///
/// `journal delete <journal-id>`
async fn delete(grpc_conn: &ApiClient, delete: Delete) -> CarbideCliResult<MeasurementJournal> {
    let response = grpc_conn.0.delete_measurement_journal(delete).await?;

    MeasurementJournal::from_grpc_opt(response.journal)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))
}

/// show_by_id shows all info about a journal entry for the provided ID.
///
/// `journal show <journal-id>`
async fn show_by_id(grpc_conn: &ApiClient, show: Show) -> CarbideCliResult<MeasurementJournal> {
    let response = grpc_conn
        .0
        .show_measurement_journal(ShowMeasurementJournalRequest::try_from(show)?)
        .await?;

    MeasurementJournal::from_grpc_opt(response.journal)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))
}

/// show_all shows all info about all journal entries.
///
/// `journal show`
async fn show_all(grpc_conn: &ApiClient, _show: Show) -> CarbideCliResult<MeasurementJournalList> {
    Ok(MeasurementJournalList(
        grpc_conn
            .0
            .show_measurement_journals()
            .await?
            .journals
            .drain(..)
            .map(|journal| {
                MeasurementJournal::from_grpc(journal)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<MeasurementJournal>>>()?,
    ))
}

/// list just lists all journal IDs.
///
/// `journal list`
async fn list(grpc_conn: &ApiClient, list: List) -> CarbideCliResult<MeasurementJournalRecordList> {
    Ok(MeasurementJournalRecordList(
        grpc_conn
            .0
            .list_measurement_journal(list)
            .await?
            .journals
            .drain(..)
            .map(|journal| {
                MeasurementJournalRecord::try_from(journal)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<MeasurementJournalRecord>>>()?,
    ))
}

/// promote promotes the report from a journal entry into
/// a measurement bundle.
///
/// Instead of its own dedicated API call, this makes two API
/// calls: one to get the journal data, and the other to promote
/// the report associated with the journal.
///
/// `journal promote <journal-id> [--pcr-registers <range0>,...]`
async fn promote(grpc_conn: &ApiClient, promote: Promote) -> CarbideCliResult<MeasurementBundle> {
    let journal = show_by_id(
        grpc_conn,
        Show {
            journal_id: Some(promote.journal_id),
        },
    )
    .await?;

    report_promote(
        grpc_conn,
        ReportPromoteArgs {
            report_id: journal.report_id,
            pcr_registers: promote.pcr_registers,
        },
    )
    .await
}

/// MeasurementJournalRecordList just implements a newtype pattern
/// for a Vec<MeasurementJournalRecord> so the ToTable trait can
/// be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementJournalRecordList(Vec<MeasurementJournalRecord>);

impl ToTable for MeasurementJournalRecordList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        if just_print_summary() {
            table.add_row(prettytable::row![
                "journal_id",
                "machine_id",
                "report_id",
                "state",
                "created_ts"
            ]);
        } else {
            table.add_row(prettytable::row![
                "journal_id",
                "machine_id",
                "report_id",
                "profile_id",
                "bundle_id",
                "state",
                "created_ts"
            ]);
        }
        for journal in self.0.iter() {
            let profile_id: String = match journal.profile_id {
                Some(profile_id) => profile_id.to_string(),
                None => "<none>".to_string(),
            };
            let bundle_id: String = match journal.bundle_id {
                Some(bundle_id) => bundle_id.to_string(),
                None => "<none>".to_string(),
            };
            if just_print_summary() {
                table.add_row(prettytable::row![
                    journal.journal_id,
                    journal.machine_id,
                    journal.report_id,
                    journal.state,
                    journal.ts
                ]);
            } else {
                table.add_row(prettytable::row![
                    journal.journal_id,
                    journal.machine_id,
                    journal.report_id,
                    profile_id,
                    bundle_id,
                    journal.state,
                    journal.ts
                ]);
            }
        }
        Ok(table.to_string())
    }
}

/// MeasurementJournalList just implements a newtype
/// pattern for a Vec<MeasurementJournal> so the ToTable
/// trait can be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementJournalList(Vec<MeasurementJournal>);

// When `journal show` gets called (for all entries), and the output format
// is the default table view, this gets used to print a pretty table.
impl ToTable for MeasurementJournalList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["journal_id", "details"]);
        for journal in self.0.iter() {
            let profile_id: String = match journal.profile_id {
                Some(profile_id) => profile_id.to_string(),
                None => "<none>".to_string(),
            };
            let bundle_id: String = match journal.bundle_id {
                Some(bundle_id) => bundle_id.to_string(),
                None => "<none>".to_string(),
            };
            let mut details_table = prettytable::Table::new();
            details_table.add_row(prettytable::row!["machine_id", journal.machine_id]);
            details_table.add_row(prettytable::row!["report_id", journal.report_id]);
            if !just_print_summary() {
                details_table.add_row(prettytable::row!["profile_id", profile_id]);
                details_table.add_row(prettytable::row!["bundle_id", bundle_id]);
            }
            details_table.add_row(prettytable::row!["state", journal.state]);
            details_table.add_row(prettytable::row!["created_ts", journal.ts]);
            table.add_row(prettytable::row![journal.journal_id, details_table,]);
        }
        Ok(table.to_string())
    }
}
