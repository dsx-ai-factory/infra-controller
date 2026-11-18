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

// src/executor.rs
// Command executor handles running mlxconfig commands with retry logic,
// timeout support, and temporary file management using proper timeout
// and backoff crates for robust execution.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use backon::{BlockingRetryable, ExponentialBuilder};
use uuid::Uuid;
use wait_timeout::ChildExt;

use crate::runner::command_builder::CommandSpec;
use crate::runner::error::MlxRunnerError;
use crate::runner::exec_options::ExecOptions;

// CommandExecutor handles the execution of mlxconfig commands with
// retry logic, timeout support, and temporary file management.
// Uses the `wait-timeout` and `backon` crates for command execution.
pub struct CommandExecutor<'a> {
    // options contains the execution options controlling retry,
    // timeout, and interactive confirmation behavior.
    pub options: &'a ExecOptions,
}

impl<'a> CommandExecutor<'a> {
    /// `execute_with_retry` runs a command with the configured retry and timeout
    /// limits. If a spawned command's termination cannot be established, it
    /// returns `ProcessUnsettled` without retrying; callers must retain mutation
    /// exclusion until the command has been reconciled.
    pub fn execute_with_retry(&self, command_spec: &CommandSpec) -> Result<Output, MlxRunnerError> {
        if self.options.verbose {
            println!("[EXEC] Executing command with timeout and retry: {command_spec}",);
            println!(
                "[EXEC] Retry config: {} attempts, {:.1}x multiplier, {} initial delay, {} max delay",
                u64::from(self.options.retries) + 1,
                self.options.retry_multiplier,
                format_duration(self.options.retry_delay),
                format_duration(self.options.max_retry_delay)
            );
        }

        // Create exponential backoff strategy with our custom options.
        let backoff = ExponentialBuilder::default()
            .with_min_delay(self.options.retry_delay)
            .with_max_delay(self.options.max_retry_delay)
            .with_max_times(self.options.retries as usize)
            .with_factor(self.options.retry_multiplier);

        // Create a closure that captures the command
        // spec for retrying with backon.
        let execute_fn = || self.execute_single_attempt(command_spec);

        let result = execute_fn
            .retry(&backoff)
            .when(|err| self.should_retry_error(err))
            .notify(|err, dur| {
                if self.options.verbose {
                    println!(
                        "[RETRY] Retrying after {} due to error: {err}",
                        format_duration(dur)
                    );
                }
            })
            .call();

        match result {
            Ok(output) => {
                if self.options.verbose {
                    println!("[EXEC] Command completed successfully");
                }
                Ok(output)
            }
            Err(error) => {
                if self.options.verbose {
                    println!("[EXEC] Command failed after all retries: {error}");
                }
                Err(error)
            }
        }
    }

    // Executes a single attempt of the command with timeout handling.
    // This is called by the retry logic for each attempt.
    fn execute_single_attempt(&self, command_spec: &CommandSpec) -> Result<Output, MlxRunnerError> {
        let start_time = Instant::now();
        let mut command = command_spec.to_command();

        // Spawn the child process
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(MlxRunnerError::Io)?;

        // Apply timeout if configured
        let output = if let Some(timeout) = self.options.timeout {
            self.execute_with_timeout(child, timeout, start_time, command_spec)?
        } else {
            // No timeout - just wait for completion
            child
                .wait_with_output()
                .map_err(|source| MlxRunnerError::ProcessUnsettled {
                    command: command_spec.to_string(),
                    source,
                })?
        };

        // Check if command succeeded
        if output.status.success() {
            Ok(output)
        } else {
            Err(MlxRunnerError::command_execution(
                command_spec.to_string(),
                output,
            ))
        }
    }

