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

//! Private OCI runtime building blocks for Machine Validation plugins.
//!
//! This module deliberately has no control-plane integration. Existing Machine
//! Validation tests do not construct or call this runner. A later change will
//! supply approved plugin definitions and opt into this execution path.

#[cfg(unix)]
use std::ffi::CString;
use std::path::{Component, Path};
use std::process::Stdio;
#[cfg(unix)]
use std::{os::unix::ffi::OsStrExt, os::unix::fs::MetadataExt, os::unix::fs::PermissionsExt};

use carbide_utils::cmd::TokioCmd;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::{info, warn};
use uuid::Uuid;

use crate::plugin_contract::{PluginResult, read_plugin_result};

const MAX_OUTPUT_SIZE: usize = 1024 * 1024;
const MAX_ERROR_OUTPUT_SIZE: usize = 4096;
const MAX_INPUT_SIZE: usize = 64 * 1024;
const PLUGIN_UID: u32 = 65532;
const PLUGIN_GID: u32 = 65532;
const CONTAINER_CLEANUP_TIMEOUT_SECONDS: u64 = 30;
const CONTAINER_REMOVE_ATTEMPTS: u8 = 3;

const ATTEMPT_BASE_DIR: &str = "/run/nico/machine-validation";

/// The runtime access granted to an approved plugin revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginPrivilege {
    /// A non-root container with no network and no additional capabilities.
    Isolated,
    /// A privileged container, without a host-root bind mount.
    Privileged,
    /// A privileged container with a writable host-root bind mount at `/host`.
    FullHost,
}

/// Immutable execution settings supplied by a later control-plane integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginRuntimeSpec {
    pub(crate) image: String,
    pub(crate) entrypoint: Vec<String>,
    pub(crate) privilege: PluginPrivilege,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PluginExecution {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) result: PluginResult,
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Ensures a cancelled execution still removes its named container.
///
/// `nerdctl` can create a container before its client process exits. The
/// caller's future may be cancelled during Scout shutdown, so relying only on
/// `kill_on_drop` would leave that container running.
struct ContainerCleanupGuard<F>
where
    F: FnOnce(String),
{
    container_name: Option<String>,
    schedule_cleanup: Option<F>,
}

impl<F> ContainerCleanupGuard<F>
where
    F: FnOnce(String),
{
    fn new(container_name: String, schedule_cleanup: F) -> Self {
        Self {
            container_name: Some(container_name),
            schedule_cleanup: Some(schedule_cleanup),
        }
    }

    fn disarm(&mut self) {
        self.container_name = None;
    }
}

impl<F> Drop for ContainerCleanupGuard<F>
where
    F: FnOnce(String),
{
    fn drop(&mut self) {
        let Some(container_name) = self.container_name.take() else {
            return;
        };
        if let Some(schedule_cleanup) = self.schedule_cleanup.take() {
            schedule_cleanup(container_name);
        } else {
            warn!("Could not schedule cleanup for cancelled machine validation plugin container");
        }
    }
}

fn schedule_container_cleanup(container_name: String) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            std::mem::drop(handle.spawn(async move {
                    if let Err(error) = force_remove_plugin_container(&container_name).await {
                        warn!(%container_name, %error, "Failed to remove cancelled machine validation plugin container");
                    }
                }));
        }
        Err(error) => {
            warn!(%container_name, %error, "Could not schedule cleanup for cancelled machine validation plugin container");
        }
    }
}

