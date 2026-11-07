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

mod coverage;
mod job;
mod machines;
mod profile;
mod trigger;

use clap::Parser;
use prettytable::Table;
use rpc::admin_cli::OutputFormat;
use serde::Serialize;

use crate::cfg::dispatch::Dispatch;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::{async_write, async_writeln};

/// Writes one command's result: the table it built, or the same view
/// serialized. A view spells the wire enums the way `--mode` does, so a table
/// and a serialized run say the same thing.
///
/// CSV is refused rather than approximated, because these views have a repeated
/// field that a single cell would have to flatten.
async fn write_output<T: Serialize>(
    view: &T,
    table: Table,
    what: &str,
    format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    match format {
        OutputFormat::Json => async_writeln!(output, "{}", serde_json::to_string_pretty(view)?)?,
        OutputFormat::Yaml => async_writeln!(output, "{}", serde_yaml::to_string(view)?)?,
        OutputFormat::AsciiTable => async_write!(output, "{table}")?,
        OutputFormat::Csv => {
            return Err(CarbideCliError::GenericError(format!(
                "CSV is not supported for {what}"
            )));
        }
    }
    Ok(())
}

// a list of subcommands
#[derive(Dispatch, Parser, Debug)]
pub(crate) enum Cmd {
    #[clap(about = "Show which hardware classes the site has and what would attest each")]
    Coverage(coverage::args::Args),
    #[dispatch]
    #[clap(
        subcommand,
        about = "Manage SPDM attestation jobs",
        visible_alias = "jobs"
    )]
    Job(job::Cmd),
    #[dispatch]
    #[clap(
        subcommand,
        about = "Inspect predicted fleet statuses or assess one machine using current attesters",
        defer = |command| {
            // Machine predictions always use machine-ID order.
            command.mut_arg("sort_by", |arg| arg.hide(true))
        }
    )]
    Machines(machines::Cmd),
    #[dispatch]
    #[clap(
        subcommand,
        about = "Manage the attestation policy stored for each hardware class"
    )]
    Profile(profile::Cmd),
    #[clap(about = "Trigger attestation for a given machine with id")]
    Trigger(trigger::args::Args),
}
