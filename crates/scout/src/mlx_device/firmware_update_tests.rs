// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Exercise typed Scout delivery against the real supervisor with isolated tools.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::response::Redirect;
use axum::routing::get;
use libmlx::firmware::config::{
    FirmwareFlasherProfile, FirmwareSpec, FirmwareUpdateContext, FlashOptions, FlashSpec,
};
use libmlx::firmware::operation::{
    FirmwareOperation, FirmwareOperationPhase, FirmwareOperationState, FirmwareSupervisorClient,
    SupervisorConfig, SupervisorLaunchMode,
};
use rpc::forge_agent_control_response as fac;
use rpc::protos::mlx_device as pb;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};

const DEVICE: &str = "ffff:fe:1f.0";
const SLOT: &str = "ffff:fe:1f";
const IMAGE: &str = "checked firmware image";
const CONFIGURATION: &str = "checked device configuration";
const WATCHDOG: Duration = Duration::from_secs(10);
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Scout preserves checked inputs and retry settings through real supervisor
/// admission; a settled Verify failure retries without repeating the mutations.
#[tokio::test]
async fn checked_update_stages_owned_inputs_and_publishes_correlated_receipts() {
    timeout(TEST_TIMEOUT, async {
        let directory = private_fixture();
        let fixture = directory.path();
        fake_tools(fixture).await;
        let (base_url, fetches, mut server) = artifact_server().await;
        let mut daemon = TestDaemon::start(fixture).await;
        let config = SupervisorConfig {
            state_directory: fixture.join("state"),
            supervisor_path: Some(supervisor_binary()),
            launch_mode: SupervisorLaunchMode::Standalone,
        };

        // The image is correct, but a wrong configuration digest must stop
        // before either mutation. Checking only the image would miss this.
        let mut rejected = update_task(&base_url);
        rejected.operation_id = "bad-configuration".into();
        rejected
            .expected_digests
            .as_mut()
            .unwrap()
            .configuration_sha256 = Some("00".repeat(32));
        let failed = poll_until(&rejected, &config, FirmwareOperationState::Failed).await;
        assert_eq!(failed.phase, Some(FirmwareOperationPhase::Prepare));
        assert!(!failed.report.flashed);
        assert!(
            failed
                .diagnostic
                .as_deref()
                .unwrap()
                .contains("configuration")
        );
        assert!(!fixture.join("mutations").exists());
        let failed_record = journal_record(fixture, &rejected.operation_id);
        assert_eq!(failed_record["mutation_started"], false);
        wait_for_claim_release(fixture).await;
        assert_retained_inputs(fixture, &failed_record);
        assert_eq!(fetches.load(Ordering::SeqCst), 2);

        let task = update_task(&base_url);
        let accepted = deliver(&task, &config)
            .await
            .expect("admit checked firmware");
        assert!(matches!(
            accepted.state,
            FirmwareOperationState::Accepted | FirmwareOperationState::Running
        ));
        let prepared_image = wait_for_file(&fixture.join("prepared-image")).await;
        let prepared_configuration = wait_for_file(&fixture.join("prepared-configuration")).await;
        let running = deliver(&task, &config)
            .await
            .expect("observe the running burn");
        assert_eq!(running.state, FirmwareOperationState::Running);
        assert_eq!(running.phase, Some(FirmwareOperationPhase::Burn));
        assert!(!running.report.flashed);
        let claim: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.join("state/mutation.json")).unwrap())
                .unwrap();
        assert_eq!(claim["operation_id"], task.operation_id);
        assert_eq!(claim["device_identity"], SLOT);

        // The first delivery is already gone. Both files are still present
        // under the supervisor's slot, and the journal names those exact bytes.
        let record = journal_record(fixture, &task.operation_id);
        assert_eq!(record["operation"], serde_json::to_value(&running).unwrap());
        assert_eq!(record["mutation_started"], true);
        assert_eq!(
            record["requested"]["update_context"]["phase_id"],
            task.phase_id
        );
        assert_eq!(
            record["requested"]["options"]["post_reset_verify_retry_interval_seconds"],
            1
        );
        assert_eq!(
            record["requested"]["options"]["post_reset_verify_timeout_seconds"],
            20
        );
        assert_retained_inputs(fixture, &record);
        for (kind, actual_path) in [
            ("image", prepared_image.as_str()),
            ("configuration", prepared_configuration.as_str()),
        ] {
            let artifact = &record["prepared_inputs"][kind];
            let relative = Path::new(artifact["path"].as_str().unwrap());
            let owned = config.state_directory.join(SLOT).join(relative);
            assert_eq!(Path::new(actual_path), owned);
            assert_eq!(
                record["requested"]["update_context"]["expected_digests"][format!("{kind}_sha256")],
                artifact["sha256"]
            );
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 4);
        let mutations = fs::read_to_string(fixture.join("mutations")).unwrap();
        assert_eq!(mutations, "configure\nburn\n");

        // No download source remains when Core delivers the task again. The
        // same ID must return its receipt without fetching, configuring, or burning.
        timeout(WATCHDOG, server.shutdown())
            .await
            .expect("stop artifact server");
        let mut conflicting = task.clone();
        conflicting.phase_id = "different-dispatch-phase".into();
        let conflict = deliver(&conflicting, &config)
            .await
            .expect_err("changed work must use Submit, not a Get-first shortcut");
        assert_eq!(
            conflict.code,
            pb::MlxDeviceUpdateFirmwareErrorCode::Conflict as i32
        );
        let duplicate = deliver(&task, &config)
            .await
            .expect("observe the original request");
        assert_eq!(
            serde_json::to_value(duplicate).unwrap(),
            serde_json::to_value(running).unwrap()
        );
        assert_eq!(
            fs::read_to_string(fixture.join("mutations")).unwrap(),
            mutations
        );
        assert_eq!(fetches.load(Ordering::SeqCst), 4);

        fs::write(fixture.join("release"), "release").unwrap();
        // The second Verify waits for us after a settled first failure. This
        // proves Scout's requested retry policy reached execution while the
        // immutable inputs and successful reset remain in the saved receipt.
        wait_for_file(&fixture.join("verification-retried")).await;
        let retrying = deliver(&task, &config)
            .await
            .expect("observe the retried verification");
        assert_eq!(retrying.state, FirmwareOperationState::Running);
        assert_eq!(retrying.phase, Some(FirmwareOperationPhase::VerifyImage));
        assert!(retrying.report.flashed);
        assert_eq!(retrying.report.reset, Some(true));
        assert_eq!(retrying.report.verified_image, Some(false));
        assert_eq!(retrying.report.verified_version, Some(false));
        let retry_record = journal_record(fixture, &task.operation_id);
        assert_eq!(
            retry_record["operation"],
            serde_json::to_value(&retrying).unwrap()
        );
        assert_eq!(retry_record["requested"], record["requested"]);
        assert_eq!(retry_record["prepared_inputs"], record["prepared_inputs"]);
        assert_eq!(retry_record["request_sha256"], record["request_sha256"]);
        let completed_mutations = "configure\nburn\nreset\n";
        assert_eq!(
            fs::read_to_string(fixture.join("mutations")).unwrap(),
            completed_mutations
        );
        assert_eq!(
            fs::read_to_string(fixture.join("verification-attempts")).unwrap(),
            "verify\nverify\n"
        );
        fs::write(fixture.join("release-verification"), "release").unwrap();
        let complete = poll_until(&task, &config, FirmwareOperationState::Complete).await;
        assert!(complete.is_complete());
        assert!(complete.report.flashed);
        assert_eq!(complete.report.reset, Some(true));
        assert_eq!(complete.report.verified_image, Some(true));
        assert_eq!(complete.report.verified_version, Some(true));
        assert_eq!(complete.report.observed_version.as_deref(), Some("2.0"));
        let complete_record = journal_record(fixture, &task.operation_id);
        assert_eq!(complete_record["requested"], record["requested"]);
        assert_eq!(
            complete_record["prepared_inputs"],
            record["prepared_inputs"]
        );
        assert_eq!(complete_record["request_sha256"], record["request_sha256"]);
        wait_for_claim_release(fixture).await;
        assert!(!Path::new(&prepared_image).exists());
        assert!(!Path::new(&prepared_configuration).exists());
        assert_eq!(
            fs::read_to_string(fixture.join("mutations")).unwrap(),
            completed_mutations
        );
        // The restarted daemon must deserialize the same checked request and
        // return its terminal receipt even though its source server is gone.
        daemon.stop().await;
        daemon = TestDaemon::start(fixture).await;
        let repeated = deliver(&task, &config)
            .await
            .expect("observe the completed request after supervisor restart");
        assert_eq!(
            serde_json::to_value(repeated).unwrap(),
            serde_json::to_value(complete).unwrap()
        );
        assert_eq!(journal_record(fixture, &task.operation_id), complete_record);
        assert_eq!(
            fs::read_to_string(fixture.join("mutations")).unwrap(),
            completed_mutations
        );
        assert_eq!(
            fs::read_to_string(fixture.join("verification-attempts")).unwrap(),
            "verify\nverify\n"
        );
        assert!(!fixture.join("state/mutation.json").exists());
        assert_eq!(fetches.load(Ordering::SeqCst), 4);
        daemon.stop().await;
    })
    .await
    .expect("checked Scout update exceeded its independent watchdog");
}

