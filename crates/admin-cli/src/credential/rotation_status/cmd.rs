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

use clap::ValueEnum;
use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;
use rpc::forge as forgerpc;
use serde::Serialize;

use super::args::Args;
use crate::cfg::runtime::RuntimeConfig;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

#[derive(Serialize)]
struct RotationStatusOutput {
    credential_type: String,
    target_version: u32,
    converged: u64,
    pending: u64,
    quarantined: u64,
    complete: bool,
    started_at: String,
    quarantined_device_macs: Vec<String>,
    // Present only when the report was scoped to a single device by MAC.
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<DeviceStatusOutput>,
}

#[derive(Serialize)]
struct DeviceStatusOutput {
    device_mac: String,
    current_version: Option<u32>,
    rotating_to_version: Option<u32>,
    converged: bool,
    quarantined: bool,
    quarantined_until: Option<String>,
    rotate_attempts: u32,
    last_attempt_at: Option<String>,
    last_error: Option<String>,
}

// Renders an Option as the value or a "-" placeholder for empty table cells.
fn or_dash<T: ToString>(value: &Option<T>) -> String {
    value
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".to_string())
}

// Site-wide aggregate, rendered as a single row. Example `AsciiTable` output:
//
//   +------------+--------+-----------+---------+-------------+----------+----------------------+-------------------+
//   | Credential | Target | Converged | Pending | Quarantined | Complete | Started              | QuarantinedMacs   |
//   +------------+--------+-----------+---------+-------------+----------+----------------------+-------------------+
//   | bmc        | 3      | 128       | 4       | 1           | false    | 2026-06-26T19:30:00Z | 00:11:22:33:44:55 |
//   +------------+--------+-----------+---------+-------------+----------+----------------------+-------------------+
//
// The trailing column lists the quarantined device MACs so the `Quarantined`
// count is actionable in the table/CSV formats (JSON/YAML carry the same field).
fn build_status_table(status: &RotationStatusOutput) -> Table {
    let mut table = Table::new();
    table.set_titles(Row::new(vec![
        Cell::new("Credential"),
        Cell::new("Target"),
        Cell::new("Converged"),
        Cell::new("Pending"),
        Cell::new("Quarantined"),
        Cell::new("Complete"),
        Cell::new("Started"),
        Cell::new("QuarantinedMacs"),
    ]));
    // Empty renders as "-" to match the per-device table's optional cells.
    let quarantined_macs = if status.quarantined_device_macs.is_empty() {
        "-".to_string()
    } else {
        status.quarantined_device_macs.join(", ")
    };
    table.add_row(prettytable::row![
        status.credential_type,
        status.target_version,
        status.converged,
        status.pending,
        status.quarantined,
        status.complete,
        status.started_at,
        quarantined_macs,
    ]);
    table
}

// Single-device detail (returned when --mac-address is given). A "-" marks an
// unset optional. Example `AsciiTable` output:
//
//   +------------+-------------------+--------+---------+------------+-----------+-------------+----------------------+----------+----------------------+-----------+
//   | Credential | Device            | Target | Current | RotatingTo | Converged | Quarantined | Until                | Attempts | LastAttempt          | LastError |
//   +------------+-------------------+--------+---------+------------+-----------+-------------+----------------------+----------+----------------------+-----------+
//   | bmc        | 00:11:22:33:44:55 | 3      | 2       | -          | false     | true        | 2026-06-26T20:30:00Z | 2        | 2026-06-26T19:31:00Z | timed out |
//   +------------+-------------------+--------+---------+------------+-----------+-------------+----------------------+----------+----------------------+-----------+
fn build_device_table(
    credential_type: &str,
    target_version: u32,
    device: &DeviceStatusOutput,
) -> Table {
    let mut table = Table::new();
    table.set_titles(Row::new(vec![
        Cell::new("Credential"),
        Cell::new("Device"),
        Cell::new("Target"),
        Cell::new("Current"),
        Cell::new("RotatingTo"),
        Cell::new("Converged"),
        Cell::new("Quarantined"),
        Cell::new("Until"),
        Cell::new("Attempts"),
        Cell::new("LastAttempt"),
        Cell::new("LastError"),
    ]));
    table.add_row(prettytable::row![
        credential_type,
        device.device_mac,
        target_version,
        or_dash(&device.current_version),
        or_dash(&device.rotating_to_version),
        device.converged,
        device.quarantined,
        or_dash(&device.quarantined_until),
        device.rotate_attempts,
        or_dash(&device.last_attempt_at),
        or_dash(&device.last_error),
    ]);
    table
}

pub(super) async fn rotation_status(
    api_client: &ApiClient,
    args: Args,
    config: &RuntimeConfig,
) -> CarbideCliResult<()> {
    let credential_type = args
        .credential_type
        .to_possible_value()
        .map(|v| v.get_name().to_string())
        .unwrap_or_default();

    let response = api_client
        .0
        .get_credential_rotation_status(forgerpc::CredentialRotationStatusRequest {
            credential_type: forgerpc::RotationCredentialType::from(args.credential_type) as i32,
            device_mac: args.mac_address.map(|mac| mac.to_string()),
        })
        .await?;

    let device = response.device.map(|d| DeviceStatusOutput {
        device_mac: d.device_mac,
        current_version: d.current_version,
        rotating_to_version: d.rotating_to_version,
        converged: d.converged,
        quarantined: d.quarantined,
        quarantined_until: d.quarantined_until.map(|t| t.to_string()),
        rotate_attempts: d.rotate_attempts,
        last_attempt_at: d.last_attempt_at.map(|t| t.to_string()),
        last_error: d.last_error,
    });

    let output = RotationStatusOutput {
        credential_type,
        target_version: response.target_version,
        converged: response.converged,
        pending: response.pending,
        quarantined: response.quarantined,
        complete: response.complete,
        started_at: response
            .started_at
            .map(|t| t.to_string())
            .unwrap_or_default(),
        quarantined_device_macs: response.quarantined_device_macs,
        device,
    };

    // A device-scoped report renders the per-device detail; otherwise the
    // site-wide aggregate. JSON/YAML always carry the full structure.
    let table = match &output.device {
        Some(device) => build_device_table(&output.credential_type, output.target_version, device),
        None => build_status_table(&output),
    };

    match config.format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&output)?),
        OutputFormat::Yaml => println!("{}", serde_yaml::to_string(&output)?),
        OutputFormat::Csv => {
            table
                .to_csv(std::io::stdout())
                .map_err(CarbideCliError::CsvError)?
                .flush()?;
        }
        OutputFormat::AsciiTable => table.printstd(),
    }

    Ok(())
}
