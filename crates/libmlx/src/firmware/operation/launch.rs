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

//! Explicit supervisor placement with a bounded, read-only readiness handshake.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::str::FromStr;
use std::time::Duration;
use std::{fmt, io};

use nix::libc;
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep_until, timeout_at};

use super::client::{FirmwareSupervisorClient, FirmwareSupervisorClientError};
use super::identity::{DeviceIdentity, current_boot_id};

const DEFAULT_STATE_DIRECTORY: &str = "/run/forge/mlx-firmware";
const READINESS_TIMEOUT: Duration = Duration::from_secs(10);
const READINESS_POLL: Duration = Duration::from_millis(50);
const SOCKET_PATH_LIMIT: usize = 107;

/// Selects the helper's process placement; no mode probes or falls back to another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SupervisorLaunchMode {
    /// Start a new session without a service-manager dependency (the default).
    /// An enclosing service's cgroup termination can still kill this helper.
    #[default]
    Standalone,
    /// Start in a fresh systemd system scope outside Scout's service cgroup.
    /// Requires permission to create system scopes; failure never falls back.
    SystemdScope,
}

impl fmt::Display for SupervisorLaunchMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Standalone => "standalone",
            Self::SystemdScope => "systemd-scope",
        })
    }
}

impl FromStr for SupervisorLaunchMode {
    type Err = SupervisorLaunchError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "standalone" => Ok(Self::Standalone),
            "systemd-scope" => Ok(Self::SystemdScope),
            _ => Err(SupervisorLaunchError::InvalidConfiguration(
                "supervisor launch mode must be standalone or systemd-scope",
            )),
        }
    }
}

/// Configuration shared by all callers that address the same local supervisors.
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    /// Absolute shared journal root, defaulting to `/run/forge/mlx-firmware`.
    /// It must be owned by the effective user with mode 0700 and no symlink
    /// components. Ancestors must belong to root or that user; shared writable
    /// ancestors must use the sticky bit. The resulting socket path must
    /// contain at most 107 bytes. The default `/run` root and other tmpfs roots
    /// can disappear on reboot. Persistent journals from an earlier boot remain
    /// readable, but reject mutation.
    pub state_directory: PathBuf,
    /// Absolute matching helper executable, or `None` for the
    /// `mlx-firmware-supervisor` sibling of the calling executable.
    pub supervisor_path: Option<PathBuf>,
    /// Explicit process placement; omission defaults to standalone.
    pub launch_mode: SupervisorLaunchMode,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            state_directory: DEFAULT_STATE_DIRECTORY.into(),
            supervisor_path: None,
            launch_mode: SupervisorLaunchMode::default(),
        }
    }
}