/// Checked admission must reach the strict resolver and live PCI comparison
/// before any mutation, retaining the inputs that were fully staged.
#[tokio::test]
async fn checked_update_rejects_redirects_and_mismatched_pci_before_mutation() {
    #[derive(Clone, Copy, Debug)]
    enum Rejection {
        Redirect,
        PciMismatch,
    }

    for rejection in [Rejection::Redirect, Rejection::PciMismatch] {
        timeout(TEST_TIMEOUT, async {
            let directory = private_fixture();
            let fixture = directory.path();
            fake_tools(fixture).await;
            let (base_url, fetches, mut server) = artifact_server().await;
            let mut task = update_task(&base_url);
            match rejection {
                Rejection::Redirect => {
                    // The redirect destination serves the expected image, so
                    // following it would pass both digest checks and mutate.
                    task.profile
                        .as_mut()
                        .unwrap()
                        .flash_spec
                        .as_mut()
                        .unwrap()
                        .firmware_url = format!("{base_url}/redirect-image.bin");
                }
                Rejection::PciMismatch => {
                    // Part, PSID, and both hashes still match. Only the live
                    // query's reported PCI function differs from the request.
                    fs::write(fixture.join("mismatched-pci"), "mismatch").unwrap();
                }
            }
            let expected_context = FirmwareUpdateContext {
                phase_id: task.phase_id.clone(),
                expected_digests: task.expected_digests.clone().unwrap().try_into().unwrap(),
            };
            let mut daemon = TestDaemon::start(fixture).await;
            let config = SupervisorConfig {
                state_directory: fixture.join("state"),
                supervisor_path: Some(supervisor_binary()),
                launch_mode: SupervisorLaunchMode::Standalone,
            };
            let failed = poll_until(&task, &config, FirmwareOperationState::Failed).await;
            assert_eq!(
                failed.phase,
                Some(FirmwareOperationPhase::Prepare),
                "{rejection:?}"
            );
            assert!(!failed.report.flashed, "{rejection:?}");
            let record = journal_record(fixture, &task.operation_id);
            assert_eq!(record["operation"], serde_json::to_value(&failed).unwrap());
            assert_eq!(record["mutation_started"], false);
            assert_eq!(
                record["requested"]["update_context"],
                serde_json::to_value(expected_context).unwrap()
            );
            assert!(!fixture.join("mutations").exists(), "{rejection:?}");
            wait_for_claim_release(fixture).await;
            match rejection {
                Rejection::Redirect => {
                    assert!(
                        failed
                            .diagnostic
                            .as_deref()
                            .unwrap()
                            .contains("status=Some(307)")
                    );
                    assert_eq!(
                        fetches.load(Ordering::SeqCst),
                        0,
                        "redirect destination was fetched"
                    );
                    assert!(record["prepared_inputs"].is_null());
                }
                Rejection::PciMismatch => {
                    assert!(
                        failed
                            .diagnostic
                            .as_deref()
                            .unwrap()
                            .contains("PCI address")
                    );
                    assert_eq!(fetches.load(Ordering::SeqCst), 2);
                    assert_retained_inputs(fixture, &record);
                }
            }
            timeout(WATCHDOG, server.shutdown())
                .await
                .expect("stop artifact server");
            daemon.stop().await;
        })
        .await
        .unwrap_or_else(|_| panic!("checked rejection {rejection:?} exceeded its watchdog"));
    }
}

