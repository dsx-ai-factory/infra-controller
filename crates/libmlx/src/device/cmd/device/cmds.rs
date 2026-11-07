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

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use prettytable::{Cell, Row, Table};

use crate::device::cmd::device::args::{DeviceAction, DeviceArgs, OutputFormat};
use crate::device::filters::{DeviceFilter, DeviceFilterSet};
use crate::device::report::MlxDeviceReport;

// build_filter_set creates a DeviceFilterSet from a vector of DeviceFilters.
fn build_filter_set(filters: Vec<DeviceFilter>) -> DeviceFilterSet {
    let mut filter_set = DeviceFilterSet::new();
    for filter in filters {
        filter_set.add_filter(filter);
    }
    filter_set
}

// handle processes device command arguments and dispatches to appropriate handlers.
pub fn handle(args: DeviceArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.action {
        DeviceAction::List { detailed, format } => handle_list(detailed, format),
        DeviceAction::Filter {
            detailed,
            format,
            filter,
        } => handle_filter(detailed, format, filter),
        DeviceAction::Describe { device, format } => handle_describe(device, format),
        DeviceAction::Report {
            detailed: _detailed,
            format,
            filter,
        } => handle_report(format, filter),
    }
}

// handle_list discovers and displays all available devices.
fn handle_list(_detailed: bool, format: OutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let devices = crate::device::discovery::discover_devices()?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&devices)?;
            println!("{json}");
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&devices)?;
            println!("{yaml}");
        }
        OutputFormat::AsciiTable => {
            print_devices_table(&devices);
        }
    }

    Ok(())
}

// handle_filter discovers and displays devices that match the specified filters.
fn handle_filter(
    _detailed: bool,
    format: OutputFormat,
    filters: Vec<DeviceFilter>,
) -> Result<(), Box<dyn std::error::Error>> {
    let filter_set = build_filter_set(filters);

    let all_devices = crate::device::discovery::discover_devices()?;

    let filtered_devices: Vec<MlxDeviceInfo> = if filter_set.has_filters() {
        all_devices
            .into_iter()
            .filter(|device| filter_set.matches(device))
            .collect()
    } else {
        all_devices
    };

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&filtered_devices)?;
            println!("{json}");
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&filtered_devices)?;
            println!("{yaml}");
        }
        OutputFormat::AsciiTable => {
            print_devices_table(&filtered_devices);
        }
    }

    Ok(())
}

// handle_describe finds and displays detailed information about a specific device.
fn handle_describe(
    device_spec: String,
    format: OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let device = resolve_device(&device_spec)?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&device)?;
            println!("{json}");
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&device)?;
            println!("{yaml}");
        }
        OutputFormat::AsciiTable => {
            print_device_details_table(&device);
        }
    }

    Ok(())
}

// handle_report generates and displays a complete device discovery report.
fn handle_report(
    format: OutputFormat,
    filters: Vec<DeviceFilter>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = MlxDeviceReport::new();

    // Add all filters to the builder
    for filter in filters {
        builder = builder.with_filter(filter);
    }

    let report = builder
        .collect()
        .map_err(|e| Box::new(std::io::Error::other(e)) as Box<dyn std::error::Error>)?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&report)?;
            println!("{json}");
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&report)?;
            println!("{yaml}");
        }
        OutputFormat::AsciiTable => {
            print_report_table(&report);
        }
    }

    Ok(())
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

    // Add device filters row.
    let filters_str = match &report.filters {
        Some(filters) => filters.to_string(),
        None => "None".to_string(),
    };

    header_table.add_row(Row::new(vec![
        Cell::new("Device Filters"),
        Cell::new(&filters_str),
    ]));

    // Print the header table.
    header_table.printstd();
    println!(); // Add spacing between header and device table

    // Print the devices table.
    print_devices_table(&report.devices);
}

// wrap_text wraps text at the specified character limit with newlines.
fn wrap_text(text: &str, width: usize) -> String {
    if text.len() <= width {
        return text.to_string();
    }

    let mut result = String::new();
    let mut current_line_length = 0;

    for word in text.split_whitespace() {
        if current_line_length == 0 {
            // First word on the line.
            result.push_str(word);
            current_line_length = word.len();
        } else if current_line_length + 1 + word.len() <= width {
            // Word fits on current line.
            result.push(' ');
            result.push_str(word);
            current_line_length += 1 + word.len();
        } else {
            // Word doesn't fit, start new line.
            result.push('\n');
            result.push_str(word);
            current_line_length = word.len();
        }
    }

    result
}

// resolve_device finds a device by its PCI address or identifier.
pub fn resolve_device(
    device_spec: &str,
) -> Result<carbide_libmlx_model::device::info::MlxDeviceInfo, Box<dyn std::error::Error>> {
    // Use existing device discovery logic.
    let devices = crate::device::discovery::discover_devices()?;

    // Find device by PCI address.
    if let Some(found_device) = devices.iter().find(|d| {
        crate::device::discovery::convert_pci_name_to_address(&d.pci_name).unwrap_or_default()
            == device_spec
    }) {
        Ok(found_device.clone())
    } else {
        Err(format!("Device '{device_spec}' not found").into())
    }
}
