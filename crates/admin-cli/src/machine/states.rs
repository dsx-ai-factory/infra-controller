/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Compact lifecycle state reads for operator listings. Collect every page before
//! rendering so a failed read cannot leave output that looks like a complete fleet.

use std::time::Duration;

use carbide_uuid::machine::MachineId;
use clap::Parser;
use eyre::{Context, eyre};
use futures::{StreamExt, TryStreamExt, stream};
use prettytable::{Row, Table};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{
    MachineSearchConfig, MachineStateList, MachineStateSummary, MachineStatesByIdsRequest,
    MachinesByIdsRequest,
};
use tokio::time::timeout;

use crate::cfg::cli_options::SortField;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::rpc::PAGED_LIST_FETCH_CONCURRENCY;
use crate::table_utils::MaxWidthArgs;
use crate::{async_write, async_write_table_as_csv, async_writeln};

// Core's client timeout covers headers only. Include response bodies in the
// configured deadline, using the same 300-second default when it is unset.
const READ_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Parser, Debug)]
#[command(
    long_about = "Display machine IDs and their current lifecycle states without health, hardware, or history data.\n\n\
    With no IDs, list hosts, predicted hosts, and DPUs. Unknown IDs are omitted. \
    --hosts includes predicted hosts; --dpus selects only DPUs. Both flags select all types. \
    Explicit IDs cannot be combined with either flag.\n\n\
    Requests use the smaller of --internal-page-size and the batch limit advertised by Core. \
    Each RPC operation shares one FORGE_CLIENT_REQUEST_TIMEOUT_SECS deadline \
    across its retries and complete response. This accepts positive whole seconds; unset, zero, or invalid \
    values use 300 seconds. A failed call fails the command without partial output.\n\n\
    If Core returns UNIMPLEMENTED or PERMISSION_DENIED for the compact RPC, the command uses separately \
    authorized full machine reads for that page and the remaining pages. Other failures are reported.\n\n\
    ASCII and CSV contain Id and State columns; JSON and YAML contain a machines list of id/state records. \
    --sort-by selects primary-id (default) or state order, with IDs breaking state ties. \
    State strings preserve the capitalization from Core. --max-width affects only ASCII output.",
    after_long_help = "\
EXAMPLES:

List host and DPU lifecycle states:
    $ nico-admin-cli machine states

List only hosts or only DPUs:
    $ nico-admin-cli machine states --hosts
    $ nico-admin-cli machine states --dpus

Read one machine's state:
    $ nico-admin-cli machine states fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Export states as JSON, sorted by state:
    $ nico-admin-cli --format json --sort-by state machine states

Limit long state values in the table:
    $ nico-admin-cli machine states --max-width State=40

