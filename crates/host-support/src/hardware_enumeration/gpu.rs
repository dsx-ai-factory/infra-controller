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
                "clocks.applications.gr,pci.bus_id,platform.chassis_serial_number,platform.slot_number,",
                "platform.tray_index,platform.host_id,platform.module_id,platform.gpu_fabric_guid"
            )
        ])
        .output()?;

    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .trim(csv::Trim::All)
        .from_reader(cmd.as_bytes());
    let mut gpus = Vec::default();
    for result in csv_reader.deserialize() {
        match result {
            Ok(gpu) => gpus.push(gpu),
            Err(error) => tracing::error!(%error, "Could not parse nvidia-smi output"),
        }
    }

    Ok(gpus)
}
