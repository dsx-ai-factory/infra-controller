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
use ::rpc::machine_discovery::Gpu as RpcGpu;
use carbide_utils::cmd::Cmd;

use super::HardwareEnumerationResult;

/// Retrieve nvidia-smi data about a machine.
///
/// It is assumed that the machine should have the nvidia kernel module loaded, or this call will fail.
pub(super) fn get_nvidia_smi_data() -> HardwareEnumerationResult<Vec<RpcGpu>> {
    let cmd = Cmd::new("timeout")
        .args(vec![
            "--kill-after=120s",
            "60s",
            "nvidia-smi",
            "--format=csv,noheader",
            concat!(
                "--query-gpu=name,serial,driver_version,vbios_version,inforom.image,memory.total,",
                "clocks.applications.gr,pci.bus_id,pci.device_id,",
                "platform.chassis_serial_number,platform.slot_number,",
                "platform.tray_index,platform.host_id,platform.module_id,platform.gpu_fabric_guid"
            ),
        ])
        .output()?;

    Ok(parse_nvidia_smi_csv(cmd.as_bytes()))
}

/// Parse `nvidia-smi --format=csv,noheader` output. Columns are mapped onto
/// `RpcGpu` fields by position, so the `--query-gpu` order above must match the
/// field declaration order of `Gpu` / `GpuPlatformInfo` in machine_discovery.proto.
fn parse_nvidia_smi_csv(output: &[u8]) -> Vec<RpcGpu> {
    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .trim(csv::Trim::All)
        .from_reader(output);
    let mut gpus = Vec::default();
    for result in csv_reader.deserialize::<RpcGpu>() {
        match result {
            Ok(mut gpu) => {
                gpu.pci_device_id = normalize_pci_device_id(&gpu.pci_device_id);
                gpus.push(gpu);
            }
            Err(error) => tracing::error!(%error, "Could not parse nvidia-smi output"),
        }
    }
    gpus
}

/// nvidia-smi reports `pci.device_id` as a combined 32-bit value: device id in the
/// upper 16 bits, vendor id in the lower 16 bits (e.g. `0x233010DE` for an H100).
/// Return just the device id in the same format udev uses for other PCI devices
/// (`0x2330`). Anything else, including `[N/A]`, becomes empty (surfaced as
/// `null` over REST), so the value is always a device id or absent.
fn normalize_pci_device_id(raw: &str) -> String {
    match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) if hex.len() == 8 && hex.chars().all(|c| c.is_ascii_hexdigit()) => {
            format!("0x{}", hex[..4].to_ascii_lowercase())
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::{normalize_pci_device_id, parse_nvidia_smi_csv};

    #[test]
    fn normalize_pci_device_id_keeps_only_device_half() {
        value_scenarios!(
            normalize_pci_device_id:
            "device+vendor is reduced to lowercase device id" {
                "0x233010DE" => "0x2330".to_string(),
            }

            "unavailable or unexpected values become empty" {
                "" => String::new(),
                "N/A" => String::new(),
                "[N/A]" => String::new(),
                "Unknown Error" => String::new(),
            }
        );
    }

    #[test]
    fn parse_nvidia_smi_csv_maps_pci_device_id_and_platform_info() {
        let output = "NVIDIA GB200, 1652924000001, 570.86.15, 97.00.82.00.5F, G548.0201.00.06, \
                      189471 MiB, 1965 MHz, 00000008:01:00.0, 0x294110DE, 1821324000123, 4, 2, 1, 2, \
                      0xfeeeeeeeeeeeee01\n";
        let gpus = parse_nvidia_smi_csv(output.as_bytes());
        assert_eq!(gpus.len(), 1);
        let gpu = &gpus[0];
        assert_eq!(gpu.name, "NVIDIA GB200");
        assert_eq!(gpu.pci_bus_id, "00000008:01:00.0");
        assert_eq!(gpu.pci_device_id, "0x2941");
        let platform = gpu.platform_info.as_ref().expect("platform_info");
        assert_eq!(platform.chassis_serial, "1821324000123");
        assert_eq!(platform.slot_number, 4);
        assert_eq!(platform.tray_index, 2);
        assert_eq!(platform.host_id, 1);
        assert_eq!(platform.module_id, 2);
        assert_eq!(platform.fabric_guid, "0xfeeeeeeeeeeeee01");
    }
}