"
)]
pub(crate) struct Args {
    #[arg(
        value_name = "MACHINE_ID",
        conflicts_with_all = ["hosts", "dpus"],
        help = "Machine IDs to query. Omit to list all machines. Repeated IDs return one entry."
    )]
    machine_ids: Vec<MachineId>,
    #[arg(
        long,
        help = "Show only hosts and predicted hosts; combine with --dpus to show all types"
    )]
    hosts: bool,
    #[arg(long, help = "Show only DPUs; combine with --hosts to show all types")]
    dpus: bool,
    #[command(flatten)]
    width: MaxWidthArgs,
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let read_timeout = ctx.config.request_timeout.unwrap_or(READ_TIMEOUT);
        let mut ids = if self.machine_ids.is_empty() {
            timeout(
                read_timeout,
                ctx.api_client.0.find_machine_ids(MachineSearchConfig {
                    include_dpus: self.dpus || !self.hosts,
                    exclude_hosts: self.dpus && !self.hosts,
                    include_predicted_host: self.hosts || !self.dpus,
                    ..Default::default()
                }),
            )
            .await
            .context("list machine IDs before the deadline")?
            .context("list machine IDs")?
            .machine_ids
        } else {
            self.machine_ids
        };
        ids.sort_unstable();
        ids.dedup();

        let mut states = MachineStateList {
            machines: Vec::with_capacity(ids.len()),
        };
        if !ids.is_empty() {
            let page_size = timeout(
                read_timeout,
                ctx.api_client.effective_chunk_size(ctx.config.page_size),
            )
            .await
            .context("read the machine batch limit before the deadline")?
            .context("read the machine batch limit")?;
            let api_client = &ctx.api_client;
            let read_compact_page = |machine_ids: &[MachineId]| {
                let request = MachineStatesByIdsRequest {
                    machine_ids: machine_ids.to_vec(),
                };
                async move {
                    timeout(
                        read_timeout,
                        api_client.0.find_machine_states_by_ids(request),
                    )
                    .await
                    .context("read machine states before the deadline")
                }
            };
            let mut completed_pages = 0;
            {
                // Probe once before overlapping pages so older servers receive
                // only one unsupported request. Ordered results identify the
                // first page that must be fetched again after a late fallback.
                let pages = ids.chunks(page_size);
                let mut responses = stream::iter(pages.clone().take(1))
                    .map(read_compact_page)
                    .buffered(1)
                    .chain(
                        stream::iter(pages.skip(1))
                            .map(read_compact_page)
                            .buffered(PAGED_LIST_FETCH_CONCURRENCY),
                    );
                while let Some(response) = responses.next().await {
                    match response? {
                        Ok(page) => {
                            states.machines.extend(page.machines);
                            completed_pages += 1;
                        }
                        // The full-record request still undergoes its own authorization.
                        Err(status) if crate::rpc::maybe_unimplemented(&status) => {
                            tracing::warn!(grpc_status_code = ?status.code(), "compact machine states unavailable; using full machine records");
                            break;
                        }
                        Err(status) => {
                            return Err(eyre::Report::new(status)
                                .wrap_err("read machine states")
                                .into());
                        }
                    }
                }
            }
            // Leaving the scope above cancels unconsumed compact pages before
            // re-reading that suffix. Nothing is printed until every page succeeds.
            stream::iter(ids.chunks(page_size).skip(completed_pages))
                .map(|machine_ids| async move {
                    timeout(
                        read_timeout,
                        api_client.0.find_machines_by_ids(MachinesByIdsRequest {
                            machine_ids: machine_ids.to_vec(),
                            include_history: false,
                        }),
                    )
                    .await
                    .context("read full machine records before the deadline")?
                    .context("read full machine records for lifecycle states")
                })
                .buffered(PAGED_LIST_FETCH_CONCURRENCY)
                .try_for_each(|page| {
                    states
                        .machines
                        .extend(
                            page.machines
                                .into_iter()
                                .map(|machine| MachineStateSummary {
                                    id: machine.id,
                                    state: machine.state,
                                }),
                        );
                    futures::future::ok(())
                })
                .await?;
        }

        if states.machines.iter().any(|machine| machine.id.is_none()) {
            return Err(eyre!("machine state response is missing a machine ID").into());
        }
        match ctx.config.sort_by {
            SortField::PrimaryId => states.machines.sort_by_key(|machine| machine.id),
            SortField::State => states
                .machines
                .sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.id.cmp(&b.id))),
        }

        match ctx.config.format {
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(&states)
                    .context("serialize machine states as JSON")?;
                async_writeln!(ctx.output_file, "{json}").context("write machine states")?;
            }
            OutputFormat::Yaml => {
                let yaml =
                    serde_yaml::to_string(&states).context("serialize machine states as YAML")?;
                async_write!(ctx.output_file, "{yaml}").context("write machine states")?;
            }
            OutputFormat::AsciiTable | OutputFormat::Csv => {
                let widths = self.width.widths();
                if let Some(message) = widths.describe_unmatched_columns(&["Id", "State"]) {
                    tracing::warn!(warning = %message, "unmatched width setting");
                }
                let mut table = Table::new();
                table.set_titles(Row::from(vec!["Id", "State"]));
                for machine in states.machines {
                    let id = machine
                        .id
                        .ok_or_else(|| eyre!("machine state response is missing a machine ID"))?;
                    let values = [("Id", id.to_string()), ("State", machine.state)].map(
                        |(header, value)| {
                            if ctx.config.format == OutputFormat::AsciiTable {
                                widths.truncate(header, &value)
                            } else {
                                value
                            }
                        },
                    );
                    table.add_row(Row::from(values.to_vec()));
                }
                if ctx.config.format == OutputFormat::Csv {
                    async_write_table_as_csv!(ctx.output_file, table)
                        .context("write machine states as CSV")?;
                } else {
                    async_write!(ctx.output_file, "{table}")
                        .context("write machine states as a table")?;
                }
            }
        }
        Ok(())
    }
}
