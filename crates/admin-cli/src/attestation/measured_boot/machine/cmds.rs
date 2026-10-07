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
//! `measurement mock-machine` subcommand dispatcher + backing functions.

use ::rpc::measured_boot::FromGrpcOpt;
use ::rpc::protos::measured_boot::ShowCandidateMachineRequest;
use measured_boot::ToTable;
use measured_boot::machine::CandidateMachine;
use measured_boot::records::CandidateMachineSummary;
use measured_boot::report::MeasurementReport;
use serde::Serialize;

use crate::attestation::measured_boot::machine::args::{Attest, List, Show};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

impl Run for Attest {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            attest(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Show {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        if self.machine_id.is_some() {
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
            list(&ctx.api_client).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

/// attest sends attestation data for the given machine ID, as in, PCR
/// register + value pairings, which results in a journal entry being made.
async fn attest(grpc_conn: &ApiClient, attest: Attest) -> CarbideCliResult<MeasurementReport> {
    let response = grpc_conn.0.attest_candidate_machine(attest).await?;

    MeasurementReport::from_grpc_opt(response.report)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))
}

/// show_by_id shows all info about a given machine ID.
async fn show_by_id(grpc_conn: &ApiClient, show: Show) -> CarbideCliResult<CandidateMachine> {
    let response = grpc_conn
        .0
        .show_candidate_machine(ShowCandidateMachineRequest::try_from(show)?)
        .await?;

    CandidateMachine::from_grpc_opt(response.machine)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))
}

/// show_all shows all info about all machines.
async fn show_all(grpc_conn: &ApiClient, _show: Show) -> CarbideCliResult<CandidateMachineList> {
    Ok(CandidateMachineList(
        grpc_conn
            .0
            .show_candidate_machines()
            .await?
            .machines
            .into_iter()
            .map(|machine| {
                CandidateMachine::try_from(machine)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<CandidateMachine>>>()?,
    ))
}

/// list lists all machine IDs.
async fn list(grpc_conn: &ApiClient) -> CarbideCliResult<CandidateMachineSummaryList> {
    Ok(CandidateMachineSummaryList(
        grpc_conn
            .0
            .list_candidate_machines()
            .await?
            .machines
            .into_iter()
            .map(|machine| {
                CandidateMachineSummary::try_from(machine)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<CandidateMachineSummary>>>()?,
    ))
}

/// CandidateMachineSummaryList just implements a newtype pattern
/// for a Vec<CandidateMachineSummary> so the ToTable trait can
/// be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct CandidateMachineSummaryList(Vec<CandidateMachineSummary>);

impl ToTable for CandidateMachineSummaryList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["machine_id", "created_ts"]);
        for rec in self.0.iter() {
            table.add_row(prettytable::row![rec.machine_id, rec.ts]);
        }
        Ok(table.to_string())
    }
}

/// CandidateMachineList just implements a newtype
/// pattern for a Vec<CandidateMachine> so the ToTable
/// trait can be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct CandidateMachineList(Vec<CandidateMachine>);

impl ToTable for CandidateMachineList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row![
            "machine_id",
            "state",
            "created_ts",
            "updated_ts",
            "journal",
            "attributes",
        ]);
        for record in self.0.iter() {
            let journal_table = match &record.journal {
                Some(journal) => journal.to_nested_prettytable(),
                None => {
                    let mut not_found = prettytable::Table::new();
                    not_found.add_row(prettytable::row!["<no journal found>"]);
                    not_found
                }
            };
            let mut attrs_table = prettytable::Table::new();
            attrs_table.add_row(prettytable::row!["name", "value"]);
            for (key, value) in record.attrs.iter() {
                attrs_table.add_row(prettytable::row![key, value]);
            }
            table.add_row(prettytable::row![
                record.machine_id,
                record.state,
                record.created_ts,
                record.updated_ts,
                journal_table,
                attrs_table,
            ]);
        }
        Ok(table.to_string())
    }
}