/// Runs an image that is already available to nerdctl.
///
/// The caller supplies the versioned plugin input document. On timeout, the
/// named container is removed before its bind-mounted attempt directory is
/// deleted. That matters especially for a FullHost plugin.
pub(crate) async fn execute_plugin(
    spec: &PluginRuntimeSpec,
    input: &Value,
    timeout: std::time::Duration,
    contract_dir: &Path,
) -> Result<PluginExecution, String> {
    validate_runtime_spec(spec)?;
    validate_contract_dir(contract_dir)?;
    let input = serialize_plugin_input(input)?;
    let attempt_dir = plugin_attempt_directory()?;
    let input_dir = attempt_dir.join("input");
    let output_dir = attempt_dir.join("output");
    if let Err(error) = prepare_plugin_directories(&attempt_dir, &input_dir, &output_dir) {
        return match remove_attempt_directory(&attempt_dir) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!("{error}; {cleanup_error}")),
        };
    }

    let (execution, remove_attempt_dir) = async {
        if let Err(error) = std::fs::write(input_dir.join("input.json"), input) {
            return (Err(format!("failed to write plugin input: {error}")), true);
        }

        let container_name = format!("mv-plugin-{}", Uuid::new_v4());
        let mut command = tokio::process::Command::new("nerdctl");
        command
            .args(plugin_runtime_args(
                &input_dir,
                &output_dir,
                &container_name,
                spec,
                contract_dir,
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return (Err(format!("failed to execute plugin: {error}")), true),
        };
        let mut cleanup_guard =
            ContainerCleanupGuard::new(container_name.clone(), schedule_container_cleanup);
        match tokio::time::timeout(timeout, collect_plugin_output(child)).await {
            Ok(Ok((status, stdout, stderr))) if status.success() => {
                cleanup_guard.disarm();
                (
                    read_plugin_result(&output_dir.join("result.json")).map(|result| {
                        PluginExecution {
                            stdout: output_to_string(stdout),
                            stderr: output_to_string(stderr),
                            result,
                        }
                    }),
                    true,
                )
            }
            Ok(Ok((status, stdout, stderr))) => {
                cleanup_guard.disarm();
                (
                    Err(format!(
                        "plugin exited unsuccessfully with status {:?}; ignoring result.json; stdout: {}; stderr: {}",
                        status.code(),
                        output_error_summary(stdout),
                        output_error_summary(stderr),
                    )),
                    true,
                )
            }
            Ok(Err(error)) => match force_remove_plugin_container(&container_name).await {
                Ok(()) => {
                    cleanup_guard.disarm();
                    (Err(error), true)
                }
                Err(cleanup_error) => (
                    Err(format!(
                        "{error}; preserving attempt directory because {cleanup_error}"
                    )),
                    false,
                ),
            },
            Err(_) => {
                let timeout_error = format!("plugin timed out after {} seconds", timeout.as_secs());
                match force_remove_plugin_container(&container_name).await {
                    Ok(()) => {
                        cleanup_guard.disarm();
                        (Err(timeout_error), true)
                    }
                    Err(error) => (
                        Err(format!(
                            "{timeout_error}; preserving attempt directory because {error}"
                        )),
                        false,
                    ),
                }
            }
        }
    }
    .await;

    if remove_attempt_dir {
        remove_attempt_directory(&attempt_dir)?;
    }
    execution
}

fn validate_runtime_spec(spec: &PluginRuntimeSpec) -> Result<(), String> {
    let Some((image_name, digest)) = spec.image.split_once('@') else {
        return Err("plugin image must include a SHA-256 digest".to_owned());
    };
    if image_name.is_empty() || image_name.contains(char::is_whitespace) || digest.contains('@') {
        return Err("plugin image must contain one image name and digest".to_owned());
    }
    let Some(digest_hex) = digest.strip_prefix("sha256:") else {
        return Err("plugin image digest must use the sha256 algorithm".to_owned());
    };
    if digest_hex.len() != 64
        || !digest_hex
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err("plugin image digest must be a 64-character SHA-256 hex value".to_owned());
    }
    if spec.entrypoint.is_empty()
        || spec
            .entrypoint
            .iter()
            .any(|argument| argument.is_empty() || argument.contains('\0'))
    {
        return Err("plugin entrypoint must contain non-empty arguments".to_owned());
    }
    Ok(())
}

fn validate_contract_dir(contract_dir: &Path) -> Result<(), String> {
    if !contract_dir.is_absolute()
        || contract_dir.as_os_str().is_empty()
        || contract_dir.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
    {
        return Err("plugin contract directory must be an absolute normalized path".to_owned());
    }
    Ok(())
}

