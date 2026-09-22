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
use ::rpc::forge::{self as forgerpc};
use prettytable::{Cell, Row, Table, row};
use serde::Serialize;

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::machine::{HealthReportTemplates, get_empty_template, get_health_report};

/// Display a list of health report entries.
pub(crate) fn display_health_reports(
    entries: Vec<forgerpc::HealthReportEntry>,
    output_format: OutputFormat,
) -> CarbideCliResult<()> {
    let mut rows = vec![];
    for entry in entries {
        let report = entry.report.ok_or(CarbideCliError::GenericError(
            "missing response".to_string(),
        ))?;
        let mode = match forgerpc::HealthReportApplyMode::try_from(entry.mode)
            .map_err(|_| CarbideCliError::GenericError("invalide response".to_string()))?
        {
            forgerpc::HealthReportApplyMode::Merge => "Merge",
            forgerpc::HealthReportApplyMode::Replace => "Replace",
        };
        rows.push((report, mode));
    }
    match output_format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(
                &rows
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "report": r.0,
                            "mode": r.1,
                        })
                    })
                    .collect::<Vec<_>>(),
            )?
        ),
        _ => {
            let mut table = Table::new();
            table.set_titles(row!["Report", "Mode"]);
            for row in rows {
                table.add_row(row![serde_json::to_string(&row.0)?, row.1]);
            }
            table.printstd();
        }
    }
    Ok(())
}

/// One row of an object's health history, summarized for tabular display.
#[derive(Serialize)]
struct HealthHistoryRecordView {
    time: String,
    source: String,
    status: String,
    alerts: Vec<String>,
}

impl From<&forgerpc::HealthHistoryRecord> for HealthHistoryRecordView {
    fn from(record: &forgerpc::HealthHistoryRecord) -> Self {
        let health = record.health.as_ref();
        let source = health.map(|h| h.source.clone()).unwrap_or_default();
        let alerts: Vec<String> = health
            .map(|h| h.alerts.iter().map(|alert| alert.id.clone()).collect())
            .unwrap_or_default();
        let status = if alerts.is_empty() {
            "Healthy"
        } else {
            "Alerting"
        }
        .to_string();
        Self {
            time: record.time.map(|time| time.to_string()).unwrap_or_default(),
            source,
            status,
            alerts,
        }
    }
}

fn health_history_table(records: &[HealthHistoryRecordView]) -> Table {
    let mut table = Table::new();
    table.set_titles(Row::new(
        ["Time", "Source", "Status", "Alerts"]
            .into_iter()
            .map(Cell::new)
            .collect(),
    ));
    for record in records {
        let alerts = if record.alerts.is_empty() {
            "-".to_string()
        } else {
            record.alerts.join(", ")
        };
        table.add_row(row![record.time, record.source, record.status, alerts]);
    }
    table
}

/// Display an object's health history in the requested output format. Shared by
/// the per-resource `health-history` subcommands, whose only difference is the
/// RPC that produced `history`.
pub(crate) fn display_health_history(
    object_id: &str,
    history: Vec<forgerpc::HealthHistoryRecord>,
    output_format: OutputFormat,
) -> CarbideCliResult<()> {
    if history.is_empty() {
        println!("No health history found for {object_id}");
        return Ok(());
    }

    let records: Vec<HealthHistoryRecordView> =
        history.iter().map(HealthHistoryRecordView::from).collect();
    match output_format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&records)?),
        OutputFormat::Yaml => println!("{}", serde_yaml::to_string(&records)?),
        OutputFormat::Csv => {
            health_history_table(&records).to_csv(std::io::stdout())?;
        }
        OutputFormat::AsciiTable => health_history_table(&records).printstd(),
    }
    Ok(())
}

/// Resolve a health report from either a template or raw JSON.
pub(crate) fn resolve_health_report(
    template: Option<HealthReportTemplates>,
    health_report_json: Option<String>,
    message: Option<String>,
) -> CarbideCliResult<health_report::HealthReport> {
    if let Some(template) = template {
        Ok(get_health_report(template, message))
    } else if let Some(json) = health_report_json {
        serde_json::from_str::<health_report::HealthReport>(&json)
            .map_err(CarbideCliError::JsonError)
    } else {
        Err(CarbideCliError::GenericError(
            "Either health_report or template name must be provided.".to_string(),
        ))
    }
}

/// Print the empty health report template.
pub(crate) fn print_empty_template() {
    println!(
        "{}",
        serde_json::to_string_pretty(&get_empty_template())
            .expect("empty template should convert to json")
    );
}
