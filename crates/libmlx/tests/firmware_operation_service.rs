// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "linux")]

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use libmlx::firmware::config::{FirmwareFlasherProfile, FirmwareSpec, FlashOptions, FlashSpec};
use libmlx::firmware::operation::{
    FirmwareOperationPhase, FirmwareOperationState, FirmwareSupervisorClient,
    FirmwareSupervisorClientError, ObserverExitBehavior, SupervisorConfig, SupervisorLaunchError,
    SupervisorLaunchMode, connect_or_launch,
};
use nix::errno::Errno;
use nix::sys::prctl::set_child_subreaper;
use nix::sys::signal::{Signal, kill};
use nix::sys::socket::{getsockopt, sockopt};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::time::{sleep, timeout};
use wait_timeout::ChildExt;

const SUPERVISOR: &str = env!("CARGO_BIN_EXE_mlx-firmware-supervisor");
const DEVICE: &str = "ffff:fe:1f.0";
const SLOT: &str = "ffff:fe:1f";
const WATCHDOG: Duration = Duration::from_secs(10);
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A failed assertion must still stop and reap the daemon it launched.
struct TestProcess(Child);

impl TestProcess {
    fn stop(&mut self) {
        if self
            .0
            .try_wait()
            .expect("inspect fixture process")
            .is_some()
        {
            return;
        }
        self.0.kill().expect("stop fixture process");
        self.0
            .wait_timeout(WATCHDOG)
            .expect("wait for stopped fixture process")
            .expect("reap fixture process within watchdog");
    }
}

impl Drop for TestProcess {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(error) = self.0.kill() {
            eprintln!("failed to stop fixture process: {error}");
        }
        match self.0.wait_timeout(WATCHDOG) {
            Ok(Some(_)) => {}
            result => eprintln!("failed to reap fixture process within watchdog: {result:?}"),
        }
    }
}

fn private_fixture() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    builder.prefix("mlx-operation-");
    let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
        Some(parent) => builder.tempdir_in(parent),
        None => builder.tempdir(),
    }
    .expect("create private operation fixture");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
        .expect("restrict fixture root");
    for child in ["state", "tools"] {
        let path = directory.path().join(child);
        fs::create_dir(&path).expect("create fixture directory");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("restrict fixture directory");
    }
    directory
}

fn daemon(fixture: &Path) -> TestProcess {
    daemon_with_executable(fixture, Path::new(SUPERVISOR), DEVICE)
}

fn daemon_with_executable(fixture: &Path, executable: &Path, device: &str) -> TestProcess {
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(fixture.join("daemon.log"))
        .expect("open daemon diagnostics");
    TestProcess(
        Command::new(executable)
            .args(["serve", "--device", device, "--state-directory"])
            .arg(fixture.join("state"))
            .env_clear()
            .env("PATH", fixture.join("tools"))
            .env("FIXTURE", fixture)
            .env("DEVICE", device)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("start actual operation daemon"),
    )
}

async fn connect(process: &mut TestProcess, fixture: &Path) -> FirmwareSupervisorClient {
    connect_slot(process, fixture, SLOT).await
}

/// Wait for the slot's socket, failing early if its daemon exits.
async fn connect_slot(
    process: &mut TestProcess,
    fixture: &Path,
    slot: &str,
) -> FirmwareSupervisorClient {
    let socket = fixture.join("state").join(slot).join("control.sock");
    timeout(WATCHDOG, async {
        loop {
            if let Some(status) = process.0.try_wait().expect("inspect daemon readiness") {
                panic!(
                    "daemon exited before readiness: {status}; {}",
                    fs::read_to_string(fixture.join("daemon.log")).unwrap()
                );
            }
            match FirmwareSupervisorClient::connect(&socket).await {
                Ok(client) => return client,
                Err(FirmwareSupervisorClientError::Transport(_)) => {
                    sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("daemon readiness failed: {error}"),
            }
        }
    })
    .await
    .expect("operation daemon became ready within watchdog")
}

/// A saved receipt remains bound to its request and full execution BDF across restart.
#[tokio::test]
async fn no_work_receipt_survives_restart_and_rejects_conflicting_reuse() {
    timeout(TEST_TIMEOUT, async {
        let directory = private_fixture();
        let fixture = directory.path();
        // Any accidental tool invocation must leave evidence even for no-work requests.
        fake_tools(fixture);
        let mut process = daemon(fixture);
        let mut client = connect(&mut process, fixture).await;
        let status = client.status().await.expect("read daemon identity");
        assert_eq!(status.device_identity, SLOT);
        assert_eq!(
            status.boot_id,
            fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .unwrap()
                .trim()
        );

        // Slot-level exclusion must not redirect a function-specific request.
        let config = SupervisorConfig {
            state_directory: fixture.join("state"),
            supervisor_path: Some(PathBuf::from(SUPERVISOR)),
            launch_mode: SupervisorLaunchMode::Standalone,
        };
        let error = connect_or_launch("ffff:fe:1f.1", &config)
            .await
            .expect_err("another function must not reuse this execution device");
        assert!(matches!(error, SupervisorLaunchError::IdentityMismatch {
            expected, observed, ..
        } if expected == "ffff:fe:1f.1" && observed == DEVICE));

        let accepted = client
            .submit("no-work", None, ObserverExitBehavior::Continue)
            .await
            .expect("admit no-work operation");
        assert_eq!(accepted.operation_id, "no-work");
        let complete = timeout(WATCHDOG, async {
            loop {
                let operation = client.get("no-work").await.expect("read accepted receipt");
                if operation.state == FirmwareOperationState::Complete {
                    return operation;
                }
                assert_eq!(operation.state, FirmwareOperationState::Accepted);
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("no-work operation completes within watchdog");
        assert!(complete.is_complete());
        assert!(!complete.report.flashed);
        assert_eq!(complete.phase, None);
        assert_eq!(complete.report.reset, None);
        assert_eq!(complete.report.verified_image, None);
        assert_eq!(complete.report.verified_version, None);
        let receipt = serde_json::to_value(&complete).expect("encode completed receipt");
        let duplicate = client
            .submit("no-work", None, ObserverExitBehavior::Continue)
            .await
            .expect("retrieve identical submission");
        assert_eq!(serde_json::to_value(duplicate).unwrap(), receipt);
        let conflict = client
            .submit("no-work", None, ObserverExitBehavior::Terminate)
            .await
            .expect_err("different behavior changes the immutable request");
        assert!(
            matches!(conflict, FirmwareSupervisorClientError::Rpc(status)
            if status.code() == tonic::Code::AlreadyExists)
        );

        let ledger_path = fixture.join("state").join(SLOT).join("operations.json");
        let ledger = fs::read(&ledger_path).expect("read durable completed ledger");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&ledger).unwrap()["execution_device"],
            DEVICE
        );
        drop(client);
        process.stop();

        let mismatch_log = fixture.join("other-function.log");
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&mismatch_log)
            .expect("open mismatched daemon diagnostics");
        let mut other_function = TestProcess(
            Command::new(SUPERVISOR)
                .args(["serve", "--device", "ffff:fe:1f.1", "--state-directory"])
                .arg(fixture.join("state"))
                .env_clear()
                .env("PATH", fixture.join("tools"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .expect("start daemon for another function in the slot"),
        );
        let exit = other_function
            .0
            .wait_timeout(WATCHDOG)
            .expect("wait for mismatched daemon")
            .expect("mismatched daemon must exit within watchdog");
        assert!(!exit.success());
        assert!(
            fs::read_to_string(mismatch_log)
                .unwrap()
                .contains("ExecutionDeviceMismatch")
        );
        assert_eq!(fs::read(&ledger_path).unwrap(), ledger);

        let mut restarted = daemon(fixture);
        let mut client = connect(&mut restarted, fixture).await;
        assert_eq!(
            serde_json::to_value(client.get("no-work").await.unwrap()).unwrap(),
            receipt
        );
        assert_eq!(
            serde_json::to_value(
                client
                    .submit("no-work", None, ObserverExitBehavior::Continue)
                    .await
                    .expect("restart retrieves the saved receipt")
            )
            .unwrap(),
            receipt
        );
        assert_eq!(fs::read(ledger_path).unwrap(), ledger);
        assert!(!fixture.join("invocations").exists());
        drop(client);
        restarted.stop();
    })
    .await
    .expect("no-work daemon test exceeded its independent watchdog");
}

// Publish the burn PID, its owning worker PID, and staged input before waiting
// for release. Tests can then stop the daemon or break its journal while the
// accepted command is still running in the separate worker process.
fn fake_tools(fixture: &Path) {
    let script = r#"#!/bin/sh
set -eu
printf '%s\n' "${0##*/} $*" >> "$FIXTURE/invocations"
if [ "${1-}" = --version ]; then
    if [ "${0##*/}" = flint ] && [ -f "$FIXTURE/missing-burn" ]; then
        /bin/rm -- "$0"
    fi
    exit 0
fi
case "${0##*/}" in
    mlxfwmanager)
        printf '<Devices><Device pciName="%s" type="BlueField3" partNumber="test-part" psid="test-psid"><Versions><FW current="1.0"/></Versions></Device></Devices>' "$DEVICE"
        ;;
    flint)
        [ "$#" -eq 6 ] && [ "$1" = -d ] && [ "$2" = "$DEVICE" ] &&
            [ "$3" = -y ] && [ "$4" = -i ] && [ "$6" = burn ] || exit 42
        [ -s "$5" ] || exit 43
        printf '%s' "$5" > "$FIXTURE/prepared-image"
        printf '%s' "$$" > "$FIXTURE/burn-pid"
        printf '%s' "$PPID" > "$FIXTURE/worker-pid"
        printf launch >> "$FIXTURE/burns"
        i=0
        while [ ! -f "$FIXTURE/release" ]; do
            [ -d "$FIXTURE" ] && [ "$i" -lt 2000 ] || exit 77
            i=$((i + 1))
            /bin/sleep 0.01
        done
        if [ -f "$FIXTURE/overflow-burn" ]; then
            printf '%01048577d' 0
            exit 0
        fi
        printf tool-finished
        printf finished > "$FIXTURE/burn-completed"
        ;;
    mlxfwreset)
        [ "$#" -eq 6 ] && [ "$1" = --device ] && [ "$2" = "$DEVICE" ] &&
            [ "$3" = --level ] && [ "$4" = 3 ] && [ "$5" = reset ] &&
            [ "$6" = -y ] || exit 45
        printf reset > "$FIXTURE/reset-completed"
        ;;
    *) exit 44 ;;