fn serialize_plugin_input(input: &Value) -> Result<Vec<u8>, String> {
    let input = serde_json::to_vec(input)
        .map_err(|error| format!("failed to serialize plugin input: {error}"))?;
    if input.len() > MAX_INPUT_SIZE {
        return Err("plugin input exceeds the 64 KiB limit".to_owned());
    }
    Ok(input)
}

async fn collect_plugin_output(
    mut child: tokio::process::Child,
) -> Result<(std::process::ExitStatus, CapturedOutput, CapturedOutput), String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "plugin stdout was not captured".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "plugin stderr was not captured".to_owned())?;
    let (status, stdout, stderr) =
        tokio::try_join!(child.wait(), read_limited(stdout), read_limited(stderr))
            .map_err(|error| format!("failed to collect plugin output: {error}"))?;
    Ok((status, stdout, stderr))
}

async fn read_limited<R>(mut reader: R) -> Result<CapturedOutput, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(MAX_OUTPUT_SIZE);
    let mut buffer = [0; 8192];
    let mut truncated = false;
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        let remaining = MAX_OUTPUT_SIZE.saturating_sub(bytes.len());
        let copied = bytes_read.min(remaining);
        bytes.extend_from_slice(&buffer[..copied]);
        truncated |= copied < bytes_read;
    }
    Ok(CapturedOutput { bytes, truncated })
}

fn output_to_string(output: CapturedOutput) -> String {
    let mut contents = String::from_utf8_lossy(&output.bytes).into_owned();
    if output.truncated {
        contents.push_str("\n[plugin output truncated at 1 MiB]");
    }
    contents
}

fn output_error_summary(output: CapturedOutput) -> String {
    let output = output_to_string(output);
    if output.len() <= MAX_ERROR_OUTPUT_SIZE {
        return output;
    }
    let mut end = MAX_ERROR_OUTPUT_SIZE;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[plugin output shortened for the error summary]",
        &output[..end]
    )
}

fn plugin_runtime_args(
    input_dir: &Path,
    output_dir: &Path,
    container_name: &str,
    spec: &PluginRuntimeSpec,
    contract_dir: &Path,
) -> Vec<String> {
    let input_path = contract_dir.join("input");
    let output_path = contract_dir.join("output");
    let mut args = vec![
        "-n".to_owned(),
        "default".to_owned(),
        "run".to_owned(),
        "--rm".to_owned(),
        "--network".to_owned(),
        "none".to_owned(),
        "--mount".to_owned(),
        format!(
            "type=bind,src={},dst={},options=rbind:ro",
            input_dir.display(),
            input_path.display()
        ),
        "--mount".to_owned(),
        format!(
            "type=bind,src={},dst={},options=rbind:rw",
            output_dir.display(),
            output_path.display()
        ),
    ];

    match spec.privilege {
        PluginPrivilege::Isolated => args.extend([
            "--user".to_owned(),
            format!("{PLUGIN_UID}:{PLUGIN_GID}"),
            "--cap-drop".to_owned(),
            "ALL".to_owned(),
            "--security-opt".to_owned(),
            "no-new-privileges".to_owned(),
        ]),
        PluginPrivilege::Privileged => args.push("--privileged".to_owned()),
        PluginPrivilege::FullHost => args.extend([
            "--privileged".to_owned(),
            "--mount".to_owned(),
            "type=bind,src=/,dst=/host,options=rbind:rw".to_owned(),
        ]),
    }

    args.extend([
        "--name".to_owned(),
        container_name.to_owned(),
        spec.image.clone(),
    ]);
    args.extend(spec.entrypoint.iter().cloned());
    args
}

fn plugin_container_remove_args(container_name: &str) -> [&str; 5] {
    ["-n", "default", "rm", "--force", container_name]
}

