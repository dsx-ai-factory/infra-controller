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
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use carbide_instrument::{Event, LabelValue, Outcome, emit};
use carbide_utils::cmd::TokioCmd;
use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::MachineValidationId;
use chrono::Utc;
use forge_tls::client_config::ClientCert;
use rpc::forge_tls_client;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tracing::{error, info, trace};

use crate::plugin_runner::{PluginPrivilege, PluginRuntimeSpec, execute_plugin};
use crate::{
    IMAGE_LIST_FILE, MACHINE_VALIDATION_IMAGE_FILE, MACHINE_VALIDATION_IMAGE_PATH,
    MACHINE_VALIDATION_RUNNER_BASE_PATH, MACHINE_VALIDATION_RUNNER_TAG, MACHINE_VALIDATION_SERVER,
    MachineValidation, MachineValidationError, MachineValidationFilter, MachineValidationManager,
    SCHME,
};
const MAX_STRING_STD_SIZE: usize = 1024 * 1024; // 1MB in bytes;
const DEFAULT_TIMEOUT: u64 = 3600;

// The API manager clamps heartbeat-based stale reconciliation to at least three missed beats, so
// low stale_run_timeout config values cannot fail healthy runs between these heartbeat updates.
const MACHINE_VALIDATION_HEARTBEAT_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum MachineValidationHeartbeatStage {
    Initial,
    Periodic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum MachineValidationHeartbeatFailureReason {
    Rejected,
    Rpc,
}

/// A machine-validation heartbeat did not land. Each variant is one
/// (stage, reason) pair; only the RPC cases have an error to report.
#[derive(Event)]
#[event(
    event_name = "machine_validation_heartbeat_failed",
    metric_name = "carbide_machine_validation_heartbeat_failures_total",
    component = "nico-scout",
    metric = counter,
    log = error,
    describe = "Number of machine validation heartbeat failures, by stage and reason.",
    labels(
        stage: MachineValidationHeartbeatStage,
        reason: MachineValidationHeartbeatFailureReason,
    ),
)]
enum MachineValidationHeartbeatFailed {
    #[event(
        labels(
            stage = MachineValidationHeartbeatStage::Initial,
            reason = MachineValidationHeartbeatFailureReason::Rejected
        ),
        message = "initial machine validation heartbeat was rejected"
    )]
    InitialRejected {
        #[context]
        machine_validation_id: MachineValidationId,
        #[context]
        test_id: String,
    },

    #[event(
        labels(
            stage = MachineValidationHeartbeatStage::Periodic,
            reason = MachineValidationHeartbeatFailureReason::Rejected
        ),
        message = "machine validation heartbeat was rejected because run or attempt is no longer active"
    )]
    PeriodicRejected {
        #[context]
        machine_validation_id: MachineValidationId,
        #[context]
        test_id: String,
    },

    #[event(
        labels(
            stage = MachineValidationHeartbeatStage::Initial,
            reason = MachineValidationHeartbeatFailureReason::Rpc
        ),
        message = "failed to send initial machine validation heartbeat"
    )]
    InitialRpc {
        #[context]
        machine_validation_id: MachineValidationId,
        #[context]
        test_id: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            stage = MachineValidationHeartbeatStage::Periodic,
            reason = MachineValidationHeartbeatFailureReason::Rpc
        ),
        message = "failed to send machine validation heartbeat"
    )]
    PeriodicRpc {
        #[context]
        machine_validation_id: MachineValidationId,
        #[context]
        test_id: String,
        #[context]
        error: String,
    },
}

impl MachineValidationHeartbeatFailed {
    /// A heartbeat the server declined; there is no transport error.
    fn rejected(
        stage: MachineValidationHeartbeatStage,
        machine_validation_id: MachineValidationId,
        test_id: String,
    ) -> Self {
        match stage {
            MachineValidationHeartbeatStage::Initial => Self::InitialRejected {
                machine_validation_id,
                test_id,
            },
            MachineValidationHeartbeatStage::Periodic => Self::PeriodicRejected {
                machine_validation_id,
                test_id,
            },
        }
    }

    /// A heartbeat that never reached the server.
    fn rpc(
        stage: MachineValidationHeartbeatStage,
        machine_validation_id: MachineValidationId,
        test_id: String,
        error: impl std::fmt::Display,
    ) -> Self {
        let error = error.to_string();
        match stage {
            MachineValidationHeartbeatStage::Initial => Self::InitialRpc {
                machine_validation_id,
                test_id,
                error,
            },
            MachineValidationHeartbeatStage::Periodic => Self::PeriodicRpc {
                machine_validation_id,
                test_id,
                error,
            },
        }
    }
}
/// One machine-validation result write. Each variant is the result.
#[derive(Event)]
#[event(
    event_name = "machine_validation_result_persistence_finished",
    metric_name = "carbide_machine_validation_result_persistence_attempts_total",
    component = "nico-scout",
    metric = counter,
    describe = "Number of machine validation result persistence attempts, by outcome.",
    labels(outcome: Outcome),
)]
enum MachineValidationResultPersistenceFinished {
    #[event(
        labels(outcome = Outcome::Ok),
        log = info,
        message = "Sent machine validation result to API server"
    )]
    Ok {
        #[context]
        machine_validation_id: MachineValidationId,
        #[context]
        test_id: String,
        #[context]
        test_name: String,
    },

    #[event(
        labels(outcome = Outcome::Error),
        log = error,
        message = "Failed to send machine validation result to API server"
    )]
    Error {
        #[context]
        machine_validation_id: MachineValidationId,
        #[context]
        test_id: String,
        #[context]
        test_name: String,
        #[context]
        error: String,
    },
}

impl MachineValidationResultPersistenceFinished {
    /// Which case a persist attempt landed in. `Ok` has no error to report, so
    /// the failure text exists only on `Error`.
    fn from_result<E>(
        machine_validation_id: MachineValidationId,
        test_id: String,
        test_name: String,
        result: &Result<(), E>,
    ) -> Self
    where
        E: std::fmt::Display,
    {
        match result {
            Result::Ok(()) => Self::Ok {
                machine_validation_id,
                test_id,
                test_name,
            },
            Result::Err(error) => Self::Error {
                machine_validation_id,
                test_id,
                test_name,
                error: error.to_string(),
            },
        }
    }
}
struct MachineValidationExecution {
    result: rpc::forge::MachineValidationResult,
    heartbeat: Option<MachineValidationHeartbeatGuard>,
}

/// The immutable plugin data attached to a run item when its plan was created.
struct PluginRunItem {
    run_item_id: String,
    attempt: u32,
    plugin: Option<rpc::forge::MachineValidationPlugin>,
    full_host_approved: bool,
}

impl MachineValidationExecution {
    fn without_heartbeat(result: rpc::forge::MachineValidationResult) -> Self {
        Self {
            result,
            heartbeat: None,
        }
    }

