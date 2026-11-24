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
//! Existing Machine Validation tests do not construct or call this runner.
//! Plugin output can be sent over a bounded channel, but persistence remains
//! outside this module so the runner never depends on control-plane delivery.

#[cfg(unix)]
use std::ffi::CString;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
#[cfg(unix)]
use std::{os::unix::ffi::OsStrExt, os::unix::fs::MetadataExt, os::unix::fs::PermissionsExt};

use carbide_utils::cmd::TokioCmd;
use machine_validation_adapters::{
    AdapterContext, AdapterInvocation, AdapterOutput, PluginResult, RuntimeRequirements,
};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

const MAX_OUTPUT_SIZE: usize = 1024 * 1024;
const MAX_ERROR_OUTPUT_SIZE: usize = 4096;
const MAX_INPUT_SIZE: usize = 64 * 1024;
const PLUGIN_UID: u32 = 65532;
const PLUGIN_GID: u32 = 65532;
const PLUGIN_CONTRACT_DIR_ENV: &str = "NICO_MV_CONTRACT_DIR";
const CONTAINER_CLEANUP_TIMEOUT_SECONDS: u64 = 30;
const CONTAINER_REMOVE_ATTEMPTS: u8 = 3;
const MAX_LOG_CHUNK_BYTES: usize = 4096;

const ATTEMPT_BASE_DIR: &str = "/run/nico/machine-validation";

/// The runtime access granted to an approved plugin revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginPrivilege {
    /// A container with dropped capabilities and the approved adapter identity
    /// and namespaces; standard plugins remain non-root and network-isolated.
    Isolated,
    /// A privileged container, without a host-root bind mount.
    Privileged,
    /// A privileged container with a writable host-root bind mount at `/host`.
    FullHost,
}

/// Immutable execution settings supplied by a later control-plane integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginRuntimeSpec {
    pub(crate) container_plugin_adapter: Option<String>,
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

/// One bounded, UTF-8 log record emitted while a plugin is running.
///
/// The runner uses a bounded channel and never waits for the control plane to
/// consume it. A slow or unavailable API must not block the container's output
/// pipes or affect validation execution.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PluginLogChunk {
    pub(crate) stream: PluginLogStream,
    pub(crate) content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginLogStream {
    Stdout,
    Stderr,
}

#[derive(Clone)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
    log_dropped: bool,
}

/// Owns attempt files until cleanup succeeds, including cancellation before launch.
///
/// `nerdctl` can create a container before its client process exits. The
/// caller's future may be cancelled during Scout shutdown, so relying only on
/// `kill_on_drop` would leave that container running.
struct PluginCleanupGuard<F>
where
    F: FnOnce(String, PathBuf),
{
    attempt_dir: Option<PathBuf>,
    container_name: Option<String>,
    schedule_cleanup: Option<F>,
}

impl<F> PluginCleanupGuard<F>
where
    F: FnOnce(String, PathBuf),
{
    fn new(attempt_dir: PathBuf, schedule_cleanup: F) -> Self {
        Self {
            attempt_dir: Some(attempt_dir),
            container_name: None,
            schedule_cleanup: Some(schedule_cleanup),
        }
    }

    fn disarm(&mut self) {
        self.attempt_dir = None;
        self.container_name = None;
    }
}

impl<F> Drop for PluginCleanupGuard<F>
where
    F: FnOnce(String, PathBuf),
{
    fn drop(&mut self) {
        let Some(attempt_dir) = self.attempt_dir.take() else {
            return;
        };
        if let Some(container_name) = self.container_name.take() {
            if let Some(schedule_cleanup) = self.schedule_cleanup.take() {
                schedule_cleanup(container_name, attempt_dir);
            }
        } else if let Err(error) = remove_attempt_directory(&attempt_dir) {
            warn!(%error, "Failed to remove cancelled plugin attempt directory");
        }
    }
}