    // A timeout or wait failure must settle the child before returning an error
    // that permits a retry. Cleanup uncertainty stops the retry sequence instead.
    fn execute_with_timeout(
        &self,
        mut child: Child,
        timeout: Duration,
        start_time: Instant,
        command_spec: &CommandSpec,
    ) -> Result<Output, MlxRunnerError> {
        if self.options.verbose {
            println!(
                "[TIMEOUT] Waiting for command with timeout: {}",
                format_duration(timeout)
            );
        }

        // Wait for the child process with timeout
        match child.wait_timeout(timeout) {
            Ok(Some(status)) => {
                // Process completed within timeout
                let execution_time = start_time.elapsed();
                if self.options.verbose {
                    println!(
                        "[TIMEOUT] Command completed in {}",
                        format_duration(execution_time)
                    );
                }

                // Collect stdout and stderr
                let stdout = if let Some(mut stdout) = child.stdout.take() {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut stdout, &mut buf)
                        .map_err(MlxRunnerError::Io)?;
                    buf
                } else {
                    Vec::new()
                };

                let stderr = if let Some(mut stderr) = child.stderr.take() {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut stderr, &mut buf)
                        .map_err(MlxRunnerError::Io)?;
                    buf
                } else {
                    Vec::new()
                };

                Ok(Output {
                    status,
                    stdout,
                    stderr,
                })
            }
            Ok(None) => {
                // Process timed out - kill it and return timeout error
                let execution_time = start_time.elapsed();
                if self.options.verbose {
                    println!(
                        "[TIMEOUT] Command timed out after {}, killing process",
                        format_duration(execution_time)
                    );
                }

                terminate_child(&mut child, command_spec)?;

                Err(MlxRunnerError::Timeout {
                    command: command_spec.to_string(),
                    duration: timeout,
                })
            }
            Err(error) => {
                terminate_child(&mut child, command_spec)?;
                Err(MlxRunnerError::Io(error))
            }
        }
    }

    /// `should_retry_error` classifies ordinary I/O and command failures as
    /// retryable, but refuses validation errors and unknown process termination.
    pub fn should_retry_error(&self, error: &MlxRunnerError) -> bool {
        match error {
            // These errors are likely permanent and shouldn't be retried
            MlxRunnerError::VariableNotFound { .. } => false,
            MlxRunnerError::ArraySizeMismatch { .. } => false,
            MlxRunnerError::ValueConversion { .. } => false,
            MlxRunnerError::InvalidArrayIndex { .. } => false,
            MlxRunnerError::DeviceMismatch { .. } => false,
            MlxRunnerError::NoDeviceFound => false,
            MlxRunnerError::ConfirmationDeclined { .. } => false,
            MlxRunnerError::JsonParsing { .. } => false,
            // Retrying could overlap the command whose exit is still unknown.
            MlxRunnerError::ProcessUnsettled { .. } => false,

            // These errors might be transient and worth retrying
            MlxRunnerError::CommandExecution { .. } => true,
            MlxRunnerError::TempFileError { .. } => true,
            MlxRunnerError::Timeout { .. } => true,
            MlxRunnerError::Io(_) => true,
            MlxRunnerError::GenericError(_) => true,
        }
    }

    // Prompts the user for confirmation when modifying "destructive" variables.
    // Returns true if the user confirms, false if they decline.
    pub fn prompt_for_confirmation(
        &self,
        destructive_vars: &[String],
    ) -> Result<bool, MlxRunnerError> {
        println!("WARNING: You are about to modify destructive variables:");
        for var in destructive_vars {
            println!(" - {var}");
        }
        println!();
        print!("Continue? (y/N): ");

        std::io::stdout().flush().map_err(MlxRunnerError::Io)?;

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(MlxRunnerError::Io)?;

        let response = input.trim().to_lowercase();
        let confirmed = response == "y" || response == "yes";

        if self.options.verbose {
            println!(
                "[CONFIRM] User {} destructive operation",
                if confirmed { "confirmed" } else { "declined" }
            );
        }

        Ok(confirmed)
    }

    // Creates a temporary file for mlxconfig JSON output using the specified
    // prefix (which defaults to /tmp) and a UUID for uniqueness.
    pub fn create_temp_file(&self, prefix: &str) -> Result<PathBuf, MlxRunnerError> {
        let filename = format!("mlxconfig-runner-{}.json", Uuid::new_v4());
        let path = Path::new(prefix).join(filename);

        fs::File::create(&path).map_err(|e| MlxRunnerError::temp_file_error(path.clone(), e))?;

        if self.options.verbose {
            println!("[TEMP] Created temporary file: {}", path.display());
        }

        Ok(path)
    }

    // Cleans up the temporary mlxconfig JSON output file if it exists.
    // Safe to call even if the file doesn't exist.
    pub fn cleanup_temp_file(&self, temp_file: &Path) -> Result<(), MlxRunnerError> {
        if temp_file.exists() {
            fs::remove_file(temp_file)
                .map_err(|e| MlxRunnerError::temp_file_error(temp_file.to_path_buf(), e))?;

            if self.options.verbose {
                println!("[TEMP] Cleaned up temporary file: {}", temp_file.display());
            }
        }
        Ok(())
    }

    // Executes a dry-run operation by just logging what would be executed.
    // Used when dry_run is enabled in ExecOptions.
    pub fn execute_dry_run(&self, command_spec: &CommandSpec, operation_type: &str) {
        println!("[DRY RUN] Would execute {operation_type}: {command_spec}");
    }

    // Returns whether the current executor run is configured for dry-run mode.
    pub fn is_dry_run(&self) -> bool {
        self.options.dry_run
    }
}

