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
use std::collections::HashMap;
use std::error::Error;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use carbide_instrument::emit;
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::dpu_remediations::RemediationId;
use carbide_uuid::machine::DpuMachineId;
use rand::RngExt;
use rpc::Metadata;
use rpc::forge::{
    GetNextRemediationForMachineRequest, RemediationApplicationStatus, RemediationAppliedRequest,
};
use rpc::forge_tls_client::ForgeClientConfig;

use crate::metrics::{
    RemediationApplyFailed, RemediationClientCreationFailed, RemediationFetchFailed,
    RemediationResponseInvalid, RemediationStatusOutputDecodeFailed,
};

const MIN_INITIAL_DELAY_TIME_SECS: u64 = 48; // 80% of 60
const MAX_INITIAL_DELAY_TIME_SECS: u64 = 72; // 120% of 60

const MIN_LOOP_DELAY_TIME_SECS: u64 = 240; // 80% of 300
const MAX_LOOP_DELAY_TIME_SECS: u64 = 360; // 120% of 300

const MAX_SCRIPT_TIMEOUT_SECS: u64 = 120; // two minutes, best I can do.

pub struct MachineInfo {
    machine_id: DpuMachineId,
}

impl MachineInfo {
    pub fn new(machine_id: DpuMachineId) -> Self {
        Self { machine_id }
    }
    pub fn get_envs(&self, status_path: &Path) -> HashMap<String, String> {
        HashMap::from([
            ("FORGE_MACHINE_ID".to_string(), self.machine_id.to_string()),
            (
                "FORGE_SCRIPT_JSON_STATUS_PATH".to_string(),
                status_path.display().to_string(),
            ),
        ])
    }
}

pub struct RemediationExecutor {
    forge_api_server: String,
    forge_client_config: Arc<ForgeClientConfig>,
    machine_info: MachineInfo,
}

impl RemediationExecutor {
    pub fn new(
        forge_api_server: String,
        forge_client_config: Arc<ForgeClientConfig>,
        machine_info: MachineInfo,
    ) -> Self {
        Self {
            forge_api_server,
            forge_client_config,
            machine_info,
        }
    }

