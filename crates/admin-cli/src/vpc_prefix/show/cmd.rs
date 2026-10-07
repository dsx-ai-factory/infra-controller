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

use std::borrow::Cow;
use std::fs::File;
use std::io::{Write, stdout};

use ::rpc::admin_cli::output::OutputFormat;
use prettytable::{Row, Table};
use rpc::forge::{PrefixMatchType, StateHistoryRecord, VpcPrefix};
use serde::{Deserialize, Serialize};

use super::args::Args;
use crate::Destination;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::vpc_prefix::common::{VpcPrefixSelector, get_by_ids, match_all, search};

pub(super) async fn show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    batch_size: usize,
) -> CarbideCliResult<()> {
    let show_method = ShowMethod::from(args);
    let output = fetch(api_client, batch_size, show_method).await?;

    output
        .write_output(output_format, crate::Destination::Stdout())
        .map_err(CarbideCliError::from)
}

#[derive(Debug)]
enum ShowMethod {
    Get {
        selector: VpcPrefixSelector,
        deleted: rpc::forge::DeletedFilter,
    },
    Search(rpc::forge::VpcPrefixSearchQuery),
}

pub(in crate::vpc_prefix) enum ShowOutput {
    One {
        vpc_prefix: VpcPrefix,
        history: Vec<StateHistoryRecord>,
    },
    Many(Vec<VpcPrefix>),
}

impl ShowOutput {
    fn as_slice(&self) -> &[VpcPrefix] {
        match self {
            ShowOutput::One { vpc_prefix, .. } => std::slice::from_ref(vpc_prefix),
            ShowOutput::Many(vpc_prefixes) => vpc_prefixes.as_slice(),
        }
    }
}

impl From<Args> for ShowMethod {
    fn from(show_args: Args) -> Self {
        match show_args.prefix_selector {
            Some(selector) => ShowMethod::Get {
                selector,
                deleted: show_args.deleted,
            },
            None => {
                let mut search = match_all();
                search.deleted = show_args.deleted as i32;
                search.vpc_id = show_args.vpc_id;
                if let Some(prefix) = &show_args.contains {
                    search.prefix_match_type = Some(PrefixMatchType::PrefixContains as i32);
                    search.prefix_match = Some(prefix.to_string());
                };
                if let Some(prefix) = &show_args.contained_by {
                    search.prefix_match_type = Some(PrefixMatchType::PrefixContainedBy as i32);
                    search.prefix_match = Some(prefix.to_string());
                };
                ShowMethod::Search(search)
            }
        }
    }
}

async fn fetch(
    api_client: &ApiClient,
    batch_size: usize,
    show_method: ShowMethod,
) -> Result<ShowOutput, CarbideCliError> {
    match show_method {
        ShowMethod::Get { selector, deleted } => {
            let vpc_prefix = selector.fetch(api_client, deleted).await?;

            let history = match vpc_prefix.id {
                Some(id) => api_client
                    .get_vpc_prefix_state_history(id)
                    .await
                    .unwrap_or_default(),
                None => Vec::new(),
            };

            Ok(ShowOutput::One {
                vpc_prefix,
                history,
            })
        }
        ShowMethod::Search(query) => {
            let deleted = query.deleted;
            let vpc_prefix_ids = search(api_client, query).await?;
            get_by_ids(api_client, batch_size, vpc_prefix_ids.as_slice(), deleted)
                .await
                .map(ShowOutput::Many)
        }
    }
}

impl ShowOutput {
    /// Format the output data as bytes (probably UTF-8 text).
    fn format_output(&self, format: OutputFormat) -> Vec<u8> {
        match format {
            OutputFormat::Json => {
                serde_json::to_vec_pretty(self).expect("Could not serialize as JSON")
            }
            OutputFormat::Yaml => {
                let mut out = Vec::new();
                serde_yaml::to_writer(&mut out, self).expect("Could not serialize as YAML");
                out
            }
            OutputFormat::AsciiTable => self.render_ascii_table(),
            OutputFormat::Csv => self.render_csv_table(),
        }
    }

    /// Format the output data and write it to the specified destination.
    pub(in crate::vpc_prefix) fn write_output(
        &self,
        format: OutputFormat,
        destination: Destination,
    ) -> std::io::Result<()> {
        let output = self.format_output(format);
        match destination {
            Destination::Stdout() => {
                let mut stdout_guard = stdout().lock();
                stdout_guard.write_all(output.as_slice())
            }
            Destination::Path(path) => {
                File::create(path).and_then(|mut file| file.write_all(output.as_slice()))
            }
        }
    }