/// Keep sockets short and private using the same fixture root as libmlx tests.
fn private_fixture() -> tempfile::TempDir {
    let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
        Some(root) => tempfile::tempdir_in(root),
        None => tempfile::tempdir(),
    }
    .expect("create checked firmware fixture");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for child in ["state", "tools"] {
        let path = directory.path().join(child);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

/// The normal test prerequisite builds this cross-package binary beside `deps`.
fn supervisor_binary() -> PathBuf {
    let executable = std::env::current_exe().unwrap();
    let helper = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mlx-firmware-supervisor");
    assert!(
        helper.is_file(),
        "run cargo make build-nic-firmware-test-helper before Scout unit tests"
    );
    helper
}

/// The test owns the actual daemon; a failed assertion releases its gated tools.
struct TestDaemon {
    process: Child,
    fixture: PathBuf,
}

impl TestDaemon {
    async fn start(fixture: &Path) -> Self {
        let process = Command::new(supervisor_binary())
            .args(["serve", "--device", DEVICE, "--state-directory"])
            .arg(fixture.join("state"))
            .env_clear()
            .env("PATH", fixture.join("tools"))
            .env("FIXTURE", fixture)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(fixture.join("daemon.log")).unwrap())
            .kill_on_drop(true)
            .spawn()
            .expect("start actual firmware supervisor");
        let mut daemon = Self {
            process,
            fixture: fixture.to_path_buf(),
        };
        let socket = fixture.join("state").join(SLOT).join("control.sock");
        timeout(WATCHDOG, async {
            loop {
                if let Some(status) = daemon.process.try_wait().unwrap() {
                    panic!(
                        "supervisor exited: {status}; {}",
                        fs::read_to_string(fixture.join("daemon.log")).unwrap()
                    );
                }
                if FirmwareSupervisorClient::connect(&socket).await.is_ok() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual firmware supervisor became ready");
        daemon
    }

    async fn stop(&mut self) {
        self.process.start_kill().expect("stop settled supervisor");
        timeout(WATCHDOG, self.process.wait())
            .await
            .expect("supervisor exits within watchdog")
            .expect("reap supervisor");
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        for gate in ["release", "release-verification"] {
            if let Err(error) = fs::write(self.fixture.join(gate), "release") {
                eprintln!("failed to release fake firmware tool gate {gate}: {error}");
            }
        }
    }
}

/// Strict tools reject unexpected commands and log mutations separately from
/// verification attempts, so retries cannot conceal a repeated burn or reset.
async fn fake_tools(fixture: &Path) {
    let script = r#"#!/bin/sh
set -eu
if [ "${1-}" = --version ]; then exit 0; fi
wait_for_release() {
    attempts=0
    while [ ! -f "$FIXTURE/$1" ]; do
        [ -d "$FIXTURE" ] && [ "$attempts" -lt 2000 ] || exit 43
        attempts=$((attempts + 1))
        /bin/sleep 0.01
    done
}
case "${0##*/}" in
    mlxfwmanager)
        [ "$#" -eq 4 ] && [ "$1" = --dev ] && [ "$2" = ffff:fe:1f.0 ] &&
            [ "$3" = --query-format ] && [ "$4" = xml ] || exit 40
        version=1.0
        if [ -f "$FIXTURE/reset-complete" ]; then version=2.0; fi
        pci=ffff:fe:1f.0
        if [ -f "$FIXTURE/mismatched-pci" ]; then pci=ffff:fe:1e.0; fi
        printf '<Devices><Device pciName="%s" type="BlueField3" partNumber="test-part" psid="test-psid"><Versions><FW current="%s"/></Versions></Device></Devices>' "$pci" "$version"
        ;;
    mlxconfig)
        [ "$#" -eq 5 ] && [ "$1" = -d ] && [ "$2" = ffff:fe:1f.0 ] &&
            [ "$3" = --yes ] && [ "$4" = apply ] && [ -s "$5" ] || exit 41
        printf '%s' "$5" > "$FIXTURE/prepared-configuration"
        printf 'configure\n' >> "$FIXTURE/mutations"
        ;;
    flint)
        [ "$1" = -d ] && [ "$2" = ffff:fe:1f.0 ] || exit 42
        if [ "$#" -eq 6 ] && [ "$3" = -y ] && [ "$4" = -i ] && [ -s "$5" ] && [ "$6" = burn ]; then
            printf 'burn\n' >> "$FIXTURE/mutations"
            printf '%s' "$5" > "$FIXTURE/prepared-image"
            wait_for_release release
        elif [ "$#" -eq 5 ] && [ "$3" = -i ] && [ -s "$4" ] && [ "$5" = verify ]; then
            printf 'verify\n' >> "$FIXTURE/verification-attempts"
            if [ ! -f "$FIXTURE/verification-failed" ]; then
                printf 'failed' > "$FIXTURE/verification-failed"
                printf 'device is still returning from reset\n' >&2
                exit 1
            fi
            printf 'retrying' > "$FIXTURE/verification-retried"
            wait_for_release release-verification
        else
            exit 42
        fi
        ;;
    mlxfwreset)
        [ "$#" -eq 6 ] && [ "$1" = --device ] && [ "$2" = ffff:fe:1f.0 ] &&
            [ "$3" = --level ] && [ "$4" = 3 ] && [ "$5" = reset ] && [ "$6" = -y ] || exit 45
        printf 'reset\n' >> "$FIXTURE/mutations"
        printf 'reset' > "$FIXTURE/reset-complete"
        ;;
    *) exit 44 ;;