async fn force_remove_plugin_container(container_name: &str) -> Result<(), String> {
    // Dropping the `nerdctl run` client does not stop a container that
    // containerd already created. Remove it before cleaning up its mounts.
    let mut last_error = None;
    let mut only_not_found_errors = true;
    for attempt in 1..=CONTAINER_REMOVE_ATTEMPTS {
        match TokioCmd::new("nerdctl")
            .args(plugin_container_remove_args(container_name))
            .timeout(CONTAINER_CLEANUP_TIMEOUT_SECONDS)
            .output_with_timeout()
            .await
        {
            Ok(result) if result.exit_code == 0 => {
                info!(%container_name, attempt, "Removed timed out machine validation plugin container");
                return Ok(());
            }
            Ok(result) => {
                only_not_found_errors &= container_not_found(&result.stderr);
                last_error = Some(format!(
                    "nerdctl returned {}: {}",
                    result.exit_code, result.stderr
                ));
            }
            Err(error) => {
                only_not_found_errors = false;
                last_error = Some(error.to_string());
            }
        }
        warn!(%container_name, attempt, error = ?last_error, "Failed to remove timed out machine validation plugin container");
        if attempt < CONTAINER_REMOVE_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    if only_not_found_errors {
        // A timeout can occur before `nerdctl` has created the container, or
        // after `--rm` has removed it. Retrying first avoids accepting that
        // response while the killed client is still starting the container.
        info!(%container_name, "Timed out plugin container was already absent");
        return Ok(());
    }
    Err(format!(
        "could not remove timed out plugin container {container_name} after {CONTAINER_REMOVE_ATTEMPTS} attempts: {}",
        last_error.unwrap_or_else(|| "unknown error".to_owned())
    ))
}

fn container_not_found(stderr: &str) -> bool {
    stderr.to_ascii_lowercase().contains("no such container")
}

fn plugin_attempt_directory() -> Result<std::path::PathBuf, String> {
    let base_dir = Path::new(ATTEMPT_BASE_DIR);
    if let Some(parent) = base_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!("failed to create plugin runtime parent directory: {error}")
        })?;
        validate_root_owned_directory(parent, "plugin runtime parent directory")?;
    }
    std::fs::create_dir_all(base_dir)
        .map_err(|error| format!("failed to create plugin runtime directory: {error}"))?;
    validate_root_owned_directory(base_dir, "plugin runtime directory")?;

    #[cfg(unix)]
    {
        std::fs::set_permissions(base_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("failed to restrict plugin runtime directory: {error}"))?;
    }

    Ok(base_dir.join(Uuid::new_v4().to_string()))
}

