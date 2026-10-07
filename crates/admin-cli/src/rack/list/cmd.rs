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
use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;

use crate::cfg::runtime::RuntimeConfig;
use crate::rpc::ApiClient;

fn build_rack_table(racks: &[rpc::forge::Rack]) -> Table {
    let mut table = Table::new();
    let headers = vec!["Rack ID", "Rack State"];
    table.set_titles(Row::new(
        headers.into_iter().map(Cell::new).collect::<Vec<Cell>>(),
    ));
    for r in racks {
        table.add_row(prettytable::row![
            r.id.as_ref().map(|id| id.to_string()).unwrap_or_default(),
            r.rack_state.as_str(),
        ]);
    }
    table
}

pub(super) async fn list_racks(api_client: &ApiClient, config: &RuntimeConfig) -> Result<()> {
    let response = api_client.get_all_racks(config.page_size).await?;
    let racks = response.racks;
    if racks.is_empty() {
        println!("No racks found");
        return Ok(());
    }

    let format = OutputFormat::AsciiTable;
    match format {
        OutputFormat::AsciiTable => {
            build_rack_table(&racks).printstd();
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&racks)?);
        }
        _ => {
            println!("output format not supported for Rack");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rpc::forge::{Metadata, Rack};

    use super::*;

    fn make_rack(id: Option<&str>, state: &str) -> Rack {
        Rack {
            id: id.map(|s| s.parse().unwrap()),
            rack_state: state.to_string(),
            metadata: Some(Metadata {
                name: "NVL72".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn table_to_string(table: &Table) -> String {
        let mut buf = Vec::new();
        table.print(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    /////////////////////////////////////////////////////////////////////////////
    // Table structure

    /// The rendered table contains "Rack ID" and "Rack State" headers and no
    /// component columns. Regression guard against re-adding removed columns.
    #[test]
    fn rack_list_table_has_rack_id_and_state_headers() {
        let racks = vec![make_rack(Some("Rack1"), "Created")];
        let rendered = table_to_string(&build_rack_table(&racks));
        assert!(rendered.contains("Rack ID"), "expected 'Rack ID' header");
        assert!(
            rendered.contains("Rack State"),
            "expected 'Rack State' header"
        );
        assert!(!rendered.contains("Compute"), "unexpected 'Compute' column");
        assert!(!rendered.contains("Power"), "unexpected 'Power' column");
        assert!(!rendered.contains("Switch"), "unexpected 'Switch' column");
    }

    /// Each Rack produces one row with the correct ID and state values.
    #[test]
    fn rack_list_table_row_maps_id_and_state() {
        let racks = vec![make_rack(Some("Rack1"), "Created")];
        let table = build_rack_table(&racks);
        assert_eq!(table.len(), 1);
        let row = table.get_row(0).unwrap();
        assert_eq!(row.get_cell(0).unwrap().get_content(), "Rack1");
        assert_eq!(row.get_cell(1).unwrap().get_content(), "Created");
    }

    /// A Rack with no ID falls back to an empty string in the ID cell.
    #[test]
    fn rack_list_table_empty_id_falls_back_to_empty_string() {
        let racks = vec![make_rack(None, "Created")];
        let table = build_rack_table(&racks);
        let row = table.get_row(0).unwrap();
        assert_eq!(row.get_cell(0).unwrap().get_content(), "");
    }

    /// Multiple racks produce the correct number of rows.
    #[test]
    fn rack_list_table_multiple_racks_produce_correct_row_count() {
        let racks = vec![
            make_rack(Some("Rack1"), "Created"),
            make_rack(Some("Rack2"), "Provisioned"),
            make_rack(Some("Rack3"), "Maintenance"),
        ];
        let table = build_rack_table(&racks);
        assert_eq!(table.len(), 3);
    }
}
