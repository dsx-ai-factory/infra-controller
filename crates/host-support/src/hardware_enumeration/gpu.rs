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
use ::rpc::machine_discovery::{Gpu as RpcGpu, GpuPlatformInfo};
use carbide_utils::cmd::Cmd;

use super::HardwareEnumerationResult;

/// Retrieve nvidia-smi data about a machine.
///
/// It is assumed that the machine should have the nvidia kernel module loaded, or this call will fail.
///
/// GPU frequency prefers `clocks.applications.gr` so existing instance-type
/// filters keep matching, and falls back to `clocks.max.gr`. Driver 615 returns
/// `[Requested functionality has been deprecated]` instead of a clock for the
/// former.
pub(super) fn get_nvidia_smi_data() -> HardwareEnumerationResult<Vec<RpcGpu>> {
    let cmd = Cmd::new("timeout")
        .args(vec![
            "--kill-after=120s",
            "60s",
            "nvidia-smi",
            "--format=csv,noheader",
            concat!(
                "--query-gpu=name,serial,driver_version,vbios_version,inforom.image,memory.total,",
                "clocks.applications.gr,clocks.max.gr,pci.bus_id,",
                "platform.chassis_serial_number,platform.slot_number,platform.tray_index,",
                "platform.host_id,platform.module_id,platform.gpu_fabric_guid"
            )
        ])
        .output()?;

    Ok(parse_nvidia_smi_data(&cmd))
}

/// One `nvidia-smi` CSV row.
///
/// Field order must match the `--query-gpu` order in [`get_nvidia_smi_data`].
#[derive(serde::Deserialize)]
struct NvidiaSmiGpu {
    name: String,
    serial: String,
    driver_version: String,
    vbios_version: String,
    inforom_version: String,
    total_memory: String,
    applications_frequency: String,
    max_frequency: String,
    pci_bus_id: String,
    platform_info: Option<GpuPlatformInfo>,
}

impl From<NvidiaSmiGpu> for RpcGpu {
    fn from(gpu: NvidiaSmiGpu) -> Self {
        Self {
            name: gpu.name,
            serial: gpu.serial,
            driver_version: gpu.driver_version,
            vbios_version: gpu.vbios_version,
            inforom_version: gpu.inforom_version,
            total_memory: gpu.total_memory,
            frequency: select_gpu_frequency(&gpu.applications_frequency, &gpu.max_frequency),
            pci_bus_id: gpu.pci_bus_id,
            platform_info: gpu.platform_info,
        }
    }
}

/// Parse `nvidia-smi --format=csv,noheader` output into GPUs.
fn parse_nvidia_smi_data(output: &str) -> Vec<RpcGpu> {
    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .trim(csv::Trim::All)
        .from_reader(output.as_bytes());
    let mut gpus = Vec::default();
    for result in csv_reader.deserialize::<NvidiaSmiGpu>() {
        match result {
            Ok(gpu) => gpus.push(gpu.into()),
            Err(error) => tracing::error!(%error, "Could not parse nvidia-smi output"),
        }
    }

    gpus
}

/// `nvidia-smi` reports unavailable or deprecated fields as bracketed status
/// text, such as `[N/A]` or `[Requested functionality has been deprecated]`.
fn is_nvidia_smi_placeholder(value: &str) -> bool {
    value == "N/A" || (value.starts_with('[') && value.ends_with(']'))
}

/// Prefer the application clock so existing instance-type filters keep
/// matching, and fall back to the maximum clock when nvidia-smi no longer
/// reports a usable application clock.
fn select_gpu_frequency(applications: &str, max: &str) -> String {
    let usable = |value: &str| !value.is_empty() && !is_nvidia_smi_placeholder(value);

    if usable(applications) {
        applications.to_string()
    } else if usable(max) {
        max.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    fn gpu_row(applications_frequency: &str, max_frequency: &str) -> String {
        format!(
            "NVIDIA H200,1650123456789,615.71.10,96.00.30.00.01,165.0.15.0.0,143771 MiB,\
             {applications_frequency},{max_frequency},0000:3b:00.0,SN-1,1,0,0,0,gpu-1"
        )
    }

    /// Every column lands in its `Gpu` field, including the nested platform
    /// fields that follow the frequency columns.
    #[test]
    fn parses_gpu_row_fields() {
        let gpus = parse_nvidia_smi_data(&gpu_row("1980 MHz", "1980 MHz"));

        assert_eq!(gpus.len(), 1);
        let gpu = &gpus[0];
        assert_eq!(gpu.name, "NVIDIA H200");
        assert_eq!(gpu.serial, "1650123456789");
        assert_eq!(gpu.driver_version, "615.71.10");
        assert_eq!(gpu.vbios_version, "96.00.30.00.01");
        assert_eq!(gpu.inforom_version, "165.0.15.0.0");
        assert_eq!(gpu.total_memory, "143771 MiB");
        assert_eq!(gpu.frequency, "1980 MHz");
        assert_eq!(gpu.pci_bus_id, "0000:3b:00.0");
        let platform_info = gpu.platform_info.as_ref().expect("platform info");
        assert_eq!(platform_info.chassis_serial, "SN-1");
        assert_eq!(platform_info.slot_number, 1);
        assert_eq!(platform_info.tray_index, 0);
        assert_eq!(platform_info.host_id, 0);
        assert_eq!(platform_info.module_id, 0);
        assert_eq!(platform_info.fabric_guid, "gpu-1");
    }

    #[test]
    fn selects_gpu_frequency_with_max_fallback() {
        struct Row {
            applications: &'static str,
            max: &'static str,
        }

        value_scenarios!(run = |Row { applications, max }| {
            parse_nvidia_smi_data(&gpu_row(applications, max))[0]
                .frequency
                .clone()
        };
            "application clock available" {
                Row {
                    applications: "1980 MHz",
                    max: "2000 MHz",
                } => "1980 MHz".to_string(),
                Row {
                    applications: "1980 MHz",
                    max: "[N/A]",
                } => "1980 MHz".to_string(),
            }

            "application clock unavailable" {
                Row {
                    applications: "[Requested functionality has been deprecated]",
                    max: "1980 MHz",
                } => "1980 MHz".to_string(),
                Row {
                    applications: "[N/A]",
                    max: "1980 MHz",
                } => "1980 MHz".to_string(),
                Row {
                    applications: "",
                    max: "1980 MHz",
                } => "1980 MHz".to_string(),
            }

            "no clock reported" {
                Row {
                    applications: "[N/A]",
                    max: "[N/A]",
                } => String::new(),
                Row {
                    applications: "",
                    max: "",
                } => String::new(),
            }
        );
    }
}
