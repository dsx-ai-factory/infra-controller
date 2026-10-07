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

use std::path::Path;
use std::time::Duration;
use std::{fs, io};

use carbide_test_support::Outcome::*;
use carbide_test_support::{Case, Check, check_cases, check_values};
use libmlx::runner::command_builder::CommandSpec;
use libmlx::runner::error::MlxRunnerError;
use libmlx::runner::exec_options::ExecOptions;
use libmlx::runner::executor::CommandExecutor;

#[test]
fn temporary_file_lifecycle() {
    let options = ExecOptions::new().with_verbose(true);
    let executor = CommandExecutor { options: &options };
    let temp_dir = tempfile::tempdir().unwrap();
    let prefix = temp_dir.path().to_str().unwrap();

    let first = executor.create_temp_file(prefix).unwrap();
    let second = executor.create_temp_file(prefix).unwrap();

    for path in [&first, &second] {
        assert!(path.exists());
        assert_eq!(path.parent(), Some(temp_dir.path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("mlxconfig-runner-")
        );
        assert_eq!(path.extension().unwrap(), "json");
    }
    assert_ne!(first, second);

    fs::write(&first, "test data").unwrap();
    assert_eq!(fs::read_to_string(&first).unwrap(), "test data");

    executor.cleanup_temp_file(&first).unwrap();
    executor.cleanup_temp_file(&second).unwrap();
    assert!(!first.exists());
    assert!(!second.exists());
}

#[test]
fn missing_temporary_file_cleanup_is_a_noop() {
    let options = ExecOptions::default();
    let executor = CommandExecutor { options: &options };
    let temp_dir = tempfile::tempdir().unwrap();
    let missing = temp_dir.path().join("missing.json");

    executor.cleanup_temp_file(&missing).unwrap();

    assert!(!missing.exists());
}

#[derive(Clone, Copy)]
enum CommandBehavior {
    Counted { succeed_on: usize, timeout: bool },
    Missing,
}

struct ExecutionInput {
    behavior: CommandBehavior,
    retries: u32,
    timeout: Option<Duration>,
    verbose: bool,
}

#[derive(Debug, PartialEq)]
struct ExecutionSuccess {
    attempts: usize,
    stdout: String,
}

#[derive(Debug, PartialEq)]
enum ExecutionFailure {
    CommandExecution {
        attempts: usize,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    Timeout {
        attempts: usize,
        duration: Duration,
    },
    Io(io::ErrorKind),
    Unexpected(String),
}

const ATTEMPT_SCRIPT: &str = r#"
attempts=0
if [ -f "$1" ]; then
    attempts=$(cat "$1")
fi
attempts=$((attempts + 1))
printf '%s' "$attempts" > "$1"

if [ "$3" = "timeout" ]; then
    exec sleep 1
fi

if [ "$2" -gt 0 ] && [ "$attempts" -ge "$2" ]; then
    printf 'success-%s\n' "$attempts"
    exit 0
fi

printf 'failure-%s\n' "$attempts" >&2
exit 7
"#;

fn counted_command(counter_path: &Path, succeed_on: usize, should_timeout: bool) -> CommandSpec {
    // The fixture records its attempt before deciding how that attempt ends,
    // so a process killed by the timeout still leaves us an exact count.
    // `exec sleep` replaces the shell so the executor cannot orphan its child.
    CommandSpec::new("sh").args([
        "-c".to_string(),
        ATTEMPT_SCRIPT.to_string(),
        "retry-fixture".to_string(),
        counter_path.to_string_lossy().into_owned(),
        succeed_on.to_string(),
        if should_timeout {
            "timeout".to_string()
        } else {
            "complete".to_string()
        },
    ])
}

fn read_attempts(counter_path: &Path) -> usize {
    fs::read_to_string(counter_path).unwrap().parse().unwrap()
}

fn execute_case(input: ExecutionInput) -> Result<ExecutionSuccess, ExecutionFailure> {
    let temp_dir = tempfile::tempdir().unwrap();
    let counter_path = temp_dir.path().join("attempts");
    let counted = matches!(input.behavior, CommandBehavior::Counted { .. });
    let command_spec = match input.behavior {
        CommandBehavior::Counted {
            succeed_on,
            timeout,
        } => counted_command(&counter_path, succeed_on, timeout),
        CommandBehavior::Missing => CommandSpec::new("duppet_is_real_but_not_like_this"),
    };
    let options = ExecOptions::new()
        .with_timeout(input.timeout)
        .with_retries(input.retries)
        .with_retry_delay(Duration::ZERO)
        .with_max_retry_delay(Duration::ZERO)
        .with_retry_multiplier(1.0)
        .with_verbose(input.verbose);
    let executor = CommandExecutor { options: &options };

    match executor.execute_with_retry(&command_spec) {
        Ok(output) => Ok(ExecutionSuccess {
            attempts: read_attempts(&counter_path),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        }),
        Err(MlxRunnerError::CommandExecution {
            exit_code,
            stdout,
            stderr,
            ..
        }) => Err(ExecutionFailure::CommandExecution {
            attempts: read_attempts(&counter_path),
            exit_code,
            stdout: stdout.trim().to_string(),
            stderr: stderr.trim().to_string(),
        }),
        Err(MlxRunnerError::Timeout { duration, .. }) => Err(ExecutionFailure::Timeout {
            attempts: read_attempts(&counter_path),
            duration,
        }),
        Err(MlxRunnerError::Io(error)) if !counted => Err(ExecutionFailure::Io(error.kind())),
        Err(error) => Err(ExecutionFailure::Unexpected(format!("{error:?}"))),
    }
}

#[test]
fn execute_with_retry_cases() {
    check_cases(
        [
            Case {
                scenario: "an immediate success without a timeout runs once",
                input: ExecutionInput {
                    behavior: CommandBehavior::Counted {
                        succeed_on: 1,
                        timeout: false,
                    },
                    retries: 2,
                    timeout: None,
                    verbose: false,
                },
                expect: Yields(ExecutionSuccess {
                    attempts: 1,
                    stdout: "success-1".to_string(),
                }),
            },
            Case {
                scenario: "a command that completes within its timeout runs once",
                input: ExecutionInput {
                    behavior: CommandBehavior::Counted {
                        succeed_on: 1,
                        timeout: false,
                    },
                    retries: 1,
                    timeout: Some(Duration::from_secs(1)),
                    verbose: true,
                },
                expect: Yields(ExecutionSuccess {
                    attempts: 1,
                    stdout: "success-1".to_string(),
                }),
            },
            Case {
                scenario: "a command failure exhausts two retries after three attempts",
                input: ExecutionInput {
                    behavior: CommandBehavior::Counted {
                        succeed_on: 0,
                        timeout: false,
                    },
                    retries: 2,
                    timeout: None,
                    verbose: true,
                },
                expect: FailsWith(ExecutionFailure::CommandExecution {
                    attempts: 3,
                    exit_code: Some(7),
                    stdout: String::new(),
                    stderr: "failure-3".to_string(),
                }),
            },
            Case {
                scenario: "one transient failure succeeds on the retry",
                input: ExecutionInput {
                    behavior: CommandBehavior::Counted {
                        succeed_on: 2,
                        timeout: false,
                    },
                    retries: 1,
                    timeout: None,
                    verbose: true,
                },
                expect: Yields(ExecutionSuccess {
                    attempts: 2,
                    stdout: "success-2".to_string(),
                }),
            },
            Case {
                scenario: "zero configured retries makes one attempt",
                input: ExecutionInput {
                    behavior: CommandBehavior::Counted {
                        succeed_on: 0,
                        timeout: false,
                    },
                    retries: 0,
                    timeout: None,
                    verbose: false,
                },
                expect: FailsWith(ExecutionFailure::CommandExecution {
                    attempts: 1,
                    exit_code: Some(7),
                    stdout: String::new(),
                    stderr: "failure-1".to_string(),
                }),
            },
            Case {
                scenario: "a timeout exhausts two retries after three attempts",
                input: ExecutionInput {
                    behavior: CommandBehavior::Counted {
                        succeed_on: 0,
                        timeout: true,
                    },
                    retries: 2,
                    timeout: Some(Duration::from_millis(500)),
                    verbose: true,
                },
                expect: FailsWith(ExecutionFailure::Timeout {
                    attempts: 3,
                    duration: Duration::from_millis(500),
                }),
            },
            Case {
                scenario: "a missing executable reports its I/O error",
                input: ExecutionInput {
                    behavior: CommandBehavior::Missing,
                    retries: 0,
                    timeout: None,
                    verbose: false,
                },
                expect: FailsWith(ExecutionFailure::Io(io::ErrorKind::NotFound)),
            },
            Case {
                scenario: "the maximum retry count is safe when verbose",
                input: ExecutionInput {
                    behavior: CommandBehavior::Counted {
                        succeed_on: 1,
                        timeout: false,
                    },
                    retries: u32::MAX,
                    timeout: None,
                    verbose: true,
                },
                expect: Yields(ExecutionSuccess {
                    attempts: 1,
                    stdout: "success-1".to_string(),
                }),
            },
        ],
        execute_case,
    );
}

#[test]
fn should_retry_error_classification() {
    check_values(
        [
            Check {
                scenario: "VariableNotFound",
                input: MlxRunnerError::VariableNotFound {
                    variable_name: "TEST".to_string(),
                },
                expect: false,
            },
            Check {
                scenario: "ArraySizeMismatch",
                input: MlxRunnerError::ArraySizeMismatch {
                    variable_name: "TEST".to_string(),
                    expected: 4,
                    found: 6,
                },
                expect: false,
            },
            Check {
                scenario: "ValueConversion",
                input: MlxRunnerError::ValueConversion {
                    variable_name: "TEST".to_string(),
                    value: "test".to_string(),
                    error: libmlx::variables::value::MlxValueError::TypeMismatch {
                        expected: "int".to_string(),
                        got: "string".to_string(),
                    },
                },
                expect: false,
            },
            Check {
                scenario: "InvalidArrayIndex",
                input: MlxRunnerError::InvalidArrayIndex {
                    variable_name: "TEST[invalid]".to_string(),
                },
                expect: false,
            },
            Check {
                scenario: "DeviceMismatch",
                input: MlxRunnerError::DeviceMismatch {
                    expected: "01:00.0".to_string(),
                    actual: "02:00.0".to_string(),
                },
                expect: false,
            },
            Check {
                scenario: "NoDeviceFound",
                input: MlxRunnerError::NoDeviceFound,
                expect: false,
            },
            Check {
                scenario: "ConfirmationDeclined",
                input: MlxRunnerError::ConfirmationDeclined {
                    variables: vec!["TEST".to_string()],
                },
                expect: false,
            },
            Check {
                scenario: "JsonParsing",
                input: MlxRunnerError::JsonParsing {
                    content: "{}".to_string(),
                    error: serde_json::from_str::<serde_json::Value>("invalid json{").unwrap_err(),
                },
                expect: false,
            },
            Check {
                scenario: "CommandExecution",
                input: MlxRunnerError::CommandExecution {
                    command: "test".to_string(),
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: "error".to_string(),
                },
                expect: true,
            },
            Check {
                scenario: "TempFileError",
                input: MlxRunnerError::TempFileError {
                    path: "/tmp/test".into(),
                    error: io::Error::new(io::ErrorKind::PermissionDenied, "test"),
                },
                expect: true,
            },
            Check {
                scenario: "Timeout",
                input: MlxRunnerError::Timeout {
                    command: "test".to_string(),
                    duration: Duration::from_secs(1),
                },
                expect: true,
            },
            Check {
                scenario: "Io",
                input: MlxRunnerError::Io(io::Error::new(io::ErrorKind::ConnectionRefused, "test")),
                expect: true,
            },
            Check {
                scenario: "GenericError",
                input: MlxRunnerError::GenericError("test".to_string()),
                expect: true,
            },
        ],
        |error| {
            let options = ExecOptions::default();
            CommandExecutor { options: &options }.should_retry_error(&error)
        },
    );
}
