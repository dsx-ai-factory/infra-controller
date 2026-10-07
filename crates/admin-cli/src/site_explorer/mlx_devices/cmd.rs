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
use ::rpc::site_explorer::{BlueFieldOperatingMode, ExploredMlxDevice, MlxDeviceKind};
use prettytable::{Row, Table};
use serde::Serialize;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_write_table_as_csv};

pub(super) async fn mlx_devices(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    api_client: &ApiClient,
    opts: Args,
    page_size: usize,
) -> CarbideCliResult<()> {
    let nic_mode_only = opts.nic_mode_only;
    let expected_version = opts.expected_version;

    let devices: Vec<MlxDeviceRow> = api_client
        .get_all_explored_mlx_devices(page_size, opts.host)
        .await?
        .into_iter()
        // Only devices operating as NICs, when asked.
        .filter(|d| !nic_mode_only || operating_as_nic(d))
        // Only devices whose NIC firmware is below the desired version, when given
        // one. A device with no firmware, or a version we can't parse, is surfaced
        // rather than hidden.
        .filter(|d| {
            expected_version
                .as_deref()
                .is_none_or(|expected| firmware_below(d.firmware_version.as_deref(), expected))
        })
        .map(MlxDeviceRow::from)
        .collect();

    match output_format {
        OutputFormat::Json => {
            async_write!(output_file, "{}", serde_json::to_string_pretty(&devices)?)?;
        }
        OutputFormat::Yaml => {
            async_write!(output_file, "{}", serde_yaml::to_string(&devices)?)?;
        }
        OutputFormat::Csv => {
            async_write_table_as_csv!(output_file, build_table(&devices))?;
        }
        _ => {
            async_write!(output_file, "{}", build_table(&devices))?;
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct MlxDeviceRow {
    host_bmc_ip: String,
    machine_id: Option<String>,
    kind: String,
    part_number: Option<String>,
    serial_number: Option<String>,
    firmware_version: Option<String>,
    nic_mode: Option<String>,
    dpu_bmc_ip: Option<String>,
    pcie_id: Option<String>,
    description: Option<String>,
}

impl From<ExploredMlxDevice> for MlxDeviceRow {
    fn from(device: ExploredMlxDevice) -> Self {
        MlxDeviceRow {
            host_bmc_ip: device.host_bmc_ip,
            machine_id: device.machine_id,
            kind: kind_label(device.device_kind),
            part_number: device.part_number,
            serial_number: device.serial_number,
            firmware_version: device.firmware_version,
            nic_mode: nic_mode_label(device.nic_mode),
            dpu_bmc_ip: device.dpu_bmc_ip,
            pcie_id: device.pcie_id,
            description: device.description,
        }
    }
}

/// Whether a device is operating as a plain NIC -- its Arm OS is down, so scout
/// can't report its firmware and this inventory is the only one that sees it.
///
/// The authoritative signal is `nic_mode`, the mode the DPU's own BMC reports;
/// a `900-9D3B6` DPU flipped into NIC mode passes on that signal even though
/// its SKU says DPU. When the mode is unknown (`nic_mode` unset -- no DPU BMC
/// matched, or its firmware predates mode reporting), fall back to the SKU:
/// the SuperNIC families ship running as NICs, and like the firmware filter
/// below we err toward surfacing a device rather than hiding it.
fn operating_as_nic(device: &ExploredMlxDevice) -> bool {
    match device.nic_mode {
        Some(mode) => mode == BlueFieldOperatingMode::Nic as i32,
        None => {
            device.device_kind == MlxDeviceKind::Bf3NicMode as i32
                || device.device_kind == MlxDeviceKind::Bf3SuperNic as i32
        }
    }
}

fn kind_label(device_kind: i32) -> String {
    match MlxDeviceKind::try_from(device_kind) {
        // Both SuperNIC SKU families render under NVIDIA's product name; the
        // part-number column alongside is the discriminator.
        Ok(MlxDeviceKind::Bf3NicMode) | Ok(MlxDeviceKind::Bf3SuperNic) => "BlueField-3 SuperNIC",
        Ok(MlxDeviceKind::Bf3DpuMode) => "BlueField-3 DPU",
        Ok(MlxDeviceKind::Bf2Dpu) => "BlueField-2 DPU",
        Ok(MlxDeviceKind::Unknown) | Err(_) => "Unknown",
    }
    .to_string()
}

fn nic_mode_label(nic_mode: Option<i32>) -> Option<String> {
    nic_mode.and_then(|mode| match BlueFieldOperatingMode::try_from(mode) {
        Ok(BlueFieldOperatingMode::Nic) => Some("NIC".to_string()),
        Ok(BlueFieldOperatingMode::Dpu) => Some("DPU".to_string()),
        Err(_) => None,
    })
}

fn build_table(devices: &[MlxDeviceRow]) -> Box<Table> {
    let mut table = Table::new();
    table.set_titles(Row::from(vec![
        "Host BMC IP",
        "Kind",
        "Part Number",
        "Serial",
        "NIC FW",
        "NIC Mode",
        "DPU BMC IP",
    ]));
    for device in devices {
        table.add_row(Row::from(vec![
            device.host_bmc_ip.clone(),
            device.kind.clone(),
            device.part_number.clone().unwrap_or_default(),
            device.serial_number.clone().unwrap_or_default(),
            device.firmware_version.clone().unwrap_or_default(),
            device.nic_mode.clone().unwrap_or_default(),
            device.dpu_bmc_ip.clone().unwrap_or_default(),
        ]));
    }
    Box::new(table)
}

/// Whether `actual` NIC firmware is below `expected`, comparing dotted numeric
/// versions (e.g. `32.38.1002` is below `32.42.1000`). A device with no reported
/// firmware -- or a version that doesn't parse as dotted numerics -- is surfaced
/// rather than hidden: better to show a device we can't verify than to miss an
/// outdated one.
fn firmware_below(actual: Option<&str>, expected: &str) -> bool {
    let Some(actual) = actual else {
        return true;
    };
    match (parse_version(actual), parse_version(expected)) {
        (Some(actual), Some(expected)) => actual < expected,
        _ => actual != expected,
    }
}

/// Parses a dotted numeric version (e.g. `32.42.1000`) into comparable parts,
/// returning `None` if any component isn't a non-negative integer.
fn parse_version(version: &str) -> Option<Vec<u64>> {
    version
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn operating_as_nic_prefers_reported_mode_over_sku() {
        struct DeviceState {
            device_kind: MlxDeviceKind,
            nic_mode: Option<BlueFieldOperatingMode>,
        }

        value_scenarios!(
            run = |DeviceState {
                device_kind,
                nic_mode,
            }| {
                let device = ExploredMlxDevice {
                    device_kind: device_kind as i32,
                    nic_mode: nic_mode.map(|mode| mode as i32),
                    ..Default::default()
                };
                operating_as_nic(&device)
            };
            "reported mode takes precedence over SKU" {
                DeviceState {
                    device_kind: MlxDeviceKind::Bf3DpuMode,
                    nic_mode: Some(BlueFieldOperatingMode::Nic),
                } => true,
                DeviceState {
                    device_kind: MlxDeviceKind::Bf3DpuMode,
                    nic_mode: Some(BlueFieldOperatingMode::Dpu),
                } => false,
                DeviceState {
                    device_kind: MlxDeviceKind::Bf3NicMode,
                    nic_mode: Some(BlueFieldOperatingMode::Dpu),
                } => false,
            }

            "missing reported mode falls back to SKU" {
                DeviceState {
                    device_kind: MlxDeviceKind::Bf3NicMode,
                    nic_mode: None,
                } => true,
                DeviceState {
                    device_kind: MlxDeviceKind::Bf3SuperNic,
                    nic_mode: None,
                } => true,
                DeviceState {
                    device_kind: MlxDeviceKind::Bf3DpuMode,
                    nic_mode: None,
                } => false,
                DeviceState {
                    device_kind: MlxDeviceKind::Bf2Dpu,
                    nic_mode: None,
                } => false,
            }
        );
    }

    #[test]
    fn firmware_below_compares_dotted_versions() {
        // Outdated -> below the target.
        assert!(firmware_below(Some("32.38.1002"), "32.42.1000"));
        // At the target -> not below.
        assert!(!firmware_below(Some("32.42.1000"), "32.42.1000"));
        // Newer than the target -> not below (the false positive the != filter had).
        assert!(!firmware_below(Some("32.43.0000"), "32.42.1000"));
        // No reported firmware -> surfaced.
        assert!(firmware_below(None, "32.42.1000"));
        // Unparsable versions fall back to a plain mismatch.
        assert!(firmware_below(Some("weird-fw"), "32.42.1000"));
        assert!(!firmware_below(Some("BF-24.07"), "BF-24.07"));
    }
}
