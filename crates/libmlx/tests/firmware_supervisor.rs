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

#![cfg(target_os = "linux")]

use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use libmlx::firmware::error::{AttemptError, AttemptFailure};
use libmlx::runner::FirmwareToolExecutor;
use libmlx::runner::command_builder::CommandSpec;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::unistd::{Pid, getpgid, getsid};
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use wait_timeout::ChildExt;

const WATCHDOG: Duration = Duration::from_secs(10);
// Cancellation must reap fixtures before their own deadline can stop them.
const CANCELLATION_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(60);
const SUPERVISOR: &str = env!("CARGO_BIN_EXE_mlx-firmware-supervisor");

/// Direct protocol tests must reap their helper even if an assertion fails.
struct SupervisorProcess(std::process::Child);

impl SupervisorProcess {
    fn spawn() -> Self {
        let process = Self(
            std::process::Command::new(SUPERVISOR)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start actual firmware supervisor"),
        );
        // A broken helper must not block the test while accepting a tiny
        // request or leave an inherited descriptor keeping a final read open.
        for descriptor in [
            process.0.stdin.as_ref().unwrap().as_fd(),
            process.0.stdout.as_ref().unwrap().as_fd(),
            process.0.stderr.as_ref().unwrap().as_fd(),
        ] {
            fcntl(descriptor, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
                .expect("make fixture I/O nonblocking");
        }
        process
    }

    fn expect_timeout(&mut self, watchdog: Duration) {
        let status = self
            .0
            .wait_timeout(watchdog)
            .expect("wait for supervisor timeout")
            .expect("supervisor exceeded its independent test watchdog");
        let mut stderr = String::new();
        self.0
            .stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut stderr)
            .expect("read exited supervisor diagnostics");
        assert_eq!(status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("TimedOut"), "{stderr}");
        assert!(stderr.contains("supervisor deadline expired"), "{stderr}");
    }
}

impl Drop for SupervisorProcess {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(error) = self.0.kill() {
            eprintln!("failed to kill fixture supervisor: {error}");
        }
        match self.0.wait_timeout(WATCHDOG) {
            Ok(Some(_)) => {}
            result => eprintln!("failed to reap fixture supervisor within watchdog: {result:?}"),
        }
    }
}

/// An open control pipe with only part of a frame must expire without EOF.
#[test]
fn stalled_request_exits_at_the_request_deadline() {
    let started = Instant::now();
    let mut helper = SupervisorProcess::spawn();
    // `wait_timeout` closes stdin stored on the child. Hold it separately so
    // only the helper's request deadline can end this incomplete dispatch.
    let mut control = helper.0.stdin.take().unwrap();
    control.write_all(&100_u32.to_be_bytes()).unwrap();
    control.write_all(b"{").unwrap();

    helper.expect_timeout(Duration::from_secs(40));
    assert!(started.elapsed() >= Duration::from_secs(30));
    drop(control);
    let mut stdout = Vec::new();
    helper
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_to_end(&mut stdout)
        .expect("read exited supervisor handshake");
    assert_eq!(stdout, b"MLX-SUPERVISOR-1\n");
}

/// An oversized frame fails from its length prefix without waiting for a body or EOF.
#[test]
fn oversized_worker_request_is_rejected_before_reading_its_body() {
    let mut helper = SupervisorProcess::spawn();
    let mut control = helper.0.stdin.take().unwrap();
    control
        .write_all(&(128_u32 * 1024 + 1).to_be_bytes())
        .unwrap();
    let status = helper
        .0
        .wait_timeout(WATCHDOG)
        .unwrap()
        .expect("oversized request fails before the request deadline");
    assert_eq!(status.code(), Some(1));
    let mut diagnostic = String::new();
    helper
        .0
        .stderr
        .as_mut()
        .unwrap()
        .read_to_string(&mut diagnostic)
        .unwrap();
    assert!(
        diagnostic.contains("supervisor request exceeds 128 KiB"),
        "{diagnostic}"
    );
    let mut response = Vec::new();
    helper
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_to_end(&mut response)
        .unwrap();
    assert_eq!(response, b"MLX-SUPERVISOR-1\n");
    drop(control);
}