impl SupervisorConfig {
    /// Read environment configuration once, without probing a service manager.
    ///
    /// `MLX_FIRMWARE_STATE_DIRECTORY` overrides the default absolute journal
    /// root. `MLX_FIRMWARE_SUPERVISOR_LAUNCH` accepts exactly `standalone` or
    /// `systemd-scope`, defaulting to `standalone` when absent. Explicitly empty
    /// values are invalid. Programmatic fields can override these values before
    /// connecting; a running supervisor retains its existing placement.
    pub fn from_env() -> Result<Self, SupervisorLaunchError> {
        let state_directory = std::env::var_os("MLX_FIRMWARE_STATE_DIRECTORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| DEFAULT_STATE_DIRECTORY.into());
        validate_absolute_path(&state_directory)
            .map_err(|source| io_error("validate state directory", &state_directory, source))?;
        let launch_mode = match std::env::var("MLX_FIRMWARE_SUPERVISOR_LAUNCH") {
            Ok(value) => value.parse()?,
            Err(std::env::VarError::NotPresent) => SupervisorLaunchMode::Standalone,
            Err(source) => return Err(SupervisorLaunchError::Environment(source)),
        };
        Ok(Self {
            state_directory,
            supervisor_path: None,
            launch_mode,
        })
    }
}

/// Launch or identity failures; none of these errors authorizes firmware replay.
#[derive(Debug, Error)]
pub enum SupervisorLaunchError {
    /// A configured value violates its documented format or bound.
    #[error("invalid firmware supervisor configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// The launch-mode environment value is not valid Unicode.
    #[error("invalid MLX_FIRMWARE_SUPERVISOR_LAUNCH environment value: {0}")]
    Environment(#[source] std::env::VarError),
    /// The device is not a supported PCI address.
    #[error("invalid firmware supervisor device: {0}")]
    Device(#[source] io::Error),
    /// A local filesystem, boot-identity, or process operation failed.
    #[error("failed to {operation} at {path}: {source}")]
    Io {
        /// Local operation that failed.
        operation: &'static str,
        /// Path associated with the operation.
        path: PathBuf,
        /// Original operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A reachable service rejected the protocol or status request.
    #[error("firmware supervisor readiness failed: {0}")]
    Client(#[from] FirmwareSupervisorClientError),
    /// A reachable service identifies a different PCI slot, function, or boot.
    #[error("firmware supervisor {field} mismatch: expected {expected}, observed {observed}")]
    IdentityMismatch {
        /// Identity dimension that disagreed.
        field: &'static str,
        /// Identity selected by the caller.
        expected: String,
        /// Identity reported by the service.
        observed: String,
    },
    /// The launched process exited without a matching service becoming ready.
    #[error("firmware supervisor launcher exited with {status}; diagnostics: {log_path}")]
    Exited {
        /// Reaped launcher exit status.
        status: ExitStatus,
        /// Private file containing launcher and helper stderr.
        log_path: PathBuf,
    },
    /// Readiness exceeded ten seconds; a launched helper is left running.
    #[error("firmware supervisor readiness exceeded ten seconds at {socket}")]
    ReadinessTimeout {
        /// Unix socket that did not become ready.
        socket: PathBuf,
        /// Most recent connection failure, when a socket was present.
        #[source]
        last_error: Option<Box<FirmwareSupervisorClientError>>,
    },
}

/// Connect to the selected PCI-slot supervisor, starting it when unavailable.
///
/// `device` must be a `dddd:bb:ss.f` PCI address, or `bb:ss.f` for domain zero.
/// Functions of one slot share a socket but must match its execution function.
/// A slot does not establish a physical-card/reset domain.
/// Readiness has one ten-second deadline covering connection/status attempts
/// and 50-millisecond polling. This function never submits firmware work.
///
/// Cancellation or timeout leaves a launched helper alive. Its `Child` handle
/// uses Tokio's built-in orphan reaping after drop; this is best effort while
/// the runtime lives, and process exit transfers reaping to the operating system.
/// No detached Rust task owns the helper, and no service-manager failure causes
/// an implicit standalone launch.
pub async fn connect_or_launch(
    device: &str,
    config: &SupervisorConfig,
) -> Result<FirmwareSupervisorClient, SupervisorLaunchError> {
    let deadline = Instant::now() + READINESS_TIMEOUT;
    if let Some(helper) = &config.supervisor_path {
        validate_absolute_path(helper)
            .map_err(|source| io_error("validate supervisor executable", helper, source))?;
    }
    let identity = DeviceIdentity::parse(device).map_err(SupervisorLaunchError::Device)?;
    let directory = config.state_directory.join(&identity.key);
    let socket = directory.join("control.sock");
    if socket.as_os_str().as_bytes().len() > SOCKET_PATH_LIMIT {
        return Err(SupervisorLaunchError::InvalidConfiguration(
            "firmware supervisor socket path exceeds 107 bytes",
        ));
    }
    ensure_state_directory(&config.state_directory)
        .map_err(|source| io_error("prepare state directory", &config.state_directory, source))?;
    let boot_id = current_boot_id().map_err(|source| {
        io_error(
            "read boot identity",
            Path::new("/proc/sys/kernel/random/boot_id"),
            source,
        )
    })?;
    let log_path = config
        .state_directory
        .join(format!("{}.supervisor.log", identity.key));
    let mut child: Option<Child> = None;
    let mut launched = false;
    let mut exit_status = None;
    let mut last_error = None;
    loop {
        if Instant::now() >= deadline {
            return Err(match exit_status {
                Some(status) => SupervisorLaunchError::Exited { status, log_path },
                None => SupervisorLaunchError::ReadinessTimeout { socket, last_error },
            });
        }
        if socket_exists(&directory, &socket)
            .map_err(|source| io_error("inspect supervisor socket", &socket, source))?
        {
            match timeout_at(deadline, connect_checked(&socket, &identity, &boot_id)).await {
                Ok(Ok(client)) => return Ok(client),
                Ok(Err(SupervisorLaunchError::Client(
                    error @ FirmwareSupervisorClientError::Transport(_),
                ))) => {
                    last_error = Some(Box::new(error));
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    return Err(SupervisorLaunchError::ReadinessTimeout { socket, last_error });
                }
            }
        }
        if !launched {
            child = Some(launch(&identity, config, &log_path)?);
            launched = true;
        }
        if let Some(process) = child.as_mut()
            && let Some(status) = process
                .try_wait()
                .map_err(|source| io_error("reap supervisor launcher", &log_path, source))?
        {
            exit_status = Some(status);
            child = None;
        }
        // A losing concurrent launcher can exit before the winning helper has
        // bound its socket. Keep checking within the same readiness deadline.
        sleep_until(deadline.min(Instant::now() + READINESS_POLL)).await;
    }
}

/// Check for a host-wide unresolved mutation claim without creating local state.
///
/// An absent state root or marker returns `false`; an owned private marker
/// returns `true` even if its contents are incomplete. Existing state must meet
/// the same trust requirements as launch; unreadable or unsafe paths fail closed.
/// This observation neither acquires exclusion nor proves that starting work is
/// safe: another process can acquire a claim immediately after the check.
pub fn has_unresolved_firmware_claim(
    config: &SupervisorConfig,
) -> Result<bool, SupervisorLaunchError> {
    let root = &config.state_directory;
    if !state_directory_exists(root)
        .map_err(|source| io_error("inspect firmware state directory", root, source))?
    {
        return Ok(false);
    }
    let path = root.join("mutation.json");
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io_error("inspect firmware mutation claim", &path, source)),
    };
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect firmware mutation claim", &path, source))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io_error(
            "inspect firmware mutation claim",
            &path,
            unsafe_path(),
        ));
    }
    validate_metadata(&metadata, 0o600)
        .map_err(|source| io_error("inspect firmware mutation claim", &path, source))?;
    Ok(true)
}

