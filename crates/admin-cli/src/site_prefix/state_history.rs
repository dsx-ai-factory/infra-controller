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

use carbide_uuid::site_prefix::SitePrefixId;
use clap::Parser;
use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;
use rpc::forge::StateHistoryRecord;
use serde::{Deserialize, Serialize};

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::{async_write, async_write_table_as_csv, async_writeln};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show every lifecycle transition recorded for a SitePrefix:
    $ nico-admin-cli site-prefix state-history 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(value_name = "SITE_PREFIX_ID", help = "SitePrefix history to show")]
    site_prefix_id: SitePrefixId,
}

#[derive(Debug, Serialize)]
struct HistoryRecordView {
    state: String,
    version: String,
    time: Option<String>,
}

#[derive(Deserialize)]
struct LifecycleStateDocument {
    state: String,
}

impl From<StateHistoryRecord> for HistoryRecordView {
    fn from(record: StateHistoryRecord) -> Self {
        Self {
            state: display_state(&record.state),
            version: record.version,
            time: record.time.map(|time| time.to_string()),
        }
    }
}

impl HistoryRecordView {
    fn table(records: &[Self]) -> Table {
        let mut table = Table::new();
        table.set_titles(Row::new(
            ["State", "Version", "Time"]
                .into_iter()
                .map(Cell::new)
                .collect(),
        ));
        for record in records {
            table.add_row(Row::new(vec![
                Cell::new(&record.state),
                Cell::new(&record.version),
                Cell::new(record.time.as_deref().unwrap_or_default()),
            ]));
        }
        table
    }
}

fn display_state(state: &str) -> String {
    serde_json::from_str::<LifecycleStateDocument>(state)
        .map(|document| document.state)
        .unwrap_or_else(|_| state.to_string())
}

async fn write_history(
    records: &[HistoryRecordView],
    format: &OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    match format {
        OutputFormat::Json => {
            async_writeln!(output_file, "{}", serde_json::to_string_pretty(records)?)?
        }
        OutputFormat::Yaml => async_write!(output_file, "{}", serde_yaml::to_string(records)?)?,
        OutputFormat::AsciiTable => {
            async_write!(output_file, "{}", HistoryRecordView::table(records))?
        }
        OutputFormat::Csv => {
            async_write_table_as_csv!(output_file, HistoryRecordView::table(records))?
        }
    }

    Ok(())
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let history = ctx
            .api_client
            .get_site_prefix_state_history(self.site_prefix_id)
            .await?
            .into_iter()
            .map(HistoryRecordView::from)
            .collect::<Vec<_>>();

        write_history(&history, &ctx.config.format, &mut ctx.output_file).await
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::display_state;

    #[test]
    fn lifecycle_state_json_is_readable() {
        value_scenarios!(display_state:
            "state documents" {
                r#"{"state":"provisioning"}"# => "provisioning".to_string(),
                r#"{"state":"ready"}"# => "ready".to_string(),
                r#"{"state":"deleting"}"# => "deleting".to_string(),
                r#"{"state":"error"}"# => "error".to_string(),
            }

            "legacy or malformed values" {
                "Ready" => "Ready".to_string(),
                "not-json" => "not-json".to_string(),
            }
        );
    }
}