esac
"#;
    // A separate writer avoids leaking an open executable into another test's
    // fork, which can otherwise make exec fail with ETXTBSY.
    let status = timeout(WATCHDOG, Command::new("/bin/sh")
        .args(["-c", "umask 077; for tool in mlxfwmanager mlxconfig flint mlxfwreset; do printf '%s' \"$2\" > \"$1/$tool\"; done", "fixture-writer"])
        .arg(fixture.join("tools"))
        .arg(script)
        .kill_on_drop(true)
        .status()).await.expect("tool writer exits within watchdog").expect("write fake tools");
    assert!(status.success());
    for tool in ["mlxfwmanager", "mlxconfig", "flint", "mlxfwreset"] {
        fs::set_permissions(
            fixture.join("tools").join(tool),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
}

/// Count downloads so repeated delivery cannot silently restage immutable work.
async fn artifact_server() -> (String, Arc<AtomicUsize>, JoinSet<std::io::Result<()>>) {
    let fetches = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/redirect-image.bin",
            get(|| async { Redirect::temporary("/image.bin") }),
        )
        .route(
            "/image.bin",
            get({
                let fetches = Arc::clone(&fetches);
                move || {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    async { IMAGE }
                }
            }),
        )
        .route(
            "/configuration.bin",
            get({
                let fetches = Arc::clone(&fetches);
                move || {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    async { CONFIGURATION }
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut server = JoinSet::new();
    server.spawn(async move { axum::serve(listener, app).await });
    (format!("http://{address}"), fetches, server)
}

fn update_task(base_url: &str) -> fac::MlxDeviceUpdateFirmware {
    let profile = FirmwareFlasherProfile {
        firmware_spec: FirmwareSpec {
            part_number: "test-part".into(),
            psid: "test-psid".into(),
            version: "2.0".into(),
        },
        flash_spec: FlashSpec {
            firmware_url: format!("{base_url}/image.bin"),
            firmware_credentials: None,
            device_conf_url: Some(format!("{base_url}/configuration.bin")),
            device_conf_credentials: None,
            verify_from_cache: false,
            cache_dir: None,
        },
        flash_options: FlashOptions {
            reset: true,
            verify_image: true,
            verify_version: true,
            post_reset_verify_retry_interval_seconds: Some(1),
            post_reset_verify_timeout_seconds: Some(20),
            ..Default::default()
        },
    };
    fac::MlxDeviceUpdateFirmware {
        operation_id: "checked-update".into(),
        phase_id: "firmware-phase-1".into(),
        profile: Some(profile.into()),
        expected_digests: Some(pb::FirmwareArtifactDigests {
            image_sha256: hex::encode(Sha256::digest(IMAGE.as_bytes())),
            configuration_sha256: Some(hex::encode(Sha256::digest(CONFIGURATION.as_bytes()))),
        }),
    }
}

/// Every poll uses the production dispatcher and publisher, never direct Get.
async fn deliver(
    task: &fac::MlxDeviceUpdateFirmware,
    config: &SupervisorConfig,
) -> Result<FirmwareOperation, pb::MlxDeviceUpdateFirmwareError> {
    let machine_id = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"
        .parse()
        .unwrap();
    let mut publication = None;
    crate::collect_and_publish_mlx_observations(
        &machine_id,
        vec![fac::MlxDeviceAction {
            pci_name: DEVICE.into(),
            command: Some(fac::mlx_device_action::Command::UpdateFirmware(
                task.clone(),
            )),
        }],
        Ok(config),
        |_| panic!("generic firmware results must not enter Scout's legacy device discovery"),
        |request| {
            publication = Some(request);
            std::future::ready(Ok(pb::PublishMlxObservationReportResponse::default()))
        },
    )
    .await;
    let report = publication
        .expect("Scout published the generic firmware result")
        .report
        .unwrap();
    assert_eq!(report.machine_id, Some(machine_id));
    assert!(report.timestamp.is_some());
    assert_eq!(report.observations.len(), 1);
    let observation = report.observations.into_iter().next().unwrap();
    let result = observation.firmware_update_result.clone().unwrap();
    assert_eq!(
        observation,
        pb::MlxObservation {
            firmware_update_result: Some(result.clone()),
            ..Default::default()
        }
    );
    assert_eq!(result.operation_id, task.operation_id);
    assert_eq!(result.phase_id, task.phase_id);
    assert_eq!(result.pci_name, DEVICE);
    match result
        .result
        .expect("a published firmware result must be present")
    {
        pb::mlx_device_update_firmware_result::Result::Receipt(receipt) => {
            assert_eq!(receipt.operation_id, task.operation_id);
            Ok(receipt
                .try_into()
                .expect("supervisor receipt decodes as a typed operation"))
        }
        pb::mlx_device_update_firmware_result::Result::Error(error) => Err(error),
    }
}

async fn poll_until(
    task: &fac::MlxDeviceUpdateFirmware,
    config: &SupervisorConfig,
    expected: FirmwareOperationState,
) -> FirmwareOperation {
    timeout(WATCHDOG, async {
        loop {
            let operation = deliver(task, config)
                .await
                .expect("observe accepted firmware work");
            if operation.state == expected {
                return operation;
            }
            assert!(
                matches!(
                    operation.state,
                    FirmwareOperationState::Accepted | FirmwareOperationState::Running
                ),
                "unexpected receipt: {operation:?}"
            );
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Scout observed the expected terminal firmware receipt")
}

fn journal_record(fixture: &Path, operation_id: &str) -> serde_json::Value {
    let ledger: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.join("state").join(SLOT).join("operations.json")).unwrap(),
    )
    .unwrap();
    ledger["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["operation"]["operation_id"] == operation_id)
        .expect("admitted operation has a durable record")
        .clone()
}

/// A receipt's actual digests must describe the retained bytes under its slot,
/// including when a mismatched expected digest or identity stopped preparation.
fn assert_retained_inputs(fixture: &Path, record: &serde_json::Value) {
    for (kind, bytes) in [
        ("image", IMAGE.as_bytes()),
        ("configuration", CONFIGURATION.as_bytes()),
    ] {
        let artifact = &record["prepared_inputs"][kind];
        let relative = Path::new(artifact["path"].as_str().unwrap());
        assert!(!relative.is_absolute());
        let owned = fixture.join("state").join(SLOT).join(relative);
        assert_eq!(fs::read(owned).unwrap(), bytes);
        assert_eq!(
            artifact["sha256"],
            serde_json::to_value(Sha256::digest(bytes).to_vec()).unwrap()
        );
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
    .expect("fake firmware tool published its staged input")
}

async fn wait_for_claim_release(fixture: &Path) {
    timeout(WATCHDOG, async {
        while fixture.join("state/mutation.json").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("settled firmware work released host exclusion")
}
