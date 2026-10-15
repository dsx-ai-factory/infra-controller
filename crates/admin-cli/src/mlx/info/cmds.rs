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

// info/cmds.rs
// Command handlers for info operations.

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use libmlx::device::report::MlxDeviceReport;
use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;
use rpc::protos::mlx_device as mlx_device_pb;

use super::args::{InfoDeviceCommand, InfoIdentitiesCommand, InfoMachineCommand};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::mlx::wrap_text;
use crate::{async_write, async_write_table_as_csv, async_writeln};

// handle_device_info gets an MlxDeviceInfo for a device on a machine.
impl Run for InfoDeviceCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminDeviceInfoRequest = self.into();
        let response = ctx.api_client.0.mlx_admin_show_device(request).await?;

        let device_info: MlxDeviceInfo = match response.device_info {
            Some(device_info) => device_info.try_into().map_err(|e| {
                CarbideCliError::GenericError(format!("failed to convert device info: {}", e))
            }),
            None => Err(CarbideCliError::GenericError(
                "no device info found for device".to_string(),
            )),
        }?;

        match &ctx.config.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&device_info)?);
            }
            OutputFormat::Yaml => {
                println!("{}", serde_yaml::to_string(&device_info)?);
            }
            OutputFormat::AsciiTable => {
                print_device_details_table(&device_info);
            }
            OutputFormat::Csv => {
                println!("CSV not yet supported");
            }
        }
        Ok(())
    }
}

// handle_device_report gets an MlxDeviceReport for a machine.
impl Run for InfoMachineCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminDeviceReportRequest = self.into();
        let response = ctx.api_client.0.mlx_admin_show_machine(request).await?;

        let device_report: MlxDeviceReport = match response.device_report {
            Some(device_report) => device_report.try_into().map_err(|e| {
                CarbideCliError::GenericError(format!("failed to convert device report: {}", e))
            }),
            None => Err(CarbideCliError::GenericError(
                "no device report found for device".to_string(),
            )),
        }?;
        match &ctx.config.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&device_report)?);
            }
            OutputFormat::Yaml => {
                println!("{}", serde_yaml::to_string(&device_report)?);
            }
            OutputFormat::AsciiTable => {
                print_report_table(&device_report);
            }
            OutputFormat::Csv => {
                println!("CSV not yet supported");
            }
        }

        Ok(())
    }
}

impl Run for InfoIdentitiesCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminDeviceIdentitiesRequest = self.into();
        let response = ctx
            .api_client
            .0
            .mlx_admin_show_device_identities(request)
            .await?;

        match ctx.config.format {
            OutputFormat::Json => {
                async_writeln!(
                    ctx.output_file,
                    "{}",
                    serde_json::to_string_pretty(&response)?
                )?;
            }
            OutputFormat::Yaml => {
                async_write!(ctx.output_file, "{}", serde_yaml::to_string(&response)?)?;
            }
            OutputFormat::AsciiTable => {
                let Some(report) = response.report else {
                    async_writeln!(ctx.output_file, "No stored NIC observation")?;
                    return Ok(());
                };

                let observed_at = report
                    .observed_at
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "Unknown".to_string());
                async_writeln!(ctx.output_file, "Scout observed at: {observed_at}")?;
                async_writeln!(
                    ctx.output_file,
                    "Identity evidence only; firmware/reset eligibility is not established."
                )?;
                async_write!(ctx.output_file, "{}", identity_report_table(&report))?;
            }
            OutputFormat::Csv => {
                let report = response.report.unwrap_or_default();
                async_write_table_as_csv!(ctx.output_file, identity_report_table(&report))?;
            }
        }
        Ok(())
    }
}

fn identity_report_table(report: &mlx_device_pb::MlxDeviceIdentityReport) -> Table {
    let mut table = Table::new();
    table.set_titles(Row::new(
        ["PCI Name", "Base MAC", "Base GUID", "Managed DPU"]
            .into_iter()
            .map(Cell::new)
            .collect(),
    ));
    for device in &report.devices {
        let info = device.device_info.as_ref();
        let managed_dpu = match device.managed_dpu_machine_ids.as_slice() {
            [] => "Unknown".to_string(),
            [machine_id] => machine_id.to_string(),
            machine_ids => format!(
                "Conflicting:\n{}",
                machine_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        };
        table.add_row(Row::new(vec![
            Cell::new(info.map(|info| info.pci_name.as_str()).unwrap_or_default()),
            Cell::new(info.map(|info| info.base_mac.as_str()).unwrap_or_default()),
            Cell::new(
                info.and_then(|info| info.base_guid.as_deref())
                    .unwrap_or_default(),
            ),
            Cell::new(&managed_dpu),
        ]));
    }
    table
}

// print_devices_table displays a list of devices in an ASCII table format.
fn print_devices_table(devices: &[MlxDeviceInfo]) {
    let mut table = Table::new();

    // Add header row.
    table.add_row(Row::new(vec![
        Cell::new("PCI Name"),
        Cell::new("Base MAC"),
        Cell::new("PSID"),
        Cell::new("Device Type"),
        Cell::new("Part Number"),
        Cell::new("FW Version"),
    ]));

    // Add device rows.
    for device in devices {
        table.add_row(Row::new(vec![
            Cell::new(&device.pci_name_pretty()),
            Cell::new(&device.base_mac_pretty()),
            Cell::new(&device.psid_pretty()),
            Cell::new(&device.device_type_pretty()),
            Cell::new(&device.part_number_pretty()),
            Cell::new(&device.fw_version_current_pretty()),
        ]));
    }

    table.printstd();
}

// print_device_details_table displays detailed device information in an ASCII table format.
fn print_device_details_table(device: &MlxDeviceInfo) {
    let mut table = Table::new();

    // Add header row.
    table.add_row(Row::new(vec![Cell::new("Field"), Cell::new("Value")]));

    // Add all device fields.
    for field_name in MlxDeviceInfo::get_all_fields() {
        let value = device.get_field_value(field_name);
        let wrapped_value = wrap_text(&value, 60);

        table.add_row(Row::new(vec![
            Cell::new(field_name),
            Cell::new(&wrapped_value),
        ]));
    }

    table.printstd();
}

// print_report_table displays a device report in ASCII table format with metadata header.
fn print_report_table(report: &MlxDeviceReport) {
    let mut header_table = Table::new();

    // Add hostname row.
    header_table.add_row(Row::new(vec![
        Cell::new("Hostname"),
        Cell::new(&report.hostname),
    ]));

    // Add timestamp row.
    let timestamp_str = report.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    header_table.add_row(Row::new(vec![
        Cell::new("Timestamp"),
        Cell::new(&timestamp_str),
    ]));

    // Add device filters row if present.
    if let Some(filters) = &report.filters {
        let filters_str = filters.to_string();
        header_table.add_row(Row::new(vec![
            Cell::new("Device Filters"),
            Cell::new(&filters_str),
        ]));
    }

    // Print the header table.
    header_table.printstd();
    println!(); // Add spacing between header and device table

    // Print the devices table.
    print_devices_table(&report.devices);
}