#[cfg(unix)]
fn validate_root_owned_directory(directory: &Path, name: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(directory)
        .map_err(|error| format!("failed to inspect {name}: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || metadata.uid() != 0 {
        return Err(format!("{name} must be a root-owned directory"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_root_owned_directory(_directory: &Path, _name: &str) -> Result<(), String> {
    Ok(())
}

fn remove_attempt_directory(attempt_dir: &Path) -> Result<(), String> {
    std::fs::remove_dir_all(attempt_dir)
        .map_err(|error| format!("failed to remove plugin attempt directory: {error}"))
}

#[cfg(unix)]
fn prepare_plugin_directories(
    attempt_dir: &Path,
    input_dir: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(attempt_dir)
        .map_err(|error| format!("failed to create plugin attempt directory: {error}"))?;
    std::fs::set_permissions(attempt_dir, std::fs::Permissions::from_mode(0o711))
        .map_err(|error| format!("failed to restrict plugin attempt directory: {error}"))?;

    for directory in [input_dir, output_dir] {
        std::fs::create_dir(directory)
            .map_err(|error| format!("failed to create plugin directory: {error}"))?;
        let path = CString::new(directory.as_os_str().as_bytes())
            .map_err(|_| "plugin directory path contains a null byte".to_owned())?;
        // Scout creates these directories for the fixed non-root container
        // identity. The restrictive mode prevents another host user from
        // pre-seeding or replacing the plugin's result.
        // SAFETY: `path` is NUL-terminated and remains valid for this call.
        if unsafe { libc::chown(path.as_ptr(), PLUGIN_UID, PLUGIN_GID) } != 0 {
            return Err(format!(
                "failed to grant plugin directory access: {}",
                std::io::Error::last_os_error()
            ));
        }
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("failed to restrict plugin directory: {error}"))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_plugin_directories(
    _attempt_dir: &Path,
    _input_dir: &Path,
    _output_dir: &Path,
) -> Result<(), String> {
    Err("machine validation plugin execution requires a Unix host".to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::DEFAULT_PLUGIN_CONTRACT_DIR;

    fn spec(privilege: PluginPrivilege) -> PluginRuntimeSpec {
        PluginRuntimeSpec {
            image: "registry.example/plugin@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
            entrypoint: vec!["/plugin/check".to_owned(), "--verify".to_owned()],
            privilege,
        }
    }

    #[test]
    fn isolated_plugin_is_non_root_and_network_isolated() {
        let args = plugin_runtime_args(
            Path::new("/tmp/input"),
            Path::new("/tmp/output"),
            "plugin-test",
            &spec(PluginPrivilege::Isolated),
            Path::new(DEFAULT_PLUGIN_CONTRACT_DIR),
        );

        assert!(args.windows(2).any(|pair| pair == ["--network", "none"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--user", "65532:65532"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--cap-drop", "ALL"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--security-opt", "no-new-privileges"])
        );
        assert!(args.iter().any(|arg| arg.contains("/opt/forge/mv/input")));
        assert!(args.iter().any(|arg| arg.contains("/opt/forge/mv/output")));
        assert!(!args.iter().any(|arg| arg == "--privileged"));
        assert!(!args.iter().any(|arg| arg.contains("dst=/host")));
    }

    #[test]
    fn plugin_contract_directory_can_be_changed_for_a_scout_deployment() {
        let contract_dir = Path::new("/var/lib/nico/plugin-contract");
        let args = plugin_runtime_args(
            Path::new("/tmp/input"),
            Path::new("/tmp/output"),
            "plugin-test",
            &spec(PluginPrivilege::Isolated),
            contract_dir,
        );

        assert!(
            args.iter()
                .any(|arg| arg.contains("dst=/var/lib/nico/plugin-contract/input"))
        );
        assert!(
            args.iter()
                .any(|arg| arg.contains("dst=/var/lib/nico/plugin-contract/output"))
        );
    }

    #[test]
    fn plugin_contract_directory_must_be_absolute_and_normalized() {
        assert!(validate_contract_dir(Path::new("/var/lib/nico/plugin-contract")).is_ok());
        assert!(validate_contract_dir(Path::new("relative/plugin-contract")).is_err());
        assert!(validate_contract_dir(Path::new("/opt/nico/../plugin-contract")).is_err());
    }

    #[test]
    fn privileged_plugin_has_no_host_root_mount() {
        let args = plugin_runtime_args(
            Path::new("/tmp/input"),
            Path::new("/tmp/output"),
            "plugin-test",
            &spec(PluginPrivilege::Privileged),
            Path::new(DEFAULT_PLUGIN_CONTRACT_DIR),
        );

        assert!(args.iter().any(|arg| arg == "--privileged"));
        assert!(!args.iter().any(|arg| arg.contains("dst=/host")));
    }

    #[test]
    fn full_host_plugin_has_privileged_runtime_and_host_root_mount() {
        let args = plugin_runtime_args(
            Path::new("/tmp/input"),
            Path::new("/tmp/output"),
            "plugin-test",
            &spec(PluginPrivilege::FullHost),
            Path::new(DEFAULT_PLUGIN_CONTRACT_DIR),
        );

        assert!(args.iter().any(|arg| arg == "--privileged"));
        assert!(
            args.iter()
                .any(|arg| arg.contains("dst=/host,options=rbind:rw"))
        );
    }

    #[test]
    fn runtime_spec_requires_a_sha256_image_digest_and_entrypoint() {
        let mut runtime_spec = spec(PluginPrivilege::Isolated);
        assert!(validate_runtime_spec(&runtime_spec).is_ok());

        runtime_spec.image = "registry.example/plugin:latest".to_owned();
        assert!(validate_runtime_spec(&runtime_spec).is_err());

        runtime_spec.image = "registry.example/plugin@sha256:not-a-digest".to_owned();
        assert!(validate_runtime_spec(&runtime_spec).is_err());

        runtime_spec = spec(PluginPrivilege::Isolated);
        runtime_spec.entrypoint = vec![];
        assert!(validate_runtime_spec(&runtime_spec).is_err());
    }

    #[test]
    fn plugin_input_is_bounded() {
        assert!(serialize_plugin_input(&serde_json::json!({ "input": "ok" })).is_ok());
        assert!(
            serialize_plugin_input(&serde_json::json!({
                "input": "x".repeat(MAX_INPUT_SIZE),
            }))
            .is_err()
        );
    }

    #[test]
    fn timed_out_plugin_removal_targets_the_named_container() {
        assert_eq!(
            plugin_container_remove_args("mv-plugin-example"),
            ["-n", "default", "rm", "--force", "mv-plugin-example"]
        );
    }

    #[test]
    fn container_not_found_is_idempotent_cleanup() {
        assert!(container_not_found(
            "FATA[0000] no such container: mv-plugin-example"
        ));
        assert!(!container_not_found("permission denied"));
    }

    #[test]
    fn cancelled_execution_schedules_container_cleanup() {
        let removed_containers = Arc::new(Mutex::new(Vec::new()));
        let scheduler_containers = Arc::clone(&removed_containers);
        {
            let _cleanup_guard =
                ContainerCleanupGuard::new("mv-plugin-example".to_owned(), move |container_name| {
                    scheduler_containers
                        .lock()
                        .expect("lock scheduled cleanup")
                        .push(container_name);
                });
        }

        assert_eq!(
            *removed_containers.lock().expect("lock scheduled cleanup"),
            ["mv-plugin-example"]
        );
    }

    #[test]
    fn completed_execution_does_not_schedule_container_cleanup() {
        let removed_containers = Arc::new(Mutex::new(Vec::new()));
        let scheduler_containers = Arc::clone(&removed_containers);
        let mut cleanup_guard =
            ContainerCleanupGuard::new("mv-plugin-example".to_owned(), move |container_name| {
                scheduler_containers
                    .lock()
                    .expect("lock scheduled cleanup")
                    .push(container_name);
            });

        cleanup_guard.disarm();
        drop(cleanup_guard);
        assert!(
            removed_containers
                .lock()
                .expect("lock scheduled cleanup")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn plugin_output_is_bounded_while_the_stream_is_drained() {
        let (mut writer, reader) = tokio::io::duplex(8192);
        let payload = vec![b'x'; MAX_OUTPUT_SIZE + 1];
        let writer = tokio::spawn(async move { writer.write_all(&payload).await });

        let output = read_limited(reader).await.expect("read plugin output");
        writer
            .await
            .expect("join writer")
            .expect("write plugin output");

        assert_eq!(output.bytes.len(), MAX_OUTPUT_SIZE);
        assert!(output.truncated);
        assert!(output_to_string(output).ends_with("[plugin output truncated at 1 MiB]"));
    }

    #[test]
    fn failed_plugin_output_is_shortened_for_result_errors() {
        let output = CapturedOutput {
            bytes: vec![b'x'; MAX_ERROR_OUTPUT_SIZE + 1],
            truncated: false,
        };

        let summary = output_error_summary(output);
        assert!(summary.contains("[plugin output shortened for the error summary]"));
        assert!(summary.len() > MAX_ERROR_OUTPUT_SIZE);
        assert!(summary.len() < MAX_ERROR_OUTPUT_SIZE + 128);
    }
}
