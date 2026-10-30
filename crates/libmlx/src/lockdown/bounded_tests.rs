// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;
use std::path::Path;

use super::*;
use crate::runner::attempt::{AttemptError, AttemptFailure};

/// Select the public bounded entry point without bypassing manager validation.
enum Operation {
    Query,
    Lock,
    Unlock,
}

impl Operation {
    /// Keep each fixture's runner, credentials, executor, and deadline explicit.
    async fn run(
        self,
        manager: &LockdownManager,
        device: &str,
        key: &str,
        executor: &FirmwareToolExecutor,
        timeout: Duration,
    ) -> MlxResult<LockStatus> {
        match self {
            Self::Query => manager.get_status_bounded(device, executor, timeout).await,
            Self::Lock => {
                manager
                    .lock_device_bounded(device, key, executor, timeout)
                    .await
            }
            Self::Unlock => {
                manager
                    .unlock_device_bounded(device, key, executor, timeout)
                    .await
            }
        }
    }
}

/// Write a fake vendor executable without leaving a writable descriptor in the
/// test process for another test's fork to inherit and cause ETXTBSY on launch.
async fn write_fixture_script(executable: &Path, script: &str) {
    let writer = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("/bin/sh")
            .args([
                "-c",
                "set -eu; umask 077; printf '%s' \"$2\" > \"$1\"; chmod 700 \"$1\"",
                "fixture-writer",
            ])
            .arg(executable)
            .arg(script)
            .kill_on_drop(true)
            .status(),
    )
    .await
    .expect("fixture writer must finish within its watchdog")
    .unwrap();
    assert!(writer.success(), "{writer}");
}

/// Bounded public calls must reach the requested vendor operation and preserve
/// its status or classified error, including nonzero exits with access markers.
#[tokio::test]
async fn bounded_access_calls_preserve_vendor_arguments_and_results() {
    struct Case {
        name: &'static str,
        operation: Operation,
        response: &'static str,
        check: fn(&MlxResult<LockStatus>, &str) -> bool,
        expected_args: &'static [&'static str],
    }

    let cases = [
        // An accessible card's query succeeds without a key or mutation.
        Case {
            name: "query unlocked card",
            operation: Operation::Query,
            response: "exit 0",
            check: |result, _| matches!(result, Ok(LockStatus::Unlocked)),
            expected_args: &["-d", "0000:03:00.0", "q"],
        },
        // Disabled hardware access is a status report, not a query failure.
        Case {
            name: "query locked card",
            operation: Operation::Query,
            response: "printf '%s\\n' 'HW access is disabled' >&2; exit 1",
            check: |result, _| matches!(result, Ok(LockStatus::Locked)),
            expected_args: &["-d", "0000:03:00.0", "q"],
        },
        // A failed query without the disabled marker must not imply Locked.
        Case {
            name: "ordinary query failure",
            operation: Operation::Query,
            response: "printf '%s\\n' 'query failed' >&2; exit 1",
            check: |result, _| {
                matches!(
                    result,
                    Err(MlxError::CommandFailed(message))
                        if message == "stdout: \nstderr: query failed"
                )
            },
            expected_args: &["-d", "0000:03:00.0", "q"],
        },
        // Locking must use disable and retain the caller's exact key argument.
        Case {
            name: "lock card",
            operation: Operation::Lock,
            response: "exit 0",
            check: |result, _| matches!(result, Ok(LockStatus::Locked)),
            expected_args: &[
                "-d",
                "0000:03:00.0",
                "hw_access",
                "disable",
                "0123aBcD4567eF89",
            ],
        },
        // A collected nonzero exit still reaches key-command classification;
        // the Already result must not retain the credential echoed by flint.
        Case {
            name: "lock already disabled",
            operation: Operation::Lock,
            response: "printf '%s\\n' \"already disabled: $5\" >&2; exit 1",
            check: |result, key| match result {
                Err(error @ MlxError::AlreadyLocked) => {
                    !error.to_string().contains(key) && !format!("{error:?}").contains(key)
                }
                _ => false,
            },
            expected_args: &[
                "-d",
                "0000:03:00.0",
                "hw_access",
                "disable",
                "0123aBcD4567eF89",
            ],
        },
        // Unlocking must select enable, not reuse the locking command.
        Case {
            name: "unlock card",
            operation: Operation::Unlock,
            response: "exit 0",
            check: |result, _| matches!(result, Ok(LockStatus::Unlocked)),
            expected_args: &[
                "-d",
                "0000:03:00.0",
                "hw_access",
                "enable",
                "0123aBcD4567eF89",
            ],
        },
    ];
    let executor = crate::runner::test_firmware_executor();
    for case in cases {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("flint");
        write_fixture_script(
            &executable,
            &format!(
                "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" >> \"$0.args\"\n{}\n",
                case.response
            ),
        )
        .await;
        let manager =
            LockdownManager::with_runner(FlintRunner::with_path(executable.to_str().unwrap()));
        let timeout = Duration::from_secs(5);
        let device = "0000:03:00.0";
        let key = "0123aBcD4567eF89";
        let result = case
            .operation
            .run(&manager, device, key, &executor, timeout)
            .await;

        // A constant status, swapped action, or omitted/repeated vendor call
        // must fail even when the fake tool accepts every command line.
        assert!(
            (case.check)(&result, key),
            "{}: unexpected result: {result:?}",
            case.name
        );
        let arguments = std::fs::read_to_string(executable.with_extension("args")).unwrap();
        assert_eq!(
            arguments.lines().collect::<Vec<_>>(),
            case.expected_args,
            "{}",
            case.name
        );
    }
}

