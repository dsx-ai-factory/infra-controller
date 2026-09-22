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

use std::fs;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use carbide_uuid::machine::MachineId;

use super::args::MachineHardwareInfoGpus;
use crate::async_write;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn handle_update_machine_hardware_info_gpus(
    api_client: &ApiClient,
    gpus: MachineHardwareInfoGpus,
) -> CarbideCliResult<()> {
    let gpu_file_contents = fs::read_to_string(gpus.gpu_json_file)?;
    let gpus_from_json: Vec<::rpc::machine_discovery::Gpu> =
        serde_json::from_str(&gpu_file_contents)?;
    api_client
        .update_machine_hardware_info(
            gpus.machine,
            forgerpc::MachineHardwareInfoUpdateType::Gpus,
            gpus_from_json,
        )
        .await
}

#[allow(deprecated)]
pub(super) async fn handle_show_machine_hardware_info(
    api_client: &ApiClient,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: &OutputFormat,
    machine_id: MachineId,
) -> CarbideCliResult<()> {
    let machine = api_client.get_machine(machine_id).await?;
    let mut discovery_info = machine.discovery_info.ok_or_else(|| {
        CarbideCliError::GenericError(format!("Machine {machine_id} has no hardware info"))
    })?;

    // `memory_device_groups` didn't exist before condensing was introduced; rehydrate and clear
    // it here so this raw dump stays byte-for-byte identical to the pre-condensing output, which
    // only ever had `memory_devices`. The field is `#[serde(skip_serializing_if = "Vec::is_empty")]`
    // (see rpc/build.rs), so clearing it drops the key entirely. Serializing `discovery_info`
    // directly (rather than through `serde_json::Value`) keeps the remaining fields in their
    // original struct declaration order without needing the `preserve_order` feature.
    discovery_info.rehydrate_memory_devices()?;

    match output_format {
        OutputFormat::Json => {
            async_write!(
                output_file,
                "{}",
                serde_json::to_string_pretty(&discovery_info)?
            )?;
        }
        OutputFormat::Yaml => {
            async_write!(output_file, "{}", serde_yaml::to_string(&discovery_info)?)?;
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            return Err(CarbideCliError::NotImplemented(
                "ASCII table/CSV formatted output".to_string(),
            ));
        }
    }

    Ok(())
}