/// A completed result must expire when the caller keeps stdout open but unread.
#[test]
fn blocked_result_delivery_exits_at_the_delivery_deadline() {
    let started = Instant::now();
    let mut helper = SupervisorProcess::spawn();
    let stdout = helper.0.stdout.as_ref().unwrap();
    fcntl(stdout, FcntlArg::F_SETPIPE_SZ(4096)).expect("reduce result pipe capacity");
    let capacity = usize::try_from(
        fcntl(stdout, FcntlArg::F_GETPIPE_SZ).expect("measure result pipe capacity"),
    )
    .unwrap();
    let output_len = capacity + 1;
    assert!(
        output_len <= 1024 * 1024,
        "fixture stays within output limit"
    );
    let request = serde_json::to_vec(&serde_json::json!({
        "program": "/bin/sh",
        "args": ["-c", format!("printf '%0{output_len}d' 0")],
        "timeout": Duration::from_secs(10),
    }))
    .unwrap();
    let mut control = helper.0.stdin.take().unwrap();
    control
        .write_all(&u32::try_from(request.len()).unwrap().to_be_bytes())
        .unwrap();
    control.write_all(&request).unwrap();

    // The handshake fits in the pipe; the successful result cannot. No read
    // may free space until the helper has exited on its delivery deadline.
    helper.expect_timeout(Duration::from_secs(15));
    assert!(started.elapsed() >= Duration::from_secs(5));
    drop(control);
    let mut stdout = Vec::new();
    helper
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_to_end(&mut stdout)
        .expect("read the result prefix after helper exit");
    let result = stdout
        .strip_prefix(b"MLX-SUPERVISOR-1\n")
        .expect("actual helper handshake");
    let length = u32::from_be_bytes(result[..4].try_into().unwrap()) as usize;
    assert!(
        length > capacity,
        "result must exceed measured pipe capacity"
    );
    assert!(result[4..].starts_with(b"{\"Finished\":"));
    assert!(result.len() - 4 < length, "delivery must remain incomplete");
}

/// The fake tool stays inside its test's temporary directory.
fn write_script(directory: &Path, name: &str, script: &str) -> PathBuf {
    let path = directory.join(name);
    // Another test can fork while a writable executable FD is open, inheriting
    // it until exec even with CLOEXEC. Keep that FD in a separate process so
    // concurrent spawns cannot inherit it and make this script fail with ETXTBSY.
    let mut writer = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "umask 077; printf '%s' \"$2\" > \"$1\"",
            "fixture-writer",
        ])
        .arg(&path)
        .arg(format!("#!/bin/sh\nset -eu\n{script}\n"))
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
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("make fake tool executable");
    path
}