/// Verify the endpoint belongs to the requested execution device and boot before use.
async fn connect_checked(
    socket: &Path,
    identity: &DeviceIdentity,
    boot_id: &str,
) -> Result<FirmwareSupervisorClient, SupervisorLaunchError> {
    let mut client = FirmwareSupervisorClient::connect(socket).await?;
    let status = client.status().await?;
    for (field, expected, observed) in [
        ("PCI slot", identity.key.as_str(), status.device_identity),
        (
            "execution device",
            identity.device.as_str(),
            status.execution_device,
        ),
        ("boot", boot_id, status.boot_id),
    ] {
        if expected != observed {
            return Err(SupervisorLaunchError::IdentityMismatch {
                field,
                expected: expected.to_string(),
                observed,
            });
        }
    }
    Ok(client)
}

/// Start one helper in the configured lifetime boundary without fallback placement.
fn launch(
    identity: &DeviceIdentity,
    config: &SupervisorConfig,
    log_path: &Path,
) -> Result<Child, SupervisorLaunchError> {
    let helper = match &config.supervisor_path {
        Some(path) => path.clone(),
        None => std::env::current_exe()
            .map_err(|source| {
                io_error(
                    "locate calling executable",
                    Path::new("/proc/self/exe"),
                    source,
                )
            })?
            .with_file_name("mlx-firmware-supervisor"),
    };
    let mut command = match config.launch_mode {
        SupervisorLaunchMode::Standalone => Command::new(&helper),
        SupervisorLaunchMode::SystemdScope => {
            let mut command = Command::new("systemd-run");
            command.args([
                "--system",
                "--scope",
                "--no-ask-password",
                "--quiet",
                "--slice=system.slice",
            ]);
            // A prior scope may still contain a continuing tool after its daemon
            // dies. Journal ownership permits a replacement reader without
            // terminating that tool; let systemd assign an independent scope.
            command.arg(&helper);
            command
        }
    };
    command
        .arg("serve")
        .arg("--device")
        .arg(&identity.device)
        .arg("--state-directory")
        .arg(&config.state_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(
            private_log(log_path)
                .map_err(|source| io_error("open supervisor log", log_path, source))?,
        )
        .kill_on_drop(false);
    // SAFETY: setsid is async-signal-safe, and this closure captures no state
    // or locks. The child has not become a process-group leader before it runs.
    unsafe {
        command.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(io::Error::from));
    }
    command
        .spawn()
        .map_err(|source| io_error("launch firmware supervisor", &helper, source))
}