// `kill` can race with an ordinary exit. A failed kill only establishes
// settlement if `try_wait` observes that exit; waiting for a still-live child
// here could otherwise block forever after the termination failure.
fn terminate_child(child: &mut Child, command_spec: &CommandSpec) -> Result<(), MlxRunnerError> {
    let settled = match child.kill() {
        Ok(()) => child.wait().map(|_| ()),
        Err(kill_error) => match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(kill_error),
            Err(wait_error) => Err(wait_error),
        },
    };
    settled.map_err(|source| MlxRunnerError::ProcessUnsettled {
        command: command_spec.to_string(),
        source,
    })
}

// Formats a Duration for human-readable display in console output/log messages.
fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use nix::errno::Errno;
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    use super::*;

    /// Termination must reap the child itself; a later `try_wait` could hide a
    /// missing wait by doing the reaping on behalf of the implementation.
    #[test]
    fn termination_reaps_the_child_before_returning() {
        // The open stdin keeps the shell in `read` until the executor kills it;
        // no wall-clock delay is needed to keep the command running.
        let command_spec = CommandSpec::new("/bin/sh").args(["-c", "read line"]);
        let mut child = command_spec
            .to_command()
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();

        let child_pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
        let result = terminate_child(&mut child, &command_spec);

        // Ask the kernel before any `Child` method can return cached status or
        // reap a child that termination left behind. Clean up a live child
        // before failing the proof; an observed exit is already reaped here.
        let observed = waitpid(child_pid, Some(WaitPidFlag::WNOHANG));
        let cleanup = if matches!(observed, Ok(WaitStatus::StillAlive)) {
            Some((child.kill(), child.wait()))
        } else {
            None
        };

        assert!(
            result.is_ok(),
            "termination failed: {result:?}; cleanup: {cleanup:?}"
        );
        assert_eq!(observed, Err(Errno::ECHILD), "cleanup: {cleanup:?}");
    }

    /// A successful kill is not sufficient when the subsequent wait loses its
    /// result. Linux auto-reaping gives the real termination path `ECHILD`.
    #[test]
    fn termination_without_exit_status_is_process_unsettled() {
        const CHILD_FLAG: &str = "LIBMLX_TERMINATION_LOST_STATUS_CHILD";
        const WATCHDOG: Duration = Duration::from_secs(10);

        if std::env::var_os(CHILD_FLAG).is_none() {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build termination watchdog runtime")
                .block_on(async {
            // The signal disposition must not affect other tests. Bash
            // preserves ignored `SIGCHLD` across exec; dash resets it.
            let log = tempfile::NamedTempFile::new().expect("create termination test log");
            let output = log.reopen().unwrap();
            let mut child = tokio::process::Command::new("/bin/bash")
                .args(["-c", "trap '' CHLD; exec \"$@\"", "ignored-sigchld"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runner::executor::tests::termination_without_exit_status_is_process_unsettled",
                    "--nocapture",
                ])
                .env(CHILD_FLAG, "1")
                .stdin(Stdio::null())
                .stdout(output.try_clone().unwrap())
                .stderr(output)
                .kill_on_drop(true)
                .spawn()
                .expect("start isolated termination test");
            let result = tokio::time::timeout(WATCHDOG, child.wait()).await;
            let status = match result {
                Ok(Ok(status)) => status,
                error => {
                    let stop = child.start_kill();
                    let reap = tokio::time::timeout(WATCHDOG, child.wait()).await;
                    panic!(
                        "termination test did not finish: {error:?}; stop: {stop:?}; reap: {reap:?}\n{}",
                        fs::read_to_string(log.path()).unwrap()
                    );
                }
            };
            let output = fs::read_to_string(log.path()).unwrap();
            assert!(
                status.success() && output.contains("1 passed; 0 failed"),
                "isolated termination test failed: {status}\n{output}"
            );
                });
            return;
        }

        // The open stdin keeps this shell alive until termination. If the
        // enclosing subprocess is killed, closing stdin also lets it exit.
        let command_spec = CommandSpec::new("/bin/sh").args(["-c", "read line"]);
        let mut child = command_spec
            .to_command()
            .stdin(Stdio::piped())
            .spawn()
            .expect("start shell awaiting termination");
        let child_pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
        let result = terminate_child(&mut child, &command_spec);
        let observed = waitpid(child_pid, Some(WaitPidFlag::WNOHANG));
        let cleanup = if matches!(observed, Ok(WaitStatus::StillAlive)) {
            Some((child.kill(), child.wait()))
        } else {
            None
        };

        assert!(
            matches!(
                result,
                Err(MlxRunnerError::ProcessUnsettled { ref source, .. })
                    if source.raw_os_error() == Some(Errno::ECHILD as i32)
            ),
            "lost wait must preserve uncertainty: {result:?}; cleanup: {cleanup:?}"
        );
        assert_eq!(observed, Err(Errno::ECHILD), "cleanup: {cleanup:?}");
    }
}
