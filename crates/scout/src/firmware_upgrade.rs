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

use std::ffi::OsString;
use std::time::Duration;

use libmlx::firmware::error::{AttemptError, AttemptFailure};
use libmlx::firmware::operation::{DirectMlxClaim, SupervisorConfig};
use libmlx::runner::FirmwareToolExecutor;
use libmlx::runner::command_builder::CommandSpec;
use rpc::scout_firmware_upgrade::ScoutFirmwareUpgradeTask as FirmwareUpgradeTask;

use crate::firmware_staging::{FirmwareStaging, LoggableFirmwareUrl};
use crate::metrics::FirmwareDownloadKind;

const SCRIPT_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

fn firmware_task_text_context(value: &str, task: &FirmwareUpgradeTask) -> String {
    let urls = task.script.iter().map(|script| script.url.as_str()).chain(
        task.file_artifacts
            .iter()
            .map(|artifact| artifact.url.as_str()),
    );
    urls.fold(value.to_string(), |text, url| {
        LoggableFirmwareUrl::new(url).redact_from(&text)
    })
}

// FirmwareUpgradeResult captures the outcome of a firmware upgrade execution.
#[derive(Clone, Debug)]
pub(super) struct FirmwareUpgradeResult {
    pub(super) success: bool,
    pub(super) exit_code: i32,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) error: String,
}

impl FirmwareUpgradeResult {
    fn with_url_context(mut self, task: &FirmwareUpgradeTask) -> Self {
        self.stdout = firmware_task_text_context(&self.stdout, task);
        self.stderr = firmware_task_text_context(&self.stderr, task);
        self.error = firmware_task_text_context(&self.error, task);
        self
    }
}

/// Claim host hardware access using `config`, verify the script and artifacts,
/// and execute the script under descendant supervision. Abnormal execution
/// retains its claim and inputs for operator reconciliation.
pub(super) async fn handle_firmware_upgrade(
    client: &reqwest::Client,
    task: &FirmwareUpgradeTask,
    config: &SupervisorConfig,
) -> FirmwareUpgradeResult {
    let result = match run_firmware_upgrade(client, task, config).await {
        Ok(result) => result,
        Err(e) => FirmwareUpgradeResult {
            success: false,
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            error: format!("firmware upgrade failed: {e}"),
        },
    };
    result.with_url_context(task)
}

/// Hold host hardware exclusion and staged inputs through one script execution.
/// A dropped future cannot establish whether dispatch or partial mutation occurred.
struct ScriptAttempt {
    staging: FirmwareStaging,
    claim: Option<DirectMlxClaim>,
}

impl ScriptAttempt {
    /// Reserve the host marker before any script or artifact download.
    fn acquire(
        config: &SupervisorConfig,
        task: &FirmwareUpgradeTask,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut staging = FirmwareStaging::new()?;
        staging
            .work_dir()
            .to_str()
            .ok_or("firmware staging path must be valid UTF-8")?;
        let context = format!(
            "scout-script task={} inputs={}",
            task.upgrade_task_id,
            staging.work_dir().display(),
        );
        // Claim before downloading: a Core retry has a new task ID, but must
        // neither mutate again nor accumulate more retained firmware copies.
        let claim = DirectMlxClaim::acquire(config, &context).map_err(
            |error| -> Box<dyn std::error::Error + Send + Sync> {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    return format!(
                        "cannot acquire firmware hardware claim in {}: {error}; existing work must finish or be reconciled before another script can run",
                        config.state_directory.display(),
                    ).into();
                }
                tracing::warn!(
                    %error,
                    state_directory = %config.state_directory.display(),
                    "could not acquire firmware hardware claim",
                );
                error.into()
            },
        )?;
        staging.retain_on_drop();
        Ok(Self {
            staging,
            claim: Some(claim),
        })
    }

    /// After proven non-dispatch or success, attempt both cleanups independently.
    /// Filesystem failure must not turn a completed mutation into another attempt.
    fn finish(mut self) -> String {
        let mut errors = Vec::new();
        if let Err(error) = std::fs::remove_dir_all(self.staging.work_dir()) {
            errors.push(format!(
                "could not remove staged inputs at {}: {error}",
                self.staging.work_dir().display(),
            ));
        }
        if let Err(error) = self.claim.take().expect("attempt owns its claim").release() {
            // A directory-sync error may follow a successful unlink. Do not
            // claim that the marker remains merely because release returned Err.
            errors.push(format!(
                "could not release firmware hardware claim: {error}"
            ));
        }
        let diagnostic = errors.join("; ");
        if !diagnostic.is_empty() {
            tracing::warn!(
                staged_inputs = %self.staging.work_dir().display(),
                cleanup = %diagnostic,
                "firmware script cleanup requires operator attention; no script replay is authorized by this diagnostic",
            );
        }
        diagnostic
    }
}