/// Observe fixture handshakes without assuming tool startup time.
async fn wait_for_file(path: &Path) -> String {
    timeout(WATCHDOG, async {
        loop {
            match fs::read_to_string(path) {
                Ok(contents) if !contents.is_empty() => return contents,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("read fixture handshake {}: {error}", path.display()),
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fake tool published its handshake")
}

/// Require that a fixture PID is gone, not merely a zombie.
async fn wait_until_reaped(pid: &str) {
    let pid: u32 = pid.trim().parse().expect("fixture published a PID");
    let process = Path::new("/proc").join(pid.to_string());
    timeout(WATCHDOG, async {
        while process.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("supervisor reaped the fixture process");
}

/// A descendant can replace itself after its leader exits. Closed output pipes
/// must not let the attempt finish before the replacement releases its inputs.
#[tokio::test]
async fn replacement_descendant_finishes_before_success() {
    let directory = tempfile::tempdir().expect("create replacement fixture");
    let fixture = directory.path();
    let script = write_script(
        fixture,
        "replace.sh",
        r#"
fixture=$1
generation=$2
printf '%s' "$$" > "$fixture/pid-$generation"
case "$generation" in
    leader)
        /bin/sh "$0" "$fixture" first >/dev/null 2>&1 &
        exit 0 ;;
    first)
        gate=replace ;;
    replacement)
        gate=release ;;
esac
# The bound also stops the fixture if its test fails before opening the gate.
i=0
while [ ! -f "$fixture/$gate" ]; do
    [ -d "$fixture" ] && [ "$i" -lt 1000 ] || exit 77
    i=$((i + 1))
    sleep 0.01
done
if [ "$generation" = first ]; then
    /bin/sh "$0" "$fixture" replacement >/dev/null 2>&1 &
else
    printf finished > "$fixture/completed"
fi
"#,
    );
    let command = CommandSpec::new(script.to_str().expect("UTF-8 fixture path"))
        .arg(fixture.to_str().expect("UTF-8 fixture path"))
        .arg("leader");
    let executor = FirmwareToolExecutor::with_supervisor_path(SUPERVISOR);
    let execution = executor.execute(&command, WATCHDOG);
    tokio::pin!(execution);

    let replace = async {
        let leader = wait_for_file(&fixture.join("pid-leader")).await;
        let first = wait_for_file(&fixture.join("pid-first")).await;
        wait_until_reaped(&leader).await;
        fs::write(fixture.join("replace"), "replace").expect("release first descendant");
        let replacement = wait_for_file(&fixture.join("pid-replacement")).await;
        wait_until_reaped(&first).await;
        assert!(Path::new("/proc").join(replacement.trim()).exists());
    };
    tokio::select! {
        result = &mut execution => panic!("attempt finished before replacement release: {result:?}"),
        () = replace => {}
    }
    // Both earlier processes have been reaped and neither output pipe is open.
    // Only the adopted replacement keeps the attempt pending at this point.
    assert!(
        timeout(Duration::from_millis(100), &mut execution)
            .await
            .is_err()
    );
    fs::write(fixture.join("release"), "release").expect("release replacement descendant");
    let output = execution.await.expect("replacement finishes naturally");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(fixture.join("completed")).unwrap(),
        "finished"
    );
    wait_until_reaped(&wait_for_file(&fixture.join("pid-replacement")).await).await;
}

/// Leave a leader and child alive until the test releases them.
fn gated_command(fixture: &Path) -> CommandSpec {
    let script = write_script(
        fixture,
        "gated.sh",
        r#"
fixture=$1
role=$2
printf '%s' "$$" > "$fixture/$role"
if [ "$role" = leader ]; then
    /bin/sh "$0" "$fixture" descendant >/dev/null 2>&1 &
    wait
    exit 0
fi
# Natural fixture expiry must outlast the cancellation watchdog.
i=0
while [ ! -f "$fixture/release" ]; do
    [ -d "$fixture" ] && [ "$i" -lt 6000 ] || exit 77
    i=$((i + 1))
    sleep 0.01
done
printf finished > "$fixture/completed"
"#,
    );
    CommandSpec::new(script.to_str().expect("UTF-8 fixture path"))
        .arg(fixture.to_str().expect("UTF-8 fixture path"))
        .arg("leader")
}

/// Cancelling one caller must stop its descendants without reaping or signalling
/// a different in-flight attempt in the same embedding process.
#[tokio::test]
async fn cancellation_does_not_stop_another_attempt() {
    let cancelled = tempfile::tempdir().expect("create cancelled fixture");
    let surviving = tempfile::tempdir().expect("create surviving fixture");
    let cancelled_command = gated_command(cancelled.path());
    let surviving_command = gated_command(surviving.path());
    let executor = FirmwareToolExecutor::with_supervisor_path(SUPERVISOR);
    let cancelled_task = tokio::spawn({
        let executor = executor.clone();
        async move {
            executor
                .execute(&cancelled_command, CANCELLATION_ATTEMPT_TIMEOUT)
                .await
        }
    });
    let surviving_execution = executor.execute(&surviving_command, CANCELLATION_ATTEMPT_TIMEOUT);
    tokio::pin!(surviving_execution);
    let cancel = async {
        let cancelled_leader = wait_for_file(&cancelled.path().join("leader")).await;
        let cancelled_child = wait_for_file(&cancelled.path().join("descendant")).await;
        let surviving_child = wait_for_file(&surviving.path().join("descendant")).await;
        cancelled_task.abort();
        assert!(cancelled_task.await.unwrap_err().is_cancelled());
        wait_until_reaped(&cancelled_leader).await;
        wait_until_reaped(&cancelled_child).await;
        // This child belongs to a separate helper despite sharing our caller.
        assert!(Path::new("/proc").join(surviving_child.trim()).exists());
        assert!(!cancelled.path().join("completed").exists());
        fs::write(surviving.path().join("release"), "release").expect("release surviving attempt");
    };
    tokio::select! {
        result = &mut surviving_execution => panic!("other attempt ended before release: {result:?}"),
        () = cancel => {}
    }
    let output = surviving_execution
        .await
        .expect("other attempt finishes naturally");
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(surviving.path().join("completed")).unwrap(),
        "finished"
    );
}

/// Starting a new session does not release descendant ownership. Once the
/// leader exits, cancellation must still find and reap its detached child.
#[tokio::test]
async fn cancellation_reaps_a_descendant_in_another_session() {
    let directory = tempfile::tempdir().expect("create detached descendant fixture");
    let fixture = directory.path();
    let script = write_script(
        fixture,
        "detach.sh",
        r#"
fixture=$1
role=$2
printf '%s' "$$" > "$fixture/$role"
if [ "$role" = leader ]; then
    setsid /bin/sh "$0" "$fixture" detached >/dev/null 2>&1 &
    exit 0
fi
# Bound leaks while keeping natural expiry beyond the cancellation watchdog.
i=0
while [ -d "$fixture" ] && [ "$i" -lt 6000 ]; do
    i=$((i + 1))
    sleep 0.01
done
"#,
    );
    let command = CommandSpec::new(script.to_str().expect("UTF-8 fixture path"))
        .arg(fixture.to_str().expect("UTF-8 fixture path"))
        .arg("leader");
    let task = tokio::spawn(async move {
        FirmwareToolExecutor::with_supervisor_path(SUPERVISOR)
            .execute(&command, CANCELLATION_ATTEMPT_TIMEOUT)
            .await
    });
    let leader = wait_for_file(&fixture.join("leader")).await;
    let detached = wait_for_file(&fixture.join("detached")).await;
    wait_until_reaped(&leader).await;
    let detached_pid = Pid::from_raw(detached.trim().parse().expect("detached PID"));
    assert_eq!(getsid(Some(detached_pid)).unwrap(), detached_pid);
    assert_eq!(getpgid(Some(detached_pid)).unwrap(), detached_pid);
    let stat = fs::read_to_string(Path::new("/proc").join(detached.trim()).join("stat"))
        .expect("detached descendant remains present");
    assert!(
        !matches!(stat.rsplit_once(") ").unwrap().1.as_bytes()[0], b'Z' | b'X'),
        "detached descendant must still be executing before cancellation"
    );
    assert!(
        !task.is_finished(),
        "closed pipes cannot establish completion"
    );

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    wait_until_reaped(&detached).await;
}

/// A deadline must continue the partially consumed frame rather than restart
/// its length read and lose the helper's status and collected diagnostics.
#[tokio::test]
async fn partial_result_survives_the_attempt_deadline() {
    let directory = tempfile::tempdir().expect("create partial result fixture");
    let fixture = directory.path();
    let helper = SUPERVISOR.replace('\'', "'\\''");
    let fixture_path = fixture
        .to_str()
        .expect("UTF-8 fixture path")
        .replace('\'', "'\\''");
    let proxy = write_script(
        fixture,
        "partial-helper.sh",
        &format!(
            r#"
exec 3<&0
# A separate pipe lets the helper make its stdin nonblocking without changing
# the original control descriptor that the result relay uses to wait for EOF.
cat | '{helper}' | {{
    dd bs=1 count={} 2>/dev/null
    printf prefix > '{fixture_path}/prefix'
    # The real helper consumed the request before producing this result.
    # EOF now means the caller's deadline closed its control writer.
    cat <&3 >/dev/null
    printf eof > '{fixture_path}/control-eof'
    cat
}}
"#,
            b"MLX-SUPERVISOR-1\n".len() + 2,
        ),
    );
    let marker = fixture.join("launches");
    let command = CommandSpec::new("/bin/sh")
        .args([
            "-c",
            "printf launch >> \"$1\"; printf command-out; printf command-err >&2; exit 7",
            "fixture",
        ])
        .arg(marker.to_str().expect("UTF-8 fixture path"));
    let deadline = Duration::from_secs(2);
    let error = FirmwareToolExecutor::with_supervisor_path(proxy)
        .execute(&command, deadline)
        .await
        .expect_err("result is deliberately held until the attempt deadline");
    let AttemptError::Started {
        status,
        stdout,
        stderr,
        reason,
        cleanup_errors,
        ..
    } = error
    else {
        panic!("command must have started: {error}");
    };
    assert!(matches!(reason, AttemptFailure::Deadline(duration) if duration == deadline));
    assert_eq!(
        status
            .expect("complete status survives the frame split")
            .code(),
        Some(7)
    );
    assert_eq!(stdout, b"command-out");
    assert_eq!(stderr, b"command-err");
    assert!(cleanup_errors.is_empty(), "{cleanup_errors:?}");
    assert_eq!(
        fs::read_to_string(fixture.join("prefix")).unwrap(),
        "prefix"
    );
    assert_eq!(
        fs::read_to_string(fixture.join("control-eof")).unwrap(),
        "eof"
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), "launch");
}