    fn render_ascii_table(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let table = self.make_table();
        table.print(&mut out).expect("Couldn't render ASCII table");
        if let ShowOutput::One { history, .. } = self {
            Self::render_state_history(history, &mut out);
        }
        out
    }

    fn render_csv_table(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let table = self.make_table();
        table.to_csv(&mut out).expect("Couldn't render CSV table");
        out
    }

    // This is not a trait method in order to keep the `prettytable` types
    // out of the public API.
    fn make_table(&self) -> Table {
        let mut table = Table::new();
        let header = Row::from(self.header());
        table.set_titles(header);
        let rows = self.all_rows();
        rows.iter().for_each(|row| {
            let values = Self::row_values(row);
            let row = Row::from(values);
            table.add_row(row);
        });

        table
    }
}

impl Serialize for ShowOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ShowOutput::One { vpc_prefix, .. } => vpc_prefix.serialize(serializer),
            ShowOutput::Many(vpc_prefixes) => vpc_prefixes.serialize(serializer),
        }
    }
}

#[derive(Deserialize)]
struct LifecycleState {
    state: String,
}

impl ShowOutput {
    fn header(&self) -> &[&str] {
        &[
            "VpcPrefixId",
            "VpcId",
            "Prefix",
            "Name",
            "State",
            "State Version",
            "Total Linknets",
            "Available Linknets",
        ]
    }

    fn all_rows(&self) -> &[VpcPrefix] {
        self.as_slice()
    }

    fn row_values(row: &'_ VpcPrefix) -> Vec<Cow<'_, str>> {
        let vpc_prefix_id: Cow<str> = row.id.map(|id| id.to_string().into()).unwrap_or("".into());
        let vpc_id: Cow<str> = row
            .vpc_id
            .as_ref()
            .map(|id| id.to_string().into())
            .unwrap_or("".into());
        let prefix = row.prefix.as_str();
        let name = row
            .metadata
            .as_ref()
            .map(|x| x.name.as_str())
            .unwrap_or("<no name>");
        let (state, version) = Self::lifecycle_summary(row);
        let mut r = vec![
            vpc_prefix_id,
            vpc_id,
            prefix.into(),
            name.into(),
            state.into(),
            version.into(),
        ];

        if let Some(status) = &row.status {
            r.push(status.total_linknet_segments.to_string().into());
            r.push(status.available_linknet_segments.to_string().into());
        } else {
            r.push("NA".into());
            r.push("NA".into());
        }

        r
    }

    /// Extracts a compact lifecycle state and version for table output.
    fn lifecycle_summary(row: &'_ VpcPrefix) -> (String, String) {
        // Prefer the structured lifecycle status exposed by new API servers.
        let Some(lifecycle) = row
            .status
            .as_ref()
            .and_then(|status| status.lifecycle.as_ref())
        else {
            return ("NA".to_string(), "NA".to_string());
        };

        // Collapse the JSON state into its state label when it uses the common shape.
        let state = serde_json::from_str::<LifecycleState>(&lifecycle.state)
            .map(|state| state.state)
            .unwrap_or_else(|_| lifecycle.state.clone());

        (state, lifecycle.version.clone())
    }

    /// Appends VPC-prefix state history to single-object ASCII output.
    fn render_state_history(history: &[StateHistoryRecord], out: &mut Vec<u8>) {
        // Separate the history section from the primary details table.
        writeln!(out).expect("Couldn't render VPC prefix state-history spacer");
        writeln!(out, "STATE HISTORY: (Latest 5 only)")
            .expect("Couldn't render VPC prefix state-history header");

        // Render an explicit empty marker so operators can distinguish no data.
        if history.is_empty() {
            writeln!(out, "\tEMPTY").expect("Couldn't render empty VPC prefix state-history");
            return;
        }

        writeln!(out, "\tState          Version                      Time")
            .expect("Couldn't render VPC prefix state-history columns");
        writeln!(out, "\t---------------------------------------------------")
            .expect("Couldn't render VPC prefix state-history separator");
        for record in history.iter().rev().take(5).rev() {
            let state = serde_json::from_str::<LifecycleState>(&record.state)
                .map(|state| state.state)
                .unwrap_or_else(|_| record.state.clone());
            writeln!(
                out,
                "\t{:<15} {:25} {}",
                state,
                record.version,
                record.time.unwrap_or_default()
            )
            .expect("Couldn't render VPC prefix state-history record");
        }
    }
}