impl Drop for ScriptAttempt {
    fn drop(&mut self) {
        if self.claim.is_some() {
            tracing::warn!(
                staged_inputs = %self.staging.work_dir().display(),
                "firmware script inputs and hardware claim retained pending operator reconciliation; process termination does not establish that device-internal work stopped",
            );
        }
    }
}

/// Verify the script and ordered artifacts while constructing their command.
async fn stage_script(
    client: &reqwest::Client,
    task: &FirmwareUpgradeTask,
    staging: &mut FirmwareStaging,
) -> Result<CommandSpec, Box<dyn std::error::Error + Send + Sync>> {
    let script = task
        .script
        .as_ref()
        .ok_or("firmware upgrade task missing script")?;
    let script_path = staging
        .stage(
            client,
            &script.url,
            &script.sha256,
            FirmwareDownloadKind::Script,
            SCRIPT_DOWNLOAD_TIMEOUT,
        )
        .await?;
    let mut command = CommandSpec::new("sh").arg(
        script_path
            .to_str()
            .ok_or("firmware script path must be valid UTF-8")?,
    );
    let download_timeout = Duration::from_secs(task.artifact_download_timeout_seconds.into());
    for artifact in &task.file_artifacts {
        let destination = staging
            .stage(
                client,
                &artifact.url,
                &artifact.sha256,
                FirmwareDownloadKind::Artifact,
                download_timeout,
            )
            .await?;
        command = command.arg(
            destination
                .to_str()
                .ok_or("firmware artifact path must be valid UTF-8")?,
        );
    }
    Ok(command)
}

/// Keep inputs and exclusion until proven non-dispatch or script success.
async fn run_firmware_upgrade(
    client: &reqwest::Client,
    task: &FirmwareUpgradeTask,
    config: &SupervisorConfig,
) -> Result<FirmwareUpgradeResult, Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(
        component_type = %task.component_type,
        target_version = %task.target_version,
        "[firmware_upgrade] starting",
    );
    let mut attempt = ScriptAttempt::acquire(config, task)?;
    let command = match stage_script(client, task, &mut attempt.staging).await {
        Ok(command) => command,
        Err(error) => {
            // No executor was called, so this exact attempt cannot have mutated.
            let cleanup = attempt.finish();
            if !cleanup.is_empty() {
                return Err(format!("{error}; cleanup: {cleanup}").into());
            }
            return Err(error);
        }
    };
    let executor = match &config.supervisor_path {
        Some(path) => FirmwareToolExecutor::with_supervisor_path(path),
        None => FirmwareToolExecutor::default(),
    }
    .with_working_directory(attempt.staging.work_dir())
    .with_environment([
        (
            OsString::from("DOWNLOAD_DIR"),
            attempt.staging.download_dir().as_os_str().to_owned(),
        ),
        (
            OsString::from("COMPONENT_TYPE"),
            OsString::from(&task.component_type),
        ),
        (
            OsString::from("TARGET_VERSION"),
            OsString::from(&task.target_version),
        ),
    ]);
    tracing::info!(
        staged_inputs = %attempt.staging.work_dir().display(),
        "[firmware_upgrade] files downloaded; executing supervised script",
    );
    let timeout = Duration::from_secs(task.execution_timeout_seconds.into());
    let result = executor.execute(&command, timeout).await;
    let result = match result {
        Ok(output) => {
            let success = output.status.success();
            let error = if success {
                attempt.finish()
            } else {
                // A completed opaque script can fail after partial mutation.
                format!(
                    "script exited unsuccessfully; inputs retained at {}; hardware access remains blocked pending operator reconciliation",
                    attempt.staging.work_dir().display(),
                )
            };
            FirmwareUpgradeResult {
                success,
                exit_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                error,
            }
        }
        Err(error @ (AttemptError::NotStarted { .. } | AttemptError::SupervisorNotStarted { .. })) => {
            let cleanup = attempt.finish();
            if !cleanup.is_empty() {
                return Err(format!("{error}; cleanup: {cleanup}").into());
            }
            return Err(error.into());
        }
        Err(AttemptError::Started {
            status,
            stdout,
            stderr,
            reason,
            cleanup_errors,
            ..
        }) => {
            let reason = match reason {
                AttemptFailure::Deadline(_) => format!(
                    "script timed out after {} seconds",
                    task.execution_timeout_seconds
                ),
                reason => reason.to_string(),
            };
            let cleanup = cleanup_errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
            let error = format!(
                "{reason}; cleanup: {cleanup}; inputs retained at {}; hardware access remains blocked pending operator reconciliation; process termination does not establish that device-internal work stopped",
                attempt.staging.work_dir().display(),
            );
            FirmwareUpgradeResult {
                success: false,
                exit_code: status.and_then(|status| status.code()).unwrap_or(-1),
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
                error,
            }
        }
    }.with_url_context(task);
    if !result.stdout.is_empty() {
        tracing::info!(stdout = %result.stdout, "[firmware_upgrade] script stdout");
    }
    if !result.stderr.is_empty() {
        tracing::warn!(stderr = %result.stderr, "[firmware_upgrade] script stderr");
    }
    Ok(result)
}

