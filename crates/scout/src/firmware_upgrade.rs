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

use std::time::Duration;

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

// handle_firmware_upgrade downloads file artifacts and a script from carbide-api,
// then executes the script on the host.
pub(super) async fn handle_firmware_upgrade(
    client: &reqwest::Client,
    task: &FirmwareUpgradeTask,
) -> FirmwareUpgradeResult {
    let result = match run_firmware_upgrade(client, task).await {
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

async fn run_firmware_upgrade(
    client: &reqwest::Client,
    task: &FirmwareUpgradeTask,
) -> Result<FirmwareUpgradeResult, Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(
        component_type = %task.component_type,
        target_version = %task.target_version,
        "[firmware_upgrade] starting",
    );

    let mut staging = FirmwareStaging::new()?;

    let download_timeout = Duration::from_secs(task.artifact_download_timeout_seconds.into());
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

    // Keep every input alive until the script exits, including inputs whose
    // basenames collide. Arguments still follow the task's artifact order.
    let mut downloaded_artifacts = Vec::with_capacity(task.file_artifacts.len());
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
        downloaded_artifacts.push(destination);
    }

    tracing::info!(
        script_path = ?script_path,
        "[firmware_upgrade] files downloaded; executing script",
    );

    // kill_on_drop terminates the direct shell when execution times out.
    // It does not supervise descendants the script starts.
    let mut command = tokio::process::Command::new("sh");
    command
        .arg(&script_path)
        .args(&downloaded_artifacts)
        .env("DOWNLOAD_DIR", staging.download_dir())
        .env("COMPONENT_TYPE", &task.component_type)
        .env("TARGET_VERSION", &task.target_version);

    let child = command
        .current_dir(staging.work_dir())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let timeout = std::time::Duration::from_secs(task.execution_timeout_seconds.into());
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8(output.stdout)
                .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned());
            let stderr = String::from_utf8(output.stderr)
                .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned());
            let stdout = firmware_task_text_context(&stdout, task);
            let stderr = firmware_task_text_context(&stderr, task);
            let exit_code = output.status.code().unwrap_or(-1);
            let success = output.status.success();

            if !stdout.is_empty() {
                tracing::info!("[firmware_upgrade] stdout: {stdout}");
            }
            if !stderr.is_empty() {
                tracing::warn!("[firmware_upgrade] stderr: {stderr}");
            }

            Ok(FirmwareUpgradeResult {
                success,
                exit_code,
                stdout,
                stderr,
                error: String::new(),
            })
        }
        Ok(Err(e)) => Err(format!("failed to execute script: {e}").into()),
        Err(_) => Ok(FirmwareUpgradeResult {
            success: false,
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            error: format!(
                "script timed out after {} seconds",
                task.execution_timeout_seconds
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::routing::get;
    use carbide_instrument::testing::capture_logs;
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

                let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;
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

    #[tokio::test]
    async fn test_successful_upgrade() {
        let script = "#!/bin/sh\necho \"upgrade complete\"";
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

        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

        assert!(
            result.success,
            "expected success, got error: {}",
            result.error
        );
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("upgrade complete"));
        assert!(result.error.is_empty());
    }

    #[tokio::test]
    async fn test_script_failure_returns_exit_code() {
        let script = "#!/bin/sh\necho \"something went wrong\" >&2\nexit 42";
        let base = start_file_server(vec![("/scripts/fail.sh", script)]).await;

        let task = FirmwareUpgradeTask {
            upgrade_task_id: "test-upgrade-task-id".into(),
            component_type: "bios".into(),
            target_version: "2.0.0".into(),
            script: Some(script_artifact(&base, "/scripts/fail.sh", script)),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 30,
            file_artifacts: vec![],
        };

        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

        assert!(!result.success);
        assert_eq!(result.exit_code, 42);
        assert!(result.stderr.contains("something went wrong"));
    }

    #[tokio::test]
    async fn test_script_timeout() {
        let script = "#!/bin/sh\nsleep 60";
        let base = start_file_server(vec![("/scripts/slow.sh", script)]).await;

        let task = FirmwareUpgradeTask {
            upgrade_task_id: "test-upgrade-task-id".into(),
            component_type: "cpld".into(),
            target_version: "1.0.0".into(),
            script: Some(script_artifact(&base, "/scripts/slow.sh", script)),
            execution_timeout_seconds: 1,
            artifact_download_timeout_seconds: 30,
            file_artifacts: vec![],
        };

        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

        assert!(!result.success);
        assert!(result.error.contains("timed out"));
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

        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

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

        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

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

        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

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

        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

        assert!(!result.success);
        assert!(result.error.contains("checksum mismatch"));
        assert!(result.error.contains(&format!("{base}/firmware/fw.bin")));
        assert!(!result.error.contains(SECRET));
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

        let result = handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

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