fn schedule_container_cleanup(container_name: String, attempt_dir: PathBuf) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            std::mem::drop(handle.spawn(async move {
                    if let Err(error) = force_remove_plugin_container(&container_name).await {
                        warn!(%container_name, %error, "Failed to remove cancelled machine validation plugin container");
                    } else if let Err(error) = remove_attempt_directory(&attempt_dir) {
                        warn!(%container_name, %error, "Failed to remove cancelled plugin attempt directory");
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
    log_sender: Option<mpsc::Sender<PluginLogChunk>>,
    authorize_launch: impl std::future::Future<Output = Result<(), String>>,
) -> Result<PluginExecution, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    validate_runtime_spec(spec)?;
    let adapter = machine_validation_adapters::resolve(spec.container_plugin_adapter.as_deref())?;
    adapter.validate_registration(&spec.entrypoint, &input["parameters"])?;
    let requirements = adapter.requirements();
    validate_contract_dir(contract_dir)?;
    let serialized_input = serialize_plugin_input(input)?;
    let attempt_dir = plugin_attempt_directory()?;
    let input_dir = attempt_dir.join("input");
    let output_dir = attempt_dir.join("output");
    let directory_owner = if requirements.run_as_root {
        (0, 0)
    } else {
        (PLUGIN_UID, PLUGIN_GID)
    };
    if let Err(error) =
        prepare_plugin_directories(&attempt_dir, &input_dir, &output_dir, directory_owner)
    {
        return match remove_attempt_directory(&attempt_dir) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!("{error}; {cleanup_error}")),
        };
    }

    let mut cleanup_guard =
        PluginCleanupGuard::new(attempt_dir.clone(), schedule_container_cleanup);
    let (execution, remove_attempt_dir) = async {
        if let Err(error) = std::fs::write(input_dir.join("input.json"), serialized_input) {
            return (Err(format!("failed to write plugin input: {error}")), true);
        }

        let context = AdapterContext {
            input,
            input_dir: &input_dir,
            output_dir: &output_dir,
            contract_dir,
        };
        let invocation =
            match tokio::time::timeout_at(deadline, adapter.prepare(context, &spec.entrypoint))
                .await
            {
                Ok(Ok(invocation)) => invocation,
                Ok(Err(error)) => return (Err(error), true),
                Err(_) => {
                    return (
                        Err("plugin timeout exhausted preparing invocation".to_owned()),
                        true,
                    );
                }
            };
        if let Err(error) =
            validate_invocation(&invocation, requirements, spec.privilege, contract_dir)
        {
            return (Err(error), true);
        }
        if let Err(error) = authorize_before_launch(deadline, authorize_launch).await {
            return (Err(error), true);
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
                requirements,
                &invocation,
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if tokio::time::Instant::now() >= deadline {
            return (
                Err("plugin timeout exhausted before launch".to_owned()),
                true,
            );
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return (Err(format!("failed to execute plugin: {error}")), true),
        };
        cleanup_guard.container_name = Some(container_name.clone());
        match tokio::time::timeout_at(deadline, collect_plugin_output(child, log_sender)).await {
            Ok(Ok((status, stdout, stderr))) => {
                cleanup_guard.container_name = None;
                warn_if_log_chunks_dropped(&stdout, &stderr);
                let result = adapter
                    .interpret(
                        context,
                        AdapterOutput {
                            exit_code: status.code(),
                            stdout: &stdout.bytes,
                            stdout_truncated: stdout.truncated,
                        },
                    )
                    .map_err(|error| {
                        format!(
                            "{error}; stdout: {}; stderr: {}",
                            output_error_summary(stdout.clone()),
                            output_error_summary(stderr.clone())
                        )
                    });
                (
                    result.map(|result| PluginExecution {
                        stdout: output_to_string(stdout),
                        stderr: output_to_string(stderr),
                        result,
                    }),
                    true,
                )
            }
            Ok(Err(error)) => match force_remove_plugin_container(&container_name).await {
                Ok(()) => {
                    cleanup_guard.container_name = None;
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
                        cleanup_guard.container_name = None;
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
        cleanup_guard.disarm();
    }
    execution
}

async fn authorize_before_launch(
    deadline: tokio::time::Instant,
    authorize: impl std::future::Future<Output = Result<(), String>>,
) -> Result<(), String> {
    if tokio::time::Instant::now() >= deadline {
        return Err("plugin timeout exhausted before launch".to_owned());
    }
    tokio::time::timeout_at(deadline, authorize)
        .await
        .map_err(|_| "plugin timeout exhausted authorizing launch".to_owned())??;
    if tokio::time::Instant::now() >= deadline {
        return Err("plugin timeout exhausted before launch".to_owned());
    }
    Ok(())
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
    if contract_dir.to_string_lossy().contains([',', '\0']) {
        return Err("plugin contract directory must not contain commas".to_owned());
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
    log_sender: Option<mpsc::Sender<PluginLogChunk>>,
) -> Result<(std::process::ExitStatus, CapturedOutput, CapturedOutput), String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "plugin stdout was not captured".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "plugin stderr was not captured".to_owned())?;
    let (status, stdout, stderr) = tokio::try_join!(
        child.wait(),
        read_limited(stdout, PluginLogStream::Stdout, log_sender.clone()),
        read_limited(stderr, PluginLogStream::Stderr, log_sender)
    )
    .map_err(|error| format!("failed to collect plugin output: {error}"))?;
    Ok((status, stdout, stderr))
}

async fn read_limited<R>(
    mut reader: R,
    stream: PluginLogStream,
    log_sender: Option<mpsc::Sender<PluginLogChunk>>,
) -> Result<CapturedOutput, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(MAX_OUTPUT_SIZE);
    let mut buffer = [0; 8192];
    let mut truncated = false;
    let mut log_dropped = false;
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        let remaining = MAX_OUTPUT_SIZE.saturating_sub(bytes.len());
        let copied = bytes_read.min(remaining);
        bytes.extend_from_slice(&buffer[..copied]);
        truncated |= copied < bytes_read;
        if let Some(sender) = &log_sender {
            log_dropped |= !send_log_chunks(sender, stream, &buffer[..bytes_read]);
        }
    }
    Ok(CapturedOutput {
        bytes,
        truncated,
        log_dropped,
    })
}

fn send_log_chunks(
    sender: &mpsc::Sender<PluginLogChunk>,
    stream: PluginLogStream,
    bytes: &[u8],
) -> bool {
    // The public log contract is UTF-8. Preserve valid plugin text and replace
    // malformed bytes rather than allowing binary output to stop execution.
    let content = String::from_utf8_lossy(bytes);
    let mut start = 0;
    while start < content.len() {
        let mut end = (start + MAX_LOG_CHUNK_BYTES).min(content.len());
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        if sender
            .try_send(PluginLogChunk {
                stream,
                content: content[start..end].to_owned(),
            })
            .is_err()
        {
            return false;
        }
        start = end;
    }
    true
}

fn output_to_string(output: CapturedOutput) -> String {
    let mut contents = String::from_utf8_lossy(&output.bytes).into_owned();
    if output.truncated {
        contents.push_str("\n[plugin output truncated at 1 MiB]");
    }
    contents
}

fn warn_if_log_chunks_dropped(stdout: &CapturedOutput, stderr: &CapturedOutput) {
    if stdout.log_dropped || stderr.log_dropped {
        warn!(
            "Plugin output exceeded the live log buffer; some attempt log chunks were not stored"
        );
    }
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

// Adapter dependencies are typed read-only data, never arbitrary runtime flags.
fn validate_invocation(
    invocation: &AdapterInvocation,
    requirements: RuntimeRequirements,
    privilege: PluginPrivilege,
    contract_dir: &Path,
) -> Result<(), String> {
    if !invocation.socket_mounts.is_empty() && !requirements.host_sockets {
        return Err("adapter host sockets require a declared host_sockets requirement".to_owned());
    }
    if (!invocation.mounts.is_empty() || !invocation.socket_mounts.is_empty())
        && !requirements.run_as_root
        && privilege == PluginPrivilege::Isolated
    {
        return Err(
            "plugin host dependencies require approved adapter access or privilege".to_owned(),
        );
    }
    for (source, target) in invocation
        .mounts
        .iter()
        .map(|mount| (&mount.source, &mount.target))
        .chain(
            invocation
                .socket_mounts
                .iter()
                .map(|mount| (&mount.source, &mount.target)),
        )
    {
        validate_contract_dir(source)?;
        validate_contract_dir(target)?;
        if source == Path::new("/")
            || target == Path::new("/")
            || target.starts_with(contract_dir)
            || contract_dir.starts_with(target)
        {
            return Err(
                "adapter dependencies must not mount the host root or replace contract files"
                    .to_owned(),
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        for mount in &invocation.mounts {
            let metadata = std::fs::metadata(&mount.source)
                .map_err(|error| format!("cannot inspect adapter data dependency: {error}"))?;
            if metadata.file_type().is_socket() {
                return Err("host Unix sockets must be declared as socket mounts".to_owned());
            }
        }
        for mount in &invocation.socket_mounts {
            let metadata = std::fs::metadata(&mount.source)
                .map_err(|error| format!("cannot inspect adapter host socket: {error}"))?;
            if !metadata.file_type().is_socket() {
                return Err("adapter host socket mount must reference a Unix socket".to_owned());
            }
        }
    }
    if invocation
        .environment
        .keys()
        .any(|key| key.is_empty() || key.contains(['=', '\0']) || key == PLUGIN_CONTRACT_DIR_ENV)
        || invocation
            .environment
            .values()
            .any(|value| value.contains('\0'))
        || invocation
            .arguments
            .iter()
            .any(|argument| argument.contains('\0'))
        || invocation
            .entrypoint
            .as_ref()
            .is_some_and(|entrypoint| entrypoint.is_empty() || entrypoint.contains('\0'))
    {
        return Err("invalid adapter invocation".to_owned());
    }
    Ok(())
}

fn plugin_runtime_args(
    input_dir: &Path,
    output_dir: &Path,
    container_name: &str,
    spec: &PluginRuntimeSpec,
    contract_dir: &Path,
    requirements: RuntimeRequirements,
    invocation: &AdapterInvocation,
) -> Vec<String> {
    let input_path = contract_dir.join("input");
    let output_path = contract_dir.join("output");
    let mut args = vec![
        "-n".to_owned(),
        "default".to_owned(),
        "run".to_owned(),
        "--rm".to_owned(),
        "--network".to_owned(),
        if requirements.host_network {
            "host"
        } else {
            "none"
        }
        .to_owned(),
        "--env".to_owned(),
        format!("{PLUGIN_CONTRACT_DIR_ENV}={}", contract_dir.display()),
        "--mount".to_owned(),
        format!(
            "type=bind,src={},dst={},readonly",
            input_dir.display(),
            input_path.display()
        ),
        "--mount".to_owned(),
        format!(
            "type=bind,src={},dst={}",
            output_dir.display(),
            output_path.display()
        ),
    ];

    match spec.privilege {
        PluginPrivilege::Isolated => args.extend([
            "--user".to_owned(),
            if requirements.run_as_root {
                "0:0".to_owned()
            } else {
                format!("{PLUGIN_UID}:{PLUGIN_GID}")
            },
            "--cap-drop".to_owned(),
            "ALL".to_owned(),
            "--security-opt".to_owned(),
            "no-new-privileges".to_owned(),
        ]),
        PluginPrivilege::Privileged => args.push("--privileged".to_owned()),
        PluginPrivilege::FullHost => args.extend([
            "--privileged".to_owned(),
            "--pid".to_owned(),
            "host".to_owned(),
            "--mount".to_owned(),
            "type=bind,src=/,dst=/host".to_owned(),
        ]),
    }

    if requirements.host_pid && spec.privilege != PluginPrivilege::FullHost {
        args.extend(["--pid".to_owned(), "host".to_owned()]);
    }
    if requirements.run_as_root && spec.privilege != PluginPrivilege::Isolated {
        args.extend(["--user".to_owned(), "0:0".to_owned()]);
    }
    if requirements.all_gpus {
        args.extend(["--gpus".to_owned(), "all".to_owned()]);
    }
    if let Some(entrypoint) = &invocation.entrypoint {
        args.extend(["--entrypoint".to_owned(), entrypoint.clone()]);
    }
    for (key, value) in &invocation.environment {
        args.extend(["--env".to_owned(), format!("{key}={value}")]);
    }
    for mount in &invocation.mounts {
        args.extend([
            "--mount".to_owned(),
            format!(
                "type=bind,src={},dst={},readonly",
                mount.source.display(),
                mount.target.display()
            ),
        ]);
    }
    // A read-only socket bind still permits protocol requests. These mounts
    // are distinct from data dependencies and require explicit site approval.
    for mount in &invocation.socket_mounts {
        args.extend([
            "--mount".to_owned(),
            format!(
                "type=bind,src={},dst={},readonly",
                mount.source.display(),
                mount.target.display()
            ),
        ]);
    }

    args.extend([
        "--name".to_owned(),
        container_name.to_owned(),
        spec.image.clone(),
    ]);
    args.extend(invocation.arguments.iter().cloned());
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
        last_error.as_deref().unwrap_or("unknown error")
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
    owner: (u32, u32),
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
        // Match the approved container identity, including root without DAC
        // override capabilities. Restrictive modes prevent other host users
        // from pre-seeding inputs or replacing results.
        // SAFETY: `path` is NUL-terminated and remains valid for this call.
        if unsafe { libc::chown(path.as_ptr(), owner.0, owner.1) } != 0 {
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
    _owner: (u32, u32),
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
            container_plugin_adapter: None,
            image: "registry.example/plugin@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
            entrypoint: vec!["/plugin/check".to_owned(), "--verify".to_owned()],
            privilege,
        }
    }

    // Hardware qualification is opt-in: never run containers on a developer
    // host merely because the unit test suite is being executed.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires root, a pre-pulled HealthValidation image, GPUs, and Scout journal/runtime dependencies"]
    async fn dsx_scout_qualification() {
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        assert_eq!(unsafe { libc::geteuid() }, 0, "qualification requires root");
        let image = std::env::var("NICO_MV_DSX_TEST_IMAGE")
            .expect("set NICO_MV_DSX_TEST_IMAGE to a pre-pulled digest-pinned image");
        let expected = std::env::var("NICO_MV_DSX_EXPECTED_SEVERITY")
            .expect("set NICO_MV_DSX_EXPECTED_SEVERITY to the established node severity");
        let (outcome, severity) = match expected.as_str() {
            "PASS" => ("pass", None),
            "INFO" => ("pass", Some("info")),
            "WARNING" => ("pass", Some("warning")),
            "CRITICAL" => ("fail", Some("critical")),
            "UNKNOWN" => ("fail", Some("unknown")),
            _ => panic!("unsupported expected severity {expected:?}"),
        };
        let mut parameters = serde_json::json!({});
        if let Ok(count) = std::env::var("NICO_MV_DSX_GPU_COUNT") {
            parameters["gpu_count"] = count.parse::<u32>().expect("invalid GPU count").into();
        }
        let input = serde_json::json!({
            "machineId": "nico-dsx-qualification",
            "parameters": parameters,
        });
        let mut runtime = spec(PluginPrivilege::Isolated);
        runtime.image = image;
        runtime.container_plugin_adapter = Some("DSXHealthValidation".to_owned());
        // Qualify the DCGM 4 profile against the PR #269 CLI, which defaults
        // to TENANT_GUEST unless OPERATOR_INFRA is selected explicitly.
        runtime.entrypoint = vec![
            "/health-validation".to_owned(),
            "run".to_owned(),
            "--execution-context".to_owned(),
            "OPERATOR_INFRA".to_owned(),
        ];
        let (sender, mut receiver) = mpsc::channel::<PluginLogChunk>(1024);
        let logs = tokio::spawn(async move {
            let mut stdout = String::new();
            let mut stderr = String::new();
            while let Some(chunk) = receiver.recv().await {
                match chunk.stream {
                    PluginLogStream::Stdout => stdout.push_str(&chunk.content),
                    PluginLogStream::Stderr => stderr.push_str(&chunk.content),
                }
            }
            (stdout, stderr)
        });
        let execution = execute_plugin(
            &runtime,
            &input,
            std::time::Duration::from_secs(180),
            Path::new(DEFAULT_PLUGIN_CONTRACT_DIR),
            Some(sender),
            async { Ok(()) },
        )
        .await
        .expect("the real DSX adapter must complete an assessment");
        let (stdout, stderr) = logs.await.unwrap();
        assert_eq!(stdout, execution.stdout, "stdout must reach attempt logs");
        assert_eq!(stderr, execution.stderr, "stderr must reach attempt logs");
        let report: Value = serde_json::from_str(&execution.stdout).unwrap();
        assert_eq!(report["overall_severity"], expected);
        assert_eq!(execution.result.outcome, outcome);
        assert_eq!(execution.result.severity.as_deref(), severity);
        println!("HealthValidation stdout: {}", execution.stdout);
        println!("HealthValidation stderr: {}", execution.stderr);

        // The published Ubuntu-based image also supplies a shell and sleep.
        // Exercise the same GPU/root/journal profile with a process that cannot
        // complete before the deadline. Success here requires container removal
        // and attempt-directory cleanup, otherwise execute_plugin adds an error.
        runtime.entrypoint = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "exec sleep 90".to_owned(),
        ];
        let error = execute_plugin(
            &runtime,
            &input,
            std::time::Duration::from_secs(30),
            Path::new(DEFAULT_PLUGIN_CONTRACT_DIR),
            None,
            async { Ok(()) },
        )
        .await
        .expect_err("qualification process must time out");
        assert_eq!(error, "plugin timed out after 30 seconds");
    }

    #[test]
    fn isolated_plugin_is_non_root_and_network_isolated() {
        let args = plugin_runtime_args(
            Path::new("/tmp/input"),
            Path::new("/tmp/output"),
            "plugin-test",
            &spec(PluginPrivilege::Isolated),
            Path::new(DEFAULT_PLUGIN_CONTRACT_DIR),
            RuntimeRequirements::default(),
            &AdapterInvocation {
                arguments: spec(PluginPrivilege::Isolated).entrypoint,
                ..Default::default()
            },
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
        assert!(args.windows(2).any(|pair| {
            pair == [
                "--mount",
                "type=bind,src=/tmp/input,dst=/opt/forge/mv/input,readonly",
            ]
        }));
        assert!(args.windows(2).any(|pair| {
            pair == [
                "--mount",
                "type=bind,src=/tmp/output,dst=/opt/forge/mv/output",
            ]
        }));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--env", "NICO_MV_CONTRACT_DIR=/opt/forge/mv"])
        );
        assert!(!args.iter().any(|arg| arg == "--privileged"));
        assert!(!args.iter().any(|arg| arg == "--pid"));
        assert!(!args.iter().any(|arg| arg.contains("dst=/host")));
        assert!(!args.iter().any(|arg| arg == "--entrypoint"));
        assert_eq!(
            &args[args.len() - 3..],
            [
                spec(PluginPrivilege::Isolated).image,
                "/plugin/check".to_owned(),
                "--verify".to_owned()
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn attempt_directories_use_the_selected_container_identity() {
        let attempt = std::env::temp_dir().join(format!("adapter-directories-{}", Uuid::new_v4()));
        std::fs::create_dir(&attempt).unwrap();
        let metadata = std::fs::metadata(&attempt).unwrap();
        let owner = (metadata.uid(), metadata.gid());
        let input = attempt.join("input");
        let output = attempt.join("output");
        prepare_plugin_directories(&attempt, &input, &output, owner).unwrap();
        for directory in [&input, &output] {
            let metadata = std::fs::metadata(directory).unwrap();
            assert_eq!((metadata.uid(), metadata.gid()), owner);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
        std::fs::remove_dir_all(attempt).unwrap();
    }

    #[test]
    fn approved_gpu_adapter_uses_declared_namespaces_without_privileged_or_host_root_access() {
        let dependency =
            std::env::temp_dir().join(format!("adapter-dependency-{}", Uuid::new_v4()));
        std::fs::write(&dependency, b"fixture").unwrap();
        let requirements = machine_validation_adapters::resolve(Some("DSXHealthValidation"))
            .unwrap()
            .requirements();
        let invocation = AdapterInvocation {
            entrypoint: Some("/custom/check".to_owned()),
            arguments: vec!["run".to_owned()],
            environment: [(
                "NVIDIA_DRIVER_CAPABILITIES".to_owned(),
                "utility".to_owned(),
            )]
            .into(),
            mounts: vec![machine_validation_adapters::ReadOnlyMount {
                source: dependency.clone(),
                target: "/usr/bin/journalctl".into(),
            }],
            socket_mounts: Vec::new(),
        };
        let contract = Path::new(DEFAULT_PLUGIN_CONTRACT_DIR);
        validate_invocation(
            &invocation,
            requirements,
            PluginPrivilege::Isolated,
            contract,
        )
        .unwrap();
        assert!(
            validate_invocation(
                &invocation,
                RuntimeRequirements::default(),
                PluginPrivilege::Isolated,
                contract
            )
            .is_err()
        );
        let runtime = spec(PluginPrivilege::Isolated);
        let args = plugin_runtime_args(
            Path::new("/tmp/input"),
            Path::new("/tmp/output"),
            "test",
            &runtime,
            contract,
            requirements,
            &invocation,
        );
        let mount_argument = format!(
            "type=bind,src={},dst=/usr/bin/journalctl,readonly",
            dependency.display()
        );
        for pair in [
            ["--gpus", "all"],
            ["--user", "0:0"],
            ["--cap-drop", "ALL"],
            ["--network", "host"],
            ["--pid", "host"],
            ["--entrypoint", "/custom/check"],
            ["--env", "NVIDIA_DRIVER_CAPABILITIES=utility"],
            ["--mount", mount_argument.as_str()],
        ] {
            assert!(args.windows(2).any(|actual| actual == pair));
        }
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--privileged" || arg.contains("dst=/host"))
        );
        assert_eq!(&args[args.len() - 2..], [runtime.image, "run".to_owned()]);
        std::fs::remove_file(dependency).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn host_service_sockets_require_a_declaration_and_cannot_be_data_mounts() {
        use std::os::unix::net::UnixDatagram;

        use machine_validation_adapters::{HostSocketMount, ReadOnlyMount};
        let directory = std::env::temp_dir().join(format!("adapter-socket-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("socket");
        let _socket = UnixDatagram::bind(&path).unwrap();
        let mut invocation = AdapterInvocation {
            socket_mounts: vec![HostSocketMount {
                source: path.clone(),
                target: "/run/service/socket".into(),
            }],
            ..Default::default()
        };
        let mut requirements = RuntimeRequirements {
            run_as_root: true,
            ..Default::default()
        };
        let contract = Path::new(DEFAULT_PLUGIN_CONTRACT_DIR);
        assert!(
            validate_invocation(
                &invocation,
                requirements,
                PluginPrivilege::Isolated,
                contract
            )
            .is_err()
        );
        requirements.host_sockets = true;
        validate_invocation(
            &invocation,
            requirements,
            PluginPrivilege::Isolated,
            contract,
        )
        .unwrap();
        let args = plugin_runtime_args(
            &directory,
            &directory,
            "socket-test",
            &spec(PluginPrivilege::Isolated),
            contract,
            requirements,
            &invocation,
        );
        assert!(args.windows(2).any(|pair| pair
            == [
                "--mount",
                &format!(
                    "type=bind,src={},dst=/run/service/socket,readonly",
                    path.display()
                )
            ]));
        invocation.socket_mounts[0].target = contract.join("input/audit");
        assert!(
            validate_invocation(
                &invocation,
                requirements,
                PluginPrivilege::Isolated,
                contract
            )
            .unwrap_err()
            .contains("replace contract files")
        );
        invocation.socket_mounts.clear();
        invocation.mounts.push(ReadOnlyMount {
            source: path.clone(),
            target: "/run/service/socket".into(),
        });
        assert!(
            validate_invocation(
                &invocation,
                requirements,
                PluginPrivilege::Isolated,
                contract
            )
            .is_err()
        );
        invocation.mounts.clear();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"not a socket").unwrap();
        invocation.socket_mounts.push(HostSocketMount {
            source: path,
            target: "/run/service/socket".into(),
        });
        assert!(
            validate_invocation(
                &invocation,
                requirements,
                PluginPrivilege::Isolated,
                contract
            )
            .is_err()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn adapter_mounts_cannot_inject_options_or_replace_contract_files() {
        for (source, target, expected) in [
            ("/", "/dependency", "must not mount"),
            (
                "/host/lib,readonly=false",
                "/dependency",
                "must not contain commas",
            ),
            ("/host/lib", "/opt/forge/mv/input", "replace contract files"),
            ("/host/lib", "/", "must not mount"),
        ] {
            let invocation = AdapterInvocation {
                mounts: vec![machine_validation_adapters::ReadOnlyMount {
                    source: source.into(),
                    target: target.into(),
                }],
                ..Default::default()
            };
            assert!(
                validate_invocation(
                    &invocation,
                    RuntimeRequirements {
                        run_as_root: true,
                        ..Default::default()
                    },
                    PluginPrivilege::Isolated,
                    Path::new(DEFAULT_PLUGIN_CONTRACT_DIR)
                )
                .unwrap_err()
                .contains(expected)
            );
        }
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
            RuntimeRequirements::default(),
            &AdapterInvocation {
                arguments: spec(PluginPrivilege::Isolated).entrypoint,
                ..Default::default()
            },
        );

        assert!(
            args.iter()
                .any(|arg| arg.contains("dst=/var/lib/nico/plugin-contract/input"))
        );
        assert!(
            args.iter()
                .any(|arg| arg.contains("dst=/var/lib/nico/plugin-contract/output"))
        );
        assert!(args.windows(2).any(|pair| {
            pair == [
                "--env",
                "NICO_MV_CONTRACT_DIR=/var/lib/nico/plugin-contract",
            ]
        }));
    }

    #[test]
    fn plugin_contract_directory_must_be_absolute_and_normalized() {
        assert!(validate_contract_dir(Path::new("/var/lib/nico/plugin-contract")).is_ok());
        assert!(validate_contract_dir(Path::new("relative/plugin-contract")).is_err());
        assert!(validate_contract_dir(Path::new("/opt/nico/../plugin-contract")).is_err());
        assert!(validate_contract_dir(Path::new("/opt/nico,mount-options")).is_err());
    }

    #[test]
    fn privileged_plugin_has_no_host_root_mount() {
        let args = plugin_runtime_args(
            Path::new("/tmp/input"),
            Path::new("/tmp/output"),
            "plugin-test",
            &spec(PluginPrivilege::Privileged),
            Path::new(DEFAULT_PLUGIN_CONTRACT_DIR),
            RuntimeRequirements::default(),
            &AdapterInvocation {
                arguments: spec(PluginPrivilege::Isolated).entrypoint,
                ..Default::default()
            },
        );

        assert!(args.iter().any(|arg| arg == "--privileged"));
        assert!(!args.iter().any(|arg| arg == "--pid"));
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
            RuntimeRequirements::default(),
            &AdapterInvocation {
                arguments: spec(PluginPrivilege::Isolated).entrypoint,
                ..Default::default()
            },
        );

        assert!(args.iter().any(|arg| arg == "--privileged"));
        assert!(args.windows(2).any(|pair| pair == ["--pid", "host"]));
        assert!(args.iter().any(|arg| arg == "type=bind,src=/,dst=/host"));
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

    #[tokio::test]
    async fn cancelled_execution_cleans_files_only_after_the_container_is_gone() {
        for launched in [false, true] {
            let directory = std::env::temp_dir().join(format!("plugin-cancel-{}", Uuid::new_v4()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("input.json"), b"{}").unwrap();
            let removed_containers = Arc::new(Mutex::new(Vec::new()));
            let scheduler_containers = Arc::clone(&removed_containers);
            let attempt_dir = directory.clone();
            let (ready, waiting) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let mut guard = PluginCleanupGuard::new(attempt_dir, move |name, path| {
                    assert!(path.join("input.json").exists());
                    scheduler_containers.lock().unwrap().push((name, path));
                });
                if launched {
                    guard.container_name = Some("mv-plugin-example".to_owned());
                }
                ready.send(()).unwrap();
                std::future::pending::<()>().await;
                drop(guard);
            });
            waiting.await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            let scheduled = removed_containers.lock().unwrap();
            if launched {
                assert_eq!(
                    *scheduled,
                    [("mv-plugin-example".to_owned(), directory.clone())]
                );
                assert!(
                    directory.exists(),
                    "preserve mounts until container cleanup succeeds"
                );
                std::fs::remove_dir_all(&directory).unwrap();
            } else {
                assert!(scheduled.is_empty());
                assert!(
                    !directory.exists(),
                    "preparation cancellation must remove input files"
                );
            }
        }
    }

    #[test]
    fn completed_execution_does_not_schedule_container_cleanup() {
        let removed_containers = Arc::new(Mutex::new(Vec::new()));
        let scheduler_containers = Arc::clone(&removed_containers);
        let mut cleanup_guard =
            PluginCleanupGuard::new("/unused-attempt".into(), move |container_name, _| {
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
    async fn launch_authorization_fails_closed_and_respects_the_attempt_deadline() {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        assert!(
            authorize_before_launch(deadline, async { Ok(()) })
                .await
                .is_ok()
        );
        assert_eq!(
            authorize_before_launch(deadline, async { Err("policy revoked".to_owned()) }).await,
            Err("policy revoked".to_owned())
        );
        assert_eq!(
            authorize_before_launch(tokio::time::Instant::now(), async {
                panic!("an expired attempt must not request launch authorization")
            })
            .await,
            Err("plugin timeout exhausted before launch".to_owned())
        );
        assert_eq!(
            authorize_before_launch(
                tokio::time::Instant::now() + std::time::Duration::from_millis(5),
                std::future::pending(),
            )
            .await,
            Err("plugin timeout exhausted authorizing launch".to_owned())
        );
    }

    #[tokio::test]
    async fn plugin_output_is_bounded_while_the_stream_is_drained() {
        let (mut writer, reader) = tokio::io::duplex(8192);
        let payload = vec![b'x'; MAX_OUTPUT_SIZE + 1];
        let writer = tokio::spawn(async move { writer.write_all(&payload).await });

        let output = read_limited(reader, PluginLogStream::Stdout, None)
            .await
            .expect("read plugin output");
        writer
            .await
            .expect("join writer")
            .expect("write plugin output");

        assert_eq!(output.bytes.len(), MAX_OUTPUT_SIZE);
        assert!(output.truncated);
        assert!(output_to_string(output).ends_with("[plugin output truncated at 1 MiB]"));
    }

    #[tokio::test]
    async fn log_chunks_are_utf8_bounded_and_do_not_wait_for_a_slow_consumer() {
        let (sender, mut receiver) = mpsc::channel(1);

        assert!(send_log_chunks(
            &sender,
            PluginLogStream::Stdout,
            "😀".as_bytes()
        ));
        assert!(!send_log_chunks(
            &sender,
            PluginLogStream::Stdout,
            b"later output"
        ));

        let chunk = receiver.recv().await.expect("first log chunk");
        assert_eq!(chunk.stream, PluginLogStream::Stdout);
        assert!(chunk.content.len() <= MAX_LOG_CHUNK_BYTES);
        assert!(chunk.content.is_char_boundary(chunk.content.len()));

        let (sender, mut receiver) = mpsc::channel(8);
        let content = "😀".repeat(MAX_LOG_CHUNK_BYTES);
        assert!(send_log_chunks(
            &sender,
            PluginLogStream::Stdout,
            content.as_bytes()
        ));
        while let Some(chunk) = receiver.recv().await {
            assert!(chunk.content.len() <= MAX_LOG_CHUNK_BYTES);
            assert!(chunk.content.is_char_boundary(chunk.content.len()));
            if receiver.is_empty() {
                break;
            }
        }
    }

    #[test]
    fn failed_plugin_output_is_shortened_for_result_errors() {
        let output = CapturedOutput {
            bytes: vec![b'x'; MAX_ERROR_OUTPUT_SIZE + 1],
            truncated: false,
            log_dropped: false,
        };

        let summary = output_error_summary(output);
        assert!(summary.contains("[plugin output shortened for the error summary]"));
        assert!(summary.len() > MAX_ERROR_OUTPUT_SIZE);
        assert!(summary.len() < MAX_ERROR_OUTPUT_SIZE + 128);
    }
}