/// Give each script test a private claim root and the matching compiled helper.
#[cfg(test)]
struct FirmwareUpgradeFixture {
    root: tempfile::TempDir,
    config: SupervisorConfig,
}

#[cfg(test)]
impl FirmwareUpgradeFixture {
    /// Use `LIBMLX_TEST_STATE_DIRECTORY` when scratch ancestors are shared.
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let root = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
            Some(parent) => tempfile::tempdir_in(parent),
            None => tempfile::tempdir(),
        }
        .expect("create private script firmware fixture");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("protect script fixture root");
        let executable = std::env::current_exe().expect("locate Scout test executable");
        let helper = executable
            .parent()
            .expect("test executable directory")
            .parent()
            .expect("Cargo target directory")
            .join("mlx-firmware-supervisor");
        assert!(
            helper.is_file(),
            "run cargo make build-nic-firmware-test-helper before Scout unit tests"
        );
        let config = SupervisorConfig {
            state_directory: root.path().join("firmware"),
            supervisor_path: Some(helper),
            ..SupervisorConfig::default()
        };
        Self { root, config }
    }

    /// Read the original attempt path from its durable diagnostic marker.
    fn retained_inputs(&self) -> Option<std::path::PathBuf> {
        let bytes = std::fs::read(self.config.state_directory.join("mutation.json")).ok()?;
        let marker: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let (_, path) = marker["device_identity"].as_str()?.split_once(" inputs=")?;
        Some(path.into())
    }
}

#[cfg(test)]
impl Drop for FirmwareUpgradeFixture {
    fn drop(&mut self) {
        if let Some(path) = self.retained_inputs()
            && path.exists()
            && let Err(error) = std::fs::remove_dir_all(&path)
        {
            eprintln!(
                "could not remove retained fixture inputs at {}: {error}",
                path.display()
            );
        }
    }
}