    pub async fn handle_remediation(
        &self,
        remediation_id: RemediationId,
        script: String,
        mut client: rpc::forge_tls_client::ForgeClientT,
    ) -> Result<(), Box<dyn Error>> {
        // setup tmp dir with new random UUID
        let tmp_dir_location = format!("/tmp/remediations/{}", uuid::Uuid::new_v4());
        let tmp_dir_path = Path::new(&tmp_dir_location);

        // setup a file for stdout stderr for the process
        let stdout_path = tmp_dir_path.join("stdout");
        let stderr_path = tmp_dir_path.join("stderr");
        let status_path = tmp_dir_path.join("status");
        tokio::fs::create_dir_all(tmp_dir_path).await?;
        tokio::fs::File::create(&stdout_path).await?;
        tokio::fs::File::create(&stderr_path).await?;
        tokio::fs::File::create(&status_path).await?;

        let envs = self.machine_info.get_envs(&status_path);
        let process = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .envs(envs)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()?;
        let output_fut = process.wait_with_output();

        let (succeeded, results) =
            match tokio::time::timeout(Duration::from_secs(MAX_SCRIPT_TIMEOUT_SECS), output_fut)
                .await
            {
                Ok(result) => match result {
                    Ok(output) => {
                        let exit_status = output.status;
                        let succeeded = exit_status.success();
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        tokio::fs::write(&stdout_path, &output.stdout).await.ok();
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        tokio::fs::write(&stderr_path, &output.stderr).await.ok();

                        tracing::debug!("Remediation stdout:\n{}", stdout);
                        tracing::debug!("Remediation stderr:\n{}", stderr);

                        let status_file_str = tokio::fs::read_to_string(&status_path).await?;
                        let results = if status_file_str.is_empty() {
                            HashMap::new()
                        } else {
                            match serde_json::from_str::<HashMap<String, String>>(&status_file_str)
                            {
                                Ok(results) => results,
                                Err(error) => {
                                    emit(RemediationStatusOutputDecodeFailed::new(
                                        self.machine_info.machine_id.into(),
                                        remediation_id,
                                        format!("{error:?}"),
                                    ));
                                    HashMap::new()
                                }
                            }
                        };

                        (succeeded, results)
                    }
                    Err(process_error) => {
                        let results = HashMap::from([(
                            "status".to_string(),
                            format!("process_execution_error: {process_error}"),
                        )]);

                        (false, results)
                    }
                },
                Err(_elapsed_timeout_err) => {
                    let results = HashMap::from([(
                        "status".to_string(),
                        "elapsed_script_timeout_error".to_string(),
                    )]);

                    (false, results)
                }
            };

        let metadata = results.none_if_empty().map(|results| {
            let labels = results
                .into_iter()
                .map(|(k, v)| rpc::forge::Label {
                    key: k,
                    value: Some(v),
                })
                .collect();
            Metadata {
                name: "".to_string(),
                description: "".to_string(),
                labels,
            }
        });

        let application_status = RemediationApplicationStatus {
            succeeded,
            metadata,
        };

        let applied_request = RemediationAppliedRequest {
            remediation_id: Some(remediation_id),
            dpu_machine_id: Some(self.machine_info.machine_id),
            status: Some(application_status),
        };

        client.remediation_applied(applied_request).await?;

        Ok(())
    }
    pub async fn run(&self) {
        // setup a random initial sleep AND a random per-loop sleep so that the durations don't thundering herd the api server
        let initial_delay_time =
            rand::rng().random_range(MIN_INITIAL_DELAY_TIME_SECS..MAX_INITIAL_DELAY_TIME_SECS);
        tokio::time::sleep(tokio::time::Duration::from_secs(initial_delay_time)).await;

        // setup log rotation for tmpdir so we don't fill the disk
        loop {
            match forge_dpu_agent_utils::utils::create_forge_client(
                self.forge_api_server.as_str(),
                &self.forge_client_config,
            )
            .await
            {
                Ok(mut client) => {
                    // fetch next remediation from remediation server
                    let request = GetNextRemediationForMachineRequest {
                        dpu_machine_id: Some(self.machine_info.machine_id),
                    };
                    match client.get_next_remediation_for_machine(request).await {
                        Ok(next_remediation) => {
                            let next_remediation = next_remediation.into_inner();
                            match (
                                next_remediation.remediation_script.as_ref(),
                                next_remediation.remediation_id.as_ref(),
                            ) {
                                (Some(remediation_script), Some(remediation_id)) => {
                                    match self
                                        .handle_remediation(
                                            *remediation_id,
                                            remediation_script.clone(),
                                            client,
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            tracing::debug!("Remediation successfully applied.");
                                        }
                                        Err(error) => {
                                            emit(RemediationApplyFailed::new(
                                                self.machine_info.machine_id.into(),
                                                *remediation_id,
                                                format!("{error:?}"),
                                            ));
                                        }
                                    }
                                }
                                (None, None) => {
                                    tracing::debug!("no remediation this loop, nothing to do.");
                                }
                                _ => {
                                    emit(RemediationResponseInvalid::new(
                                        self.machine_info.machine_id.into(),
                                        next_remediation.remediation_script.is_some(),
                                        next_remediation.remediation_id.is_some(),
                                    ));
                                }
                            }
                        }
                        Err(remediation_fetch_error) => {
                            emit(RemediationFetchFailed::new(
                                self.machine_info.machine_id.into(),
                                format!("{remediation_fetch_error:?}"),
                            ));
                        }
                    }
                }
                Err(client_creation_error) => {
                    emit(RemediationClientCreationFailed::new(
                        self.machine_info.machine_id.into(),
                        format!("{client_creation_error:?}"),
                    ));
                }
            }

            // setup a random initial sleep AND a random per-loop sleep so that the durations don't thundering herd the api server
            let per_loop_delay_time =
                rand::rng().random_range(MIN_LOOP_DELAY_TIME_SECS..MAX_LOOP_DELAY_TIME_SECS);
            tokio::time::sleep(tokio::time::Duration::from_secs(per_loop_delay_time)).await;
        }
    }
}