esac
"#;
    // Keep writable executable descriptors out of this multithreaded test
    // process so a concurrent fork cannot inherit one and cause ETXTBSY.
    let mut writer = TestProcess(
        Command::new("/bin/sh")
            .args([
                "-c",
                "umask 077; for tool in mlxfwmanager flint mlxfwreset mlxconfig; do printf '%s' \"$2\" > \"$1/$tool\"; done",
                "fixture-writer",
            ])
            .arg(fixture.join("tools"))
            .arg(script)
            .spawn()
            .expect("start isolated executable writer"),
    );
    assert!(
        writer
            .0
            .wait_timeout(WATCHDOG)
            .expect("wait for executable writer")
            .expect("executable writer finished within watchdog")
            .success()
    );
    for tool in ["mlxfwmanager", "flint", "mlxfwreset", "mlxconfig"] {
        fs::set_permissions(
            fixture.join("tools").join(tool),
            fs::Permissions::from_mode(0o700),
        )
        .expect("make fake tool executable");
    }
    fs::write(fixture.join("image.bin"), b"fixture firmware image\n").expect("write fake image");
}

fn firmware_profile(fixture: &Path) -> FirmwareFlasherProfile {
    FirmwareFlasherProfile {
        firmware_spec: FirmwareSpec {
            part_number: "test-part".to_string(),
            psid: "test-psid".to_string(),
            version: "2.0".to_string(),
        },
        flash_spec: FlashSpec {
            firmware_url: fixture.join("image.bin").to_str().unwrap().to_string(),
            firmware_credentials: None,
            device_conf_url: None,
            device_conf_credentials: None,
            verify_from_cache: false,
            cache_dir: None,
        },
        flash_options: FlashOptions::default(),
    }
}

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

/// Release the burn before shutdown so failed assertions do not orphan a worker.
struct GatedDaemon {
    process: TestProcess,
    fixture: PathBuf,
}

