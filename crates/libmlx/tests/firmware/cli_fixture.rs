// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES.
// SPDX-License-Identifier: Apache-2.0

//! Owned fixture processes for public firmware CLI tests; hardware tools are
//! replaced by executables in a private PATH before any process starts.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use wait_timeout::ChildExt;

pub(super) const DEVICE: &str = "0000:ab:01.0";
pub(super) const OPERATION_ID: &str = "fixture-operation";
pub(super) const WATCHDOG: Duration = Duration::from_secs(10);

/// Use a private short directory when the default temporary path exceeds the
/// Unix-socket limit or has ancestors the supervisor cannot trust.
pub(super) fn fixture_directory() -> tempfile::TempDir {
    let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
        Some(directory) => tempfile::tempdir_in(directory).unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

/// Keep writable executable descriptors out of the concurrent test process so
/// another test's fork cannot inherit one and cause ETXTBSY during execution.
pub(super) fn write_executable(path: &Path, contents: &str) {
    let mut writer = Command::new("/bin/sh")
        .args([
            "-c",
            "umask 077; printf '%s' \"$2\" > \"$1\"",
            "fixture-writer",
        ])
        .arg(path)
        .arg(contents)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start isolated fixture writer");
    let status = match writer
        .wait_timeout(WATCHDOG)
        .expect("wait for fixture writer")
    {
        Some(status) => status,
        None => {
            writer.kill().expect("stop timed-out fixture writer");
            writer
                .wait_timeout(WATCHDOG)
                .expect("wait for stopped fixture writer")
                .expect("reap stopped fixture writer within watchdog");
            panic!("fixture writer exceeded its watchdog");
        }
    };
    assert!(status.success(), "fixture writer failed: {status}");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("make fake tool executable");
}

/// Build the public CLI with isolated tool lookup and a shared private journal.
pub(super) fn cli(fixture: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mlxconfig-embedded"));
    command
        .env_clear()
        .env("PATH", fixture.join("tools"))
        .env("FIXTURE", fixture)
        .env("DEVICE", DEVICE)
        .env("FAIL_PHASE", "")
        .env("MLX_FIRMWARE_STATE_DIRECTORY", fixture.join("state"))
        .args(["--log-level", "error", "firmware"]);
    command
}

/// Keep the real service as a child of the test so cleanup never signals a PID
/// obtained from a stale file. The CLI connects to it through the production UDS.
pub(super) struct Supervisor {
    child: Child,
    fixture: PathBuf,
}

impl Supervisor {
    pub(super) fn start(fixture: &Path, failed_phase: &str) -> Self {
        let stderr = fs::File::create(fixture.join("supervisor.stderr")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_mlx-firmware-supervisor"))
            .current_dir(fixture)
            .env_clear()
            .env("PATH", fixture.join("tools"))
            .env("FIXTURE", fixture)
            .env("DEVICE", DEVICE)
            .env("FAIL_PHASE", failed_phase)
            .args(["serve", "--device", DEVICE, "--state-directory"])
            .arg(fixture.join("state"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("start fixture supervisor");
        let supervisor = Self {
            child,
            fixture: fixture.to_path_buf(),
        };
        let socket = fixture.join("state/0000:ab:01/control.sock");
        let deadline = Instant::now() + WATCHDOG;
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "supervisor socket did not appear: {}",
                fs::read_to_string(fixture.join("supervisor.stderr")).unwrap()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        supervisor
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // A failed assertion can leave the fake burn waiting at its gate.
        // Release it before asking the helper to drain accepted work and exit.
        if let Err(error) = fs::write(self.fixture.join("release-burn"), b"release") {
            eprintln!("failed to release fixture burn gate: {error}");
        }
        if let Ok(None) = self.child.try_wait() {
            let pid = Pid::from_raw(i32::try_from(self.child.id()).expect("fixture PID fits i32"));
            if let Err(error) = kill(pid, Signal::SIGTERM) {
                eprintln!("failed to stop fixture supervisor: {error}");
            }
            if !matches!(self.child.wait_timeout(WATCHDOG), Ok(Some(_))) {
                if let Err(error) = self.child.kill() {
                    eprintln!("failed to kill fixture supervisor: {error}");
                }
                if let Err(error) = self.child.wait() {
                    eprintln!("failed to reap fixture supervisor: {error}");
                }
            }
        }
    }
}

/// Bound each public CLI invocation independently of firmware operation timers.
/// Output files prevent inherited pipes from making collection wait forever.
pub(super) fn run_cli(command: &mut Command) -> Output {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = command
        .stdout(Stdio::from(stdout.try_clone().unwrap()))
        .stderr(Stdio::from(stderr.try_clone().unwrap()))
        .spawn()
        .unwrap();
    let status = match child.wait_timeout(WATCHDOG).unwrap() {
        Some(status) => status,
        None => {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("firmware CLI fixture exceeded its independent watchdog");
        }
    };
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.seek(SeekFrom::Start(0)).unwrap();
    stderr.seek(SeekFrom::Start(0)).unwrap();
    stdout.read_to_end(&mut output.stdout).unwrap();
    stderr.read_to_end(&mut output.stderr).unwrap();
    output
}