    fn with_heartbeat(
        result: rpc::forge::MachineValidationResult,
        heartbeat: MachineValidationHeartbeatGuard,
    ) -> Self {
        Self {
            result,
            heartbeat: Some(heartbeat),
        }
    }
}

struct MachineValidationHeartbeatGuard {
    task: Option<JoinHandle<()>>,
}

impl MachineValidationHeartbeatGuard {
    fn new(task: JoinHandle<()>) -> Self {
        Self { task: Some(task) }
    }

    async fn stop(mut self) {
        let Some(task) = self.task.take() else {
            return;
        };

        task.abort();
        match task.await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => {}
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => error!(error = %e, "machine validation heartbeat task failed"),
        }
    }
}

impl Drop for MachineValidationHeartbeatGuard {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

fn plugin_execution_spec(
    test: &rpc::forge::MachineValidationTest,
) -> Result<PluginRuntimeSpec, String> {
    let plugin = test
        .plugin
        .as_ref()
        .ok_or_else(|| "plugin execution requires a plugin definition".to_owned())?;
    if plugin.host_access_full && !test.full_host_approved {
        return Err("plugin full-host access is not approved for this revision".to_owned());
    }
    let privilege = if plugin.host_access_full {
        PluginPrivilege::FullHost
    } else if plugin.privileged {
        PluginPrivilege::Privileged
    } else {
        PluginPrivilege::Isolated
    };
    Ok(PluginRuntimeSpec {
        image: plugin.image.clone(),
        entrypoint: plugin.entrypoint.clone(),
        privilege,
    })
}

fn parse_plugin_parameters(parameters_json: &str) -> Result<Value, String> {
    if parameters_json.is_empty() {
        return Ok(serde_json::json!({}));
    }
    let parameters: Value = serde_json::from_str(parameters_json)
        .map_err(|error| format!("invalid plugin parameters: {error}"))?;
    if !parameters.is_object() {
        return Err("plugin parameters must be a JSON object".to_owned());
    }
    Ok(parameters)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the v1 plugin contract deliberately exposes each execution identity field"
)]
fn plugin_input(
    validation_id: MachineValidationId,
    run_item_id: &str,
    attempt: u32,
    machine_id: &MachineId,
    context: &str,
    test: &rpc::forge::MachineValidationTest,
    deadline: chrono::DateTime<Utc>,
    parameters: Value,
) -> Value {
    let plugin = test
        .plugin
        .as_ref()
        .expect("plugin input requires a plugin");
    json!({
        "contractVersion": "v1",
        "kind": "MachineValidationPluginInput",
        "runId": validation_id.to_string(),
        "runItemId": run_item_id,
        "attempt": attempt,
        "machineId": machine_id.to_string(),
        "context": context,
        "plugin": {
            "testId": test.test_id,
            "version": test.version,
            "image": plugin.image,
        },
        "deadline": deadline.to_rfc3339(),
        "parameters": parameters,
    })
}

fn bounded_plugin_output(output: String) -> String {
    if output.len() <= MAX_STRING_STD_SIZE {
        return output;
    }
    let mut end = MAX_STRING_STD_SIZE;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[plugin output truncated at 1 MiB]", &output[..end])
}

impl MachineValidation {
    pub(crate) async fn get_container_auth_config(self) -> Result<(), MachineValidationError> {
        let file_name = "/root/.docker/config.json".to_string();
        match self
            .get_external_config(file_name.clone(), Some("container_auth".to_string()))
            .await
        {
            Ok(()) => trace!(
                external_config_file = %file_name,
                "Fetched external machine validation config",
            ),
            Err(e) => trace!(
                error = %e,
                "Failed to fetch container authentication config",
            ),
        }
        Ok(())
    }
    pub(crate) async fn get_external_config(
        self,
        external_config_file: String,
        external_config_name: Option<String>,
    ) -> Result<(), MachineValidationError> {
        tracing::info!(
            external_config_file = %external_config_file,
            "Fetching external machine validation config",
        );

        let name = if let Some(name) = external_config_name {
            name
        } else {
            let path = Path::new(&external_config_file);
            path.file_name().unwrap().to_str().unwrap().to_string()
        };

        let mut client = self.create_forge_client().await?;

        let request =
            tonic::Request::new(rpc::forge::GetMachineValidationExternalConfigRequest { name });
        let response = match client.get_machine_validation_external_config(request).await {
            Ok(res) => res,
            Err(e) => {
                return Err(MachineValidationError::ApiClient(
                    "get_external_config".to_owned(),
                    e.to_string(),
                ));
            }
        };
        let config = response.into_inner().config.unwrap().config;
        let mut file = File::create(external_config_file.clone()).map_err(|e| {
            MachineValidationError::File(external_config_file.clone(), e.to_string())
        })?;
        let s = String::from_utf8(config)
            .map_err(|e| MachineValidationError::Generic(e.to_string()))?;
        file.write_all(s.as_bytes()).map_err(|e| {
            MachineValidationError::File(external_config_file.clone(), e.to_string())
        })?;
        Ok(())
    }
    pub(crate) async fn create_forge_client(
        &self,
    ) -> Result<forge_tls_client::ForgeClientT, MachineValidationError> {
        let client_config = ForgeClientConfig::new(
            self.options.root_ca.clone(),
            Some(ClientCert {
                cert_path: self.options.client_cert.clone(),
                key_path: self.options.client_key.clone(),
            }),
        );
        let api_config = ApiConfig::new(&self.options.api, &client_config);

        let client = forge_tls_client::ForgeTlsClient::retry_build(&api_config)
            .await
            .map_err(|err| MachineValidationError::Generic(err.to_string()))?;
        Ok(client)
    }

    pub(crate) async fn persist(
        self,
        data: Option<rpc::forge::MachineValidationResult>,
    ) -> Result<(), MachineValidationError> {
        tracing::info!(
            validation_name = %data.as_ref().expect("validation result").name,
            "Persisting machine validation result",
        );
        let mut client = self.create_forge_client().await?;
        let request =
            tonic::Request::new(rpc::forge::MachineValidationResultPostRequest { result: data });
        client
            .persist_validation_result(request)
            .await
            .map_err(|e| {
                MachineValidationError::ApiClient(
                    "persist_validation_result".to_owned(),
                    e.to_string(),
                )
            })?;
        Ok(())
    }

    pub(crate) async fn heartbeat_machine_validation_run(
        self,
        validation_id: MachineValidationId,
        test_id: Option<String>,
    ) -> Result<bool, MachineValidationError> {
        let mut client = self.create_forge_client().await?;
        let response = client
            .heartbeat_machine_validation_run(tonic::Request::new(
                rpc::forge::MachineValidationHeartbeatRequest {
                    validation_id: Some(validation_id),
                    target: test_id
                        .map(rpc::forge::machine_validation_heartbeat_request::Target::TestId),
                },
            ))
            .await
            .map_err(|e| {
                MachineValidationError::ApiClient(
                    "heartbeat_machine_validation_run".to_owned(),
                    e.to_string(),
                )
            })?
            .into_inner();
        Ok(response.accepted)
    }