impl Drop for GatedDaemon {
    fn drop(&mut self) {
        if matches!(self.process.0.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(error) = fs::write(self.fixture.join("release"), "release") {
            eprintln!("failed to release fixture burn during cleanup: {error}");
        }
        if let Err(error) = kill(Pid::from_raw(self.process.0.id() as i32), Signal::SIGTERM) {
            eprintln!("failed to request fixture shutdown: {error}");
        }
        match self.process.0.wait_timeout(WATCHDOG) {
            Ok(Some(_)) => {}
            result => eprintln!("fixture did not finish admitted work during cleanup: {result:?}"),
        }
    }
}

/// A daemon shutdown signal stops admission but lets the owned burn finish.
#[tokio::test]
async fn daemon_signals_drain_the_burn_before_exiting() {
    for signal in [Signal::SIGTERM, Signal::SIGINT] {
        timeout(TEST_TIMEOUT, async {
            let directory = private_fixture();
            let fixture = directory.path();
            fake_tools(fixture);
            let mut daemon = GatedDaemon {
                process: daemon(fixture),
                fixture: fixture.to_path_buf(),
            };
            let mut client = connect(&mut daemon.process, fixture).await;
            let mut profile = firmware_profile(fixture);
            profile.flash_options.reset = true;
            client
                .submit("draining", Some(profile), ObserverExitBehavior::Continue)
                .await
                .expect("admit the burn before shutdown");
            let burn_pid = wait_for_file(&fixture.join("burn-pid")).await;
            kill(Pid::from_raw(daemon.process.0.id() as i32), signal).unwrap();
            timeout(WATCHDOG, async {
                while client.status().await.is_ok() {
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("signal closes the serving transport");
            assert!(client
                .submit("after-shutdown", None, ObserverExitBehavior::Continue)
                .await
                .is_err());
            assert!(daemon.process.0.try_wait().unwrap().is_none());
            let stat = fs::read_to_string(Path::new("/proc").join(&burn_pid).join("stat")).unwrap();
            assert!(!matches!(stat.rsplit_once(") ").unwrap().1.as_bytes()[0], b'Z' | b'X'));
            assert!(!fixture.join("reset-completed").exists());

            fs::write(fixture.join("release"), "release").unwrap();
            let status = daemon.process.0.wait_timeout(WATCHDOG).unwrap()
                .expect("daemon drains admitted work before exiting");
            assert!(status.success(), "{signal:?}: {status}");
            drop(client);
            let mut restarted = daemon_with_executable(fixture, Path::new(SUPERVISOR), DEVICE);
            let mut client = connect(&mut restarted, fixture).await;
            let complete = client.get("draining").await.unwrap();
            assert_eq!(complete.state, FirmwareOperationState::Complete);
            assert!(complete.report.flashed);
            assert_eq!(complete.report.reset, Some(true));
            assert!(matches!(client.get("after-shutdown").await,
                Err(FirmwareSupervisorClientError::Rpc(status)) if status.code() == tonic::Code::NotFound));
            assert_eq!(fs::read_to_string(fixture.join("burns")).unwrap(), "launch");
            assert!(!fixture.join("state/mutation.json").exists());
            assert!(!Path::new("/proc").join(burn_pid).exists());
        })
        .await
        .unwrap_or_else(|_| panic!("daemon signal {signal:?} exceeded its watchdog"));
    }
}

/// A settled burn failure must not release the claim merely because execution returned normally.
#[tokio::test]
async fn burn_failures_retain_the_host_claim_and_skip_reset() {
    for (case, expected, started) in [
        (
            "overflow-burn",
            FirmwareOperationState::ManualIntervention,
            true,
        ),
        ("missing-burn", FirmwareOperationState::Failed, false),
    ] {
        timeout(TEST_TIMEOUT, async {
            let directory = private_fixture();
            let fixture = directory.path();
            fake_tools(fixture);
            fs::write(fixture.join(case), "enabled").unwrap();
            fs::write(fixture.join("release"), "release").unwrap();
            let mut daemon = GatedDaemon {
                process: daemon(fixture),
                fixture: fixture.to_path_buf(),
            };
            let mut client = connect(&mut daemon.process, fixture).await;
            let mut profile = firmware_profile(fixture);
            profile.flash_options.reset = true;
            client.submit("failed-burn", Some(profile.clone()), ObserverExitBehavior::Continue)
                .await.expect("admit the failing burn");
            let failed = timeout(WATCHDOG, async {
                loop {
                    let receipt = client.get("failed-burn").await.unwrap();
                    if receipt.state == expected { break receipt; }
                    assert!(matches!(receipt.state, FirmwareOperationState::Accepted | FirmwareOperationState::Running));
                    sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("burn failure is durably recorded");
            assert_eq!(failed.phase, Some(FirmwareOperationPhase::Burn));
            assert!(!failed.report.flashed);
            assert_eq!(failed.report.reset, Some(false));
            let diagnostic = failed.diagnostic.as_deref().expect("failure keeps its cause");
            assert!(diagnostic.contains(if started {
                "command output exceeded 1 MiB"
            } else {
                "firmware tool did not start"
            }), "{case}: {diagnostic}");
            assert_eq!(fixture.join("burns").exists(), started);
            assert!(!fixture.join("reset-completed").exists());
            // Wait for `execute` to finish its claim-release check. Seeing the
            // terminal receipt alone would race that final cleanup step.
            kill(Pid::from_raw(daemon.process.0.id() as i32), Signal::SIGTERM).unwrap();
            // Tonic waits for its connections to close. Keep this client's
            // runtime polling the HTTP/2 shutdown while the daemon drains.
            let status = timeout(WATCHDOG, async {
                loop {
                    if let Some(status) = daemon.process.0.try_wait().unwrap() {
                        break status;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("daemon finishes failure cleanup");
            assert!(status.success());
            drop(client);
            assert!(fixture.join("state/mutation.json").exists());
            let ledger: serde_json::Value = serde_json::from_slice(
                &fs::read(fixture.join("state").join(SLOT).join("operations.json")).unwrap()
            ).unwrap();
            assert_eq!(ledger["records"][0]["mutation_started"], true);

            let invocations = fs::read(fixture.join("invocations")).unwrap();
            let expected_receipt = serde_json::to_value(failed).unwrap();
            let mut restarted = daemon_with_executable(fixture, Path::new(SUPERVISOR), DEVICE);
            let mut reconnected = connect(&mut restarted, fixture).await;
            assert_eq!(serde_json::to_value(reconnected.get("failed-burn").await.unwrap()).unwrap(), expected_receipt);
            let repeated = reconnected.submit("failed-burn", Some(profile), ObserverExitBehavior::Continue)
                .await.expect("same ID retrieves the saved failure after restart");
            assert_eq!(serde_json::to_value(repeated).unwrap(), expected_receipt);
            assert_eq!(fs::read(fixture.join("invocations")).unwrap(), invocations);

            // Another slot has no failed receipt of its own; only the host claim can reject it.
            let mut other = daemon_with_executable(fixture, Path::new(SUPERVISOR), "ffff:fe:1e.0");
            let mut other_client = connect_slot(&mut other, fixture, "ffff:fe:1e").await;
            assert!(matches!(other_client.submit("other-slot", None, ObserverExitBehavior::Continue).await,
                Err(FirmwareSupervisorClientError::Rpc(status)) if status.code() == tonic::Code::ResourceExhausted));
        }).await.unwrap_or_else(|_| panic!("burn failure {case} exceeded its watchdog"));
    }
}

/// Losing the submit reply does not revoke admission or make a repeated ID execute twice.
#[tokio::test]
async fn submit_deadline_preserves_admitted_work_over_a_real_socket() {
    timeout(TEST_TIMEOUT, async {
        let directory = private_fixture();
        let fixture = directory.path();
        fake_tools(fixture);
        let mut daemon = GatedDaemon {
            process: daemon(fixture),
            fixture: fixture.to_path_buf(),
        };
        let mut direct = connect(&mut daemon.process, fixture).await;
        let proxy_socket = fixture.join("reply.sock");
        let listener = UnixListener::bind(&proxy_socket).unwrap();
        let destination = fixture.join("state").join(SLOT).join("control.sock");
        let (reply_gate, mut replies) = tokio::sync::watch::channel(true);
        // `JoinSet` aborts the relay if the outer watchdog or an assertion exits this scope.
        let mut proxy = tokio::task::JoinSet::new();
        proxy.spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            let server = tokio::net::UnixStream::connect(destination).await.unwrap();
            let (mut requests, mut response_writer) = client.into_split();
            let (mut responses, mut request_writer) = server.into_split();
            let forwarding = tokio::io::copy(&mut requests, &mut request_writer);
            let withholding = async {
                let mut buffer = [0; 8192];
                loop {
                    let count = responses.read(&mut buffer).await?;
                    if count == 0 { return Ok::<_, std::io::Error>(()); }
                    replies.wait_for(|open| *open).await
                        .map_err(std::io::Error::other)?;
                    response_writer.write_all(&buffer[..count]).await?;
                }
            };
            tokio::select! {
                result = forwarding => { result.expect("forward requests to the daemon"); },
                result = withholding => result.expect("forward permitted daemon replies"),
            }
        });
        let mut client = FirmwareSupervisorClient::connect(&proxy_socket).await.unwrap();
        reply_gate.send_replace(false);
        let profile = firmware_profile(fixture);
        let burn_path = fixture.join("burn-pid");
        let result = {
            let submitting = client.submit("lost-reply", Some(profile.clone()), ObserverExitBehavior::Continue);
            tokio::pin!(submitting);
            tokio::select! {
                _ = wait_for_file(&burn_path) => {},
                result = &mut submitting => panic!("submit finished before the admitted burn: {result:?}"),
            }
            submitting.await
        };
        assert!(matches!(result, Err(FirmwareSupervisorClientError::Deadline)));
        let accepted = direct.get("lost-reply").await.unwrap();
        assert_eq!(accepted.state, FirmwareOperationState::Running);
        let claim = fs::read(fixture.join("state/mutation.json")).unwrap();
        assert!(matches!(direct.submit("competing", None, ObserverExitBehavior::Continue).await,
            Err(FirmwareSupervisorClientError::Rpc(status)) if status.code() == tonic::Code::FailedPrecondition));
        assert_eq!(fs::read(fixture.join("state/mutation.json")).unwrap(), claim);
        let duplicate = direct.submit("lost-reply", Some(profile), ObserverExitBehavior::Continue).await.unwrap();
        assert_eq!(serde_json::to_value(accepted).unwrap(), serde_json::to_value(duplicate).unwrap());
        assert_eq!(fs::read_to_string(fixture.join("burns")).unwrap(), "launch");
        proxy.abort_all();
        let stopped = proxy.join_next().await.expect("relay task is still owned")
            .expect_err("relay exits through explicit cancellation");
        assert!(stopped.is_cancelled(), "relay failed: {stopped}");
        drop(client);
        fs::write(fixture.join("release"), "release").unwrap();
        timeout(WATCHDOG, async {
            while direct.get("lost-reply").await.unwrap().state != FirmwareOperationState::Complete {
                sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("accepted work completes after the reply deadline");
        assert_eq!(fs::read_to_string(fixture.join("burns")).unwrap(), "launch");
    }).await.expect("lost submit reply fixture exceeded its watchdog");
}

/// Command workers must use the running daemon's image, even after its installed path changes.
#[tokio::test]
async fn renamed_daemon_runs_firmware_and_rejects_changed_profile_reuse() {
    timeout(TEST_TIMEOUT, async {
        let directory = private_fixture();
        let fixture = directory.path();
        fake_tools(fixture);
        let executable = fixture.join("custom-supervisor");
        // Isolate the writable executable descriptor from concurrent test forks.
        let mut copier = TestProcess(
            Command::new("/bin/cp")
                .arg(SUPERVISOR)
                .arg(&executable)
                .spawn()
                .expect("copy the actual helper under a different name"),
        );
        assert!(
            copier
                .0
                .wait_timeout(WATCHDOG)
                .expect("wait for executable copy")
                .expect("executable copy finished within watchdog")
                .success()
        );
        assert!(!fixture.join("mlx-firmware-supervisor").exists());
        let mut daemon = GatedDaemon {
            process: daemon_with_executable(fixture, &executable, DEVICE),
            fixture: fixture.to_path_buf(),
        };
        let mut client = connect(&mut daemon.process, fixture).await;
        // Simulate package replacement after startup. A worker launched through
        // the installed path would now fail; the live daemon still owns its image.
        fs::rename(&executable, fixture.join("running-supervisor-image"))
            .expect("move the running helper's installed path");
        std::os::unix::fs::symlink("/bin/false", &executable)
            .expect("replace the installed helper path");
        let mut profile = firmware_profile(fixture);
        client
            .submit(
                "renamed-helper",
                Some(profile.clone()),
                ObserverExitBehavior::Continue,
            )
            .await
            .expect("admit work through the renamed daemon");
        wait_for_file(&fixture.join("burn-pid")).await;
        let running = client.get("renamed-helper").await.unwrap();
        assert_eq!(running.state, FirmwareOperationState::Running);
        assert_eq!(running.phase, Some(FirmwareOperationPhase::Burn));

        profile.firmware_spec.version = "3.0".to_string();
        let conflict = client
            .submit(
                "renamed-helper",
                Some(profile),
                ObserverExitBehavior::Continue,
            )
            .await
            .expect_err("a different profile cannot replace the admitted request");
        assert!(
            matches!(conflict, FirmwareSupervisorClientError::Rpc(status)
            if status.code() == tonic::Code::AlreadyExists)
        );
        assert_eq!(
            serde_json::to_value(client.get("renamed-helper").await.unwrap()).unwrap(),
            serde_json::to_value(running).unwrap()
        );

        fs::write(fixture.join("release"), "release").unwrap();
        let complete = timeout(WATCHDOG, async {
            loop {
                let operation = client.get("renamed-helper").await.unwrap();
                if operation.state == FirmwareOperationState::Complete {
                    return operation;
                }
                assert_eq!(operation.state, FirmwareOperationState::Running);
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the renamed helper completed its command within watchdog");
        assert!(complete.is_complete());
        assert!(complete.report.flashed);
        assert_eq!(complete.report.expected_version.as_deref(), Some("2.0"));
        assert_eq!(fs::read_to_string(fixture.join("burns")).unwrap(), "launch");
        drop(client);
        daemon.process.stop();
    })
    .await
    .expect("renamed-daemon test exceeded its independent watchdog");
}

/// Cancellation retains preparation evidence and releases host exclusion across distinct slots.
#[tokio::test]
async fn stalled_http_prepare_cancels_and_preserves_requested_report_after_restart() {
    timeout(TEST_TIMEOUT, async {
        let directory = private_fixture();
        let fixture = directory.path();
        fake_tools(fixture);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut profile = firmware_profile(fixture);
        profile.flash_spec.device_conf_url = Some(format!(
            "http://{}/configuration.bin",
            listener.local_addr().unwrap()
        ));
        profile.flash_options.reset = true;
        profile.flash_options.verify_image = true;
        profile.flash_options.verify_version = true;
        let mut process = daemon(fixture);
        let mut client = connect(&mut process, fixture).await;
        let accepted = client
            .submit(
                "stalled-prepare",
                Some(profile),
                ObserverExitBehavior::Continue,
            )
            .await
            .expect("admit firmware with a held configuration download");
        assert_eq!(accepted.state, FirmwareOperationState::Accepted);
        assert!(!accepted.report.flashed);
        assert_eq!(accepted.report.reset, Some(false));
        assert_eq!(accepted.report.verified_image, Some(false));
        assert_eq!(accepted.report.verified_version, Some(false));
        assert_eq!(accepted.report.expected_version.as_deref(), Some("2.0"));
        assert_eq!(accepted.report.observed_version, None);
        let requested_report = serde_json::to_value(accepted.report).unwrap();

        let (stream, _) = timeout(WATCHDOG, listener.accept())
            .await
            .expect("preparation requested the configuration within watchdog")
            .unwrap();
        let mut stream = BufReader::new(stream);
        timeout(WATCHDOG, async {
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(line, "GET /configuration.bin HTTP/1.1\r\n");
            loop {
                line.clear();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            // Keep the response body incomplete while the service handles cancel.
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\npartial configuration")
                .await
                .unwrap();
            stream.get_mut().flush().await.unwrap();
        })
        .await
        .expect("the server sent its partial response within watchdog");
        let preparing = client.get("stalled-prepare").await.unwrap();
        assert_eq!(preparing.state, FirmwareOperationState::Running);
        assert_eq!(preparing.phase, Some(FirmwareOperationPhase::Prepare));
        let staged: Vec<_> = fs::read_dir(fixture.join("state").join(SLOT))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        assert_eq!(staged.len(), 1);
        let staged_image = staged[0].join("image/image.bin");
        assert_eq!(fs::read(&staged_image).unwrap(), b"fixture firmware image\n");
        assert!(staged[0].join("configuration").is_dir());
        let claim = fs::read(fixture.join("state/mutation.json")).unwrap();
        let ledger_path = fixture.join("state").join(SLOT).join("operations.json");
        let ledger = fs::read(&ledger_path).unwrap();
        let invocations = fs::read_to_string(fixture.join("invocations")).unwrap();
        assert_eq!(invocations, format!("mlxfwmanager --dev {DEVICE} --query-format xml\n"));

        let other_slot = "ffff:fd:1f";
        let mut other_process =
            daemon_with_executable(fixture, Path::new(SUPERVISOR), "ffff:fd:1f.0");
        let mut other_client = connect_slot(&mut other_process, fixture, other_slot).await;
        assert_eq!(other_client.status().await.unwrap().device_identity, other_slot);
        let exclusion = other_client
            .submit("other-slot", None, ObserverExitBehavior::Continue)
            .await
            .expect_err("another slot cannot admit work while preparation owns the host");
        assert!(matches!(exclusion, FirmwareSupervisorClientError::Rpc(status)
            if status.code() == tonic::Code::ResourceExhausted
                && status.message() == "another firmware operation owns the host or requires reconciliation"));
        let missing = other_client.get("other-slot").await.expect_err("rejected work has no receipt");
        assert!(matches!(missing, FirmwareSupervisorClientError::Rpc(status)
            if status.code() == tonic::Code::NotFound));
        assert_eq!(
            serde_json::to_value(client.get("stalled-prepare").await.unwrap()).unwrap(),
            serde_json::to_value(preparing).unwrap()
        );
        assert_eq!(fs::read(&ledger_path).unwrap(), ledger);
        assert_eq!(fs::read(fixture.join("state/mutation.json")).unwrap(), claim);
        assert_eq!(fs::read_to_string(fixture.join("invocations")).unwrap(), invocations);

        let cancellation = client.cancel("stalled-prepare").await.unwrap();
        assert_eq!(cancellation.state, FirmwareOperationState::CancelRequested);
        assert_eq!(cancellation.phase, Some(FirmwareOperationPhase::Prepare));
        assert_eq!(
            serde_json::to_value(cancellation.report).unwrap(),
            requested_report
        );
        let canceled = timeout(WATCHDOG, async {
            loop {
                let operation = client.get("stalled-prepare").await.unwrap();
                if operation.state == FirmwareOperationState::Canceled {
                    return operation;
                }
                assert_eq!(operation.state, FirmwareOperationState::CancelRequested);
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancel settled without completing the HTTP response body");
        assert_eq!(canceled.phase, Some(FirmwareOperationPhase::Prepare));
        assert_eq!(
            serde_json::to_value(&canceled.report).unwrap(),
            requested_report
        );
        timeout(WATCHDOG, async {
            while fixture.join("state/mutation.json").exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pre-mutation cancellation released the host claim");
        assert_eq!(fs::read_to_string(fixture.join("invocations")).unwrap(), invocations);
        assert!(!fixture.join("burns").exists());
        assert_eq!(fs::read(&staged_image).unwrap(), b"fixture firmware image\n");
        let saved = serde_json::to_value(canceled).unwrap();

        let other_accepted = other_client
            .submit("other-slot", None, ObserverExitBehavior::Continue)
            .await
            .expect("the same second-slot request can be admitted after safe cancellation");
        assert_eq!(other_accepted.state, FirmwareOperationState::Accepted);
        assert_eq!(other_accepted.operation_id, "other-slot");
        let complete = timeout(WATCHDOG, async {
            loop {
                let operation = other_client.get("other-slot").await.unwrap();
                if operation.state == FirmwareOperationState::Complete {
                    return operation;
                }
                assert_eq!(operation.state, FirmwareOperationState::Accepted);
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the second slot completes its no-work admission within watchdog");
        assert!(complete.is_complete());
        assert!(!complete.report.flashed);
        timeout(WATCHDOG, async {
            while fixture.join("state/mutation.json").exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the second slot released the host claim after completion");
        assert_eq!(
            serde_json::to_value(client.get("stalled-prepare").await.unwrap()).unwrap(),
            saved
        );
        assert_eq!(fs::read_to_string(fixture.join("invocations")).unwrap(), invocations);
        drop(other_client);
        other_process.stop();
        drop(client);
        process.stop();

        let mut restarted = daemon(fixture);
        let mut client = connect(&mut restarted, fixture).await;
        assert_eq!(
            serde_json::to_value(client.get("stalled-prepare").await.unwrap()).unwrap(),
            saved
        );
        assert!(!fixture.join("state/mutation.json").exists());
        assert_eq!(fs::read(&staged_image).unwrap(), b"fixture firmware image\n");
        assert_eq!(
            fs::read_to_string(fixture.join("invocations")).unwrap(),
            invocations
        );
        drop(stream);
        drop(client);
        restarted.stop();
    })
    .await
    .expect("stalled-prepare test exceeded its independent watchdog");
}

/// Losing a settled burn's receipt must stop execution before the requested reset.
#[test]
fn failed_burn_receipt_write_exits_and_reopens_without_replay() {
    run_isolated_operation_fixture("failed_burn_receipt_write_fixture");
}

#[tokio::test]
#[ignore = "invoked only by failed_burn_receipt_write_exits_and_reopens_without_replay"]
async fn failed_burn_receipt_write_fixture() {
    // If the daemon exits before reaping its Continue worker, this isolated
    // process owns that worker and can release and reap it while unwinding.
    set_child_subreaper(true).expect("adopt only this fixture's orphan worker");
    timeout(TEST_TIMEOUT, async {
        let fixture_path = PathBuf::from(std::env::var_os("MLX_OPERATION_FIXTURE").unwrap());
        let fixture = fixture_path.as_path();
        let mut profile = firmware_profile(fixture);
        profile.flash_options.reset = true;
        // Drop the daemon first so a surviving worker is adopted before reaping.
        let _worker;
        let mut supervisor = GatedDaemon {
            process: daemon(fixture),
            fixture: fixture.to_path_buf(),
        };
        let mut client = connect(&mut supervisor.process, fixture).await;
        client
            .submit(
                "receipt-failure",
                Some(profile.clone()),
                ObserverExitBehavior::Continue,
            )
            .await
            .expect("admit the gated burn before its receipt fault");
        let burn_pid = wait_for_file(&fixture.join("burn-pid")).await;
        let worker_pid = wait_for_file(&fixture.join("worker-pid")).await;
        _worker = AdoptedWorker {
            pid: Pid::from_raw(worker_pid.parse().expect("tool worker PID")),
            fixture: fixture.to_path_buf(),
            reaped: false,
        };
        let prepared_image = wait_for_file(&fixture.join("prepared-image")).await;
        let running = client.get("receipt-failure").await.unwrap();
        assert_eq!(running.state, FirmwareOperationState::Running);
        assert_eq!(running.phase, Some(FirmwareOperationPhase::Burn));
        assert_eq!(running.report.reset, Some(false));
        let ledger = fixture.join("state").join(SLOT).join("operations.json");
        let saved_ledger = fixture.join("saved-operations.json");
        let saved_bytes = fs::read(&ledger).unwrap();
        let claim = fs::read(fixture.join("state/mutation.json")).unwrap();
        // Atomic replacement cannot overwrite a directory. Preserve the real
        // pre-completion receipt so recovery never manufactures execution facts.
        fs::rename(&ledger, &saved_ledger).unwrap();
        fs::create_dir(&ledger).unwrap();
        fs::write(fixture.join("release"), "release").unwrap();
        let exit = supervisor
            .process
            .0
            .wait_timeout(WATCHDOG)
            .expect("wait for the daemon after its receipt write failed")
            .expect("receipt persistence failure stopped the daemon within watchdog");
        assert!(!exit.success());
        // This fixture supplies private stderr directly; the real Serve branch
        // must initialize tracing and emit its persistence failure there.
        let log_path = fixture.join("daemon.log");
        assert_eq!(
            fs::metadata(&log_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let log = fs::read_to_string(log_path).unwrap();
        let diagnostic = log
            .lines()
            .find(|line| {
                line.contains("level=ERROR")
                    && line.contains("msg=\"firmware execution could not persist progress\"")
            })
            .expect("the real daemon emitted its service diagnostic to private stderr");
        assert!(diagnostic.contains("firmware journal must be reopened after an uncertain save"));
        assert_eq!(fs::read(&saved_ledger).unwrap(), saved_bytes);
        assert_eq!(
            fs::read_to_string(fixture.join("burn-completed")).unwrap(),
            "finished"
        );
        assert_eq!(fs::read_to_string(fixture.join("burns")).unwrap(), "launch");
        assert!(!fixture.join("reset-completed").exists());
        assert!(!Path::new("/proc").join(burn_pid).exists());
        assert!(!Path::new("/proc").join(worker_pid).exists());
        assert_eq!(
            fs::read(&prepared_image).unwrap(),
            b"fixture firmware image\n"
        );
        assert_eq!(
            fs::read(fixture.join("state/mutation.json")).unwrap(),
            claim
        );
        let invocations = fs::read_to_string(fixture.join("invocations")).unwrap();
        drop(client);

        fs::remove_dir(&ledger).unwrap();
        fs::rename(&saved_ledger, &ledger).unwrap();
        let mut restarted = daemon(fixture);
        let mut client = connect(&mut restarted, fixture).await;
        let recovered = client.get("receipt-failure").await.unwrap();
        assert_eq!(recovered.state, FirmwareOperationState::ManualIntervention);
        assert_eq!(recovered.phase, Some(FirmwareOperationPhase::Burn));
        assert!(!recovered.report.flashed);
        assert_eq!(recovered.report.reset, Some(false));
        let duplicate = client
            .submit(
                "receipt-failure",
                Some(profile),
                ObserverExitBehavior::Continue,
            )
            .await
            .expect("the original ID retrieves the unresolved receipt");
        assert_eq!(
            serde_json::to_value(duplicate).unwrap(),
            serde_json::to_value(recovered).unwrap()
        );
        assert_eq!(
            fs::read_to_string(fixture.join("invocations")).unwrap(),
            invocations
        );
        assert!(!fixture.join("reset-completed").exists());
        assert_eq!(
            fs::read(&prepared_image).unwrap(),
            b"fixture firmware image\n"
        );
        assert_eq!(
            fs::read(fixture.join("state/mutation.json")).unwrap(),
            claim
        );
        drop(client);
        restarted.stop();
        fs::write(fixture.join("checked"), "checked").expect("record fixture completion");
    })
    .await
    .expect("receipt-write failure test exceeded its independent watchdog");
}

/// Probe the kernel directly so a service regression cannot masquerade as missing support.
fn peer_pidfds_supported() -> bool {
    let (socket, _peer) = UnixStream::pair().expect("create independent peer-pidfd probe");
    match getsockopt(&socket, sockopt::PeerPidfd) {
        Ok(_) => true,
        Err(Errno::ENOPROTOOPT) => false,
        Err(error) => panic!("independent SO_PEERPIDFD probe failed unexpectedly: {error}"),
    }
}

/// Socket loss never cancels work; process-exit cancellation requires kernel evidence.
#[tokio::test]
async fn observer_exit_policy_tracks_the_submitting_process_not_its_socket() {
    let peer_pidfds = peer_pidfds_supported();
    for (behavior, while_gated, terminal, reset) in [
        (
            ObserverExitBehavior::Continue,
            FirmwareOperationState::Running,
            FirmwareOperationState::Complete,
            true,
        ),
        (
            ObserverExitBehavior::Terminate,
            FirmwareOperationState::CancelRequested,
            FirmwareOperationState::Canceled,
            false,
        ),
    ] {
        timeout(TEST_TIMEOUT, async {
            let directory = private_fixture();
            let fixture = directory.path();
            fake_tools(fixture);
            let mut daemon = GatedDaemon {
                process: daemon(fixture),
                fixture: fixture.to_path_buf(),
            };
            let mut client = connect(&mut daemon.process, fixture).await;
            let log = File::create(fixture.join("submitter.log")).unwrap();
            let mut submitter = TestProcess(
                Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--ignored",
                        "--exact",
                        "observer_submitter_fixture",
                        "--nocapture",
                    ])
                    .env("MLX_OPERATION_FIXTURE", fixture)
                    .env(
                        "MLX_OBSERVER_EXIT_BEHAVIOR",
                        serde_json::to_string(&behavior).unwrap(),
                    )
                    .stdin(Stdio::null())
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .expect("start actual submitting process"),
            );
            if behavior == ObserverExitBehavior::Terminate && !peer_pidfds {
                let status = submitter
                    .0
                    .wait_timeout(WATCHDOG)
                    .expect("wait for unsupported-policy submitter");
                assert!(
                    status.is_some_and(|status| status.success()),
                    "unsupported-policy submitter failed: {status:?}; {}",
                    fs::read_to_string(fixture.join("submitter.log")).unwrap()
                );
                assert!(fixture.join("observer-rejected").exists());
                let error = client
                    .get("observer-exit")
                    .await
                    .expect_err("unsupported policy must not create a receipt");
                assert!(matches!(error, FirmwareSupervisorClientError::Rpc(status)
                    if status.code() == tonic::Code::NotFound));
                assert!(!fixture.join("state/mutation.json").exists());
                assert!(!fixture.join("invocations").exists());
                drop(client);
                daemon.process.stop();
                return;
            }
            timeout(WATCHDOG, async {
                loop {
                    if let Some(status) = submitter.0.try_wait().unwrap() {
                        panic!(
                            "submitter failed before disconnect: {status}; {}",
                            fs::read_to_string(fixture.join("submitter.log")).unwrap()
                        );
                    }
                    if fixture.join("observer-disconnected").exists() {
                        break;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("submitter admitted work and closed its socket within watchdog");
            let burn_pid = wait_for_file(&fixture.join("burn-pid")).await;
            let worker_pid = wait_for_file(&fixture.join("worker-pid")).await;

            // The submitter has dropped its runtime, but not exited. Give the
            // daemon time to consume the closed connection before checking it.
            sleep(Duration::from_millis(100)).await;
            assert!(submitter.0.try_wait().unwrap().is_none());
            let running = client.get("observer-exit").await.unwrap();
            assert_eq!(
                running.state,
                FirmwareOperationState::Running,
                "{behavior:?}"
            );
            assert_eq!(running.phase, Some(FirmwareOperationPhase::Burn));
            submitter.stop();
            assert!(submitter.0.try_wait().unwrap().is_some());

            let gated = timeout(WATCHDOG, async {
                loop {
                    let operation = client.get("observer-exit").await.unwrap();
                    if operation.state == while_gated {
                        return operation;
                    }
                    assert_eq!(operation.state, FirmwareOperationState::Running);
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("observer death reached its expected gated state");
            assert_eq!(gated.operation_id, "observer-exit");
            assert_eq!(gated.phase, Some(FirmwareOperationPhase::Burn));
            assert!(!gated.report.flashed);
            let stat = fs::read_to_string(Path::new("/proc").join(&burn_pid).join("stat"))
                .expect("burn must remain alive after its submitter exits");
            let state = stat.rsplit_once(") ").unwrap().1.as_bytes()[0];
            assert!(!matches!(state, b'Z' | b'X'));
            assert!(!fixture.join("burn-completed").exists());
            assert!(!fixture.join("reset-completed").exists());

            fs::write(fixture.join("release"), "release").unwrap();
            let complete = timeout(WATCHDOG, async {
                loop {
                    let operation = client.get("observer-exit").await.unwrap();
                    if operation.state == terminal {
                        return operation;
                    }
                    assert!(matches!(
                        operation.state,
                        FirmwareOperationState::Running | FirmwareOperationState::CancelRequested
                    ));
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("released burn reached its expected terminal state");
            assert_eq!(complete.operation_id, "observer-exit");
            assert!(complete.report.flashed);
            assert_eq!(complete.report.reset, Some(reset));
            assert_eq!(fixture.join("reset-completed").exists(), reset);
            assert_eq!(fs::read_to_string(fixture.join("burns")).unwrap(), "launch");
            assert!(!Path::new("/proc").join(burn_pid).exists());
            assert!(!Path::new("/proc").join(worker_pid).exists());
            drop(client);
            daemon.process.stop();
        })
        .await
        .unwrap_or_else(|_| {
            panic!("observer policy {behavior:?} exceeded its independent watchdog")
        });
    }
}

/// Submit from a separate process, then close the socket without ending that process.
#[test]
#[ignore = "invoked only by observer_exit_policy_tracks_the_submitting_process_not_its_socket"]
fn observer_submitter_fixture() {
    let fixture = PathBuf::from(std::env::var_os("MLX_OPERATION_FIXTURE").unwrap());
    let behavior = serde_json::from_str(&std::env::var("MLX_OBSERVER_EXIT_BEHAVIOR").unwrap())
        .expect("decode observer policy");
    let mut profile = firmware_profile(&fixture);
    profile.flash_options.reset = true;
    let accepted = {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create submitter runtime");
        runtime.block_on(async {
            let mut client = FirmwareSupervisorClient::connect(
                &fixture.join("state").join(SLOT).join("control.sock"),
            )
            .await
            .expect("connect actual submitting process");
            let result = client
                .submit("observer-exit", Some(profile), behavior)
                .await;
            if behavior == ObserverExitBehavior::Terminate && !peer_pidfds_supported() {
                let error = result.expect_err("Terminate needs an available peer pidfd");
                assert!(matches!(error, FirmwareSupervisorClientError::Rpc(status)
                    if status.code() == tonic::Code::FailedPrecondition));
                false
            } else {
                result.expect("admit observer fixture when its kernel requirements are met");
                true
            }
        })
    };
    if !accepted {
        fs::write(fixture.join("observer-rejected"), "rejected").unwrap();
        return;
    }
    // Runtime destruction closes the transport before this process waits.
    fs::write(fixture.join("observer-disconnected"), "disconnected").unwrap();
    std::thread::sleep(TEST_TIMEOUT);
    panic!("parent did not stop the submitting process within watchdog");
}

/// This guard is used only by the isolated test subprocess that adopts the worker.
struct AdoptedWorker {
    pid: Pid,
    fixture: PathBuf,
    reaped: bool,
}

impl AdoptedWorker {
    async fn reap(&mut self) {
        timeout(WATCHDOG, async {
            loop {
                match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
                    Ok(WaitStatus::Exited(_, 0)) => {
                        self.reaped = true;
                        return;
                    }
                    Ok(WaitStatus::StillAlive) | Err(Errno::EINTR) => {
                        sleep(Duration::from_millis(10)).await;
                    }
                    result => panic!("continuing worker did not exit successfully: {result:?}"),
                }
            }
        })
        .await
        .expect("orphan worker exited and was reaped within watchdog");
    }
}

impl Drop for AdoptedWorker {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if let Err(error) = fs::write(self.fixture.join("release"), "release") {
            eprintln!("failed to release fixture burn during cleanup: {error}");
        }
        let deadline = std::time::Instant::now() + WATCHDOG;
        let process = Path::new("/proc").join(self.pid.to_string());
        loop {
            match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => break,
                Err(Errno::ECHILD) if !process.exists() => break,
                Ok(_) | Err(Errno::EINTR | Errno::ECHILD)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => {
                    eprintln!("failed to reap adopted fixture worker: {result:?}");
                    break;
                }
            }
        }
    }
}

#[test]
fn daemon_restart_does_not_replay_a_burn_that_outlives_it() {
    run_isolated_operation_fixture("pending_burn_restart_fixture");
}

/// Keep subreaper ownership isolated from other tests and bound the whole child process.
fn run_isolated_operation_fixture(test_name: &str) {
    let directory = private_fixture();
    let fixture = directory.path();
    fake_tools(fixture);
    let output = fixture.join("test.log");
    let mut process = TestProcess(
        Command::new(std::env::current_exe().expect("test executable"))
            .args(["--ignored", "--exact", test_name, "--nocapture"])
            .env("MLX_OPERATION_FIXTURE", fixture)
            .stdin(Stdio::null())
            .stdout(File::create(&output).unwrap())
            .stderr(File::create(fixture.join("test.stderr")).unwrap())
            .spawn()
            .expect("start isolated operation fixture"),
    );
    // The outer deadline also covers startup and bounded graceful drain,
    // forced daemon reaping, and adopted-worker cleanup after an inner timeout.
    let status: Option<ExitStatus> = process
        .0
        .wait_timeout(TEST_TIMEOUT + WATCHDOG * 4)
        .expect("wait for isolated operation fixture");
    assert!(
        status.is_some_and(|status| status.success()),
        "isolated fixture failed: {status:?}; stdout: {}; stderr: {}; daemon: {}",
        fs::read_to_string(output).unwrap(),
        fs::read_to_string(fixture.join("test.stderr")).unwrap(),
        fs::read_to_string(fixture.join("daemon.log")).unwrap_or_default()
    );
    assert!(
        fixture.join("checked").exists(),
        "fixture test must actually run"
    );
}

#[tokio::test]
#[ignore = "invoked only by daemon_restart_does_not_replay_a_burn_that_outlives_it"]
async fn pending_burn_restart_fixture() {
    // No other test runs in this subprocess. Adopting its orphan worker lets
    // us prove cleanup without depending on the test host's PID 1 reaper.
    set_child_subreaper(true).expect("adopt only this fixture's orphan worker");
    timeout(TEST_TIMEOUT, async {
        let fixture = PathBuf::from(std::env::var_os("MLX_OPERATION_FIXTURE").unwrap());
        let profile = firmware_profile(&fixture);
        let mut process = daemon(&fixture);
        let mut client = connect(&mut process, &fixture).await;
        client
            .submit(
                "pending-burn",
                Some(profile.clone()),
                ObserverExitBehavior::Continue,
            )
            .await
            .expect("admit fake burn");
        let burn_pid = wait_for_file(&fixture.join("burn-pid")).await;
        let worker_pid = wait_for_file(&fixture.join("worker-pid")).await;
        let mut worker = AdoptedWorker {
            pid: Pid::from_raw(worker_pid.parse().expect("tool worker PID")),
            fixture: fixture.clone(),
            reaped: false,
        };
        let prepared_image = wait_for_file(&fixture.join("prepared-image")).await;
        let running = client
            .get("pending-burn")
            .await
            .expect("read running burn receipt");
        assert_eq!(running.state, FirmwareOperationState::Running);
        assert_eq!(running.phase, Some(FirmwareOperationPhase::Burn));
        let cancellation = client
            .cancel("pending-burn")
            .await
            .expect("request a stop at the next safe boundary");
        assert_eq!(cancellation.state, FirmwareOperationState::CancelRequested);
        assert_eq!(cancellation.phase, Some(FirmwareOperationPhase::Burn));
        drop(client);
        process.stop();

        let mut restarted = daemon(&fixture);
        let mut client = connect(&mut restarted, &fixture).await;
        let interrupted = client
            .get("pending-burn")
            .await
            .expect("read recovery receipt");
        assert_eq!(
            interrupted.state,
            FirmwareOperationState::ManualIntervention
        );
        assert_eq!(interrupted.phase, Some(FirmwareOperationPhase::Burn));
        assert!(!interrupted.report.flashed);
        assert!(!interrupted.is_complete());
        let saved = serde_json::to_value(interrupted).unwrap();
        let duplicate = client
            .submit(
                "pending-burn",
                Some(profile),
                ObserverExitBehavior::Continue,
            )
            .await
            .expect("retrieve existing interrupted operation");
        assert_eq!(serde_json::to_value(duplicate).unwrap(), saved);
        let blocked = client
            .submit("new-work", None, ObserverExitBehavior::Continue)
            .await
            .expect_err("unfinished mutation blocks later admission");
        assert!(matches!(blocked, FirmwareSupervisorClientError::Rpc(status)
            if status.code() == tonic::Code::FailedPrecondition));
        assert_eq!(fs::read_to_string(fixture.join("burns")).unwrap(), "launch");
        assert_eq!(
            fs::read(&prepared_image).unwrap(),
            b"fixture firmware image\n"
        );

        fs::write(fixture.join("release"), "release").expect("release surviving fake burn");
        assert_eq!(
            wait_for_file(&fixture.join("burn-completed")).await,
            "finished"
        );
        worker.reap().await;
        assert!(!Path::new("/proc").join(burn_pid).exists());
        assert_eq!(fs::read_to_string(fixture.join("burns")).unwrap(), "launch");
        assert_eq!(
            serde_json::to_value(client.get("pending-burn").await.unwrap()).unwrap(),
            saved
        );
        assert!(Path::new(&prepared_image).is_file());
        drop(client);
        restarted.stop();
        fs::write(fixture.join("checked"), "checked").expect("record fixture completion");
    })
    .await
    .expect("pending-burn fixture exceeded its independent watchdog");
}