/// Run a staging test with isolated supervisor ownership.
#[cfg(test)]
pub(super) async fn handle_firmware_upgrade_for_test(
    client: &reqwest::Client,
    task: &FirmwareUpgradeTask,
) -> FirmwareUpgradeResult {
    let fixture = FirmwareUpgradeFixture::new();
    handle_firmware_upgrade(client, task, &fixture.config).await
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::routing::get;
    use carbide_instrument::testing::{capture_logs, capture_logs_async};
    use rpc::scout_firmware_upgrade::FileArtifact;
    use sha2::{Digest, Sha256};
    use tokio::net::TcpListener;

    use super::*;

    // start_file_server spins up a lightweight HTTP server that serves
    // static content at the given routes. Returns the base URL.
    async fn start_file_server(routes: Vec<(&'static str, &'static str)>) -> String {
        let mut app = Router::new();
        for (path, body) in routes {
            app = app.route(path, get(move || async move { body }));
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        format!("http://{addr}")
    }

    fn sha256_hex(data: &str) -> String {
        hex::encode(Sha256::digest(data.as_bytes()))
    }

    fn script_artifact(base: &str, path: &str, content: &str) -> FileArtifact {
        FileArtifact {
            url: format!("{base}{path}"),
            sha256: sha256_hex(content),
        }
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build")
    }

    #[test]
    fn firmware_upgrade_redacts_only_task_urls_from_status_context() {
        const TASK_SECRET: &str = "TASK_URL_SECRET_MARKER";
        const UNRELATED_SECRET: &str = "UNRELATED_URL_SECRET_MARKER";
        const ARTIFACT: &str = "firmware";

        let runtime = test_runtime();
        let mut output = None;
        let logs = capture_logs(|| {
            output = Some(runtime.block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let script_url =
                    format!("HTTP://{addr}/scripts/upgrade.sh?token={TASK_SECRET}#{TASK_SECRET}");
                let canonical_script_url = reqwest::Url::parse(&script_url).unwrap().to_string();
                let unrelated_url = format!(
                    "https://user:password@other.example/output.bin?token={UNRELATED_SECRET}"
                );
                let script = format!(
                    "#!/bin/sh\n\
                     echo \"raw={script_url}\"\n\
                     echo \"canonical={canonical_script_url}\"\n\
                     echo \"unrelated={unrelated_url}\" >&2\n"
                );
                let script_body = script.clone();
                let app = Router::new()
                    .route(
                        "/scripts/upgrade.sh",
                        get(move || {
                            let body = script_body.clone();
                            async move { body }
                        }),
                    )
                    .route("/firmware/blob.bin", get(|| async { ARTIFACT }));
                tokio::spawn(async move {
                    axum::serve(listener, app).await.unwrap();
                });
                let task = FirmwareUpgradeTask {
                    upgrade_task_id: "test-upgrade-task-id".into(),
                    component_type: "cpld".into(),
                    target_version: "1.2.3".into(),
                    script: Some(FileArtifact {
                        url: script_url.clone(),
                        sha256: sha256_hex(&script),
                    }),
                    execution_timeout_seconds: 30,
                    artifact_download_timeout_seconds: 30,
                    file_artifacts: vec![FileArtifact {
                        url: format!(
                            "http://{addr}/firmware/blob.bin?token={TASK_SECRET}#{TASK_SECRET}"
                        ),
                        sha256: sha256_hex(ARTIFACT),
                    }],
                };

                let result = handle_firmware_upgrade_for_test(&reqwest::Client::new(), &task).await;
                let loggable_script_url =
                    LoggableFirmwareUrl::new(&script_url).as_str().to_string();
                (result, loggable_script_url, unrelated_url)
            }));
        });
        let (result, loggable_script_url, unrelated_url) =
            output.expect("upgrade should produce a result");

        assert!(result.success, "upgrade failed: {}", result.error);
        assert_eq!(result.stdout.matches(&loggable_script_url).count(), 2);
        assert!(result.stderr.contains(&unrelated_url));
        let observed = format!("{result:?}\n{logs:?}");
        assert!(!observed.contains(TASK_SECRET));
        assert!(observed.contains(UNRELATED_SECRET));
    }

    /// Real supervisor truncation must not expose a known URL's trailing secret.
    #[tokio::test]
    async fn capped_script_output_redacts_a_truncated_task_url() {
        const SECRET: &str = "CAP_TASK_SECRET";
        const UNRELATED: &str = "http://other.example/log?token=KEEP_UNRELATED_SECRET";
        const OUTPUT_LIMIT: usize = 1024 * 1024;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/scripts/overflow.sh?token={SECRET}&tail=remaining-query-padding",
            listener.local_addr().unwrap(),
        );
        let cutoff = url.find("&tail=").unwrap() + "&tail=remaining".len();
        let filler = OUTPUT_LIMIT - UNRELATED.len() - 1 - cutoff;
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' '{UNRELATED}'\nhead -c {filler} /dev/zero | tr '\\000' x\nprintf '%s' '{url}'\n",
        );
        let body = script.clone();
        let app = Router::new().route(
            "/scripts/overflow.sh",
            get(move || {
                let body = body.clone();
                async move { body }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let task = FirmwareUpgradeTask {
            upgrade_task_id: "capped-output-script".into(),
            script: Some(FileArtifact {
                url: url.clone(),
                sha256: sha256_hex(&script),
            }),
            execution_timeout_seconds: 30,
            ..FirmwareUpgradeTask::default()
        };
        let fixture = FirmwareUpgradeFixture::new();
        let (result, logs) = capture_logs_async(handle_firmware_upgrade(
            &reqwest::Client::new(),
            &task,
            &fixture.config,
        ))
        .await;
        assert!(!result.success);
        assert!(
            result.error.contains("output exceeded 1 MiB"),
            "{}",
            result.error
        );
        assert!(
            result
                .stdout
                .ends_with(LoggableFirmwareUrl::new(&url).as_str())
        );
        assert!(result.stdout.contains(UNRELATED));
        let observed = format!("{result:?} {logs:?}");
        assert!(!observed.contains(SECRET));
        assert!(logs.iter().any(|log| {
            log.field("stdout")
                .is_some_and(|stdout| stdout.contains(UNRELATED))
        }));
    }

    /// Filesystem faults after a settled attempt must not skip the other cleanup.
    #[test]
    fn settled_script_cleanup_attempts_staging_and_claim_release_independently() {
        enum Fault {
            Staging,
            Claim,
        }
        for (scenario, fault) in [
            ("staging removal", Fault::Staging),
            ("claim unlink", Fault::Claim),
        ] {
            let fixture = FirmwareUpgradeFixture::new();
            let task = FirmwareUpgradeTask {
                upgrade_task_id: "cleanup-fault".into(),
                ..FirmwareUpgradeTask::default()
            };
            let attempt = ScriptAttempt::acquire(&fixture.config, &task).unwrap();
            let inputs = attempt.staging.work_dir().to_path_buf();
            let marker = fixture.config.state_directory.join("mutation.json");
            match fault {
                Fault::Staging => {
                    std::fs::remove_dir_all(&inputs).unwrap();
                    std::fs::write(&inputs, b"retained cleanup evidence").unwrap();
                }
                Fault::Claim => {
                    std::fs::remove_file(&marker).unwrap();
                    std::fs::create_dir(&marker).unwrap();
                }
            }
            let diagnostic = attempt.finish();
            match fault {
                Fault::Staging => {
                    let evidence = std::fs::read(&inputs).unwrap();
                    std::fs::remove_file(&inputs).unwrap();
                    assert_eq!(evidence, b"retained cleanup evidence");
                    assert!(
                        !marker.exists(),
                        "{scenario}: exclusion released despite staging fault"
                    );
                    assert!(
                        diagnostic.contains(&inputs.display().to_string()),
                        "{diagnostic}"
                    );
                }
                Fault::Claim => {
                    assert!(
                        !inputs.exists(),
                        "{scenario}: inputs removed despite claim fault"
                    );
                    assert!(marker.is_dir());
                    assert!(
                        diagnostic.contains("could not release firmware hardware claim"),
                        "{diagnostic}"
                    );
                }
            }
        }
    }

    /// A completed mutation remains successful when only local evidence cleanup fails.
    #[tokio::test]
    async fn successful_script_with_cleanup_failure_still_reports_success() {
        let script = r#"#!/bin/sh
set -eu
work="$PWD"
cd /
rm -rf "$work"
printf 'cleanup evidence' > "$work"
printf '%s\n' "$work"
printf 'mutation completed'
"#;
        let (task, _) = counted_script_task(script).await;
        let fixture = FirmwareUpgradeFixture::new();
        let (result, logs) = capture_logs_async(handle_firmware_upgrade(
            &reqwest::Client::new(),
            &task,
            &fixture.config,
        ))
        .await;
        let inputs = std::path::PathBuf::from(result.stdout.lines().next().unwrap());
        let evidence = std::fs::read(&inputs).unwrap();
        std::fs::remove_file(&inputs).unwrap();
        assert_eq!(evidence, b"cleanup evidence");
        assert!(result.success, "{}", result.error);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("mutation completed"));
        assert!(result.error.contains(&inputs.display().to_string()));
        assert!(
            !fixture
                .config
                .state_directory
                .join("mutation.json")
                .exists()
        );
        assert!(
            logs.iter()
                .any(|log| log.message.contains("cleanup requires operator attention"))
        );
        assert!(
            logs.iter()
                .all(|log| !log.message.contains("completion is uncertain"))
        );
    }

    #[tokio::test]
    async fn test_successful_upgrade() {
        let script = r#"#!/bin/sh
set -eu
test "$(cat "$1")" = "binary-data"
printf '%s\n' "$PWD"
echo "upgrade complete"
"#;
        let firmware_content = "binary-data";
        let base = start_file_server(vec![
            ("/scripts/upgrade.sh", script),
            ("/firmware/blob.bin", firmware_content),
        ])
        .await;

        let task = FirmwareUpgradeTask {
            upgrade_task_id: "test-upgrade-task-id".into(),
            component_type: "cpld".into(),
            target_version: "1.2.3".into(),
            script: Some(script_artifact(&base, "/scripts/upgrade.sh", script)),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 30,
            file_artifacts: vec![FileArtifact {
                url: format!("{base}/firmware/blob.bin"),
                sha256: sha256_hex(firmware_content),
            }],
        };

        let fixture = FirmwareUpgradeFixture::new();
        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task, &fixture.config).await;

        assert!(
            result.success,
            "expected success, got error: {}",
            result.error
        );
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("upgrade complete"));
        assert!(result.error.is_empty());
        let work_dir = std::path::Path::new(result.stdout.lines().next().unwrap());
        assert!(!work_dir.exists(), "successful inputs were removed");
        assert!(
            !fixture
                .config
                .state_directory
                .join("mutation.json")
                .exists(),
            "successful claim was released"
        );
    }

    /// Count script downloads so blocked replacement tasks cannot stage another copy.
    async fn counted_script_task(
        script: &'static str,
    ) -> (
        FirmwareUpgradeTask,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let downloads = Arc::new(AtomicUsize::new(0));
        let requests = downloads.clone();
        let app = Router::new()
            .route(
                "/scripts/upgrade.sh",
                get(move || {
                    requests.fetch_add(1, Ordering::SeqCst);
                    async move { script }
                }),
            )
            .route("/firmware/blob.bin", get(|| async { "checked-firmware" }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let task = FirmwareUpgradeTask {
            upgrade_task_id: "first-script-attempt".into(),
            component_type: "cx7".into(),
            target_version: "1.0.0".into(),
            script: Some(script_artifact(&base, "/scripts/upgrade.sh", script)),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 30,
            file_artifacts: vec![script_artifact(
                &base,
                "/firmware/blob.bin",
                "checked-firmware",
            )],
        };
        (task, downloads)
    }

    /// A new Core UUID must preserve the original marker and avoid another download.
    async fn assert_retry_blocked(
        fixture: &FirmwareUpgradeFixture,
        task: &FirmwareUpgradeTask,
        downloads: &std::sync::atomic::AtomicUsize,
    ) {
        use std::sync::atomic::Ordering;

        let before = std::fs::read(fixture.config.state_directory.join("mutation.json")).unwrap();
        let mut retry = task.clone();
        retry.upgrade_task_id = "fresh-core-task-id".into();
        let result =
            handle_firmware_upgrade(&reqwest::Client::new(), &retry, &fixture.config).await;
        assert!(!result.success);
        assert!(
            result.error.contains("existing work must finish"),
            "{}",
            result.error
        );
        assert_eq!(
            downloads.load(Ordering::SeqCst),
            1,
            "blocked retry did not download again"
        );
        assert_eq!(
            std::fs::read(fixture.config.state_directory.join("mutation.json")).unwrap(),
            before,
            "blocked retry did not replace the original claim",
        );
    }

    /// Nonzero can follow a partial hardware mutation even after all processes exit.
    #[tokio::test]
    async fn script_failure_retains_evidence_and_blocks_a_new_task() {
        let script = r#"#!/bin/sh
printf modified > "$DOWNLOAD_DIR/mutation"
printf 'something went wrong' >&2
exit 42
"#;
        let (task, downloads) = counted_script_task(script).await;
        let fixture = FirmwareUpgradeFixture::new();
        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task, &fixture.config).await;
        assert!(!result.success);
        assert_eq!(result.exit_code, 42);
        assert!(result.stderr.contains("something went wrong"));
        assert!(result.error.contains("operator reconciliation"));
        let inputs = fixture
            .retained_inputs()
            .expect("failed script retains its claim");
        assert_eq!(
            std::fs::read(inputs.join("downloads/mutation")).unwrap(),
            b"modified"
        );
        assert_eq!(
            std::fs::read(inputs.join("downloads/blob.bin")).unwrap(),
            b"checked-firmware"
        );
        assert!(
            result.error.contains(&inputs.display().to_string()),
            "{}",
            result.error
        );
        assert_retry_blocked(&fixture, &task, &downloads).await;
    }

    /// A helper spawn failure proves no script started, so a fresh task may run.
    #[tokio::test]
    async fn unavailable_supervisor_releases_claim_before_a_fresh_task() {
        use std::sync::atomic::Ordering;

        let (mut task, downloads) = counted_script_task("#!/bin/sh\nexit 0\n").await;
        let mut fixture = FirmwareUpgradeFixture::new();
        let helper = fixture.config.supervisor_path.clone();
        fixture.config.supervisor_path = Some(fixture.root.path().join("missing-supervisor"));
        let (result, logs) = capture_logs_async(handle_firmware_upgrade(
            &reqwest::Client::new(),
            &task,
            &fixture.config,
        ))
        .await;
        let inputs = logs
            .iter()
            .find_map(|log| log.field("staged_inputs"))
            .map(std::path::PathBuf::from)
            .expect("recorded original staged inputs");
        assert!(
            !inputs.exists(),
            "the original fully staged attempt was removed"
        );
        assert!(
            result.error.contains(&inputs.display().to_string()),
            "{}",
            result.error
        );
        assert!(!result.success);
        assert!(result.error.contains("could not start"), "{}", result.error);
        assert!(
            !fixture
                .config
                .state_directory
                .join("mutation.json")
                .exists()
        );

        fixture.config.supervisor_path = helper;
        task.upgrade_task_id = "fresh-task-after-no-dispatch".into();
        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task, &fixture.config).await;
        assert!(result.success, "{}", result.error);
        assert_eq!(downloads.load(Ordering::SeqCst), 2);
    }

    /// Timeout, cancellation and failed output cleanup retain checked inputs.
    /// The supervisor must reap both independent PIDs, including after future drop.
    #[tokio::test]
    async fn interrupted_scripts_reap_descendants_and_retain_inputs() {
        #[derive(Clone, Copy)]
        enum Stop {
            Deadline,
            Cancellation,
            HeldOutput,
        }
        let cases = [
            ("deadline", Stop::Deadline),
            ("future cancellation", Stop::Cancellation),
            ("unconfirmed output cleanup", Stop::HeldOutput),
        ];
        for (scenario, stop) in cases {
            let script = r#"#!/bin/sh
sleep 60 &
printf '%s %s' "$$" "$!" > "$DOWNLOAD_DIR/pids"
printf diagnostic-out
printf diagnostic-err >&2
wait
"#;
            let (mut task, downloads) = counted_script_task(script).await;
            if !matches!(stop, Stop::Cancellation) {
                task.execution_timeout_seconds = 1;
            }
            let fixture = FirmwareUpgradeFixture::new();
            let client = reqwest::Client::new();
            let mut execution = Box::pin(handle_firmware_upgrade(&client, &task, &fixture.config));
            let ready = async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let Some(inputs) = fixture.retained_inputs()
                            && let Ok(pids) = std::fs::read_to_string(inputs.join("downloads/pids"))
                            && pids.split_whitespace().count() == 2
                        {
                            break (inputs, pids);
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("script publishes shell and updater PIDs")
            };
            let (inputs, pids) = tokio::select! {
                result = &mut execution => panic!("{scenario}: script ended before readiness: {result:?}"),
                ready = ready => ready,
            };
            let held_writer = if matches!(stop, Stop::HeldOutput) {
                // This test owns the extra writer; no escaped process is needed
                // to prevent EOF after the supervised family has stopped.
                let leader = pids.split_whitespace().next().unwrap();
                Some(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(format!("/proc/{leader}/fd/1"))
                        .unwrap(),
                )
            } else {
                None
            };
            match stop {
                Stop::Cancellation => drop(execution),
                Stop::Deadline | Stop::HeldOutput => {
                    let result = tokio::time::timeout(Duration::from_secs(15), execution)
                        .await
                        .expect("script failure and cleanup stay bounded");
                    for pid in pids.split_whitespace() {
                        assert!(
                            !std::path::Path::new("/proc").join(pid).exists(),
                            "{scenario}: process {pid} still exists at response"
                        );
                    }
                    assert!(!result.success, "{scenario}");
                    assert!(
                        result.error.contains(&inputs.display().to_string()),
                        "{}",
                        result.error
                    );
                    assert!(
                        result.error.contains("timed out"),
                        "{scenario}: {}",
                        result.error
                    );
                    assert_eq!(result.stdout, "diagnostic-out", "{scenario}");
                    assert_eq!(result.stderr, "diagnostic-err", "{scenario}");
                    if held_writer.is_some() {
                        assert!(
                            result
                                .error
                                .contains("descendant termination and output drain exceeded"),
                            "{}",
                            result.error
                        );
                    }
                }
            }
            // Keep the writer until after the response: releasing it early
            // would turn the failed cleanup case into ordinary completion.
            assert_eq!(
                std::fs::read(inputs.join("downloads/blob.bin")).unwrap(),
                b"checked-firmware",
                "{scenario}: inputs still exist before observing cleanup",
            );
            drop(held_writer);
            if matches!(stop, Stop::Cancellation) {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while pids
                        .split_whitespace()
                        .any(|pid| std::path::Path::new("/proc").join(pid).exists())
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("shell and updater disappear independently after reaping");
            }
            assert_eq!(
                std::fs::read(inputs.join("downloads/blob.bin")).unwrap(),
                b"checked-firmware",
                "{scenario}"
            );
            assert_eq!(
                std::fs::read_to_string(inputs.join("upgrade.sh")).unwrap(),
                script,
                "{scenario}"
            );
            assert_retry_blocked(&fixture, &task, &downloads).await;
        }
    }

    #[tokio::test]
    async fn test_artifact_download_timeout_redacts_url_secret() {
        const SECRET: &str = "ARTIFACT_TIMEOUT_SECRET_MARKER";
        let script = "#!/bin/sh\necho ok";
        let app = Router::new()
            .route("/scripts/upgrade.sh", get(move || async move { script }))
            .route(
                "/firmware/slow.bin",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    "firmware"
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        let task = FirmwareUpgradeTask {
            upgrade_task_id: "test-upgrade-task-id".into(),
            component_type: "cpld".into(),
            target_version: "1.0.0".into(),
            script: Some(script_artifact(&base, "/scripts/upgrade.sh", script)),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 0,
            file_artifacts: vec![FileArtifact {
                url: format!("{base}/firmware/slow.bin?token={SECRET}#{SECRET}"),
                sha256: sha256_hex("firmware"),
            }],
        };

        let result = handle_firmware_upgrade_for_test(&reqwest::Client::new(), &task).await;

        assert!(!result.success);
        assert!(result.error.contains("download timed out"));
        assert!(result.error.contains(&format!("{base}/firmware/slow.bin")));
        assert!(!result.error.contains(SECRET));
    }

    #[tokio::test]
    async fn test_script_receives_env_vars() {
        let script =
            "#!/bin/sh\necho \"comp=$COMPONENT_TYPE ver=$TARGET_VERSION dir=$DOWNLOAD_DIR\"";
        let base = start_file_server(vec![("/scripts/env.sh", script)]).await;

        let task = FirmwareUpgradeTask {
            upgrade_task_id: "test-upgrade-task-id".into(),
            component_type: "cpldmb".into(),
            target_version: "3.4.5".into(),
            script: Some(script_artifact(&base, "/scripts/env.sh", script)),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 30,
            file_artifacts: vec![],
        };

        let result = handle_firmware_upgrade_for_test(&reqwest::Client::new(), &task).await;

        assert!(result.success, "error: {}", result.error);
        assert!(result.stdout.contains("comp=cpldmb"));
        assert!(result.stdout.contains("ver=3.4.5"));
        assert!(result.stdout.contains("dir="));
    }

    /// Scripts with distinct basenames retain argument order, direct
    /// DOWNLOAD_DIR paths, and the task directory as their working directory.
    #[tokio::test]
    async fn test_script_receives_artifacts_as_args_in_order() {
        let script = r#"#!/bin/sh
set -eu
echo "argc=$#"
echo "arg1=$(basename "$1")"
echo "arg2=$(basename "$2")"
test "$#" = "2"
test -f "$1"
test -f "$2"
test "$(basename "$1")" = "first.bin"
test "$(basename "$2")" = "second.bin"
test "$1" = "$DOWNLOAD_DIR/first.bin"
test "$2" = "$DOWNLOAD_DIR/second.bin"
test "$(cd "$DOWNLOAD_DIR" && pwd -P)" = "$PWD/downloads"
"#;
        let first_content = "first-binary-data";
        let second_content = "second-binary-data";
        let base = start_file_server(vec![
            ("/scripts/env.sh", script),
            ("/firmware/first.bin", first_content),
            ("/firmware/second.bin", second_content),
        ])
        .await;

        let task = FirmwareUpgradeTask {
            upgrade_task_id: "test-upgrade-task-id".into(),
            component_type: "cpldmb".into(),
            target_version: "3.4.5".into(),
            script: Some(script_artifact(&base, "/scripts/env.sh", script)),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 30,
            file_artifacts: vec![
                FileArtifact {
                    url: format!("{base}/firmware/first.bin"),
                    sha256: sha256_hex(first_content),
                },
                FileArtifact {
                    url: format!("{base}/firmware/second.bin"),
                    sha256: sha256_hex(second_content),
                },
            ],
        };

        let result = handle_firmware_upgrade_for_test(&reqwest::Client::new(), &task).await;

        assert!(result.success, "error: {}", result.error);
        assert!(result.stdout.contains("argc=2"));
        assert!(result.stdout.contains("arg1=first.bin"));
        assert!(result.stdout.contains("arg2=second.bin"));
    }

    #[tokio::test]
    async fn test_checksum_mismatch() {
        const SECRET: &str = "ARTIFACT_CHECKSUM_SECRET_MARKER";
        let script = "#!/bin/sh\necho ok";
        let base = start_file_server(vec![
            ("/scripts/upgrade.sh", script),
            ("/firmware/fw.bin", "actual-content"),
        ])
        .await;

        let task = FirmwareUpgradeTask {
            upgrade_task_id: "test-upgrade-task-id".into(),
            component_type: "cpld".into(),
            target_version: "1.0.0".into(),
            script: Some(script_artifact(&base, "/scripts/upgrade.sh", script)),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 30,
            file_artifacts: vec![FileArtifact {
                url: format!("{base}/firmware/fw.bin?token={SECRET}#{SECRET}"),
                sha256: "bad_checksum".to_string(),
            }],
        };

        let fixture = FirmwareUpgradeFixture::new();
        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task, &fixture.config).await;

        assert!(!result.success);
        assert!(result.error.contains("checksum mismatch"));
        assert!(result.error.contains(&format!("{base}/firmware/fw.bin")));
        assert!(!result.error.contains(SECRET));
        assert!(
            !fixture
                .config
                .state_directory
                .join("mutation.json")
                .exists(),
            "digest rejection happened before dispatch and released its own claim",
        );
    }

    #[tokio::test]
    async fn test_script_checksum_mismatch() {
        const SECRET: &str = "SCRIPT_CHECKSUM_SECRET_MARKER";
        let script = "#!/bin/sh\necho ok";
        let base = start_file_server(vec![("/scripts/upgrade.sh", script)]).await;

        let task = FirmwareUpgradeTask {
            upgrade_task_id: "test-upgrade-task-id".into(),
            component_type: "cpld".into(),
            target_version: "1.0.0".into(),
            script: Some(FileArtifact {
                url: format!("{base}/scripts/upgrade.sh?token={SECRET}#{SECRET}"),
                sha256: "bad_checksum".into(),
            }),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 30,
            file_artifacts: vec![],
        };

        let result = handle_firmware_upgrade_for_test(&reqwest::Client::new(), &task).await;

        assert!(!result.success);
        assert!(result.error.contains("checksum mismatch for script"));
        assert!(result.error.contains(&format!("{base}/scripts/upgrade.sh")));
        assert!(!result.error.contains(SECRET));
    }

    #[tokio::test]
    async fn test_legacy_task_json_parses_to_rpc_task() {
        let json = serde_json::json!({
            "upgrade_task_id": "roundtrip-upgrade-task-id",
            "component_type": "cpld",
            "target_version": "1.2.3",
            "script": {
                "url": "http://example.com/script.sh",
                "sha256": "scripthash",
            },
            "execution_timeout_seconds": 300,
            "artifact_download_timeout_seconds": 120,
            "file_artifacts": [{
                "url": "http://example.com/fw.bin",
                "sha256": "abc123",
            }],
        });
        let parsed: FirmwareUpgradeTask = serde_json::from_value(json).unwrap();
        let script = parsed.script.as_ref().unwrap();

        assert_eq!(parsed.component_type, "cpld");
        assert_eq!(parsed.upgrade_task_id, "roundtrip-upgrade-task-id");
        assert_eq!(parsed.target_version, "1.2.3");
        assert_eq!(script.url, "http://example.com/script.sh");
        assert_eq!(script.sha256, "scripthash");
        assert_eq!(parsed.execution_timeout_seconds, 300);
        assert_eq!(parsed.artifact_download_timeout_seconds, 120);
        assert_eq!(parsed.file_artifacts.len(), 1);
        assert_eq!(parsed.file_artifacts[0].url, "http://example.com/fw.bin");
        assert_eq!(parsed.file_artifacts[0].sha256, "abc123");
    }
}
