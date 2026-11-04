// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Exercise launcher placement through real processes without firmware tools or a service manager.

#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use libmlx::firmware::operation::{
    FirmwareOperationState, ObserverExitBehavior, SupervisorConfig, SupervisorLaunchError,
    connect_or_launch,
};
use nix::errno::Errno;
use nix::sys::prctl::set_child_subreaper;
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tokio::time::{sleep, timeout};
use wait_timeout::ChildExt;

const SUPERVISOR: &str = env!("CARGO_BIN_EXE_mlx-firmware-supervisor");
const DEVICE: &str = "ffff:fe:1f.0";
const SLOT: &str = "ffff:fe:1f";
const WATCHDOG: Duration = Duration::from_secs(10);
const INITIATOR_TIMEOUT: Duration = Duration::from_secs(25);
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Reap fixture subprocesses even when their assertions or readiness checks fail.
struct TestProcess(Child);

impl Drop for TestProcess {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(error) = self.0.kill() {
            eprintln!("failed to stop launcher fixture process: {error}");
        }
        match self.0.wait_timeout(WATCHDOG) {
            Ok(Some(_)) => {}
            result => eprintln!("failed to reap launcher fixture process: {result:?}"),
        }
    }
}

/// Only the isolated subreaper owns these PIDs, including helpers orphaned by failed assertions.
struct Daemons {
    fixture: PathBuf,
    reaped: Vec<Pid>,
}

impl Daemons {
    /// The exec wrapper records each helper before readiness so failures remain cleanable.
    fn pids(&self) -> io::Result<Vec<Pid>> {
        fs::read_to_string(self.fixture.join("daemon-pids"))?
            .lines()
            .map(|pid| pid.parse().map(Pid::from_raw).map_err(io::Error::other))
            .collect()
    }

    /// Kill only an owned child and reap it within a bound; no test submits hardware work.
    fn stop(&mut self, pid: Pid) -> io::Result<()> {
        if self.reaped.contains(&pid) {
            return Ok(());
        }
        let deadline = Instant::now() + WATCHDOG;
        let mut signaled = false;
        loop {
            match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) | Err(Errno::ECHILD) => {
                    self.reaped.push(pid);
                    return Ok(());
                }
                Ok(WaitStatus::StillAlive) if !signaled => {
                    kill(pid, Signal::SIGKILL).map_err(io::Error::from)?;
                    signaled = true;
                }
                Ok(WaitStatus::StillAlive) | Err(Errno::EINTR) => {}
                result => {
                    return Err(io::Error::other(format!(
                        "unexpected child state: {result:?}"
                    )));
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "helper was not reaped",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Daemons {
    fn drop(&mut self) {
        let pids = match self.pids() {
            Ok(pids) => pids,
            Err(error) => {
                eprintln!("failed to read launcher fixture helpers during cleanup: {error}");
                return;
            }
        };
        for pid in pids {
            if let Err(error) = self.stop(pid) {
                eprintln!("failed to reap launcher fixture helper {pid}: {error}");
            }
        }
    }
}

/// Fixture roots obey the production ownership checks without changing their configured parent.
fn private_fixture() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    builder.prefix("mlx-launch-");
    let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
        Some(parent) => builder.tempdir_in(parent),
        None => builder.tempdir(),
    }
    .expect("create private launcher fixture");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
        .expect("restrict fixture root");
    fs::create_dir(directory.path().join("tools")).expect("create isolated executable directory");
    fs::write(directory.path().join("daemon-pids"), "").unwrap();
    fs::write(directory.path().join("retained-scopes"), "").unwrap();
    directory
}

/// Shell builtins record placement and exec the real daemon without exposing host tools through PATH.
fn fake_tools(fixture: &Path) {
    for (name, script) in [
        (
            "helper",
            r#"#!/bin/sh
set -eu
printf '%s\n' "$$" >> "$MLX_LAUNCH_FIXTURE/daemon-pids"
if [ "$MLX_LAUNCH_CASE" = concurrent ]; then
    if /bin/mkdir "$MLX_LAUNCH_FIXTURE/winner" 2>/dev/null; then
        printf '%s' "$$" > "$MLX_LAUNCH_FIXTURE/winner-pid"
    else
        printf '%s' "$$" > "$MLX_LAUNCH_FIXTURE/loser-pid"
        exit 75
    fi
fi
if [ "$MLX_LAUNCH_CASE" = concurrent ] || [ "$MLX_LAUNCH_CASE" = readiness ]; then
    printf waiting > "$MLX_LAUNCH_FIXTURE/helper-waiting"
    i=0
    while [ ! -f "$MLX_LAUNCH_FIXTURE/release" ]; do
        [ "$i" -lt 3000 ] || exit 76
        i=$((i + 1))
        /bin/sleep 0.01
    done
fi
exec "$MLX_LAUNCH_SUPERVISOR" "$@"
"#,
        ),
        (
            "systemd-run",
            r#"#!/bin/sh
set -eu
printf '%s\n' "$@" >> "$MLX_LAUNCH_FIXTURE/manager-args"
if [ "$MLX_LAUNCH_CASE" = reject ]; then
    printf '%s' "$$" > "$MLX_LAUNCH_FIXTURE/rejected-pid"
    printf 'scope creation denied by fixture\n' >&2
    exit 73
fi
unit=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --unit=*) unit=${1#--unit=}; shift ;;
        --unit) unit=$2; shift 2 ;;
        --*) shift ;;
        *) break ;;
    esac
done
unit=${unit:-run-$$.scope}
while IFS= read -r retained; do
    if [ "$retained" = "$unit" ]; then
        printf 'scope name remains reserved\n' >&2
        exit 74
    fi
done < "$MLX_LAUNCH_FIXTURE/retained-scopes"
printf '%s\n' "$unit" >> "$MLX_LAUNCH_FIXTURE/retained-scopes"
exec "$@"
"#,
        ),
    ] {
        let executable = fixture.join("tools").join(name);
        fs::write(&executable, script).expect("write isolated fixture executable");
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700))
            .expect("make fixture executable private and runnable");
    }
}

