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

use std::cmp::min;
use std::io::Write;
use std::time::Duration;

use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::MachineValidationId;
use errors::MachineValidationError;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

mod errors;
mod machine_validation;
mod plugin_contract;
mod plugin_runner;

pub const MACHINE_VALIDATION_SERVER: &str = "carbide-pxe.forge";
pub const SCHME: &str = "http";

pub const MACHINE_VALIDATION_IMAGE_PATH: &str = "/public/blobs/internal/machine-validation/images/";
pub const MACHINE_VALIDATION_IMAGE_FILE: &str = "/tmp/machine_validation.tar";
pub const MACHINE_VALIDATION_RUNNER_BASE_PATH: &str = "nvcr.io/nvidian/nvforge/";
pub const MACHINE_VALIDATION_RUNNER_TAG: &str = "latest";
pub const IMAGE_LIST_FILE: &str = "/tmp/list.json";
/// Default container-visible base directory for Machine Validation plugin files.
pub const DEFAULT_PLUGIN_CONTRACT_DIR: &str = "/opt/forge/mv";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineValidationOptions {
    pub api: String,
    pub root_ca: String,
    pub client_cert: String,
    pub client_key: String,
    /// Container-visible base directory for plugin input and output files.
    pub plugin_contract_dir: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineValidation {
    pub options: MachineValidationOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MachineValidationFilter {
    pub tags: Vec<String>,
    pub allowed_tests: Vec<String>,
    pub run_unverfied_tests: Option<bool>,
    pub contexts: Option<Vec<String>>,
}

impl From<rpc::forge_agent_control_response::MachineValidationFilter> for MachineValidationFilter {
    fn from(filter: rpc::forge_agent_control_response::MachineValidationFilter) -> Self {
        Self {
            tags: filter.tags,
            allowed_tests: filter.allowed_tests,
            run_unverfied_tests: filter.run_unverfied_tests,
            contexts: filter.contexts.map(|contexts| contexts.items),
        }
    }
}

pub struct MachineValidationManager {}

impl MachineValidationManager {
    pub async fn download_file(url: &str, output_file: &str) -> Result<(), MachineValidationError> {
        let client = reqwest::ClientBuilder::new()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| MachineValidationError::Generic(format!("Client builder error: {e}")))?;

        let res = client
            .get(url)
            .send()
            .await
            .or(Err(MachineValidationError::Generic(format!(
                "Failed to GET from '{}'",
                url
            ))))?;
        let total_size = res
            .content_length()
            .ok_or(MachineValidationError::Generic(format!(
                "Failed to get content length from '{}'",
                url
            )))?;
        let _ = std::fs::remove_file(output_file).or(Err(MachineValidationError::Generic(
            format!("Failed to delete file '{output_file}'"),
        )));

        let mut file = std::fs::File::create(output_file).or(Err(
            MachineValidationError::Generic(format!("Failed to create file '{output_file}'")),
        ))?;
        let mut buffer: u64 = 0;
        let mut stream = res.bytes_stream();

        while let Some(item) = stream.next().await {
            let chunk = item.or(Err(MachineValidationError::Generic(
                "Error while reading stream".to_string(),
            )))?;
            file.write_all(&chunk)
                .or(Err(MachineValidationError::Generic(
                    "Error while writing to file".to_string(),
                )))?;
            let new = min(buffer + (chunk.len() as u64), total_size);
            buffer = new;
        }
        Ok(())
    }

    pub async fn run(
        machine_id: &MachineId,
        platform_name: String,
        options: MachineValidationOptions,
        context: String,
        validation_id: MachineValidationId,
        machine_validation_filter: MachineValidationFilter,
    ) -> Result<(), MachineValidationError> {
        let mc = MachineValidation { options };

        let tests = mc
            .clone()
            .get_machine_validation_tests(rpc::forge::MachineValidationTestsGetRequest {
                supported_platforms: vec![platform_name],
                contexts: if machine_validation_filter
                    .clone()
                    .contexts
                    .unwrap_or_default()
                    .is_empty()
                {
                    vec![context.clone()]
                } else {
                    machine_validation_filter
                        .clone()
                        .contexts
                        .unwrap_or_default()
                },
                is_enabled: Some(true),
                verified: if machine_validation_filter
                    .run_unverfied_tests
                    .unwrap_or(false)
                {
                    None // This indicates run all tests including un verified
                } else {
                    Some(true)
                },
                custom_tags: machine_validation_filter.clone().tags,
                ..rpc::forge::MachineValidationTestsGetRequest::default()
            })
            .await?;
        let mut run_request = rpc::forge::MachineValidationRunRequest {
            validation_id: Some(validation_id),
            ..rpc::forge::MachineValidationRunRequest::default()
        };
        let mut expected_time_duration = 0;
        let mut selected_tests = Vec::new();
        for test in &tests {
            if !machine_validation_filter.allowed_tests.is_empty()
                && !machine_validation_filter
                    .allowed_tests
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(&test.test_id))
            {
                continue;
            }
            run_request.total += 1;
            expected_time_duration += test.timeout.unwrap_or(7200);
            selected_tests.push(test.clone());
        }
        run_request.selected_tests = selected_tests.clone();
        run_request.duration_to_complete = Some(rpc::Duration::from(
            std::time::Duration::from_secs(expected_time_duration as u64),
        ));
        //Update the duration
        mc.clone()
            .update_machine_validation_run(run_request)
            .await?;
        mc.run(
            machine_id,
            selected_tests,
            context,
            validation_id,
            true,
            machine_validation_filter,
        )
        .await?;

        Ok(())
    }
}