/// Every public bounded call must enforce the supplied deadline, settle its
/// process tree, and erase any echoed credentials from typed failure evidence.
#[tokio::test]
async fn bounded_access_calls_reap_stalled_vendors_and_redact_output() {
    let cases = [
        // Query dispatches separately from the shared key-command runner.
        ("query", Operation::Query),
        // Lock exercises the shared bounded key-command execution.
        ("lock", Operation::Lock),
        // Unlock must not forward to the synchronous enable method.
        ("unlock", Operation::Unlock),
    ];
    let executor = crate::runner::test_firmware_executor();
    let key = "0123aBcD4567eF89";
    for (name, operation) in cases {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("flint");
        write_fixture_script(
            &executable,
            concat!(
                "#!/bin/sh\n",
                "printf '%s' \"${5-}\"; printf '%s' \"${5-}\" >&2\n",
                "/bin/sleep 30 &\n",
                "printf '%s %s' \"$$\" \"$!\" > \"$0.pids\"\n",
                "wait\n",
            ),
        )
        .await;
        let manager =
            LockdownManager::with_runner(FlintRunner::with_path(executable.to_str().unwrap()));
        // Startup shares the six-second deadline. Leave room for cleanup and
        // result delivery, but fail before the tool's natural exit.
        let timeout = Duration::from_secs(6);
        let error = tokio::time::timeout(
            Duration::from_secs(20),
            operation.run(&manager, "0000:03:00.0", key, &executor, timeout),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: bounded call exceeded its test watchdog"))
        .unwrap_err();

        let mut source: Option<&(dyn Error + 'static)> = Some(&error);
        while let Some(current) = source {
            assert!(!current.to_string().contains(key), "{name}");
            assert!(!format!("{current:?}").contains(key), "{name}");
            source = current.source();
        }
        let attempt = error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .downcast_ref::<AttemptError>()
            .expect("caller must be able to inspect typed process settlement");
        let AttemptError::Started {
            status,
            stdout,
            stderr,
            reason,
            cleanup_errors,
            ..
        } = attempt
        else {
            panic!("{name}: fixture did not start: {attempt}");
        };
        // Bypassing supervision or substituting a different timeout must fail,
        // even though the success fixture accepts either execution path.
        assert!(
            matches!(reason, AttemptFailure::Deadline(duration) if *duration == timeout),
            "{name}: {reason}"
        );
        assert!(status.is_some(), "{name}");
        assert!(cleanup_errors.is_empty(), "{name}: {cleanup_errors:?}");
        assert!(stdout.is_empty() && stderr.is_empty(), "{name}");
        let pids = std::fs::read_to_string(executable.with_extension("pids"))
            .unwrap_or_else(|error| panic!("{name}: fixture did not publish its PIDs: {error}"));
        assert_eq!(pids.split_whitespace().count(), 2, "{name}");
        for pid in pids.split_whitespace() {
            let pid: u32 = pid.parse().unwrap();
            assert!(
                !Path::new("/proc").join(pid.to_string()).exists(),
                "{name}: bounded call left process {pid} behind"
            );
        }
    }
}

/// Public device validation and the separate query dry-run guard must return
/// before either the supervisor or vendor executable is needed.
#[tokio::test]
async fn bounded_access_rejects_invalid_devices_and_short_circuits_query_dry_run() {
    struct Case {
        name: &'static str,
        operation: Operation,
        device: &'static str,
        dry_run: bool,
        check: fn(&MlxError, &Path) -> bool,
    }

    let cases = [
        // Query has its own manager-level device validation call.
        Case {
            name: "invalid query device",
            operation: Operation::Query,
            device: "",
            dry_run: false,
            check: |error, _| matches!(error, MlxError::InvalidDeviceId(_)),
        },
        // Lock must validate before preparing a key command.
        Case {
            name: "invalid lock device",
            operation: Operation::Lock,
            device: "",
            dry_run: false,
            check: |error, _| matches!(error, MlxError::InvalidDeviceId(_)),
        },
        // Unlock has a separate public validation call, despite shared execution.
        Case {
            name: "invalid unlock device",
            operation: Operation::Unlock,
            device: "",
            dry_run: false,
            check: |error, _| matches!(error, MlxError::InvalidDeviceId(_)),
        },
        // Query's dry-run guard is not part of shared key-command preparation.
        Case {
            name: "query dry-run",
            operation: Operation::Query,
            device: "0000:03:00.0",
            dry_run: true,
            check: |error, executable| {
                matches!(
                    error,
                    MlxError::DryRun(command)
                        if command == &format!("{} -d 0000:03:00.0 q", executable.display())
                )
            },
        },
    ];
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("missing-flint");
    let executor =
        FirmwareToolExecutor::with_supervisor_path(directory.path().join("missing-supervisor"));
    for case in cases {
        let manager = LockdownManager::with_runner(
            FlintRunner::with_path(executable.to_str().unwrap()).with_dry_run(case.dry_run),
        );
        let error = case
            .operation
            .run(
                &manager,
                case.device,
                "0123aBcD4567eF89",
                &executor,
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();

        // Removing either guard would expose a launch failure instead of the
        // validation or dry-run result checked beside each row's inputs.
        assert!(
            (case.check)(&error, &executable),
            "{}: unexpected error: {error:?}",
            case.name
        );
    }
}