    fn spawn_machine_validation_heartbeat(
        self,
        validation_id: MachineValidationId,
        test_id: String,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(MACHINE_VALIDATION_HEARTBEAT_INTERVAL);
            loop {
                interval.tick().await;
                match self
                    .clone()
                    .heartbeat_machine_validation_run(validation_id, Some(test_id.clone()))
                    .await
                {
                    Ok(true) => {
                        trace!(machine_validation_id = %validation_id, test_id = %test_id, "sent machine validation heartbeat")
                    }
                    Ok(false) => {
                        emit(MachineValidationHeartbeatFailed::rejected(
                            MachineValidationHeartbeatStage::Periodic,
                            validation_id,
                            test_id,
                        ));
                        return;
                    }
                    Err(e) => {
                        emit(MachineValidationHeartbeatFailed::rpc(
                            MachineValidationHeartbeatStage::Periodic,
                            validation_id,
                            test_id.clone(),
                            e,
                        ));
                    }
                }
            }
        })
    }

    pub(crate) async fn get_machine_validation_tests(
        self,
        test_request: rpc::forge::MachineValidationTestsGetRequest,
    ) -> Result<Vec<rpc::forge::MachineValidationTest>, MachineValidationError> {
        tracing::info!(
            request = ?test_request,
            "Fetching machine validation tests",
        );
        let mut client = self.create_forge_client().await?;
        let request = tonic::Request::new(test_request);
        let response = client
            .get_machine_validation_tests(request)
            .await
            .map_err(|e| {
                MachineValidationError::ApiClient(
                    "get_machine_validation_tests".to_owned(),
                    e.to_string(),
                )
            })?
            .into_inner();

        Ok(response.tests)
    }

    pub async fn get_container_images() -> Result<(), MachineValidationError> {
        let url: String = format!(
            "{}://{}{}{}",
            SCHME, MACHINE_VALIDATION_SERVER, MACHINE_VALIDATION_IMAGE_PATH, "list.json"
        );
        tracing::info!(url = %url, "Fetching machine validation image list");
        MachineValidationManager::download_file(&url, IMAGE_LIST_FILE).await?;

        let json_file_path = Path::new("/tmp/list.json");
        let reader = BufReader::new(File::open(json_file_path).map_err(|e| {
            MachineValidationError::File(
                format!(
                    "File {} open error",
                    json_file_path.to_str().unwrap_or_default()
                ),
                e.to_string(),
            )
        })?);

        #[derive(Debug, Serialize, Deserialize)]
        struct ImageList {
            images: Vec<String>,
        }

        let list: ImageList = serde_json::from_reader(reader)
            .map_err(|e| MachineValidationError::Generic(format!("Json read error: {e}")))?;
        for image_name in list.images {
            match Self::import_container(&image_name, MACHINE_VALIDATION_RUNNER_TAG).await {
                Ok(data) => {
                    trace!(
                        image_reference = %data,
                        "Imported machine validation container image",
                    )
                }
                Err(e) => error!(
                    error = %e,
                    "Failed to import machine validation container image",
                ),
            };
        }
        Ok(())
    }

    pub async fn import_container(
        image_name: &str,
        image_tag: &str,
    ) -> Result<String, MachineValidationError> {
        tracing::info!(%image_name, "Importing machine validation image");
        let url: String = format!(
            "{SCHME}://{MACHINE_VALIDATION_SERVER}{MACHINE_VALIDATION_IMAGE_PATH}{image_name}.tar"
        );
        tracing::info!(url = %url, "Fetching machine validation image");
        MachineValidationManager::download_file(&url, MACHINE_VALIDATION_IMAGE_FILE).await?;

        let command_string = format!(" ctr images import {MACHINE_VALIDATION_IMAGE_FILE}");
        info!(
            command = %command_string,
            "Executing machine validation command",
        );
        TokioCmd::new("sh")
            .args(vec!["-c".to_string(), command_string])
            .timeout(DEFAULT_TIMEOUT)
            .output_with_timeout()
            .await
            .map_err(|e| MachineValidationError::Generic(e.to_string()))?;
        Ok(format!(
            "{MACHINE_VALIDATION_RUNNER_BASE_PATH}{image_name}:{image_tag}"
        ))
    }

    /// Resolve registry credentials for `image_name` from the Nico API.
    /// Returns `(username, password, registry)` when credentials are found,
    /// or `None` when the registry cannot be determined, no credential is
    /// stored, or the RPC fails — in all cases the pull proceeds without
    /// credentials.
    async fn resolve_registry_credential(
        &self,
        image_name: &str,
    ) -> Option<(String, String, String)> {
        let registry = match Self::extract_registry(image_name) {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "Skipping registry credential lookup");
                return None;
            }
        };
        let mut client = match self.create_forge_client().await {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "Failed to build Forge client for registry credential lookup");
                return None;
            }
        };
        let response = client
            .get_container_registry_credential(tonic::Request::new(
                rpc::forge::GetContainerRegistryCredentialRequest {
                    registry: registry.to_string(),
                },
            ))
            .await;
        match response {
            Ok(resp) => {
                let r = resp.into_inner();
                Some((r.username, r.password, registry.to_string()))
            }
            Err(status) if status.code() == tonic::Code::NotFound => {
                // No credential registered — treat as public registry.
                None
            }
            Err(e) => {
                error!(registry = %registry, error = %e, "Failed to fetch registry credential");
                None
            }
        }
    }

    /// Extract the registry hostname from an OCI image reference.
    /// `"nvcr.io/foo/bar:tag"` → `Ok("nvcr.io")`.
    /// Returns an error for bare image names and Docker Hub path shorthands
    /// that carry no explicit registry hostname.
    fn extract_registry(image_name: &str) -> Result<&str, MachineValidationError> {
        // Without a slash the entire string is a bare image name or image:tag
        // (e.g. "ubuntu:22.04") — there is no registry component to extract.
        let Some(slash) = image_name.find('/') else {
            return Err(MachineValidationError::Generic(format!(
                "cannot determine registry from image reference {image_name:?}: \
                 no host component (no '/' found)"
            )));
        };
        let first = &image_name[..slash];
        // The first component is a registry hostname when it contains a dot
        // (e.g. "nvcr.io"), a colon for host:port (e.g. "localhost:5000"),
        // or is exactly "localhost" (port-less local registry).
        // Plain path prefixes like "library" are Docker Hub shorthands with no
        // explicit registry host.
        if first.contains('.') || first.contains(':') || first == "localhost" {
            Ok(first)
        } else {
            Err(MachineValidationError::Generic(format!(
                "cannot determine registry from image reference {image_name:?}: \
                 first component {first:?} is not a hostname"
            )))
        }
    }

    /// Pull `image_name` into the local containerd store via nerdctl.
    ///
    /// If the Nico API has a credential for the image's registry, logs in
    /// with `nerdctl login --password-stdin` before pulling so the password
    /// never appears in process arguments or logs.
    pub async fn pull_container(&self, image_name: &str) -> Result<(), String> {
        self.pull_container_with_timeout(image_name, Duration::from_secs(DEFAULT_TIMEOUT))
            .await
    }

    /// Pull an image while consuming no more than the supplied attempt budget.
    async fn pull_container_with_timeout(
        &self,
        image_name: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        tracing::info!(%image_name, "Pulling machine validation image");
        let deadline = Instant::now() + timeout;

        if let Some((username, password, registry)) =
            self.resolve_registry_credential(image_name).await
        {
            let registry = registry.as_str();
            // Pipe the password via stdin so it never appears in process arguments.
            // stdout/stderr are discarded (null) — we only need the exit status, and
            // leaving them piped-but-unread risks a pipe-buffer deadlock if nerdctl
            // produces enough output before exiting.
            use std::process::Stdio;
            use std::time::Duration;

            use tokio::io::AsyncWriteExt;

            const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);
            let login_timeout =
                LOGIN_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
            if login_timeout.is_zero() {
                return Err(format!("timed out before logging into registry {registry}"));
            }

            match tokio::process::Command::new("nerdctl")
                .args([
                    "-n",
                    "default",
                    "login",
                    registry,
                    "-u",
                    &username,
                    "--password-stdin",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    if let Some(mut stdin) = child.stdin.take()
                        && let Err(e) = stdin.write_all(password.as_bytes()).await
                    {
                        error!(%registry, error = %e, "Failed to write password to nerdctl login stdin");
                    }
                    match tokio::time::timeout(login_timeout, child.wait()).await {
                        Ok(Ok(status)) if status.success() => {
                            info!(%registry, "Logged in to container registry")
                        }
                        Ok(Ok(status)) => {
                            error!(%registry, exit_code = ?status.code(), "nerdctl login failed")
                        }
                        Ok(Err(e)) => {
                            error!(%registry, error = %e, "Failed to wait for nerdctl login")
                        }
                        Err(_) => {
                            error!(%registry, "nerdctl login timed out");
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }
                    }
                }
                Err(e) => error!(%registry, error = %e, "Failed to spawn nerdctl login"),
            }
        }

        let pull_timeout = deadline.saturating_duration_since(Instant::now());
        if pull_timeout.is_zero() {
            return Err(format!(
                "timed out before pulling plugin image {image_name}"
            ));
        }
        let mut pull = tokio::process::Command::new("nerdctl");
        pull.args(["-n", "default", "pull", image_name])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = pull
            .spawn()
            .map_err(|error| format!("failed to start nerdctl pull for {image_name}: {error}"))?;
        match tokio::time::timeout(pull_timeout, child.wait_with_output()).await {
            Ok(Ok(result)) if result.status.success() => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                info!(
                    %image_name,
                    stdout = %bounded_plugin_output(stdout.into_owned()),
                    "Pulled machine validation container image",
                );
                Ok(())
            }
            Ok(Ok(result)) => {
                let stderr = String::from_utf8_lossy(&result.stderr);
                let error = format!(
                    "nerdctl pull exited with status {}: {}",
                    result.status,
                    bounded_plugin_output(stderr.into_owned()),
                );
                error!(%image_name, %error, "Failed to pull machine validation container image");
                Err(format!("failed to pull plugin image {image_name}: {error}"))
            }
            Ok(Err(error)) => {
                error!(
                    %image_name,
                    %error,
                    "Failed to pull machine validation container image",
                );
                Err(format!("failed to pull plugin image {image_name}: {error}"))
            }
            Err(_) => Err(format!("timed out pulling plugin image {image_name}")),
        }
    }
    async fn execute_machinevalidation_command(
        self,
        machine_id: &MachineId,
        test: &rpc::forge::MachineValidationTest,
        in_context: String,
        validation_id: MachineValidationId,
    ) -> MachineValidationExecution {
        let mut mc_result = rpc::forge::MachineValidationResult {
            test_id: Some(test.test_id.clone()),
            name: test.name.clone(),
            description: test.description.clone().unwrap_or_default(),
            command: test.command.clone(),
            args: test.args.clone(),
            context: in_context.clone(),
            validation_id: Some(validation_id),
            ..rpc::forge::MachineValidationResult::default()
        };
        match self
            .clone()
            .heartbeat_machine_validation_run(validation_id, Some(test.test_id.clone()))
            .await
        {
            Ok(true) => trace!(
                machine_validation_id = %validation_id,
                test_id = %test.test_id,
                "sent initial machine validation heartbeat"
            ),
            Ok(false) => {
                let now = Utc::now();
                emit(MachineValidationHeartbeatFailed::rejected(
                    MachineValidationHeartbeatStage::Initial,
                    validation_id,
                    test.test_id.clone(),
                ));
                mc_result.start_time = Some(now.into());
                mc_result.end_time = Some(now.into());
                mc_result.std_err = "Machine validation heartbeat was rejected because run or attempt is no longer active".to_owned();
                mc_result.std_out = "Skipped: Machine validation heartbeat was rejected".to_owned();
                mc_result.exit_code = 0;
                return MachineValidationExecution::without_heartbeat(mc_result);
            }
            Err(e) => emit(MachineValidationHeartbeatFailed::rpc(
                MachineValidationHeartbeatStage::Initial,
                validation_id,
                test.test_id.clone(),
                e,
            )),
        }
        let heartbeat = MachineValidationHeartbeatGuard::new(
            self.clone()
                .spawn_machine_validation_heartbeat(validation_id, test.test_id.clone()),
        );
        if test.external_config_file.is_some() {
            let file_name = test.external_config_file.clone().unwrap_or_default();
            match self
                .clone()
                .get_external_config(file_name.clone(), None)
                .await
            {
                Ok(()) => trace!(
                    external_config_file = %file_name,
                    "Fetched external machine validation config",
                ),
                Err(e) => {
                    mc_result.start_time = Some(Utc::now().into());
                    mc_result.end_time = Some(Utc::now().into());
                    mc_result.std_err = format!("Error {e}");
                    mc_result.std_out = format!("Skipped: Error {e}");
                    mc_result.exit_code = 0;
                    return MachineValidationExecution::with_heartbeat(mc_result, heartbeat);
                }
            }
        }

        // Check pre_condition
        if test.pre_condition.is_some() {
            match TokioCmd::new(test.pre_condition.clone().unwrap_or("/bin/true".to_owned()))
                .timeout(DEFAULT_TIMEOUT)
                .env("CONTEXT".to_owned(), in_context.clone())
                .env(
                    "MACHINE_VALIDATION_RUN_ID".to_owned(),
                    validation_id.to_string(),
                )
                .env("MACHINE_ID".to_owned(), machine_id.to_string())
                .output_with_timeout()
                .await
            {
                Ok(result) => {
                    let exit_code = result.exit_code;
                    if exit_code != 0 {
                        mc_result.start_time = Some(result.start_time.into());
                        mc_result.end_time = Some(result.end_time.into());
                        mc_result.std_err = result.stderr;
                        mc_result.std_out = "Skipped : Pre condition failed".to_owned();
                        mc_result.exit_code = 0;
                        return MachineValidationExecution::with_heartbeat(mc_result, heartbeat);
                    }
                }
                Err(e) => {
                    mc_result.start_time = Some(Utc::now().into());
                    mc_result.end_time = Some(Utc::now().into());
                    mc_result.std_err = e.to_string();
                    mc_result.std_out = "Skipped : Pre condition failed".to_owned();
                    mc_result.exit_code = 0;
                    return MachineValidationExecution::with_heartbeat(mc_result, heartbeat);
                }
            }
        }
        // Execute command
        let mut command_string = format!("{} {}", test.command, test.args);
        // Check if container
        if test.img_name.is_some() {
            if test.execute_in_host.unwrap_or(false) {
                // Execute command in host
                command_string = format!("chroot /host /bin/bash -c \"{command_string}\"");
            }
            let _ = self
                .pull_container(&test.img_name.clone().unwrap_or_default())
                .await;
            let ctr_arg = test.container_arg.clone().unwrap_or("".to_string());
            command_string = format!(
                "ctr run --rm --privileged --no-pivot \
                --mount type=bind,src=/,dst=/host,options=rbind:rw {} \
                {} runner {}",
                ctr_arg,
                test.img_name.clone().unwrap_or_default(),
                command_string
            );
        };
        info!(
            command = %command_string,
            "Executing machine validation command",
        );

        let _ = std::fs::remove_file("/tmp/forge_env_variables");
        match File::create("/tmp/forge_env_variables") {
            Ok(mut file) => {
                let mut envs = HashMap::new();
                envs.insert("CONTEXT".to_owned(), in_context.clone());
                envs.insert(
                    "MACHINE_VALIDATION_RUN_ID".to_owned(),
                    validation_id.to_string(),
                );
                envs.insert("MACHINE_ID".to_owned(), machine_id.to_string());
                let env_vars = envs
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<String>>()
                    .join("\n");
                file.write_all(env_vars.as_bytes()).expect("write failed");
            }
            Err(_) => error!("Failed to create file"),
        }

        let command_result = TokioCmd::new("sh")
            .args(vec!["-c".to_string(), command_string])
            .timeout(test.timeout.unwrap_or(7200).try_into().unwrap())
            .env("CONTEXT".to_owned(), in_context.clone())
            .env(
                "MACHINE_VALIDATION_RUN_ID".to_owned(),
                validation_id.to_string(),
            )
            .env("MACHINE_ID".to_owned(), machine_id.to_string())
            .output_with_timeout()
            .await;

        let result = match command_result {
            Ok(result) => {
                let mut stdout_str = result.stdout;
                let mut stderr_str = result.stderr;
                if test.extra_output_file.is_some() {
                    let message: String = match tokio::fs::read_to_string(
                        test.extra_output_file.clone().unwrap_or_default(),
                    )
                    .await
                    {
                        Ok(data) => data,
                        Err(_) => "".to_owned(),
                    };
                    stdout_str += message.as_str();
                }
                if test.extra_err_file.is_some() {
                    let message: String = match tokio::fs::read_to_string(
                        test.extra_err_file.clone().unwrap_or_default(),
                    )
                    .await
                    {
                        Ok(data) => data,
                        Err(_) => "".to_owned(),
                    };
                    stderr_str += message.as_str();
                }

                mc_result.start_time = Some(result.start_time.into());
                mc_result.end_time = Some(result.end_time.into());
                mc_result.std_err = if stderr_str.len() > MAX_STRING_STD_SIZE {
                    stderr_str[..MAX_STRING_STD_SIZE].to_string()
                } else {
                    stderr_str
                };
                mc_result.std_out = if stdout_str.len() > MAX_STRING_STD_SIZE {
                    stdout_str[..MAX_STRING_STD_SIZE].to_string()
                } else {
                    stdout_str
                };
                mc_result.exit_code = result.exit_code;
                mc_result
            }
            Err(e) => {
                mc_result.start_time = Some(Utc::now().into());
                mc_result.end_time = Some(Utc::now().into());
                mc_result.std_err = e.to_string();
                mc_result.std_out = e.to_string();
                mc_result.exit_code = -1;
                mc_result
            }
        };

        MachineValidationExecution::with_heartbeat(result, heartbeat)
    }

    async fn execute_plugin_command(
        self,
        machine_id: &MachineId,
        test: &rpc::forge::MachineValidationTest,
        context: String,
        validation_id: MachineValidationId,
        run_item_id: String,
        attempt: u32,
    ) -> MachineValidationExecution {
        let mut result = rpc::forge::MachineValidationResult {
            test_id: Some(test.test_id.clone()),
            name: test.name.clone(),
            description: test.description.clone().unwrap_or_default(),
            command: test
                .plugin
                .as_ref()
                .and_then(|plugin| plugin.entrypoint.first())
                .cloned()
                .unwrap_or_default(),
            args: test
                .plugin
                .as_ref()
                .map(|plugin| {
                    plugin
                        .entrypoint
                        .iter()
                        .skip(1)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default(),
            context: context.clone(),
            validation_id: Some(validation_id),
            ..rpc::forge::MachineValidationResult::default()
        };

        match self
            .clone()
            .heartbeat_machine_validation_run(validation_id, Some(test.test_id.clone()))
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let now = Utc::now();
                result.start_time = Some(now.into());
                result.end_time = Some(now.into());
                result.std_out = "Skipped: Machine validation heartbeat was rejected".to_owned();
                result.std_err = "Machine validation heartbeat was rejected because run or attempt is no longer active".to_owned();
                return MachineValidationExecution::without_heartbeat(result);
            }
            Err(error) => emit(MachineValidationHeartbeatFailed::rpc(
                MachineValidationHeartbeatStage::Initial,
                validation_id,
                test.test_id.clone(),
                error,
            )),
        }
        let heartbeat = MachineValidationHeartbeatGuard::new(
            self.clone()
                .spawn_machine_validation_heartbeat(validation_id, test.test_id.clone()),
        );

        let started_at = Utc::now();
        let attempt_started = Instant::now();
        let execution = plugin_execution_spec(test).and_then(|spec| {
            let plugin = test
                .plugin
                .as_ref()
                .expect("plugin execution requires a plugin");
            let parameters = parse_plugin_parameters(&plugin.parameters_json)?;
            let timeout = test.timeout.unwrap_or(7200);
            let timeout = u64::try_from(timeout)
                .map_err(|_| "plugin timeout must be non-negative".to_owned())?;
            let deadline = started_at + chrono::Duration::seconds(timeout as i64);
            let input = plugin_input(
                validation_id,
                &run_item_id,
                attempt,
                machine_id,
                &context,
                test,
                deadline,
                parameters,
            );
            Ok((spec, input, attempt_started + Duration::from_secs(timeout)))
        });

        match execution {
            Ok((spec, input, deadline)) => {
                // Plugins use the credential manager, not the legacy container_auth file.
                let pull_timeout = deadline.saturating_duration_since(Instant::now());
                match self
                    .pull_container_with_timeout(&spec.image, pull_timeout)
                    .await
                {
                    Ok(()) => {
                        let execution_timeout = deadline.saturating_duration_since(Instant::now());
                        let plugin_execution = if execution_timeout.is_zero() {
                            Err("plugin timeout exhausted while pulling its image".to_owned())
                        } else {
                            execute_plugin(
                                &spec,
                                &input,
                                execution_timeout,
                                Path::new(&self.options.plugin_contract_dir),
                            )
                            .await
                        };
                        match plugin_execution {
                            Ok(execution) => {
                                result.start_time = Some(started_at.into());
                                result.end_time = Some(Utc::now().into());
                                result.std_out = bounded_plugin_output(format!(
                                    "{}\n{}",
                                    execution.result.summary, execution.stdout
                                ));
                                result.std_err = bounded_plugin_output(execution.stderr);
                                result.exit_code = match execution.result.outcome.as_str() {
                                    "pass" => 0,
                                    "fail" => 1,
                                    "error" => -2,
                                    _ => -1,
                                };
                                if execution.result.outcome != "pass" {
                                    result.std_err = bounded_plugin_output(format!(
                                        "{}\n{}",
                                        execution.result.summary, result.std_err
                                    ));
                                }
                            }
                            Err(error) => {
                                result.start_time = Some(started_at.into());
                                result.end_time = Some(Utc::now().into());
                                result.std_err = error.clone();
                                result.std_out = error;
                                result.exit_code = -1;
                            }
                        }
                    }
                    Err(error) => {
                        result.start_time = Some(started_at.into());
                        result.end_time = Some(Utc::now().into());
                        result.std_err = error.clone();
                        result.std_out = error;
                        result.exit_code = -1;
                    }
                }
            }
            Err(error) => {
                result.start_time = Some(started_at.into());
                result.end_time = Some(Utc::now().into());
                result.std_err = error.clone();
                result.std_out = error;
                result.exit_code = -1;
            }
        }

        MachineValidationExecution::with_heartbeat(result, heartbeat)
    }

    pub(crate) async fn update_machine_validation_run(
        self,
        data: rpc::forge::MachineValidationRunRequest,
    ) -> Result<(), MachineValidationError> {
        tracing::info!(
            request = ?data,
            "Updating machine validation run",
        );
        let mut client = self.create_forge_client().await?;
        let request = tonic::Request::new(data);
        let _response = client
            .update_machine_validation_run(request)
            .await
            .map_err(|e| {
                MachineValidationError::ApiClient(
                    "update_machine_validation_run".to_owned(),
                    e.to_string(),
                )
            })?;
        Ok(())
    }

    async fn run_item_execution_ids(
        &self,
        validation_id: MachineValidationId,
    ) -> Result<HashMap<String, PluginRunItem>, MachineValidationError> {
        let mut client = self.create_forge_client().await?;
        let run_item_ids = client
            .find_machine_validation_run_item_ids(tonic::Request::new(
                rpc::forge::MachineValidationRunItemSearchFilter {
                    validation_id: Some(validation_id),
                },
            ))
            .await
            .map_err(|error| {
                MachineValidationError::ApiClient(
                    "find_machine_validation_run_item_ids".to_owned(),
                    error.to_string(),
                )
            })?
            .into_inner()
            .run_item_ids;
        if run_item_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let run_items = client
            .find_machine_validation_run_items_by_ids(tonic::Request::new(
                rpc::forge::MachineValidationRunItemsByIdsRequest { run_item_ids },
            ))
            .await
            .map_err(|error| {
                MachineValidationError::ApiClient(
                    "find_machine_validation_run_items_by_ids".to_owned(),
                    error.to_string(),
                )
            })?
            .into_inner()
            .run_items;

        run_items
            .into_iter()
            .map(|item| {
                let run_item_id = item
                    .run_item_id
                    .ok_or_else(|| {
                        MachineValidationError::Generic(
                            "machine validation run item is missing its ID".to_owned(),
                        )
                    })?
                    .value;
                let attempt = item.attempt.checked_add(1).ok_or_else(|| {
                    MachineValidationError::Generic(
                        "machine validation attempt number overflowed".to_owned(),
                    )
                })?;
                Ok((
                    item.test_id,
                    PluginRunItem {
                        run_item_id,
                        attempt,
                        plugin: item.plugin,
                        full_host_approved: item.plugin_full_host_approved,
                    },
                ))
            })
            .collect()
    }

    pub async fn run(
        self,
        machine_id: &MachineId,
        tests: Vec<rpc::forge::MachineValidationTest>,
        context: String,
        validation_id: MachineValidationId,
        execute_tests_sequentially: bool,
        machine_validation_filter: MachineValidationFilter,
    ) -> Result<(), MachineValidationError> {
        if tests.iter().any(|test| test.plugin.is_none()) {
            self.clone().get_container_auth_config().await?;
            match Self::get_container_images().await {
                Ok(_) => info!("Successfully fetched container images"),
                Err(e) => error!(error = %e, "Failed to fetch container images"),
            }
        }
        let run_items = if tests.iter().any(|test| test.plugin.is_some()) {
            self.run_item_execution_ids(validation_id).await?
        } else {
            HashMap::new()
        };
        if execute_tests_sequentially {
            for test in tests {
                if !machine_validation_filter.allowed_tests.is_empty()
                    && !machine_validation_filter
                        .allowed_tests
                        .iter()
                        .any(|t| t.eq_ignore_ascii_case(&test.test_id))
                {
                    continue;
                }
                let execution = if test.plugin.is_some() {
                    match run_items.get(&test.test_id) {
                        Some(run_item) => {
                            let mut snapshot_test = test.clone();
                            snapshot_test.plugin = run_item.plugin.clone();
                            snapshot_test.full_host_approved = run_item.full_host_approved;
                            self.clone()
                                .execute_plugin_command(
                                    machine_id,
                                    &snapshot_test,
                                    context.to_string(),
                                    validation_id,
                                    run_item.run_item_id.clone(),
                                    run_item.attempt,
                                )
                                .await
                        }
                        None => {
                            let now = Utc::now();
                            MachineValidationExecution::without_heartbeat(
                                rpc::forge::MachineValidationResult {
                                    test_id: Some(test.test_id.clone()),
                                    name: test.name.clone(),
                                    description: test.description.clone().unwrap_or_default(),
                                    context: context.clone(),
                                    validation_id: Some(validation_id),
                                    start_time: Some(now.into()),
                                    end_time: Some(now.into()),
                                    std_out: format!(
                                        "Plugin test {} is missing from the run plan",
                                        test.test_id
                                    ),
                                    std_err: format!(
                                        "Plugin test {} cannot run because its immutable run-item snapshot is missing",
                                        test.test_id
                                    ),
                                    exit_code: -1,
                                    ..rpc::forge::MachineValidationResult::default()
                                },
                            )
                        }
                    }
                } else {
                    self.clone()
                        .execute_machinevalidation_command(
                            machine_id,
                            &test,
                            context.to_string(),
                            validation_id,
                        )
                        .await
                };
                let MachineValidationExecution { result, heartbeat } = execution;
                let persist_result = self.clone().persist(Some(result)).await;
                if let Some(heartbeat) = heartbeat {
                    heartbeat.stop().await;
                }
                emit(MachineValidationResultPersistenceFinished::from_result(
                    validation_id,
                    test.test_id.clone(),
                    test.name.clone(),
                    &persist_result,
                ));
            }
        } else {
            info!("To be implemented");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;

    #[derive(Clone, Copy)]
    enum InstrumentationCase {
        HeartbeatRejected(MachineValidationHeartbeatStage),
        HeartbeatRpc(MachineValidationHeartbeatStage),
        Persistence(Outcome),
    }

    #[derive(Debug, PartialEq)]
    struct InstrumentationObservation {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        stage: Option<String>,
        reason: Option<String>,
        outcome: Option<String>,
        machine_validation_id: Option<String>,
        test_id: Option<String>,
        test_name: Option<String>,
        error: Option<String>,
        counter_delta: f64,
    }

    fn observe_instrumentation(case: InstrumentationCase) -> InstrumentationObservation {
        const TEST_ID: &str = "validation-test";
        const TEST_NAME: &str = "Validation test";
        const RPC_ERROR: &str = "Forge API unavailable";

        let machine_validation_id = MachineValidationId::nil();
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| match case {
            InstrumentationCase::HeartbeatRejected(stage) => {
                emit(MachineValidationHeartbeatFailed::rejected(
                    stage,
                    machine_validation_id,
                    TEST_ID.to_string(),
                ));
            }
            InstrumentationCase::HeartbeatRpc(stage) => {
                emit(MachineValidationHeartbeatFailed::rpc(
                    stage,
                    machine_validation_id,
                    TEST_ID.to_string(),
                    RPC_ERROR,
                ));
            }
            InstrumentationCase::Persistence(Outcome::Ok) => {
                emit(MachineValidationResultPersistenceFinished::from_result(
                    machine_validation_id,
                    TEST_ID.to_string(),
                    TEST_NAME.to_string(),
                    &Result::<(), &str>::Ok(()),
                ));
            }
            InstrumentationCase::Persistence(Outcome::Error) => {
                emit(MachineValidationResultPersistenceFinished::from_result(
                    machine_validation_id,
                    TEST_ID.to_string(),
                    TEST_NAME.to_string(),
                    &Result::<(), &str>::Err(RPC_ERROR),
                ));
            }
        });

        assert_eq!(logs.len(), 1, "each Event should write one terminal record");
        let log = logs.first().expect("Event did not log");
        let counter_delta = match case {
            InstrumentationCase::HeartbeatRejected(stage) => metrics.counter_delta(
                "carbide_machine_validation_heartbeat_failures_total",
                &[
                    (
                        "stage",
                        match stage {
                            MachineValidationHeartbeatStage::Initial => "initial",
                            MachineValidationHeartbeatStage::Periodic => "periodic",
                        },
                    ),
                    ("reason", "rejected"),
                ],
            ),
            InstrumentationCase::HeartbeatRpc(stage) => metrics.counter_delta(
                "carbide_machine_validation_heartbeat_failures_total",
                &[
                    (
                        "stage",
                        match stage {
                            MachineValidationHeartbeatStage::Initial => "initial",
                            MachineValidationHeartbeatStage::Periodic => "periodic",
                        },
                    ),
                    ("reason", "rpc"),
                ],
            ),
            InstrumentationCase::Persistence(outcome) => metrics.counter_delta(
                "carbide_machine_validation_result_persistence_attempts_total",
                &[(
                    "outcome",
                    match outcome {
                        Outcome::Ok => "ok",
                        Outcome::Error => "error",
                    },
                )],
            ),
        };

        InstrumentationObservation {
            level: log.level,
            metadata_name: log.metadata_name.clone(),
            message: log.message.clone(),
            event_name: log.field("event_name").map(str::to_string),
            metric_name: log.field("metric_name").map(str::to_string),
            stage: log.field("stage").map(str::to_string),
            reason: log.field("reason").map(str::to_string),
            outcome: log.field("outcome").map(str::to_string),
            machine_validation_id: log.field("machine_validation_id").map(str::to_string),
            test_id: log.field("test_id").map(str::to_string),
            test_name: log.field("test_name").map(str::to_string),
            error: log.field("error").map(str::to_string),
            counter_delta,
        }
    }

    #[test]
    fn control_plane_delivery_events_log_and_count() {
        let machine_validation_id = Some(MachineValidationId::nil().to_string());
        value_scenarios!(
            run = observe_instrumentation;
            "heartbeat is rejected" {
                InstrumentationCase::HeartbeatRejected(MachineValidationHeartbeatStage::Initial) => InstrumentationObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "machine_validation_heartbeat_failed".to_string(),
                    message: "initial machine validation heartbeat was rejected".to_string(),
                    event_name: Some("machine_validation_heartbeat_failed".to_string()),
                    metric_name: Some("carbide_machine_validation_heartbeat_failures_total".to_string()),
                    stage: Some("initial".to_string()),
                    reason: Some("rejected".to_string()),
                    outcome: None,
                    machine_validation_id: machine_validation_id.clone(),
                    test_id: Some("validation-test".to_string()),
                    test_name: None,
                    // A rejected heartbeat has no transport error, so the key
                    // is absent from the line rather than blank.
                    error: None,
                    counter_delta: 1.0,
                },
                InstrumentationCase::HeartbeatRejected(MachineValidationHeartbeatStage::Periodic) => InstrumentationObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "machine_validation_heartbeat_failed".to_string(),
                    message: "machine validation heartbeat was rejected because run or attempt is no longer active".to_string(),
                    event_name: Some("machine_validation_heartbeat_failed".to_string()),
                    metric_name: Some("carbide_machine_validation_heartbeat_failures_total".to_string()),
                    stage: Some("periodic".to_string()),
                    reason: Some("rejected".to_string()),
                    outcome: None,
                    machine_validation_id: machine_validation_id.clone(),
                    test_id: Some("validation-test".to_string()),
                    test_name: None,
                    // A rejected heartbeat has no transport error, so the key
                    // is absent from the line rather than blank.
                    error: None,
                    counter_delta: 1.0,
                },
            }
            "heartbeat RPC fails" {
                InstrumentationCase::HeartbeatRpc(MachineValidationHeartbeatStage::Initial) => InstrumentationObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "machine_validation_heartbeat_failed".to_string(),
                    message: "failed to send initial machine validation heartbeat".to_string(),
                    event_name: Some("machine_validation_heartbeat_failed".to_string()),
                    metric_name: Some("carbide_machine_validation_heartbeat_failures_total".to_string()),
                    stage: Some("initial".to_string()),
                    reason: Some("rpc".to_string()),
                    outcome: None,
                    machine_validation_id: machine_validation_id.clone(),
                    test_id: Some("validation-test".to_string()),
                    test_name: None,
                    error: Some("Forge API unavailable".to_string()),
                    counter_delta: 1.0,
                },
                InstrumentationCase::HeartbeatRpc(MachineValidationHeartbeatStage::Periodic) => InstrumentationObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "machine_validation_heartbeat_failed".to_string(),
                    message: "failed to send machine validation heartbeat".to_string(),
                    event_name: Some("machine_validation_heartbeat_failed".to_string()),
                    metric_name: Some("carbide_machine_validation_heartbeat_failures_total".to_string()),
                    stage: Some("periodic".to_string()),
                    reason: Some("rpc".to_string()),
                    outcome: None,
                    machine_validation_id: machine_validation_id.clone(),
                    test_id: Some("validation-test".to_string()),
                    test_name: None,
                    error: Some("Forge API unavailable".to_string()),
                    counter_delta: 1.0,
                },
            }
            "result persistence finishes" {
                InstrumentationCase::Persistence(Outcome::Ok) => InstrumentationObservation {
                    level: tracing::Level::INFO,
                    metadata_name: "machine_validation_result_persistence_finished".to_string(),
                    message: "Sent machine validation result to API server".to_string(),
                    event_name: Some("machine_validation_result_persistence_finished".to_string()),
                    metric_name: Some("carbide_machine_validation_result_persistence_attempts_total".to_string()),
                    stage: None,
                    reason: None,
                    outcome: Some("ok".to_string()),
                    machine_validation_id: machine_validation_id.clone(),
                    test_id: Some("validation-test".to_string()),
                    test_name: Some("Validation test".to_string()),
                    // A successful persist has no error, so the key is absent
                    // from the line rather than blank.
                    error: None,
                    counter_delta: 1.0,
                },
                InstrumentationCase::Persistence(Outcome::Error) => InstrumentationObservation {
                    level: tracing::Level::ERROR,
                    metadata_name: "machine_validation_result_persistence_finished".to_string(),
                    message: "Failed to send machine validation result to API server".to_string(),
                    event_name: Some("machine_validation_result_persistence_finished".to_string()),
                    metric_name: Some("carbide_machine_validation_result_persistence_attempts_total".to_string()),
                    stage: None,
                    reason: None,
                    outcome: Some("error".to_string()),
                    machine_validation_id,
                    test_id: Some("validation-test".to_string()),
                    test_name: Some("Validation test".to_string()),
                    error: Some("Forge API unavailable".to_string()),
                    counter_delta: 1.0,
                },
            }
        );
    }

    #[test]
    fn extract_registry_parses_known_registries() {
        assert_eq!(
            MachineValidation::extract_registry("nvcr.io/foo/bar:latest").unwrap(),
            "nvcr.io"
        );
        assert_eq!(
            MachineValidation::extract_registry("docker.io/library/ubuntu:22.04").unwrap(),
            "docker.io"
        );
        assert_eq!(
            MachineValidation::extract_registry("ghcr.io/org/image:v1").unwrap(),
            "ghcr.io"
        );
    }

    #[test]
    fn extract_registry_handles_port_in_hostname() {
        assert_eq!(
            MachineValidation::extract_registry("localhost:5000/myimage:tag").unwrap(),
            "localhost:5000"
        );
    }

    #[test]
    fn extract_registry_handles_bare_localhost() {
        assert_eq!(
            MachineValidation::extract_registry("localhost/myrepo:tag").unwrap(),
            "localhost"
        );
    }

    #[test]
    fn extract_registry_errors_on_bare_image_name() {
        assert!(MachineValidation::extract_registry("ubuntu:22.04").is_err());
        assert!(MachineValidation::extract_registry("myimage:latest").is_err());
    }

    #[test]
    fn extract_registry_errors_on_docker_hub_shorthand() {
        // "library/ubuntu" has a slash but "library" is not a hostname
        assert!(MachineValidation::extract_registry("library/ubuntu").is_err());
    }

    fn plugin_test(
        privileged: bool,
        host_access_full: bool,
        full_host_approved: bool,
    ) -> rpc::forge::MachineValidationTest {
        rpc::forge::MachineValidationTest {
            test_id: "gpu-health".to_owned(),
            version: "1.2.3".to_owned(),
            plugin: Some(rpc::forge::MachineValidationPlugin {
                image: "registry.example.com/plugins/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                entrypoint: vec!["/plugin/entrypoint".to_owned(), "--check".to_owned()],
                parameters_json: r#"{"expectedGpuCount":8}"#.to_owned(),
                privileged,
                host_access_full,
            }),
            full_host_approved,
            ..Default::default()
        }
    }

    #[test]
    fn plugin_runtime_profile_requires_full_host_approval() {
        let rejected = plugin_test(true, true, false);
        assert!(plugin_execution_spec(&rejected).is_err());

        let approved = plugin_test(true, true, true);
        let runtime = plugin_execution_spec(&approved).expect("approved full-host plugin");
        assert_eq!(runtime.privilege, PluginPrivilege::FullHost);

        let isolated = plugin_test(false, false, false);
        let runtime = plugin_execution_spec(&isolated).expect("isolated plugin");
        assert_eq!(runtime.privilege, PluginPrivilege::Isolated);

        let privileged = plugin_test(true, false, false);
        let runtime = plugin_execution_spec(&privileged).expect("privileged plugin");
        assert_eq!(runtime.privilege, PluginPrivilege::Privileged);
    }

    #[test]
    fn plugin_input_is_versioned_and_keeps_parameters_as_json() {
        let test = plugin_test(false, false, false);
        let machine_id = MachineId::new(MachineIdSource::Tpm, [1; 32], MachineType::Host);
        let input = plugin_input(
            MachineValidationId::nil(),
            "run-item-123",
            1,
            &machine_id,
            "Discovery",
            &test,
            Utc::now(),
            parse_plugin_parameters(r#"{"expectedGpuCount":8}"#).expect("parameters"),
        );

        assert_eq!(input["contractVersion"], "v1");
        assert_eq!(input["kind"], "MachineValidationPluginInput");
        assert_eq!(input["runItemId"], "run-item-123");
        assert_eq!(input["plugin"]["version"], "1.2.3");
        assert_eq!(input["parameters"]["expectedGpuCount"], 8);
    }

    #[test]
    fn plugin_parameters_must_be_an_object() {
        assert_eq!(parse_plugin_parameters("").unwrap(), serde_json::json!({}));
        assert!(parse_plugin_parameters("[]").is_err());
        assert!(parse_plugin_parameters("not-json").is_err());
    }
}
