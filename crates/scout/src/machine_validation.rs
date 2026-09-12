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

use ::rpc::forge as rpc;
use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::MachineValidationId;
use regex::Regex;
use tokio::process::Command;

use crate::CarbideClientError;
use crate::cfg::Options;
use crate::client::create_forge_client;

pub(super) async fn completed(
    config: &Options,
    machine_id: &MachineId,
    validation_id: &MachineValidationId,
    machine_validation_error: Option<String>,
) -> Result<(), CarbideClientError> {
    let mut client = create_forge_client(config).await?;
    let request = tonic::Request::new(rpc::MachineValidationCompletedRequest {
        machine_id: Some(*machine_id),
        machine_validation_error,
        validation_id: Some(*validation_id),
    });
    client.machine_validation_completed(request).await?;
    tracing::info!("sending machine validation completed");
    Ok(())
}

async fn get_system_manufacturer_name() -> String {
    let command_string = "dmidecode -s system-sku-number".to_string();

    match Command::new("sh")
        .arg("-c")
        .arg(&command_string)
        .output()
        .await
    {
        Ok(output) => {
            if output.stdout.is_empty() {
                "default".to_string()
            } else {
                let sku = String::from_utf8_lossy(&output.stdout)
                    .to_string()
                    .replace('\n', "");

                let re = Regex::new(r"[ =;:@#\!?\-]").unwrap();
                re.replace_all(&sku, "_").to_string().to_ascii_lowercase()
            }
            // let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
        }
        Err(_) => "default".to_string(),
    }
}

pub(super) async fn run(
    cmd_config: &Options,
    machine_id: &MachineId,
    validation_id: MachineValidationId,
    context: String,
    machine_validation_filter: machine_validation::MachineValidationFilter,
) -> Result<(), CarbideClientError> {
    let platform_name = get_system_manufacturer_name().await;
    let options = machine_validation::MachineValidationOptions {
        api: cmd_config.api.clone(),
        root_ca: cmd_config.root_ca.clone(),
        client_cert: cmd_config.client_cert.clone(),
        client_key: cmd_config.client_key.clone(),
        plugin_contract_dir: cmd_config.machine_validation_plugin_contract_dir.clone(),
    };
    machine_validation::MachineValidationManager::run(
        machine_id,
        platform_name,
        options,
        context,
        validation_id,
        machine_validation_filter,
    )
    .await
    .map_err(|e| CarbideClientError::GenericError(format!("{e}")))?;
    Ok(())
}
