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
use ::rpc::forge::{
    self as forgerpc, ListNvLinkDomainHealthReportsRequest, RemoveNvLinkDomainHealthReportRequest,
};
use ::rpc::health::HealthReport;
use chrono::{DateTime, SecondsFormat, Utc};
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::health_utils;
use crate::rpc::ApiClient;

const MESSAGE_WRAP_WIDTH: usize = 60;
const TARGET_WRAP_WIDTH: usize = 36;

/// Handles NVLink domain health-report CLI subcommands.
pub(super) async fn handle_health_report(
    command: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    match command {
        Args::Show { domain_id } => {
            let response = api_client
                .0
                .list_nv_link_domain_health_reports(ListNvLinkDomainHealthReportsRequest {
                    domain_id: Some(domain_id),
                })
                .await?;

            display_health_reports(response.health_report_entries, output_format)?;
        }
        Args::Remove {
            domain_id,
            report_source,
        } => {
            api_client
                .0
                .remove_nv_link_domain_health_report(RemoveNvLinkDomainHealthReportRequest {
                    domain_id: Some(domain_id),
                    source: report_source,
                })
                .await?;
        }
        Args::PrintEmptyTemplate => {
            health_utils::print_empty_template();
        }
    }

    Ok(())
}

/// Displays NVLink domain health reports with NVL-specific table formatting.
fn display_health_reports(
    entries: Vec<forgerpc::HealthReportEntry>,
    output_format: OutputFormat,
) -> CarbideCliResult<()> {
    // Preserve the existing JSON contract used by other health-report commands.
    if output_format == OutputFormat::Json {
        return health_utils::display_health_reports(entries, output_format);
    }

    let mut rows = Vec::new();
    for entry in entries {
        let report = entry.report.ok_or(CarbideCliError::GenericError(
            "missing response".to_string(),
        ))?;
        let mode = match forgerpc::HealthReportApplyMode::try_from(entry.mode)
            .map_err(|_| CarbideCliError::GenericError("invalid response".to_string()))?
        {
            forgerpc::HealthReportApplyMode::Merge => "Merge",
            forgerpc::HealthReportApplyMode::Replace => "Replace",
        };

        rows.push((report, mode));
    }

    if rows.is_empty() {
        println!("No health report entries found.");
        return Ok(());
    }

    println!("Health report entries: {}", rows.len());

    let mut summary = Table::new();
    summary.set_titles(row!["Source", "Mode", "Observed At", "Alerts"]);
    for (report, mode) in &rows {
        summary.add_row(row![
            report.source.as_str(),
            *mode,
            format_timestamp(report.observed_at),
            report.alerts.len()
        ]);
    }

    summary.printstd();

    for (report, mode) in &rows {
        print_alerts(report, mode);
    }

    Ok(())
}

/// Prints NVLink domain alerts in a compact table layout.
fn print_alerts(report: &HealthReport, mode: &str) {
    if report.alerts.is_empty() {
        return;
    }

    println!();
    println!("Alerts for source {} ({mode})", report.source);

    let mut table = Table::new();
    table.set_titles(row!["Id", "Target", "Since", "Message", "Classifications"]);
    for alert in &report.alerts {
        let message = format_message(&alert.message);

        table.add_row(row![
            alert.id.as_str(),
            wrap_text(alert.target.as_deref().unwrap_or("-"), TARGET_WRAP_WIDTH),
            format_timestamp(alert.in_alert_since),
            wrap_text(&message, MESSAGE_WRAP_WIDTH),
            format_list(&alert.classifications)
        ]);
    }

    table.printstd();
}

/// Formats optional protobuf timestamps for table display.
fn format_timestamp<T>(timestamp: Option<T>) -> String
where
    DateTime<Utc>: TryFrom<T>,
{
    timestamp
        .and_then(|timestamp| DateTime::<Utc>::try_from(timestamp).ok())
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::AutoSi, true))
        .unwrap_or_else(|| "-".to_string())
}

/// Formats repeated values as one table line per item.
fn format_list(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.join("\n")
    }
}

/// Pretty-prints structured alert messages when they are JSON.
fn format_message(message: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| message.to_string())
    } else {
        message.to_string()
    }
}

/// Hard-wraps text for prettytable cells.
///
/// The existing MLX wrapper is feature-local and word-based; this output needs
/// to split long IDs and JSON fragments that may not contain whitespace.
fn wrap_text(value: &str, width: usize) -> String {
    value
        .lines()
        .map(|line| wrap_line(line, width))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Hard-wraps a single line without changing its content.
fn wrap_line(value: &str, width: usize) -> String {
    if width == 0 {
        return value.to_string();
    }

    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index > 0 && index % width == 0 {
            output.push('\n');
        }

        output.push(ch);
    }

    output
}