/// Each environment and subreaper change is confined to one explicitly selected subprocess test.
fn run_test_process(fixture: &Path, case: &str, test: &str, deadline: Duration) {
    let log_path = fixture.join(format!("{test}.log"));
    let log = File::create(&log_path).expect("open fixture diagnostics");
    let mut process = TestProcess(
        Command::new(std::env::current_exe().expect("test executable"))
            .args(["--ignored", "--exact", test, "--nocapture"])
            .env_clear()
            .env("PATH", fixture.join("tools"))
            .env("MLX_LAUNCH_FIXTURE", fixture)
            .env("MLX_LAUNCH_CASE", case)
            .env("MLX_LAUNCH_SUPERVISOR", SUPERVISOR)
            .env("MLX_FIRMWARE_STATE_DIRECTORY", fixture.join("state"))
            .env(
                "MLX_FIRMWARE_SUPERVISOR_LAUNCH",
                if matches!(case, "scope" | "reject") {
                    "systemd-scope"
                } else {
                    "standalone"
                },
            )
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("start isolated launcher fixture"),
    );
    let status = process
        .0
        .wait_timeout(deadline)
        .expect("wait for isolated launcher fixture");
    assert!(
        status.is_some_and(|status| status.success()),
        "fixture {test} failed: {status:?}; {}; daemon: {}",
        fs::read_to_string(log_path).unwrap(),
        fs::read_to_string(fixture.join(format!("state/{SLOT}.supervisor.log")))
            .unwrap_or_default()
    );
}

/// The completion marker prevents a misspelled test filter from silently passing.
fn run_fixture(case: &str) {
    let directory = private_fixture();
    run_test_process(
        directory.path(),
        case,
        "launcher_fixture",
        TEST_TIMEOUT + WATCHDOG * 2,
    );
    assert!(directory.path().join("checked").exists());
}

/// Production environment parsing selects placement; only the helper path is fixture-specific.
fn fixture_config(fixture: &Path) -> SupervisorConfig {
    let mut config = SupervisorConfig::from_env().expect("parse isolated launch environment");
    config.supervisor_path = Some(fixture.join("tools/helper"));
    config
}

/// Dropping the initiating process must leave a ready daemon that subsequent callers can reuse.
#[test]
fn standalone_helper_survives_its_initiating_process_and_reconnects() {
    run_fixture("standalone");
}

/// A reserved prior scope name must not prevent relaunch after the daemon disappears.
#[test]
fn systemd_scope_uses_required_arguments_and_relaunches_with_retained_scopes() {
    run_fixture("scope");
}

/// A manager rejection must remain visible without changing the helper's placement.
#[test]
fn systemd_scope_failure_never_falls_back_to_standalone() {
    run_fixture("reject");
}

/// An exited losing launcher still waits for the other helper's readiness.
#[test]
fn concurrent_launchers_wait_for_the_delayed_winner() {
    run_fixture("concurrent");
}

