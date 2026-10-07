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

use color_eyre::Result;
use model::rack::RackState;
use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;
use rpc::forge::StateHistoryRecord;
use serde::Serialize;

use super::args::Args;
use crate::cfg::runtime::RuntimeConfig;
use crate::rpc::ApiClient;

#[derive(Serialize)]
struct HistoryRecordOutput {
    state: String,
    version: String,
    time: String,
}

fn format_state(state_json: &str) -> String {
    serde_json::from_str::<RackState>(state_json)
        .map(|state| state.to_string())
        .unwrap_or_else(|_| state_json.to_string())
}

fn build_history_table(records: &[HistoryRecordOutput]) -> Table {
    let mut table = Table::new();
    table.set_titles(Row::new(vec![
        Cell::new("State"),
        Cell::new("Version"),
        Cell::new("Time"),
    ]));
    for record in records {
        table.add_row(prettytable::row![record.state, record.version, record.time]);
    }
    table
}

fn history_output(records: &[StateHistoryRecord]) -> Vec<HistoryRecordOutput> {
    records
        .iter()
        .map(|record| HistoryRecordOutput {
            state: format_state(&record.state),
            version: record.version.clone(),
            time: record.time.unwrap_or_default().to_string(),
        })
        .collect()
}

pub(super) async fn show_state_history(
    api_client: &ApiClient,
    args: Args,
    config: &RuntimeConfig,
) -> Result<()> {
    let rack_id = args.rack_id;
    let history = api_client.get_rack_state_history(rack_id.clone()).await?;

    if history.is_empty() {
        println!("No state history found for rack {rack_id}");
        return Ok(());
    }

    let output = history_output(&history);
    match config.format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&output)?),
        OutputFormat::Yaml => println!("{}", serde_yaml::to_string(&output)?),
        OutputFormat::Csv => {
            build_history_table(&output).to_csv(std::io::stdout()).ok();
        }
        OutputFormat::AsciiTable => build_history_table(&output).printstd(),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_state_parses_discovering() {
        assert_eq!(format_state(r#"{"state":"discovering"}"#), "Discovering");
    }

    #[test]
    fn format_state_parses_maintenance_completed() {
        assert_eq!(
            format_state(r#"{"state":"maintenance","rack_maintenance":"Completed"}"#),
            "Maintenance(Completed)"
        );
    }

    #[test]
    fn format_state_falls_back_to_raw_json_for_legacy_values() {
        let legacy = r#"{"state":"maintenance","rack_maintenance":{"FirmwareUpgrade":{"rack_firmware_upgrade":"Compute"}}}"#;
        assert_eq!(format_state(legacy), legacy);
    }
}
