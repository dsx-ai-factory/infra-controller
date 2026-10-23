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

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use libmlx::firmware::operation::{FirmwareOperation, FirmwareOperationState};

use super::cli_fixture::{
    DEVICE, OPERATION_ID, Supervisor, WATCHDOG, cli, fixture_directory, run_cli, write_executable,
};

const TOOL: &str = r#"#!/bin/sh
set -eu
if [ "${1-}" = --version ]; then exit 0; fi
case "${0##*/}" in
    mlxfwmanager)
        phase=query
        if [ -f "$FIXTURE/burned" ]; then phase=final-query; fi ;;
    mlxconfig) phase=apply; path=$5 ;;
    flint)
        for phase in "$@"; do :; done
        if [ "$phase" = burn ]; then path=$5; else path=$4; fi ;;
    mlxfwreset) phase=reset ;;
    *) exit 40 ;;
esac
[ "$2" = "$DEVICE" ]
printf '%s\n' "$phase" >> "$FIXTURE/calls"
if [ "$phase" = reset ]; then printf '%s\n' "$@" > "$FIXTURE/reset-args"; fi
if [ "$phase" = burn ]; then printf '%s\n' "$path" > "$FIXTURE/burn-input"; fi
if [ "$phase" = "$FAIL_PHASE" ]; then exit 41; fi
case "$phase" in
    query|final-query)
        version=32.42.1000
        if [ "$phase" = final-query ]; then version=32.43.1014; fi
        printf '<Devices><Device pciName="%s" type="BlueField3" partNumber="part" psid="PSID"><Versions><FW current="%s"/></Versions></Device></Devices>' "$DEVICE" "$version" ;;
    apply)
        IFS= read -r contents < "$path"
        [ "$contents" = device-config ]
        printf '%s\n' "$path" > "$FIXTURE/applied" ;;
    burn)
        IFS= read -r contents < "$path"
        [ "$contents" = firmware-image ]
        IFS= read -r config_path < "$FIXTURE/applied"
        [ "${path%/*}" != "${config_path%/*}" ]
        [ "$path" != "$FIXTURE/image/shared.bin" ]
        printf '%s\n' "$path" > "$FIXTURE/burned"
        printf 'changed source\n' > "$FIXTURE/image/shared.bin" ;;
    verify)
        IFS= read -r burned_path < "$FIXTURE/burned"
        [ "$path" = "$burned_path" ]
        IFS= read -r contents < "$path"
        [ "$contents" = firmware-image ] ;;
    reset) : ;;
    *) exit 42 ;;
esac
"#;

/// Keep matching input basenames and nondefault reset level visible: these prove
/// that the CLI passes the complete profile to the real executor.
fn fixture() -> tempfile::TempDir {
    let directory = fixture_directory();
    let fixture = directory.path();
    for child in ["tools", "image", "config"] {
        fs::create_dir(fixture.join(child)).unwrap();
    }
    for tool in ["mlxconfig", "flint", "mlxfwreset", "mlxfwmanager"] {
        write_executable(&fixture.join("tools").join(tool), TOOL);
    }
    let image = fixture.join("image/shared.bin");
    let configuration = fixture.join("config/shared.bin");
    fs::write(&image, "firmware-image\n").unwrap();
    fs::write(&configuration, "device-config\n").unwrap();
    fs::write(
        fixture.join("profile.toml"),
        format!(
            "part_number = 'part'\npsid = 'PSID'\nversion = '32.43.1014'\n\
             firmware_url = '{}'\ndevice_conf_url = '{}'\n\
             reset = true\nreset_level = 1\nverify_image = true\nverify_version = true\n",
            image.display(),
            configuration.display(),
        ),
    )
    .unwrap();
    directory
}