/// Readiness timeout drops the caller's child handle without stopping the helper.
#[test]
fn readiness_timeout_leaves_the_helper_available_for_reconnection() {
    run_fixture("readiness");
}

/// This process adopts detached helpers so the test never relies on the host's PID 1 reaper.
#[test]
#[ignore = "invoked only by the launcher integration tests"]
fn launcher_fixture() {
    set_child_subreaper(true).expect("adopt this isolated fixture's detached helpers");
    let fixture = PathBuf::from(std::env::var_os("MLX_LAUNCH_FIXTURE").unwrap());
    let case = std::env::var("MLX_LAUNCH_CASE").unwrap();
    fake_tools(&fixture);
    let mut daemons = Daemons {
        fixture: fixture.clone(),
        reaped: Vec::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create launcher observer runtime");
    runtime.block_on(async {
        timeout(TEST_TIMEOUT, async {
            if case == "reject" {
                let config = fixture_config(&fixture);
                let launching =
                    tokio::spawn(async move { connect_or_launch(DEVICE, &config).await });
                timeout(WATCHDOG, async {
                    loop {
                        if let Ok(pid) = fs::read_to_string(fixture.join("rejected-pid"))
                            && !pid.is_empty()
                            && !Path::new("/proc").join(pid).exists()
                        {
                            break;
                        }
                        sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("rejected launcher exited and was reaped before readiness");
                // The child has exited and been reaped. Paused runtime time can
                // now reach the readiness deadline without racing that exit.
                tokio::time::pause();
                let result = launching.await;
                tokio::time::resume();
                let error = result
                    .expect("join rejected launch")
                    .expect_err("scope creation must fail");
                assert!(matches!(error, SupervisorLaunchError::Exited { status, .. }
                    if status.code() == Some(73)));
                assert!(
                    daemons.pids().unwrap().is_empty(),
                    "rejection must not invoke the helper"
                );
                assert!(!fixture.join("state").join(SLOT).exists());
                assert!(
                    fs::read_to_string(fixture.join(format!("state/{SLOT}.supervisor.log")))
                        .unwrap()
                        .contains("scope creation denied by fixture")
                );
            } else if matches!(case.as_str(), "concurrent" | "readiness") {
                let config = fixture_config(&fixture);
                let first = tokio::spawn(async move { connect_or_launch(DEVICE, &config).await });
                timeout(WATCHDOG, async {
                    while !fixture.join("helper-waiting").exists() {
                        sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("first helper reached its readiness gate");
                assert!(
                    !fixture
                        .join("state")
                        .join(SLOT)
                        .join("control.sock")
                        .exists()
                );

                if case == "concurrent" {
                    let config = fixture_config(&fixture);
                    let second =
                        tokio::spawn(async move { connect_or_launch(DEVICE, &config).await });
                    timeout(WATCHDOG, async {
                        loop {
                            if let Ok(pid) = fs::read_to_string(fixture.join("loser-pid"))
                                && !pid.is_empty()
                                && !Path::new("/proc").join(pid).exists()
                            {
                                break;
                            }
                            sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .expect("losing launcher exited and was reaped before readiness");
                    assert!(!first.is_finished());
                    assert!(
                        !second.is_finished(),
                        "losing caller must keep waiting for readiness"
                    );
                    fs::write(fixture.join("release"), "release").unwrap();
                    let mut first = first.await.unwrap().expect("winning caller connects");
                    let mut second = second.await.unwrap().expect("losing caller connects");
                    assert_eq!(
                        first.status().await.unwrap(),
                        second.status().await.unwrap()
                    );
                    assert_eq!(daemons.pids().unwrap().len(), 2);
                } else {
                    // The external helper stays at its real readiness gate.
                    // Let paused runtime time reach the readiness deadline,
                    // then resume before releasing the helper.
                    tokio::time::pause();
                    let result = first.await;
                    tokio::time::resume();
                    let error = result
                        .expect("join readiness observer")
                        .expect_err("helper remains gated past readiness deadline");
                    assert!(matches!(
                        error,
                        SupervisorLaunchError::ReadinessTimeout { .. }
                    ));
                    let pids = daemons.pids().unwrap();
                    assert_eq!(pids.len(), 1);
                    assert_eq!(
                        waitpid(pids[0], Some(WaitPidFlag::WNOHANG)).unwrap(),
                        WaitStatus::StillAlive
                    );
                    fs::write(fixture.join("release"), "release").unwrap();
                    timeout(WATCHDOG, async {
                        while !fixture
                            .join("state")
                            .join(SLOT)
                            .join("control.sock")
                            .exists()
                        {
                            sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .expect("surviving helper binds after its caller times out");
                    let mut client = connect_or_launch(DEVICE, &fixture_config(&fixture))
                        .await
                        .expect("reconnect to the helper that outlived readiness timeout");
                    assert_eq!(client.status().await.unwrap().execution_device, DEVICE);
                    assert_eq!(
                        daemons.pids().unwrap(),
                        pids,
                        "reconnection must not launch a replacement"
                    );
                }
                assert!(!fixture.join("manager-args").exists());
            } else {
                let launches = if case == "scope" { 2 } else { 1 };
                for launch in 1..=launches {
                    run_test_process(
                        &fixture,
                        &case,
                        "launching_process_fixture",
                        INITIATOR_TIMEOUT,
                    );
                    assert!(fixture.join("initiator-checked").exists());
                    let pids = daemons.pids().expect("read helper process identities");
                    assert_eq!(pids.len(), launch);
                    let pid = *pids.last().unwrap();
                    assert_eq!(
                        waitpid(pid, Some(WaitPidFlag::WNOHANG)).unwrap(),
                        WaitStatus::StillAlive,
                        "helper must outlive its initiating process"
                    );
                    let mut client = connect_or_launch(DEVICE, &fixture_config(&fixture))
                        .await
                        .expect("reconnect to the existing helper after caller exit");
                    let status = client.status().await.unwrap();
                    assert_eq!(status.device_identity, SLOT);
                    assert_eq!(status.execution_device, DEVICE);
                    let receipt = client.get("launch-readiness").await.unwrap();
                    assert_eq!(receipt.state, FirmwareOperationState::Complete);
                    assert!(!receipt.report.flashed);
                    assert_eq!(
                        daemons.pids().unwrap(),
                        pids,
                        "reconnect must not launch another helper"
                    );
                    drop(client);
                    daemons.stop(pid).expect("stop and reap the no-work helper");
                }
                if case == "scope" {
                    let scopes = fs::read_to_string(fixture.join("retained-scopes")).unwrap();
                    let scopes: Vec<_> = scopes.lines().collect();
                    assert_eq!(scopes.len(), 2);
                    assert_ne!(scopes[0], scopes[1]);
                } else {
                    assert!(!fixture.join("manager-args").exists());
                }
            }
            if matches!(case.as_str(), "scope" | "reject") {
                let expected = [
                    "--system".to_string(),
                    "--scope".to_string(),
                    "--no-ask-password".to_string(),
                    "--quiet".to_string(),
                    "--slice=system.slice".to_string(),
                    fixture.join("tools/helper").display().to_string(),
                    "serve".to_string(),
                    "--device".to_string(),
                    DEVICE.to_string(),
                    "--state-directory".to_string(),
                    fixture.join("state").display().to_string(),
                ];
                let expected: Vec<_> = expected.iter().map(String::as_str).collect();
                let observed = fs::read_to_string(fixture.join("manager-args")).unwrap();
                let observed: Vec<_> = observed.lines().collect();
                assert_eq!(
                    observed,
                    expected.repeat(if case == "scope" { 2 } else { 1 })
                );
            }
            fs::write(fixture.join("checked"), "checked").unwrap();
        })
        .await
        .expect("launcher fixture exceeded its independent watchdog");
    });
}

/// Launch through the public API, prove readiness with no work, then exit the entire caller process.
#[test]
#[ignore = "invoked only by launcher_fixture"]
fn launching_process_fixture() {
    let fixture = PathBuf::from(std::env::var_os("MLX_LAUNCH_FIXTURE").unwrap());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create initiating process runtime")
        .block_on(async {
            let mut client = connect_or_launch(DEVICE, &fixture_config(&fixture))
                .await
                .expect("launch actual helper and complete its readiness handshake");
            client
                .submit("launch-readiness", None, ObserverExitBehavior::Continue)
                .await
                .expect("admit a no-work request through the launched helper");
            let receipt = timeout(WATCHDOG, async {
                loop {
                    let receipt = client.get("launch-readiness").await.unwrap();
                    if receipt.state == FirmwareOperationState::Complete {
                        break receipt;
                    }
                    assert!(matches!(
                        receipt.state,
                        FirmwareOperationState::Accepted | FirmwareOperationState::Running
                    ));
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("no-work request completed within watchdog");
            assert!(!receipt.report.flashed);
        });
    fs::write(fixture.join("initiator-checked"), "checked").unwrap();
}