/// An embedding process can ignore SIGCHLD. The helper must restore normal
/// child waiting before launching a tool, or the kernel discards its exit status.
#[tokio::test]
async fn inherited_ignored_sigchld_does_not_discard_tool_status() {
    let directory = tempfile::tempdir().expect("create ignored SIGCHLD fixture");
    let helper = SUPERVISOR.replace('\'', "'\\''");
    let wrapper = write_script(
        directory.path(),
        "ignore-sigchld.sh",
        &format!("trap '' CHLD\nexec '{helper}'"),
    );
    let command = CommandSpec::new("/bin/sh")
        .args(["-c", "printf command-out; printf command-err >&2; exit 7"]);
    let output = FirmwareToolExecutor::with_supervisor_path(wrapper)
        .execute(&command, WATCHDOG)
        .await
        .expect("helper resets inherited SIGCHLD disposition");
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"command-out");
    assert_eq!(output.stderr, b"command-err");
}

/// This separate process embeds the library so its abrupt death closes the
/// control pipe without depending on Tokio task cancellation or Rust destructors.
#[test]
#[ignore = "subprocess fixture invoked only by the parent death test"]
fn supervisor_parent_fixture() {
    let fixture =
        PathBuf::from(std::env::var_os("MLX_SUPERVISOR_PARENT_FIXTURE").expect("fixture path"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build fixture runtime");
    runtime.block_on(async {
        FirmwareToolExecutor::with_supervisor_path(SUPERVISOR)
            .execute(&gated_command(&fixture), CANCELLATION_ATTEMPT_TIMEOUT)
            .await
            .expect("fixture parent should be killed while waiting");
    });
}

/// Losing the embedding process must cancel the helper even when no destructor
/// runs. The helper still has to reap the tool and its adopted descendants.
#[tokio::test]
async fn parent_death_stops_owned_descendants() {
    let directory = tempfile::tempdir().expect("create parent death fixture");
    let mut parent = Command::new(std::env::current_exe().expect("test binary path"))
        .args([
            "--ignored",
            "--exact",
            "supervisor_parent_fixture",
            "--nocapture",
        ])
        .env("MLX_SUPERVISOR_PARENT_FIXTURE", directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("start embedding process");
    let leader = wait_for_file(&directory.path().join("leader")).await;
    let descendant = wait_for_file(&directory.path().join("descendant")).await;
    parent
        .start_kill()
        .expect("kill embedding process without destructors");
    assert!(
        !parent
            .wait()
            .await
            .expect("reap embedding process")
            .success()
    );
    wait_until_reaped(&leader).await;
    wait_until_reaped(&descendant).await;
    assert!(!directory.path().join("completed").exists());
}

/// Under `Continue`, closing the control pipe must not cancel work or remove its deadline.
#[test]
fn continuing_after_control_eof_keeps_the_attempt_deadline() {
    let mut helper = SupervisorProcess::spawn();
    let mut control = helper.0.stdin.take().expect("helper control pipe");
    // Outlast the attempt deadline, but finish before WATCHDOG if it stops working.
    let attempt_timeout = Duration::from_secs(2);
    let request = serde_json::to_vec(&serde_json::json!({
        "program": "/bin/sleep",
        "args": ["5"],
        "timeout": attempt_timeout,
        "owner_exit_behavior": "Continue",
    }))
    .expect("encode continuing request");
    let started = Instant::now();
    control
        .write_all(&u32::try_from(request.len()).unwrap().to_be_bytes())
        .expect("write request length");
    control.write_all(&request).expect("dispatch tool");
    // Close only the control pipe; keep the result pipe open for the helper's report.
    drop(control);
    let status = helper
        .0
        .wait_timeout(WATCHDOG)
        .expect("wait for continuing helper")
        .expect("continuing helper exits within watchdog");
    assert!(status.success(), "continuing helper exited with {status}");
    assert!(
        started.elapsed() >= attempt_timeout,
        "closing the control pipe must not stop work before its deadline"
    );
    let mut output = Vec::new();
    // The helper has exited, so its complete frame is already available. Another
    // concurrently spawning child may briefly retain a pipe writer before exec.
    if let Err(error) = helper.0.stdout.as_mut().unwrap().read_to_end(&mut output) {
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock, "{error}");
    }
    let frame = output
        .strip_prefix(b"MLX-SUPERVISOR-1\n")
        .expect("helper handshake");
    let length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    assert_eq!(frame.len() - 4, length);
    let response: serde_json::Value = serde_json::from_slice(&frame[4..]).expect("helper result");
    assert_eq!(response["Failed"]["reason"], "Deadline", "{response}");
    assert!(response["Failed"]["status"].is_i64(), "{response}");
    assert_eq!(
        response["Failed"]["cleanup_errors"],
        serde_json::json!([]),
        "{response}"
    );
}

/// Continuing on EOF does not ignore an explicit cancellation byte.
#[tokio::test]
async fn continuing_owner_policy_still_accepts_explicit_cancellation() {
    let directory = tempfile::tempdir().expect("create explicit cancellation fixture");
    let fixture = directory.path();
    let command = gated_command(fixture);
    let mut helper = SupervisorProcess::spawn();
    let mut control = helper.0.stdin.take().expect("helper control pipe");
    let request = serde_json::to_vec(&serde_json::json!({
        "program": command.program,
        "args": command.args,
        "timeout": WATCHDOG,
        "owner_exit_behavior": "Continue",
    }))
    .expect("encode continuing request");
    control
        .write_all(&u32::try_from(request.len()).unwrap().to_be_bytes())
        .expect("write request length");
    control.write_all(&request).expect("dispatch tool");
    let leader = wait_for_file(&fixture.join("leader")).await;
    let descendant = wait_for_file(&fixture.join("descendant")).await;
    control.write_all(b"x").expect("request cancellation");
    drop(control);
    assert!(
        helper
            .0
            .wait_timeout(WATCHDOG)
            .expect("wait for cancelled helper")
            .expect("cancelled helper exits within watchdog")
            .success()
    );
    let mut output = Vec::new();
    helper
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    let frame = output
        .strip_prefix(b"MLX-SUPERVISOR-1\n")
        .expect("helper handshake");
    let length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    assert_eq!(frame.len() - 4, length);
    let response: serde_json::Value = serde_json::from_slice(&frame[4..]).expect("helper result");
    assert_eq!(response["Failed"]["reason"], "Cancelled");
    wait_until_reaped(&leader).await;
    wait_until_reaped(&descendant).await;
    assert!(!fixture.join("completed").exists());
}

/// A missing helper proves no tool started, but a lost or malformed result after
/// dispatch cannot establish that certainty, even if the tool actually finished.
#[tokio::test]
async fn result_loss_preserves_execution_uncertainty() {
    #[derive(Clone, Copy)]
    enum Helper {
        Missing,
        Relative,
        WrongHello,
        OversizedRequest,
        ReplaceResult(&'static str),
        AfterResult(&'static str),
    }
    let cases = [
        // No helper exists to launch the requested command.
        (
            "missing",
            Helper::Missing,
            Some(std::io::ErrorKind::NotFound),
        ),
        // A relative path must not fall back to a PATH search.
        (
            "relative",
            Helper::Relative,
            Some(std::io::ErrorKind::InvalidInput),
        ),
        // A different helper protocol cannot accept the command request.
        (
            "wrong hello",
            Helper::WrongHello,
            Some(std::io::ErrorKind::InvalidData),
        ),
        // The client can reject an oversized request before its first dispatch byte.
        (
            "oversized request",
            Helper::OversizedRequest,
            Some(std::io::ErrorKind::InvalidInput),
        ),
        // The real helper runs the tool, but its final report never reaches us.
        ("lost", Helper::ReplaceResult(""), None),
        // Invalid result bytes must not become successful or pre-start results.
        (
            "malformed",
            Helper::ReplaceResult("printf '\\000\\000\\000\\001x'"),
            None,
        ),
        // A response length beyond the bound cannot trigger an allocation or hide execution.
        (
            "oversized response",
            Helper::ReplaceResult("printf '\\377\\377\\377\\377'"),
            None,
        ),
        // A valid frame does not authorize additional protocol output.
        ("trailing byte", Helper::AfterResult("printf x"), None),
        // The helper itself must exit cleanly before its report is accepted.
        ("helper exit", Helper::AfterResult("exit 23"), None),
    ];
    for (scenario, behavior, before_start) in cases {
        let directory = tempfile::tempdir().expect("create protocol fixture");
        let marker = directory.path().join("launches");
        let command = CommandSpec::new("/bin/sh")
            .args(["-c", "printf launch >> \"$1\"", "fixture"])
            .arg(marker.to_str().expect("UTF-8 fixture path"));
        let command = if matches!(behavior, Helper::OversizedRequest) {
            command.arg("x".repeat(128 * 1024))
        } else {
            command
        };
        let real_helper = SUPERVISOR.replace('\'', "'\\''");
        let helper = match behavior {
            Helper::Missing => directory.path().join("missing-helper"),
            Helper::Relative => PathBuf::from("relative-helper"),
            Helper::OversizedRequest => PathBuf::from(SUPERVISOR),
            Helper::WrongHello => write_script(
                directory.path(),
                "wrong-helper.sh",
                "printf 'WRONG-SUPERVISOR!\\n'",
            ),
            Helper::ReplaceResult(replacement) => {
                // Relay only the version handshake, then consume the real result.
                // The private protocol's header is deliberately not tool output.
                write_script(
                    directory.path(),
                    "broken-helper.sh",
                    &format!(
                        "'{real_helper}' | {{ dd bs=1 count={} 2>/dev/null; cat >/dev/null; {replacement}\n}}",
                        b"MLX-SUPERVISOR-1\n".len(),
                    ),
                )
            }
            Helper::AfterResult(suffix) => write_script(
                directory.path(),
                "broken-helper.sh",
                &format!("'{real_helper}'\n{suffix}"),
            ),
        };
        let error = FirmwareToolExecutor::with_supervisor_path(helper.clone())
            .execute(&command, WATCHDOG)
            .await
            .expect_err(scenario);
        assert_eq!(
            error.started(),
            before_start.is_none(),
            "{scenario}: {error}"
        );
        match error {
            AttemptError::SupervisorNotStarted {
                helper: failed_helper,
                source,
                ..
            } => {
                assert!(matches!(behavior, Helper::Missing), "{scenario}");
                assert_eq!(failed_helper, helper, "{scenario}");
                assert_eq!(Some(source.kind()), before_start, "{scenario}");
                assert!(!marker.exists(), "{scenario}: no command may start");
            }
            AttemptError::NotStarted { source, .. } => {
                assert!(
                    !matches!(behavior, Helper::Missing),
                    "missing helper needs its own error"
                );
                assert_eq!(Some(source.kind()), before_start, "{scenario}");
                assert!(!marker.exists(), "{scenario}: no command may start");
            }
            AttemptError::Started { cleanup_errors, .. } => {
                assert!(
                    !cleanup_errors.is_empty(),
                    "{scenario}: completion is unconfirmed"
                );
                assert_eq!(
                    fs::read_to_string(marker).unwrap(),
                    "launch",
                    "{scenario}: no retry"
                );
            }
        }
    }
}