/// Prepare only the shared root, leaving card-journal creation to its owner.
pub(super) fn ensure_state_directory(path: &Path) -> io::Result<()> {
    validate_absolute_path(path)?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                validate_ancestor(&metadata)?;
            }
            Ok(_) => return Err(unsafe_path()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match DirBuilder::new().mode(0o700).create(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                let metadata = fs::symlink_metadata(&current)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(unsafe_path());
                }
                validate_ancestor(&metadata)?;
            }
            Err(error) => return Err(error),
        }
    }
    validate_private_directory(path)
}

/// Inspect an existing root without turning an advisory check into state creation.
fn state_directory_exists(path: &Path) -> io::Result<bool> {
    validate_absolute_path(path)?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                validate_ancestor(&metadata)?;
            }
            Ok(_) => return Err(unsafe_path()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    validate_private_directory(path)?;
    Ok(true)
}

/// Reject paths whose meaning depends on a working directory or parent traversal.
fn validate_absolute_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be absolute, below the filesystem root, and contain no parent components",
        ));
    }
    Ok(())
}

/// Open the directory without following its final component before checking access.
fn validate_private_directory(path: &Path) -> io::Result<()> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    validate_metadata(&directory.metadata()?, 0o700)
}

/// Require the caller's ownership and exact permissions without repairing existing state.
fn validate_metadata(metadata: &fs::Metadata, mode: u32) -> io::Result<()> {
    let uid = nix::unistd::geteuid().as_raw();
    if metadata.uid() != uid || metadata.permissions().mode() & 0o7777 != mode {
        return Err(unsafe_path());
    }
    Ok(())
}

/// Reject ancestors that let another user replace the supervisor's ownership root.
fn validate_ancestor(metadata: &fs::Metadata) -> io::Result<()> {
    let uid = nix::unistd::geteuid().as_raw();
    let mode = metadata.permissions().mode();
    // An untrusted directory owner could rename the private state root and
    // replace it with a fresh journal while the first helper is still active.
    // Sticky shared roots such as /tmp protect entries owned by this user.
    if (metadata.uid() != 0 && metadata.uid() != uid) || (mode & 0o022 != 0 && mode & 0o1000 == 0) {
        return Err(unsafe_path());
    }
    Ok(())
}

