// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Real wait failures must preserve the direct command's durable exclusion.

use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use std::{fs, io};

use libmlx::device::discovery::{DeviceDiscoveryError, discover_device_with_settlement};
use libmlx::firmware::operation::{SupervisorConfig, has_unresolved_firmware_claim};
use libmlx::lockdown::error::MlxError;
use libmlx::lockdown::runner::FlintRunner;
use libmlx::runner::command_builder::CommandSpec;
use libmlx::runner::error::MlxRunnerError;
use libmlx::runner::exec_options::ExecOptions;
use libmlx::runner::executor::CommandExecutor;
use rpc::protos::mlx_device as mlx_device_pb;
use tokio::process::Command;
use tokio::time::timeout;

use super::{DirectMlxAccess, DirectMlxAccessError};

const CHILD_DIRECTORY: &str = "SCOUT_MLX_WAIT_TEST_DIRECTORY";
const WATCHDOG: Duration = Duration::from_secs(10);
const DEVICE: &str = "ffff:fe:1f.0";
// This module is Linux-only. ECHILD is 10 in the Linux errno ABI; naming it
// here avoids a new syscall dependency for an error-code assertion.
const ECHILD: i32 = 10;

/// Every direct wait producer must preserve a lost exit status. Otherwise an
/// ordinary-looking failure could release the host or retry a hardware command.
#[test]
fn lost_process_results_retain_direct_hardware_exclusion() {
    let Some(directory) = std::env::var_os(CHILD_DIRECTORY).map(PathBuf::from) else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build direct process watchdog runtime")
            .block_on(async {
        // Both PATH and SIGCHLD belong to this subprocess only. Creating the
        // scripts there also prevents concurrent test forks from inheriting
        // their writable descriptors and causing ETXTBSY during execution.
        // Bash preserves ignored SIGCHLD across exec; dash resets it.
        let directory = match std::env::var_os("LIBMLX_TEST_STATE_DIRECTORY") {
            Some(root) => tempfile::tempdir_in(root),
            None => tempfile::tempdir(),
        }
        .expect("create private direct process fixture");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let log_path = directory.path().join("child.log");
        let log = fs::File::create(&log_path).unwrap();
        let mut child = Command::new("/bin/bash")
            .args(["-c", "trap '' CHLD; exec \"$@\"", "ignored-sigchld"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mlx_access::process_tests::lost_process_results_retain_direct_hardware_exclusion",
                "--nocapture",
            ])
            .env(CHILD_DIRECTORY, directory.path())
            .env("PATH", directory.path().join("tools"))
            .env("TMPDIR", directory.path())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .expect("start isolated direct process test");
        let result = timeout(WATCHDOG, child.wait()).await;
        let status = match result {
            Ok(Ok(status)) => status,
            error => {
                let stop = child.start_kill();
                let reap = timeout(WATCHDOG, child.wait()).await;
                panic!(
                    "direct process test did not finish: {error:?}; stop: {stop:?}; reap: {reap:?}\n{}",
                    fs::read_to_string(&log_path).unwrap()
                );
            }
        };
        let output = fs::read_to_string(log_path).unwrap();
        assert!(
            status.success() && output.contains("1 passed; 0 failed"),
            "isolated direct process test failed: {status}\n{output}"
        );
            });
        return;
    };

    let tools = directory.join("tools");
    fs::create_dir(&tools).unwrap();
    let calls = directory.join("tool-calls");
    fs::write(&calls, "").unwrap();
    for program in ["mlxfwmanager", "mlxconfig", "flint"] {
        let path = tools.join(program);
        fs::write(
            &path,
            r#"#!/bin/sh
printf '%s\n' "${0##*/}" >> "$SCOUT_MLX_WAIT_TEST_DIRECTORY/tool-calls"
exit 0
"#,
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    struct Case {
        scenario: &'static str,
        program: &'static str,
        run: fn(&Path) -> Box<dyn Error>,
    }

    let cases = [
        Case {
            // Discovery waits before parsing XML, and wraps the runner error.
            scenario: "direct discovery",
            program: "mlxfwmanager",
            run: |_| {
                let error = discover_device_with_settlement(DEVICE).unwrap_err();
                assert!(matches!(
                    error,
                    DeviceDiscoveryError::DirectQuery(MlxRunnerError::ProcessUnsettled { .. })
                ));
                Box::new(error)
            },
        },
        Case {
            // Losing the first completion must stop the configured retry loop.
            scenario: "runner retries",
            program: "mlxconfig",
            run: |_| {
                // wait-timeout installs a SIGCHLD handler. This proof needs
                // the direct wait path so Linux discards the child's status.
                let options = ExecOptions {
                    timeout: None,
                    retries: 3,
                    retry_delay: Duration::from_millis(1),
                    max_retry_delay: Duration::from_millis(1),
                    ..ExecOptions::default()
                };
                let executor = CommandExecutor { options: &options };
                let command = CommandSpec::new("mlxconfig").args(["-d", DEVICE, "set"]);
                let error = executor.execute_with_retry(&command).unwrap_err();
                assert!(matches!(error, MlxRunnerError::ProcessUnsettled { .. }));
                Box::new(error)
            },
        },
        Case {
            // Tool lookup has its own wait, before any lockdown command runs.
            scenario: "lockdown version probe",
            program: "flint",
            run: |_| {
                let error = match FlintRunner::new() {
                    Ok(_) => panic!("version probe must lose its exit status"),
                    Err(error) => error,
                };
                assert!(matches!(
                    error,
                    MlxError::ProcessUnsettled {
                        operation: "version probe",
                        ..
                    }
                ));
                Box::new(error)
            },
        },
        Case {
            // Execution uses a separate wait from the version probe above.
            scenario: "lockdown execution",
            program: "flint",
            run: |tools| {
                let runner = FlintRunner::with_path(tools.join("flint").to_str().unwrap());
                let error = runner
                    .enable_hw_access(DEVICE, "0123456789abcdef")
                    .unwrap_err();
                assert!(matches!(
                    error,
                    MlxError::ProcessUnsettled {
                        operation: "enable",
                        ..
                    }
                ));
                Box::new(error)
            },
        },
    ];

    for (index, case) in cases.into_iter().enumerate() {
        let config = SupervisorConfig {
            state_directory: directory.join(format!("state-{index}")),
            ..SupervisorConfig::default()
        };
        let mut access = DirectMlxAccess::acquire(&config, DEVICE).unwrap();
        let marker = config.state_directory.join("mutation.json");
        let original_claim = fs::read(&marker).unwrap();
        let error = (case.run)(&tools);

        // The OS error must survive the production wrappers, not merely the
        // final display string. A fake tool exits once; SIGCHLD is ignored, so
        // waiting for its status returns ECHILD instead of a successful exit.
        let mut source = Some(error.as_ref());
        let mut wait_error = None;
        while let Some(error) = source {
            if let Some(error) = error.downcast_ref::<io::Error>() {
                wait_error = error.raw_os_error();
                break;
            }
            source = error.source();
        }
        assert_eq!(wait_error, Some(ECHILD), "{}: {error}", case.scenario);
        let recorded = fs::read_to_string(&calls).unwrap();
        let starts: Vec<_> = recorded.lines().collect();
        assert_eq!(starts.len(), index + 1, "{}: {starts:?}", case.scenario);
        assert_eq!(starts[index], case.program, "{}", case.scenario);

        access.observe_error(error.as_ref());
        assert!(
            matches!(access.finish(), Err(DirectMlxAccessError::ProcessUnsettled)),
            "{}",
            case.scenario
        );
        assert!(
            has_unresolved_firmware_claim(&config).unwrap(),
            "{}",
            case.scenario
        );
        let next = DirectMlxAccess::acquire(&config, "ffff:fe:1e.0");
        assert!(
            matches!(next, Err(DirectMlxAccessError::Acquire(ref error))
                if error.kind() == io::ErrorKind::AlreadyExists),
            "{}: another device must remain excluded",
            case.scenario
        );
        assert_eq!(
            fs::read(&marker).unwrap(),
            original_claim,
            "{}",
            case.scenario
        );
    }

    // Filtered profile validation discovers the device before mlxconfig runs.
    // The handler must preserve that wait failure through the profile wrapper
    // and observe it before returning the protobuf error.
    assert!(
        libmlx::registry::registries::get("mlx_generic")
            .expect("built-in mlx_generic registry")
            .has_filters()
    );
    let config = SupervisorConfig {
        state_directory: directory.join("state-profile"),
        ..SupervisorConfig::default()
    };
    let mut access = DirectMlxAccess::acquire(&config, DEVICE).unwrap();
    let marker = config.state_directory.join("mutation.json");
    let original_claim = fs::read(&marker).unwrap();
    let previous_call_count = fs::read_to_string(&calls).unwrap().lines().count();
    let response = crate::mlx_device::handle_profile_sync(
        mlx_device_pb::MlxDeviceProfileSyncRequest {
            device_id: DEVICE.to_string(),
            profile_name: "lost-wait-profile".to_string(),
            serializable_profile: Some(mlx_device_pb::SerializableMlxConfigProfile {
                name: "lost-wait-profile".to_string(),
                registry_name: "mlx_generic".to_string(),
                description: None,
                config: [("SRIOV_EN".to_string(), "true".to_string())].into(),
            }),
        },
        &mut access,
    );
    let Some(mlx_device_pb::mlx_device_profile_sync_response::Reply::Error(error)) = response.reply
    else {
        panic!("profile validation must report the lost discovery result");
    };
    assert_eq!(
        error.status,
        mlx_device_pb::MlxDeviceStreamErrorStatus::Internal as i32
    );
    assert!(
        error.message.contains("could not establish termination"),
        "{}",
        error.message
    );
    let recorded = fs::read_to_string(&calls).unwrap();
    let starts: Vec<_> = recorded.lines().collect();
    assert_eq!(&starts[previous_call_count..], ["mlxfwmanager"]);
    assert!(matches!(
        access.finish(),
        Err(DirectMlxAccessError::ProcessUnsettled)
    ));
    let next = DirectMlxAccess::acquire(&config, "ffff:fe:1e.0");
    assert!(
        matches!(next, Err(DirectMlxAccessError::Acquire(ref error))
            if error.kind() == io::ErrorKind::AlreadyExists),
        "profile validation must keep another device excluded"
    );
    assert_eq!(fs::read(&marker).unwrap(), original_claim);
}
