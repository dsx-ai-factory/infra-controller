/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use libmlx::firmware::operation::{FirmwareOperation, FirmwareOperationState};
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid, getpgid};
use wait_timeout::ChildExt;

use super::cli_fixture::{
    DEVICE, OPERATION_ID, Supervisor, WATCHDOG, cli, fixture_directory, run_cli, write_executable,
};

const TOOL: &str = r#"#!/bin/sh
set -eu
if [ "${1-}" = --version ]; then exit 0; fi
case "${0##*/}" in
    mlxfwmanager) phase=query ;;
    mlxconfig) phase=apply; path=$5 ;;
    flint)
        for phase in "$@"; do :; done
        if [ "$phase" = burn ]; then path=$5; else path=$4; fi ;;
    mlxfwreset) phase=reset ;;
    *) exit 40 ;;
esac
printf '%s\n' "$phase" >> "$FIXTURE/calls"
case "$phase" in
    query)
        printf '<Devices><Device pciName="%s" type="BlueField3" partNumber="part" psid="PSID"><Versions><FW current="32.42.1000"/></Versions></Device></Devices>' "$DEVICE" ;;
    apply)
        IFS= read -r contents < "$path"
        [ "$contents" = device-config ] ;;
    burn)
        IFS= read -r contents < "$path"
        [ "$contents" = firmware-image ]
        printf '%s\n' "$path" > "$FIXTURE/burn-image"
        printf '%s\n' "$$" > "$FIXTURE/burn.pid"
        (
            count=0
            while [ ! -f "$FIXTURE/release-burn" ] && [ "$count" -lt 1500 ]; do
                /bin/sleep 0.02
                count=$((count + 1))
            done
            [ -f "$FIXTURE/release-burn" ]
        ) &
        printf '%s\n' "$!" > "$FIXTURE/descendant.pid"
        printf 'ready\n' > "$FIXTURE/burn-started"
        wait ;;
    reset|verify) exit 41 ;;
    *) exit 42 ;;
esac
"#;

/// Own the CLI child while assertions can fail; stopping it must not be relied
/// on to clean up the independently owned supervisor or firmware processes.
struct RunningCli(Child);

impl Drop for RunningCli {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            if let Err(error) = self.0.kill() {
                eprintln!("failed to stop fixture CLI: {error}");
            }
            if let Err(error) = self.0.wait() {
                eprintln!("failed to reap fixture CLI: {error}");
            }
        }
    }
}

/// Read only fixture PIDs for liveness assertions. Cleanup uses owned child
/// handles rather than signaling PIDs whose original owners could have exited.
fn read_pid(fixture: &Path, name: &str) -> Pid {
    let value = fs::read_to_string(fixture.join(name)).unwrap();
    let pid: i32 = value.trim().parse().unwrap();
    assert!(pid > 0);
    Pid::from_raw(pid)
}

/// A CLI signal stops waiting, not burning. A new CLI can inspect the same work
/// and explicitly request cancellation, which takes effect after the burn.
#[test]
fn cli_exit_preserves_burn_and_cancel_stops_before_activation() {
    let directory = fixture_directory();
    let fixture = directory.path();
    fs::create_dir(fixture.join("tools")).unwrap();
    for tool in ["mlxconfig", "flint", "mlxfwreset", "mlxfwmanager"] {
        write_executable(&fixture.join("tools").join(tool), TOOL);
    }
    let image = fixture.join("image.bin");
    let configuration = fixture.join("device.conf");
    fs::write(&image, "firmware-image\n").unwrap();
    fs::write(&configuration, "device-config\n").unwrap();
    let profile = fixture.join("profile.toml");
    fs::write(
        &profile,
        format!(
            "part_number = 'part'\npsid = 'PSID'\nversion = '32.43.1014'\n\
             firmware_url = '{}'\ndevice_conf_url = '{}'\n\
             reset = true\nverify_image = true\nverify_version = true\n",
            image.display(),
            configuration.display(),
        ),
    )
    .unwrap();

    let _supervisor = Supervisor::start(fixture, "");
    let stdout = fs::File::create(fixture.join("stdout")).unwrap();
    let stderr = fs::File::create(fixture.join("stderr")).unwrap();
    let child = cli(fixture)
        .args(["flash-config", DEVICE])
        .arg(profile)
        .args(["--operation-id", OPERATION_ID])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();
    let mut cli = RunningCli(child);
    let deadline = Instant::now() + WATCHDOG;
    while !fixture.join("burn-started").exists() {
        assert!(
            Instant::now() < deadline,
            "burn did not start: {}",
            fs::read_to_string(fixture.join("stderr")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let burn = read_pid(fixture, "burn.pid");
    let descendant = read_pid(fixture, "descendant.pid");
    let cli_pid = Pid::from_raw(i32::try_from(cli.0.id()).unwrap());
    assert_ne!(getpgid(Some(burn)).unwrap(), cli_pid);
    assert_eq!(
        getpgid(Some(burn)).unwrap(),
        getpgid(Some(descendant)).unwrap()
    );

    // Signal only the CLI. The real daemon and both gated tool processes must
    // remain alive, before their 30-second fallback could finish naturally.
    let interrupted_at = Instant::now();
    kill(cli_pid, Signal::SIGTERM).unwrap();
    let status = cli
        .0
        .wait_timeout(WATCHDOG)
        .unwrap()
        .expect("CLI signal deadline");
    let diagnostic = fs::read_to_string(fixture.join("stderr")).unwrap();
    assert_eq!(status.code(), Some(1), "{diagnostic}");
    assert!(
        diagnostic.contains("accepted work continues"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains(OPERATION_ID), "{diagnostic}");
    for pid in [burn, descendant] {
        assert!(PathBuf::from(format!("/proc/{pid}")).exists());
    }

    let receipt = |action: &str| {
        let output = run_cli(super::cli_fixture::cli(fixture).args([action, DEVICE, OPERATION_ID]));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<FirmwareOperation>(&output.stdout).unwrap()
    };
    let running = receipt("status");
    assert_eq!(running.operation_id, OPERATION_ID);
    assert_eq!(running.state, FirmwareOperationState::Running);
    let requested = receipt("cancel");
    assert_eq!(requested.state, FirmwareOperationState::CancelRequested);
    for pid in [burn, descendant] {
        assert!(
            PathBuf::from(format!("/proc/{pid}")).exists(),
            "cancel killed an active burn"
        );
    }

    // Allow the fake burn to finish. Cancellation must retain that success but
    // stop before activation, rather than killing flint or reporting Complete.
    fs::write(fixture.join("release-burn"), b"release").unwrap();
    let deadline = Instant::now() + WATCHDOG;
    let canceled = loop {
        let operation = receipt("status");
        if operation.state == FirmwareOperationState::Canceled {
            break operation;
        }
        assert!(
            Instant::now() < deadline,
            "cancel did not reach a safe boundary"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(canceled.report.flashed);
    assert_eq!(canceled.report.reset, Some(false));
    assert_eq!(
        fs::read_to_string(fixture.join("calls")).unwrap(),
        "query\napply\nburn\n"
    );
    for pid in [burn, descendant] {
        assert!(
            !PathBuf::from(format!("/proc/{pid}")).exists(),
            "tool PID was not reaped"
        );
    }
    assert!(interrupted_at.elapsed() < Duration::from_secs(20));
    let staged = fs::read_to_string(fixture.join("burn-image")).unwrap();
    assert_eq!(fs::read(staged.trim()).unwrap(), b"firmware-image\n");
}