fn wait_for_staged_cleanup(image: &Path, configuration: &Path) {
    // `Complete` is durable before cleanup, so a receipt can arrive first.
    let deadline = Instant::now() + WATCHDOG;
    while image.try_exists().unwrap() || configuration.try_exists().unwrap() {
        assert!(
            Instant::now() < deadline,
            "successful work did not release staged inputs within the watchdog"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Both mutation commands use durable receipts. An identical invocation reads
/// accepted work without another burn, including after a failed activation.
#[test]
fn firmware_cli_preserves_receipts_and_does_not_replay_mutations() {
    struct Case {
        name: &'static str,
        flash_config: bool,
        failed_phase: &'static str,
        expected_state: FirmwareOperationState,
        expected_flashed: bool,
        expected_reset: Option<bool>,
        expected_calls: &'static str,
    }
    for case in [
        // Full profile options, including reset level, reach the executor.
        Case {
            name: "profile lifecycle completes",
            flash_config: true,
            failed_phase: "",
            expected_state: FirmwareOperationState::Complete,
            expected_flashed: true,
            expected_reset: Some(true),
            expected_calls: "query\napply\nburn\nreset\nverify\nfinal-query\n",
        },
        // The explicit flash command does not silently add activation phases.
        Case {
            name: "explicit flash completes without reset",
            flash_config: false,
            failed_phase: "",
            expected_state: FirmwareOperationState::Complete,
            expected_flashed: true,
            expected_reset: None,
            expected_calls: "query\napply\nburn\n",
        },
        // A successful burn followed by failure must remain visible on retry.
        Case {
            name: "activation failure retains completed burn",
            flash_config: true,
            failed_phase: "reset",
            expected_state: FirmwareOperationState::ManualIntervention,
            expected_flashed: true,
            expected_reset: Some(false),
            expected_calls: "query\napply\nburn\nreset\n",
        },
        // A nonzero burn can have changed hardware and must not activate or replay.
        Case {
            name: "failed burn retains inputs without activation",
            flash_config: true,
            failed_phase: "burn",
            expected_state: FirmwareOperationState::ManualIntervention,
            expected_flashed: false,
            expected_reset: Some(false),
            expected_calls: "query\napply\nburn\n",
        },
        // Exhausting verification must retain the completed burn and reset,
        // without running the final version query or retrying either mutation.
        Case {
            name: "verification budget exhaustion retains completed activation",
            flash_config: true,
            failed_phase: "verify",
            expected_state: FirmwareOperationState::ManualIntervention,
            expected_flashed: true,
            expected_reset: Some(true),
            expected_calls: "query\napply\nburn\nreset\nverify\n",
        },
    ] {
        let directory = fixture();
        let fixture = directory.path();
        if case.failed_phase == "verify" {
            // Keep the default ten-second retry interval, but exhaust this
            // operation's budget before a second read. The independent CLI
            // watchdog still catches a workflow that never reaches terminal.
            let profile_path = fixture.join("profile.toml");
            let profile = fs::read_to_string(&profile_path).unwrap();
            fs::write(
                profile_path,
                format!("{profile}post_reset_verify_timeout_seconds = 2\n"),
            )
            .unwrap();
        }
        let _supervisor = Supervisor::start(fixture, case.failed_phase);
        let command = || {
            let mut command = cli(fixture);
            if case.flash_config {
                command
                    .args(["flash-config", DEVICE])
                    .arg(fixture.join("profile.toml"));
            } else {
                command
                    .args([
                        "flash",
                        DEVICE,
                        "--part-number",
                        "part",
                        "--psid",
                        "PSID",
                        "--version",
                        "32.43.1014",
                    ])
                    .arg(fixture.join("image/shared.bin"))
                    .arg("--device-conf-url")
                    .arg(fixture.join("config/shared.bin"));
            }
            command.args(["--operation-id", OPERATION_ID]);
            command
        };
        let output = run_cli(&mut command());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        let operation: FirmwareOperation = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{}: {error}: {diagnostic}", case.name));
        assert_eq!(operation.operation_id, OPERATION_ID, "{}", case.name);
        assert_eq!(operation.state, case.expected_state, "{}", case.name);
        assert_eq!(
            operation.report.flashed, case.expected_flashed,
            "{}",
            case.name
        );
        assert_eq!(operation.report.reset, case.expected_reset, "{}", case.name);
        if case.failed_phase == "verify" {
            let diagnostic = operation
                .diagnostic
                .as_deref()
                .expect("verification exhaustion retains its last failed read");
            assert!(
                diagnostic.contains("post-reset firmware verification exceeded its 2s budget"),
                "{diagnostic}"
            );
            assert!(diagnostic.contains("ImageVerification"), "{diagnostic}");
            assert!(diagnostic.contains("exit_code=Some(41)"), "{diagnostic}");
        }
        assert_eq!(
            output.status.success(),
            operation.is_complete(),
            "{diagnostic}"
        );
        assert_eq!(
            fs::read_to_string(fixture.join("calls")).unwrap(),
            case.expected_calls
        );

        // The same request and a separate status command must return the same
        // durable receipt, even though the fake burn changed the source image.
        for mut request in [command(), {
            let mut status = cli(fixture);
            status.args(["status", DEVICE, OPERATION_ID]);
            status
        }] {
            let reread = run_cli(&mut request);
            let reread: serde_json::Value = serde_json::from_slice(&reread.stdout).unwrap();
            assert_eq!(
                reread,
                serde_json::to_value(&operation).unwrap(),
                "{}",
                case.name
            );
            assert_eq!(
                fs::read_to_string(fixture.join("calls")).unwrap(),
                case.expected_calls
            );
        }

        if case.expected_calls.contains("reset\n") {
            assert_eq!(
                fs::read_to_string(fixture.join("reset-args")).unwrap(),
                format!("--device\n{DEVICE}\n--level\n1\nreset\n-y\n")
            );
        }
        let staged_image = fs::read_to_string(fixture.join("burn-input")).unwrap();
        let staged_image = std::path::Path::new(staged_image.trim());
        let staged_config = fs::read_to_string(fixture.join("applied")).unwrap();
        let staged_config = std::path::Path::new(staged_config.trim());
        if operation.is_complete() {
            wait_for_staged_cleanup(staged_image, staged_config);
        } else {
            assert_eq!(fs::read(staged_image).unwrap(), b"firmware-image\n");
            assert_eq!(fs::read(staged_config).unwrap(), b"device-config\n");
            assert_eq!(operation.report.verified_image, Some(false));
            assert_eq!(operation.report.verified_version, Some(false));
            if case.failed_phase == "verify" {
                // Completed activation does not release exclusion when the
                // requested verification has exhausted its budget.
                let claim: serde_json::Value =
                    serde_json::from_slice(&fs::read(fixture.join("state/mutation.json")).unwrap())
                        .unwrap();
                assert_eq!(claim["operation_id"], OPERATION_ID);
                assert_eq!(claim["device_identity"], "0000:ab:01");
            }
        }
    }
}

/// A reused supervisor must stage the submitting process's local sources even
/// when its original working directory contains files with matching names.
#[test]
fn firmware_cli_resolves_sources_before_reusing_a_supervisor() {
    let directory = fixture();
    let fixture = directory.path();
    fs::write(fixture.join("image/shared.bin"), "wrong firmware\n").unwrap();
    fs::write(fixture.join("config/shared.bin"), "wrong configuration\n").unwrap();
    let submitting_directory = fixture.join("submitter");
    for child in ["image", "config"] {
        fs::create_dir_all(submitting_directory.join(child)).unwrap();
    }
    fs::write(
        submitting_directory.join("image/shared.bin"),
        "firmware-image\n",
    )
    .unwrap();
    fs::write(
        submitting_directory.join("config/shared.bin"),
        "device-config\n",
    )
    .unwrap();
    fs::write(
        submitting_directory.join("profile.toml"),
        "part_number = 'part'\npsid = 'PSID'\nversion = '32.43.1014'\n\
         firmware_url = 'image/shared.bin'\ndevice_conf_url = 'file://config/shared.bin'\n",
    )
    .unwrap();
    let _supervisor = Supervisor::start(fixture, "");
    let output = run_cli(cli(fixture).current_dir(&submitting_directory).args([
        "flash-config",
        DEVICE,
        "profile.toml",
        "--operation-id",
        OPERATION_ID,
    ]));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let operation: FirmwareOperation = serde_json::from_slice(&output.stdout).unwrap();
    assert!(operation.is_complete());
    assert!(operation.report.flashed);
    assert_eq!(
        fs::read_to_string(fixture.join("calls")).unwrap(),
        "query\napply\nburn\n"
    );
    // The fake tools checked the exact image and configuration bytes staged
    // from the submitter, and the originals remain independent of those copies.
    assert_eq!(
        fs::read(submitting_directory.join("image/shared.bin")).unwrap(),
        b"firmware-image\n"
    );
    assert_eq!(
        fs::read(submitting_directory.join("config/shared.bin")).unwrap(),
        b"device-config\n"
    );

    let staged_image = fs::read_to_string(fixture.join("burn-input")).unwrap();
    let staged_configuration = fs::read_to_string(fixture.join("applied")).unwrap();
    wait_for_staged_cleanup(
        Path::new(staged_image.trim()),
        Path::new(staged_configuration.trim()),
    );
    let ledger = fixture.join("state/0000:ab:01/operations.json");
    let saved_ledger = fs::read(&ledger).unwrap();
    let calls = fs::read(fixture.join("calls")).unwrap();
    let other_image = fs::read(fixture.join("image/shared.bin")).unwrap();

    // The same relative names now identify the other directory's files.
    let conflict = run_cli(
        cli(fixture)
            .current_dir(fixture)
            .args(["flash-config", DEVICE])
            .arg(submitting_directory.join("profile.toml"))
            .args(["--operation-id", OPERATION_ID]),
    );
    assert!(!conflict.status.success());
    assert!(
        String::from_utf8_lossy(&conflict.stderr)
            .contains("firmware operation ID identifies different work")
    );
    let status = run_cli(cli(fixture).args(["status", DEVICE, OPERATION_ID]));
    assert!(status.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&status.stdout).unwrap(),
        serde_json::to_value(operation).unwrap()
    );
    assert_eq!(fs::read(ledger).unwrap(), saved_ledger);
    assert_eq!(fs::read(fixture.join("calls")).unwrap(), calls);
    assert_eq!(
        fs::read(fixture.join("image/shared.bin")).unwrap(),
        other_image
    );
    assert_eq!(
        fs::read(submitting_directory.join("image/shared.bin")).unwrap(),
        b"firmware-image\n"
    );
    assert!(
        fs::read_dir(fixture.join("state/0000:ab:01"))
            .unwrap()
            .all(|entry| !entry.unwrap().file_type().unwrap().is_dir())
    );
}

/// Dry-run remains a read-only device query and never creates an operation
/// journal or requires a recovery ID.
#[test]
fn dry_run_firmware_does_not_admit_work() {
    let directory = fixture();
    let fixture = directory.path();
    let profile = fixture.join("profile.toml");
    let mut content = fs::read_to_string(&profile).unwrap();
    content.push_str(&format!(
        "cache_dir = '{}'\n",
        fixture.join("profile-work").display()
    ));
    fs::write(&profile, content).unwrap();
    let work_dir = fixture.join("requested-work");
    let output = run_cli(
        cli(fixture)
            .args(["--dry-run", "--work-dir"])
            .arg(&work_dir)
            .args(["flash-config", DEVICE])
            .arg(profile),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.join("calls")).unwrap(),
        "query\n"
    );
    assert!(!fixture.join("state").exists());
    assert!(
        work_dir.is_dir(),
        "explicit work directory receives preparation"
    );
    assert_eq!(fs::read_dir(work_dir).unwrap().count(), 0);
    assert!(
        !fixture.join("profile-work").exists(),
        "the CLI override takes precedence"
    );
}

/// Required mutation identity and meaningless receipt flags reject before
/// opening a profile, launching a supervisor, or touching the selected device.
#[test]
fn firmware_cli_rejects_missing_identity_and_receipt_flags() {
    for (arguments, diagnostic) in [
        // A missing profile path must not hide the more important recovery-ID error.
        (
            vec!["flash-config", DEVICE, "/missing/profile.toml"],
            "--operation-id is required",
        ),
        // An explicit empty string is not a usable retry identity.
        (
            vec![
                "flash-config",
                DEVICE,
                "/missing/profile.toml",
                "--operation-id",
                "",
            ],
            "--operation-id is required",
        ),
        // Dry-run cannot silently turn into a cancellation mutation.
        (
            vec!["--dry-run", "cancel", DEVICE, OPERATION_ID],
            "status and cancel do not accept --dry-run or --work-dir",
        ),
        // Durable preparation is owned by the journal, not an ephemeral CLI path.
        (
            vec![
                "--work-dir",
                "/unused/work",
                "flash-config",
                DEVICE,
                "/missing/profile.toml",
                "--operation-id",
                OPERATION_ID,
            ],
            "durable firmware work uses MLX_FIRMWARE_STATE_DIRECTORY, not --work-dir",
        ),
        // Receipt lookup cannot accept an ignored preparation override.
        (
            vec!["--work-dir", "/unused/work", "status", DEVICE, OPERATION_ID],
            "status and cancel do not accept --dry-run or --work-dir",
        ),
    ] {
        let directory = fixture_directory();
        let output = run_cli(cli(directory.path()).args(arguments));
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        assert!(!directory.path().join("state").exists());
    }
}

/// Standalone verification must query the device and reject a mismatch, even
/// though it does not run the flash lifecycle or print a successful observation.
#[test]
fn verify_version_requires_the_live_target() {
    struct Case {
        scenario: &'static str,
        observed_version: &'static str,
        exit_code: i32,
        diagnostic: Option<&'static str>,
    }
    for case in [
        // A matching live version succeeds after both admission and verification queries.
        Case {
            scenario: "matching live version",
            observed_version: "32.43.1014",
            exit_code: 0,
            diagnostic: None,
        },
        // Identity admission alone must not make the standalone command succeed.
        Case {
            scenario: "different live version",
            observed_version: "32.42.1000",
            exit_code: 1,
            diagnostic: Some(
                "firmware version mismatch on 'ab:01.0': expected '32.43.1014', found '32.42.1000'",
            ),
        },
    ] {
        let directory = tempfile::tempdir().unwrap();
        let fixture = directory.path();
        let query = fixture.join("mlxfwmanager");
        write_executable(
            &query,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$@" >> "$FIXTURE/calls"
printf '<Devices><Device pciName="ab:01.0" type="BlueField3" partNumber="part" psid="PSID"><Versions><FW current="%s"/></Versions></Device></Devices>' "$OBSERVED_VERSION"
"#,
        );
        // Only the read-only query tool is available. Any mutation would fail
        // rather than reaching an installed system utility.
        let mut command = Command::new(env!("CARGO_BIN_EXE_mlxconfig-embedded"));
        command
            .env_clear()
            .env("PATH", fixture)
            .env("FIXTURE", fixture)
            .env("OBSERVED_VERSION", case.observed_version)
            .args([
                "--log-level",
                "error",
                "firmware",
                "verify-version",
                "ab:01.0",
                "32.43.1014",
                "--part-number",
                "part",
                "--psid",
                "PSID",
            ]);
        let output = run_cli(&mut command);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(case.exit_code),
            "{}: {stderr}",
            case.scenario
        );
        assert_eq!(
            fs::read_to_string(fixture.join("calls")).unwrap(),
            "--dev\nab:01.0\n--query-format\nxml\n--dev\nab:01.0\n--query-format\nxml\n",
            "{}: {stderr}",
            case.scenario
        );
        if let Some(diagnostic) = case.diagnostic {
            assert!(stderr.contains(diagnostic), "{}: {stderr}", case.scenario);
        }
    }
}
