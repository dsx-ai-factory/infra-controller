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

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::ReservedAddress;
use prettytable::{Row, Table};
use serde::Serialize;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_write_table_as_csv};

/// The reserved-address listing the show command renders. Abstracted so the
/// command can be exercised without a live API connection.
pub(crate) trait ReservedAddressClient {
    async fn list_reserved_addresses(
        &self,
        page_size: usize,
        reserved_by_mac: Option<String>,
        ip_address: Option<String>,
    ) -> CarbideCliResult<Vec<ReservedAddress>>;
}

impl ReservedAddressClient for ApiClient {
    async fn list_reserved_addresses(
        &self,
        page_size: usize,
        reserved_by_mac: Option<String>,
        ip_address: Option<String>,
    ) -> CarbideCliResult<Vec<ReservedAddress>> {
        self.get_all_reserved_addresses(page_size, reserved_by_mac, ip_address)
            .await
    }
}

pub(super) async fn handle_show_reserved_addresses(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    api_client: &impl ReservedAddressClient,
    args: Args,
    page_size: usize,
) -> CarbideCliResult<()> {
    let rows: Vec<ReservedAddressRow> = api_client
        .list_reserved_addresses(
            page_size,
            args.mac_address.map(|mac| mac.to_string()),
            args.address.map(|address| address.to_string()),
        )
        .await?
        .into_iter()
        .map(ReservedAddressRow::from)
        .collect();

    match output_format {
        OutputFormat::Json => {
            async_write!(output_file, "{}", serde_json::to_string_pretty(&rows)?)?;
        }
        OutputFormat::Yaml => {
            async_write!(output_file, "{}", serde_yaml::to_string(&rows)?)?;
        }
        OutputFormat::Csv => {
            async_write_table_as_csv!(output_file, build_table(&rows))?;
        }
        _ => {
            async_write!(output_file, "{}", build_table(&rows))?;
        }
    }

    Ok(())
}

#[derive(Serialize)]
struct ReservedAddressRow {
    address: String,
    reserved_by_mac: String,
    family: String,
    allocation_type: String,
}

impl From<ReservedAddress> for ReservedAddressRow {
    fn from(reserved: ReservedAddress) -> Self {
        let family = if reserved.ip_address.contains(':') {
            "IPv6"
        } else {
            "IPv4"
        }
        .to_string();
        ReservedAddressRow {
            address: reserved.ip_address,
            reserved_by_mac: reserved.reserved_by_mac,
            family,
            allocation_type: reserved.allocation_type,
        }
    }
}

fn build_table(rows: &[ReservedAddressRow]) -> Box<Table> {
    let mut table = Table::new();
    table.set_titles(Row::from(vec![
        "Address",
        "Reserved By MAC",
        "Family",
        "Type",
    ]));
    for row in rows {
        table.add_row(Row::from(vec![
            row.address.clone(),
            row.reserved_by_mac.clone(),
            row.family.clone(),
            row.allocation_type.clone(),
        ]));
    }
    Box::new(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_write::CapturedOutput;

    struct FakeReservedClient {
        rows: Vec<ReservedAddress>,
    }

    impl ReservedAddressClient for FakeReservedClient {
        async fn list_reserved_addresses(
            &self,
            _page_size: usize,
            _reserved_by_mac: Option<String>,
            _ip_address: Option<String>,
        ) -> CarbideCliResult<Vec<ReservedAddress>> {
            Ok(self.rows.clone())
        }
    }

    fn sample_rows() -> Vec<ReservedAddress> {
        vec![
            ReservedAddress {
                ip_address: "192.0.2.10".to_string(),
                reserved_by_mac: "00:11:22:33:44:55".to_string(),
                allocation_type: "static".to_string(),
            },
            ReservedAddress {
                ip_address: "2001:db8::10".to_string(),
                reserved_by_mac: "00:11:22:33:44:66".to_string(),
                allocation_type: "dhcp".to_string(),
            },
        ]
    }

    async fn render(rows: Vec<ReservedAddress>, output_format: OutputFormat) -> String {
        let mut captured = CapturedOutput::new();
        handle_show_reserved_addresses(
            captured.writer(),
            output_format,
            &FakeReservedClient { rows },
            Args {
                mac_address: None,
                address: None,
            },
            100,
        )
        .await
        .expect("show-reserved-addresses should render");
        String::from_utf8(captured.into_bytes().await).expect("UTF-8 output")
    }

    #[tokio::test]
    async fn table_output_includes_headers_and_derived_family() {
        let display = render(sample_rows(), OutputFormat::AsciiTable).await;

        for header in ["Address", "Reserved By MAC", "Family", "Type"] {
            assert!(display.contains(header), "missing header {header}");
        }

        let row_cells = |ip: &str| -> Vec<String> {
            display
                .lines()
                .find(|line| line.contains(ip))
                .unwrap_or_else(|| panic!("missing row for {ip}"))
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect()
        };
        assert_eq!(
            row_cells("192.0.2.10"),
            ["192.0.2.10", "00:11:22:33:44:55", "IPv4", "static"]
        );
        assert_eq!(
            row_cells("2001:db8::10"),
            ["2001:db8::10", "00:11:22:33:44:66", "IPv6", "dhcp"]
        );
    }

    #[tokio::test]
    async fn empty_result_renders_headers_without_rows() {
        let display = render(Vec::new(), OutputFormat::AsciiTable).await;

        assert!(display.contains("Address"));
        assert!(display.contains("Family"));
        // No reservation data appears.
        assert!(!display.contains("192.0.2"));
        assert!(!display.contains("static"));
    }

    #[tokio::test]
    async fn json_output_serializes_rows_with_derived_family() {
        let display = render(sample_rows(), OutputFormat::Json).await;
        let parsed: serde_json::Value =
            serde_json::from_str(&display).expect("JSON output should parse");
        assert_eq!(parsed[0]["address"], "192.0.2.10");
        assert_eq!(parsed[0]["family"], "IPv4");
        assert_eq!(parsed[0]["allocation_type"], "static");
        assert_eq!(parsed[1]["family"], "IPv6");
    }

    #[tokio::test]
    async fn yaml_output_serializes_rows() {
        let display = render(sample_rows(), OutputFormat::Yaml).await;
        assert!(display.contains("address: 192.0.2.10"));
        assert!(display.contains("family: IPv4"));
        assert!(display.contains("allocation_type: dhcp"));
    }

    #[tokio::test]
    async fn csv_output_includes_header_and_values() {
        let display = render(sample_rows(), OutputFormat::Csv).await;
        let mut lines = display.lines();
        assert_eq!(
            lines.next().expect("CSV header"),
            "Address,Reserved By MAC,Family,Type"
        );
        assert!(display.contains("192.0.2.10,00:11:22:33:44:55,IPv4,static"));
        assert!(display.contains("2001:db8::10,00:11:22:33:44:66,IPv6,dhcp"));
    }
}