/// Distinguish an absent endpoint from one that violates the journal's trust boundary.
fn socket_exists(directory: &Path, socket: &Path) -> io::Result<bool> {
    match validate_private_directory(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }
    match fs::symlink_metadata(socket) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(unsafe_path());
            }
            // The verified 0700 parent restricts access even during bind/chmod.
            // Socket ownership must still match the protected journal owner.
            if metadata.uid() != nix::unistd::geteuid().as_raw() {
                return Err(unsafe_path());
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Keep detached-helper diagnostics in an owned file without redirecting through links.
fn private_log(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(unsafe_path());
    }
    validate_metadata(&metadata, 0o600)?;
    Ok(file)
}

/// Report filesystem trust failures as permission errors, not missing state.
fn unsafe_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "supervisor state must use private, owned paths without symlinks or unexpected file types",
    )
}

/// Preserve the failed operation and path alongside the original operating-system error.
fn io_error(operation: &'static str, path: &Path, source: io::Error) -> SupervisorLaunchError {
    SupervisorLaunchError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    /// Trust-boundary tests need private ancestors even when the build's TMPDIR
    /// is shared; the test harness can supply a separate fixture-only root.
    fn fixture_directory() -> tempfile::TempDir {
        let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
            Some(root) => tempfile::tempdir_in(root),
            None => tempfile::tempdir(),
        }
        .expect("create private launch fixture");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("restrict newly created fixture directory");
        directory
    }

    /// Invalid placement must not silently select a different lifetime model.
    #[test]
    fn launch_modes_are_explicit() {
        scenarios!(run = |value| SupervisorLaunchMode::from_str(value).map_err(drop);
            "supported placement" {
                "standalone" => Yields(SupervisorLaunchMode::Standalone),
                "systemd-scope" => Yields(SupervisorLaunchMode::SystemdScope),
            }
            "no automatic mode or empty fallback" {
                "auto" => Fails,
                "" => Fails,
            }
        );
    }

    /// Launcher setup must leave initial card-journal creation to its owner.
    #[test]
    fn state_root_is_private_without_precreating_card_journals() {
        let temporary = fixture_directory();
        let root = temporary.path().join("nested/state");
        ensure_state_directory(&root).expect("create private root");
        ensure_state_directory(&root).expect("reuse existing private root");
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(fs::read_dir(&root).unwrap().next().is_none());
    }

    /// Shared or replaceable ancestors cannot create independent ownership roots.
    #[test]
    fn unsafe_state_roots_fail_without_repairing_permissions() {
        let temporary = fixture_directory();
        let exposed = temporary.path().join("exposed");
        fs::create_dir(&exposed).unwrap();
        fs::set_permissions(&exposed, fs::Permissions::from_mode(0o755)).unwrap();
        let link = temporary.path().join("link");
        symlink(&exposed, &link).unwrap();
        let file = temporary.path().join("file");
        fs::write(&file, "not a directory").unwrap();
        let shared = temporary.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();

        for path in [
            exposed.clone(),
            link.join("state"),
            shared.join("state"),
            file,
            PathBuf::from("relative"),
        ] {
            assert!(ensure_state_directory(&path).is_err(), "{}", path.display());
        }
        assert_eq!(
            fs::metadata(&exposed).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!exposed.join("state").exists());
        assert!(!shared.join("state").exists());
    }

    /// Diagnostic redirection must not write through an existing unsafe path.
    #[test]
    fn launcher_log_cannot_follow_links_or_write_shared_files() {
        let temporary = fixture_directory();
        let target = temporary.path().join("target");
        fs::write(&target, "retain this content").unwrap();
        let link = temporary.path().join("linked.log");
        symlink(&target, &link).unwrap();
        assert!(private_log(&link).is_err());
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let hardlink = temporary.path().join("hardlinked.log");
        fs::hard_link(&target, &hardlink).unwrap();
        assert!(private_log(&hardlink).is_err());
        fs::remove_file(&hardlink).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_log(&target).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "retain this content");
    }

    /// Socket validation must not replace an unrelated file or create an unusable state root.
    #[tokio::test]
    async fn invalid_socket_paths_fail_without_repairing_state() {
        let temporary = fixture_directory();
        for case in ["regular-file", "overlong-path"] {
            let root = temporary.path().join(if case == "overlong-path" {
                "x".repeat(SOCKET_PATH_LIMIT)
            } else {
                case.to_string()
            });
            let socket = root.join("0000:01:00/control.sock");
            if case == "regular-file" {
                ensure_state_directory(socket.parent().unwrap()).unwrap();
                fs::write(&socket, "unrelated file").unwrap();
            }
            let error = connect_or_launch(
                "0000:01:00.0",
                &SupervisorConfig {
                    state_directory: root.clone(),
                    supervisor_path: Some(temporary.path().join("absent-helper")),
                    launch_mode: SupervisorLaunchMode::Standalone,
                },
            )
            .await
            .expect_err("unsafe socket path must fail before launch");
            match case {
                "regular-file" => {
                    assert!(matches!(error, SupervisorLaunchError::Io { source, .. }
                        if source.kind() == io::ErrorKind::PermissionDenied));
                    assert!(fs::symlink_metadata(&socket).unwrap().is_file());
                    assert_eq!(fs::read_to_string(&socket).unwrap(), "unrelated file");
                    assert!(!root.join("0000:01:00.supervisor.log").exists());
                }
                "overlong-path" => {
                    assert!(matches!(
                        error,
                        SupervisorLaunchError::InvalidConfiguration(_)
                    ));
                    assert!(!root.exists());
                }
                _ => unreachable!(),
            }
        }
    }

    /// An advisory check must neither create missing state nor clear an incomplete claim.
    #[test]
    fn unresolved_claim_check_is_read_only_and_preserves_incomplete_markers() {
        let temporary = fixture_directory();
        let config = SupervisorConfig {
            state_directory: temporary.path().join("state"),
            ..SupervisorConfig::default()
        };
        assert!(!has_unresolved_firmware_claim(&config).unwrap());
        assert!(!config.state_directory.exists());

        ensure_state_directory(&config.state_directory).unwrap();
        assert!(!has_unresolved_firmware_claim(&config).unwrap());
        assert!(
            fs::read_dir(&config.state_directory)
                .unwrap()
                .next()
                .is_none()
        );

        let marker = config.state_directory.join("mutation.json");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&marker)
            .unwrap();
        assert!(has_unresolved_firmware_claim(&config).unwrap());
        assert_eq!(fs::read(marker).unwrap(), b"");
    }

    /// Unsafe roots and marker replacements must defer automatic work, not appear absent.
    #[test]
    fn unresolved_claim_check_rejects_unsafe_existing_paths() {
        let temporary = fixture_directory();
        for case in [
            "root-mode",
            "ancestor-link",
            "marker-mode",
            "marker-link",
            "marker-directory",
        ] {
            let root = temporary.path().join(case);
            ensure_state_directory(&root).unwrap();
            let marker = root.join("mutation.json");
            let state_directory = match case {
                "root-mode" => {
                    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
                    root
                }
                "ancestor-link" => {
                    let link = temporary.path().join("linked-root");
                    symlink(&root, &link).unwrap();
                    link.join("missing")
                }
                "marker-mode" => {
                    fs::write(&marker, "unreadable claim").unwrap();
                    fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap();
                    root
                }
                "marker-link" => {
                    symlink(root.join("missing"), &marker).unwrap();
                    root
                }
                "marker-directory" => {
                    fs::create_dir(&marker).unwrap();
                    root
                }
                _ => unreachable!(),
            };
            let config = SupervisorConfig {
                state_directory,
                ..SupervisorConfig::default()
            };
            assert!(has_unresolved_firmware_claim(&config).is_err(), "{case}");
        }
    }
}
