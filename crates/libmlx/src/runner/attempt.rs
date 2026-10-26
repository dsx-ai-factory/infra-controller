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

use std::io;
use std::process::{ExitStatus, Output};
use std::time::Duration;

use thiserror::Error;

use super::command_builder::CommandSpec;
#[cfg(target_os = "linux")]
use super::supervisor_protocol::ToolOwnerExitBehavior;

/// `AttemptError` distinguishes a command that never started from an uncertain
/// result after dispatch. No variant authorizes an automatic retry.
#[derive(Debug, Error)]
pub enum AttemptError {
    /// The helper could not start; no firmware tool was dispatched.
    #[error("firmware supervisor {} could not start for command {command}: {source}", helper.display())]
    SupervisorNotStarted {
        /// Program and arguments that would have been dispatched.
        command: String,
        /// Absolute supervisor path selected for this attempt.
        helper: std::path::PathBuf,
        /// Original helper-spawn failure, including its OS error number.
        source: io::Error,
    },
    /// No tool was started, including when supervision is unsupported.
    #[error("command {command} did not start: {source}")]
    NotStarted {
        /// Program and arguments selected for this attempt.
        command: String,
        /// Failure before the child could start.
        source: io::Error,
    },
    /// The command started or may have started; hardware effects are uncertain.
    #[error(
        "command {command} failed: {reason}; status: {status:?}; stdout: {}; stderr: {}; cleanup: {cleanup_errors:?}",
        diagnostic(stdout),
        diagnostic(stderr)
    )]
    Started {
        /// Program and arguments used for this attempt.
        command: String,
        /// Tool leader's exit status, when it could be reaped.
        status: Option<ExitStatus>,
        /// Available stdout prefix, capped at 1 MiB.
        stdout: Vec<u8>,
        /// Available stderr prefix, independently capped at 1 MiB.
        stderr: Vec<u8>,
        /// Original failure, preserved even when cleanup also fails.
        #[source]
        reason: AttemptFailure,
        /// Additional failures while stopping processes and draining pipes.
        cleanup_errors: Vec<io::Error>,
    },
}

/// `AttemptFailure` describes why a started command could not be accepted.
#[derive(Debug, Error)]
pub enum AttemptFailure {
    /// The caller requested cancellation or closed a control channel configured to cancel.
    #[error("command was cancelled")]
    Cancelled,
    /// Execution and pipe collection exceeded this complete attempt duration.
    #[error("exceeded its {0:?} deadline")]
    Deadline(Duration),
    /// Waiting for the child or reading its output failed.
    #[error("command I/O failed: {0}")]
    Io(#[from] io::Error),
    /// At least one output stream exceeded its independent 1 MiB bound.
    #[error("command output exceeded 1 MiB")]
    OutputLimit,
    /// The caller rejected the collected exit status.
    #[error("command exited unsuccessfully")]
    Nonzero,
}

impl AttemptError {
    /// Whether a child started or may have started and changed the device.
    pub fn started(&self) -> bool {
        matches!(self, Self::Started { .. })
    }

    pub(crate) fn nonzero(command: &CommandSpec, output: Output) -> Self {
        Self::Started {
            command: command.to_string(),
            status: Some(output.status),
            stdout: output.stdout,
            stderr: output.stderr,
            reason: AttemptFailure::Nonzero,
            cleanup_errors: Vec::new(),
        }
    }
}

fn diagnostic(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).into_owned();
    if bytes.len() > 2048 || text.len() > 2048 {
        let mut end = text.len().min(2048);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(" [truncated]");
    }
    text
}

/// `FirmwareToolExecutor` runs one tool in an isolated Linux subreaper process.
///
/// The default helper is `mlx-firmware-supervisor` beside the calling executable,
/// never a PATH lookup. Embedders can provide the matching helper explicitly.
/// Each attempt includes descendants and capped output collection. By default,
/// closing the control pipe cancels the attempt; the helper continues independently
/// for up to five seconds to terminate and reap children. Internal durable work
/// can retain its attempt deadline after owner exit. No failure authorizes a retry,
/// and completed processes do not prove that device-internal work has stopped.
#[derive(Clone, Debug, Default)]
pub struct FirmwareToolExecutor {
    supervisor_path: Option<std::path::PathBuf>,
    #[cfg(target_os = "linux")]
    owner_exit_behavior: ToolOwnerExitBehavior,
}

impl FirmwareToolExecutor {
    /// `with_supervisor_path` selects the matching supervisor binary for an
    /// embedding application. The path must be absolute; omission uses a sibling
    /// executable. A missing or unusable helper fails before dispatching a tool.
    pub fn with_supervisor_path(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            supervisor_path: Some(path.into()),
            #[cfg(target_os = "linux")]
            owner_exit_behavior: ToolOwnerExitBehavior::default(),
        }
    }

    /// Let accepted work finish under its existing deadline if its owner exits.
    /// An explicit cancellation byte still stops the tool and its descendants.
    #[cfg(target_os = "linux")]
    pub(crate) fn continue_on_owner_exit(mut self) -> Self {
        self.owner_exit_behavior = ToolOwnerExitBehavior::Continue;
        self
    }

    /// `execute` runs once, returning the leader's status only after all owned
    /// descendants and both output streams finish. Every stream retains at most
    /// 1 MiB. The attempt deadline includes helper startup and result collection;
    /// cleanup after failure has its own five-second bound, followed by up to
    /// five seconds for result delivery. Collection allows both after a deadline.
    ///
    /// Lost results after dispatch are uncertain even if the helper exits. They
    /// retain cleanup diagnostics so callers do not remove staged inputs early.
    /// Other platforms fail before any tool is started.
    pub async fn execute(
        &self,
        command: &CommandSpec,
        timeout: Duration,
    ) -> Result<Output, AttemptError> {
        #[cfg(target_os = "linux")]
        {
            linux::run(
                self.supervisor_path.as_deref(),
                self.owner_exit_behavior,
                command,
                timeout,
            )
            .await
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(AttemptError::NotStarted {
                command: command.to_string(),
                source: io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "supervised command execution with a {timeout:?} deadline requires Linux"
                    ),
                ),
            })
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::{Child, ChildStdout, Command};
    use tokio::time::Instant;

    use super::*;
    use crate::runner::supervisor_protocol::{
        CLEANUP_TIMEOUT, Failure, HELLO, OUTPUT_LIMIT, REQUEST_LIMIT, RESPONSE_LIMIT,
        RESULT_TIMEOUT, Request, Response,
    };

    pub(super) async fn run(
        configured_helper: Option<&Path>,
        owner_exit_behavior: ToolOwnerExitBehavior,
        command: &CommandSpec,
        timeout: Duration,
    ) -> Result<Output, AttemptError> {
        let not_started = |source| AttemptError::NotStarted {
            command: command.to_string(),
            source,
        };
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            not_started(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid attempt deadline",
            ))
        })?;
        let helper = helper_path(configured_helper).map_err(not_started)?;
        let mut child = Command::new(&helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // Terminal signals target the caller's group. The independent
            // helper applies the request's owner exit policy through its pipe.
            .process_group(0)
            .spawn()
            .map_err(|source| AttemptError::SupervisorNotStarted {
                command: command.to_string(),
                helper,
                source,
            })?;
        let mut control = child.stdin.take().expect("supervisor stdin was piped");
        let mut result = child.stdout.take().expect("supervisor stdout was piped");
        let handshake = async {
            let mut hello = [0; HELLO.len()];
            result.read_exact(&mut hello).await?;
            if hello != HELLO {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected firmware supervisor protocol",
                ));
            }
            Ok::<_, io::Error>(())
        };
        match tokio::time::timeout_at(deadline, handshake).await {
            Ok(Ok(())) => {}
            result => {
                drop(control);
                // Dispatch never happened, so stopping this direct helper
                // cannot leave tool descendants behind.
                if let Err(error) = child.start_kill() {
                    tracing::warn!(%error, "failed to stop an undispatched firmware supervisor");
                }
                let error = match result {
                    Ok(Err(error)) => error,
                    Err(_) => io::Error::new(
                        io::ErrorKind::TimedOut,
                        "firmware supervisor handshake exceeded attempt deadline",
                    ),
                    Ok(Ok(())) => unreachable!("successful handshake handled above"),
                };
                return Err(not_started(error));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let request = Request {
            program: command.program.clone(),
            args: command.args.clone(),
            timeout: remaining,
            owner_exit_behavior,
        };
        let bytes = serde_json::to_vec(&request)
            .map_err(|error| not_started(io::Error::new(io::ErrorKind::InvalidInput, error)))?;
        if bytes.len() > REQUEST_LIMIT || remaining.is_zero() {
            return Err(not_started(io::Error::new(
                io::ErrorKind::InvalidInput,
                "firmware supervisor request exceeds its size or time bound",
            )));
        }

        // From the first request byte onward, a lost result cannot prove that
        // the tool did not start. Keep that uncertainty across cancellation.
        let dispatch = async {
            control.write_u32(bytes.len() as u32).await?;
            control.write_all(&bytes).await
        };
        match tokio::time::timeout_at(deadline, dispatch).await {
            Ok(Ok(())) => {}
            result => {
                drop(control);
                let failure = match result {
                    Ok(Err(error)) => AttemptFailure::Io(error),
                    Err(_) => AttemptFailure::Deadline(timeout),
                    Ok(Ok(())) => unreachable!("successful dispatch handled above"),
                };
                return Err(uncertain(command, failure, &mut child).await);
            }
        }

        // Retain this future after the deadline: restarting a frame read after
        // it consumed a prefix would corrupt the result protocol.
        let mut control = Some(control);
        let (response, deadline_expired) = {
            let response = receive_response(&mut result, &mut child);
            tokio::pin!(response);
            match tokio::time::timeout_at(deadline, &mut response).await {
                Ok(response) => (response, false),
                Err(_) => {
                    let mut cancellation_control = control.take();
                    if owner_exit_behavior == ToolOwnerExitBehavior::Terminate {
                        drop(cancellation_control.take());
                    }
                    let cancellation = async move {
                        if let Some(mut control) = cancellation_control
                            && let Err(error) = control.write_all(b"x").await
                        {
                            tracing::warn!(%error, "failed to send firmware tool deadline cancellation");
                        }
                    };
                    // EOF permits accepted work to continue, so the caller's
                    // deadline needs an explicit cancellation byte. Keep both
                    // cancellation and result delivery inside the cleanup bound.
                    let response = tokio::time::timeout(CLEANUP_TIMEOUT + RESULT_TIMEOUT, async {
                        tokio::pin!(cancellation);
                        tokio::select! {
                            response = &mut response => response,
                            () = &mut cancellation => (&mut response).await,
                        }
                    })
                    .await
                    .unwrap_or_else(|_| {
                        Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "firmware supervisor cleanup and result delivery exceeded {:?}",
                                CLEANUP_TIMEOUT + RESULT_TIMEOUT
                            ),
                        ))
                    });
                    (response, true)
                }
            }
        };
        drop(control);
        match response {
            Ok(response) => convert_response(command, timeout, deadline_expired, response),
            Err(error) if !deadline_expired => {
                Err(uncertain(command, AttemptFailure::Io(error), &mut child).await)
            }
            Err(error) => Err(AttemptError::Started {
                command: command.to_string(),
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                reason: AttemptFailure::Deadline(timeout),
                cleanup_errors: vec![
                    error,
                    io::Error::other(
                        "supervisor result unavailable; descendant cleanup is unconfirmed",
                    ),
                ],
            }),
        }
    }

    fn helper_path(configured: Option<&Path>) -> io::Result<PathBuf> {
        match configured {
            Some(path) if path.is_absolute() => Ok(path.to_owned()),
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "firmware supervisor path must be absolute",
            )),
            None => Ok(std::env::current_exe()?.with_file_name("mlx-firmware-supervisor")),
        }
    }

    async fn receive_response(result: &mut ChildStdout, child: &mut Child) -> io::Result<Response> {
        let length = result.read_u32().await? as usize;
        if length > RESPONSE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "firmware supervisor response exceeded its bound",
            ));
        }
        let mut bytes = vec![0; length];
        result.read_exact(&mut bytes).await?;
        let response: Response = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut trailing = [0; 1];
        if result.read(&mut trailing).await? != 0 || !child.wait().await?.success() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "firmware supervisor did not finish its result cleanly",
            ));
        }
        match &response {
            Response::NotStarted(_) => {}
            Response::Finished { stdout, stderr, .. } | Response::Failed { stdout, stderr, .. }
                if stdout.len() > OUTPUT_LIMIT || stderr.len() > OUTPUT_LIMIT =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "firmware supervisor returned excessive output",
                ));
            }
            _ => {}
        }
        Ok(response)
    }

    async fn uncertain(
        command: &CommandSpec,
        reason: AttemptFailure,
        child: &mut Child,
    ) -> AttemptError {
        // The control pipe has closed. Reaping the helper never proves its
        // children finished; only a valid completed response does that.
        let wait = tokio::time::timeout(CLEANUP_TIMEOUT + RESULT_TIMEOUT, child.wait()).await;
        let mut cleanup_errors = vec![io::Error::other(
            "supervisor result unavailable; descendant cleanup is unconfirmed",
        )];
        match wait {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => cleanup_errors.push(error),
            Err(_) => cleanup_errors.push(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "firmware supervisor cleanup and result delivery exceeded {:?}",
                    CLEANUP_TIMEOUT + RESULT_TIMEOUT
                ),
            )),
        }
        AttemptError::Started {
            command: command.to_string(),
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            reason,
            cleanup_errors,
        }
    }

    fn convert_response(
        command: &CommandSpec,
        timeout: Duration,
        deadline_expired: bool,
        response: Response,
    ) -> Result<Output, AttemptError> {
        match response {
            Response::NotStarted(source) => Err(AttemptError::NotStarted {
                command: command.to_string(),
                source: source.into(),
            }),
            Response::Finished {
                status,
                stdout,
                stderr,
            } if !deadline_expired => Ok(Output {
                status: ExitStatus::from_raw(status),
                stdout,
                stderr,
            }),
            Response::Finished {
                status,
                stdout,
                stderr,
            } => Err(AttemptError::Started {
                command: command.to_string(),
                status: Some(ExitStatus::from_raw(status)),
                stdout,
                stderr,
                reason: AttemptFailure::Deadline(timeout),
                cleanup_errors: Vec::new(),
            }),
            Response::Failed {
                status,
                stdout,
                stderr,
                reason,
                cleanup_errors,
            } => Err(AttemptError::Started {
                command: command.to_string(),
                status: status.map(ExitStatus::from_raw),
                stdout,
                stderr,
                reason: match reason {
                    Failure::Deadline => AttemptFailure::Deadline(timeout),
                    Failure::Cancelled if deadline_expired => AttemptFailure::Deadline(timeout),
                    Failure::Cancelled => AttemptFailure::Cancelled,
                    Failure::OutputLimit => AttemptFailure::OutputLimit,
                    Failure::Io(error) => AttemptFailure::Io(error.into()),
                },
                cleanup_errors: cleanup_errors.into_iter().map(io::Error::from).collect(),
            }),
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::fs;
    use std::path::Path;

    use carbide_test_support::value_scenarios;

    use super::*;

    /// Unit tests select Cargo's compiled helper explicitly instead of PATH.
    async fn run(command: &CommandSpec, timeout: Duration) -> Result<Output, AttemptError> {
        // The 30-second fixtures must not finish naturally and hide a broken
        // attempt deadline or cancellation path.
        tokio::time::timeout(
            Duration::from_secs(15),
            crate::runner::test_firmware_executor().execute(command, timeout),
        )
        .await
        .expect("attempt exceeded its independent test watchdog")
    }

    /// Build a shell fixture with arguments kept separate from its script.
    fn shell(script: &str) -> CommandSpec {
        CommandSpec::new("/bin/sh").args(["-c", script, "attempt-fixture"])
    }

    /// The helper owns reaping, so a zombie still violates completed cleanup.
    fn process_exists(pid: &str) -> bool {
        let pid: u32 = pid.parse().expect("fixture published a PID");
        Path::new("/proc")
            .join(pid.to_string())
            .try_exists()
            .expect("observe fixture process")
    }

    /// Diagnostic prefixes keep complete UTF-8 characters within their byte bound.
    #[test]
    fn diagnostic_truncation_preserves_utf8_boundaries() {
        value_scenarios!(run = |bytes: Vec<u8>| diagnostic(&bytes);
            "a complete character ending at the limit needs no truncation marker" {
                // The inclusive bound applies to bytes, not character count.
                format!("{}🦀", "a".repeat(2044)).into_bytes() => format!("{}🦀", "a".repeat(2044)),
            }
            "a character crossing the byte limit is omitted without a panic" {
                // The raw prefix ends inside this three-byte character.
                format!("{}€", "a".repeat(2046)).into_bytes() => format!("{} [truncated]", "a".repeat(2046)),
            }
            "lossy decoding cannot expand a short input past the display limit" {
                // Each invalid byte expands into a three-byte replacement character.
                vec![0xff; 1024] => format!("{} [truncated]", "�".repeat(682)),
            }
        );
    }

    /// Both streams drain concurrently and independently accept the full 1 MiB limit.
    #[tokio::test]
    async fn drains_both_pipes_while_the_command_runs() {
        let command = shell(
            r#"i=0; while [ "$i" -lt 1024 ]; do
                printf '%01024d' 0
                printf '%01024d' 0 >&2
                i=$((i + 1))
            done"#,
        );
        let output = run(&command, Duration::from_secs(5)).await.unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, vec![b'0'; 1024 * 1024]);
        assert_eq!(output.stderr, output.stdout);
    }

    /// An absent executable has no uncertain device effects to reconcile.
    #[tokio::test]
    async fn missing_executable_does_not_start_an_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let command = CommandSpec::new(directory.path().join("missing").to_str().unwrap());
        let error = run(&command, Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(
            error,
            AttemptError::NotStarted { source, .. } if source.kind() == io::ErrorKind::NotFound
        ));
    }

    /// A deadline stays the primary failure without hiding malformed cleanup IPC.
    #[tokio::test]
    async fn deadline_preserves_the_later_response_error() {
        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join("invalid-after-deadline");
        // Capture the cancellation byte before EOF. The malformed result can
        // only arrive after the caller has enforced its attempt deadline.
        // An isolated writer keeps writable descriptors out of concurrent forks.
        let writer = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "set -eu; umask 077; printf '%s' \"$2\" > \"$1\"; chmod 700 \"$1\"",
                    "fixture-writer",
                ])
                .arg(&helper)
                .arg("#!/bin/sh\nprintf 'MLX-SUPERVISOR-1\\n'\n/bin/cat > \"$0.request\"\nprintf '\\000\\000\\000\\001x'\n")
                .kill_on_drop(true)
                .status(),
        )
        .await
        .expect("fixture writer must finish within its watchdog")
        .expect("start isolated fixture writer");
        assert!(writer.success(), "fixture writer failed: {writer}");
        let request_path = helper.with_extension("request");
        let executor = FirmwareToolExecutor::with_supervisor_path(helper).continue_on_owner_exit();
        let command = CommandSpec::new("/bin/true");
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            executor.execute(&command, Duration::from_secs(1)),
        )
        .await
        .expect("malformed cleanup result must not leave the caller waiting")
        .unwrap_err();
        let AttemptError::Started {
            reason,
            cleanup_errors,
            ..
        } = error
        else {
            panic!("dispatch was not preserved: {error}");
        };
        assert!(matches!(reason, AttemptFailure::Deadline(_)));
        assert_eq!(cleanup_errors[0].kind(), io::ErrorKind::InvalidData);
        assert!(cleanup_errors[0].to_string().contains("expected value"));
        assert!(cleanup_errors.iter().any(|error| {
            error
                .to_string()
                .contains("descendant cleanup is unconfirmed")
        }));
        let bytes = fs::read(request_path).expect("read captured control messages");
        let request_length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        let request: crate::runner::supervisor_protocol::Request =
            serde_json::from_slice(&bytes[4..4 + request_length])
                .expect("decode dispatched request");
        assert_eq!(request.owner_exit_behavior, ToolOwnerExitBehavior::Continue);
        assert_eq!(&bytes[4 + request_length..], b"x");
    }

    /// A deadline retains the first attempt's diagnostics and never starts a retry.
    #[tokio::test]
    async fn deadline_keeps_diagnostics_without_retrying() {
        let directory = tempfile::tempdir().unwrap();
        let launch_path = directory.path().join("launches");
        let pid_path = directory.path().join("pids");
        let command = shell(
            "printf launch >> \"$1\"; printf diagnostic-out; printf diagnostic-err >&2; \
             sleep 30 & printf '%s %s' \"$$\" \"$!\" > \"$2\"; wait",
        )
        .arg(launch_path.to_str().unwrap())
        .arg(pid_path.to_str().unwrap());
        let error = run(&command, Duration::from_secs(1)).await.unwrap_err();
        let AttemptError::Started {
            reason,
            status,
            stdout,
            stderr,
            cleanup_errors,
            ..
        } = error
        else {
            panic!("command did not start: {error}");
        };
        assert!(matches!(reason, AttemptFailure::Deadline(_)));
        assert!(status.is_some());
        assert_eq!(stdout, b"diagnostic-out");
        assert_eq!(stderr, b"diagnostic-err");
        assert!(cleanup_errors.is_empty(), "{cleanup_errors:?}");
        assert_eq!(fs::read_to_string(launch_path).unwrap(), "launch");
        let pids = fs::read_to_string(pid_path).unwrap();
        assert_eq!(pids.split_whitespace().count(), 2);
        for pid in pids.split_whitespace() {
            assert!(!process_exists(pid), "process {pid} was not reaped");
        }
    }

    /// A successful leader cannot make unfinished descendant work successful.
    #[tokio::test]
    async fn leader_exit_does_not_accept_unfinished_descendants() {
        let cases = [
            // Open inherited pipes keep output collection pending as well.
            ("inherited pipes", ""),
            // Closed pipes do not prove that descendants stopped using inputs.
            ("redirected pipes", "> /dev/null 2>&1"),
        ];
        for (scenario, redirect) in cases {
            let directory = tempfile::tempdir().unwrap();
            let pid_path = directory.path().join("pid");
            let command = shell(&format!(
                "sleep 30 {redirect} & printf '%s' \"$!\" > \"$1\""
            ))
            .arg(pid_path.to_str().unwrap());
            let error = run(&command, Duration::from_secs(1)).await.unwrap_err();
            let AttemptError::Started {
                reason,
                status,
                cleanup_errors,
                ..
            } = error
            else {
                panic!("{scenario}: command did not start: {error}");
            };
            assert!(
                matches!(reason, AttemptFailure::Deadline(_)),
                "{scenario}: {reason}"
            );
            assert!(
                status.unwrap().success(),
                "{scenario}: leader should exit zero"
            );
            assert!(cleanup_errors.is_empty(), "{scenario}: {cleanup_errors:?}");
            let pid = fs::read_to_string(pid_path).unwrap();
            assert!(
                !process_exists(&pid),
                "{scenario}: descendant {pid} was not reaped"
            );
        }
    }

    /// Descendants may finish after their leader, without being killed as cleanup.
    #[tokio::test]
    async fn leader_exit_waits_for_descendants_to_finish_before_the_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("pids");
        let release_path = directory.path().join("release");
        let completed_path = directory.path().join("completed");
        // The descendant closes both output pipes and waits for our release.
        // Its completion file distinguishes finished work from forced cleanup.
        let command = shell(
            r#"(while [ ! -f "$2" ]; do sleep 0.01; done; printf finished > "$3") > /dev/null 2>&1 &
            printf '%s %s' "$$" "$!" > "$1""#,
        )
        .arg(pid_path.to_str().unwrap())
        .arg(release_path.to_str().unwrap())
        .arg(completed_path.to_str().unwrap());
        let execution = run(&command, Duration::from_secs(5));
        tokio::pin!(execution);
        let release = async {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Ok(pids) = fs::read_to_string(&pid_path) {
                        let pids: Vec<_> = pids.split_whitespace().collect();
                        if pids.len() == 2 && !process_exists(pids[0]) && process_exists(pids[1]) {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("leader exited while its descendant waited for release");
            // Give the runner time to observe the exited leader. Returning or
            // killing the descendant during this interval would be premature.
            tokio::time::sleep(Duration::from_millis(100)).await;
            fs::write(release_path, "release").unwrap();
        };
        let output = tokio::select! {
            result = &mut execution => panic!("attempt finished before descendant release: {result:?}"),
            () = release => execution.await.unwrap(),
        };
        assert!(output.status.success());
        assert_eq!(fs::read_to_string(completed_path).unwrap(), "finished");
        for pid in fs::read_to_string(pid_path).unwrap().split_whitespace() {
            assert!(!process_exists(pid), "process {pid} was not reaped");
        }
    }

    /// Each raw output stream has its own cap, including during failure cleanup.
    #[tokio::test]
    async fn excessive_output_retains_only_the_bounded_prefix() {
        let cases = [
            // Stdout cannot grow beyond the bound while stderr is collected.
            ("stdout", "printf diagnostic >&2", "", true),
            // Stderr has the same independent bound, not a shared output budget.
            ("stderr", "printf diagnostic", ">&2", false),
        ];
        for (scenario, diagnostic, redirect, stdout_overflow) in cases {
            let command = shell(&format!(
                "{diagnostic}; i=0; while [ \"$i\" -lt 1024 ]; do printf '%01024d' 0 {redirect}; i=$((i + 1)); done; printf 0 {redirect}"
            ));
            let error = run(&command, Duration::from_secs(5)).await.unwrap_err();
            let AttemptError::Started {
                reason,
                stdout,
                stderr,
                cleanup_errors,
                ..
            } = error
            else {
                panic!("{scenario}: command did not start: {error}");
            };
            assert!(
                matches!(reason, AttemptFailure::OutputLimit),
                "{scenario}: {reason}"
            );
            let (bounded, other) = if stdout_overflow {
                (stdout, stderr)
            } else {
                (stderr, stdout)
            };
            assert_eq!(bounded, vec![b'0'; 1024 * 1024], "{scenario}");
            assert_eq!(other, b"diagnostic", "{scenario}");
            assert!(cleanup_errors.is_empty(), "{scenario}: {cleanup_errors:?}");
        }
    }

    /// Output overflow stops the child before its deadline. A held pipe reports
    /// a cleanup timeout without replacing the original output failure.
    #[tokio::test]
    async fn pipe_cleanup_timeout_preserves_the_original_output_failure() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("pid");
        let release_path = directory.path().join("release");
        let command = shell(
            r#"printf '%s' "$$" > "$1"
            while [ ! -f "$2" ]; do sleep 0.01; done
            i=0; while [ "$i" -lt 1025 ]; do printf '%01024d' 0; i=$((i + 1)); done
            sleep 30"#,
        )
        .arg(pid_path.to_str().unwrap())
        .arg(release_path.to_str().unwrap());
        let mut held_writer = None;
        let setup = async {
            let pid = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Ok(pid) = fs::read_to_string(&pid_path)
                        && pid.parse::<u32>().is_ok()
                    {
                        break pid;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("fixture published its PID before writing excessive output");
            // Holding an extra write-end prevents EOF even after the supervised
            // descendants stop. RAII closes it on every test exit, without creating
            // an escaped process for the fixture to find and kill later.
            held_writer = Some(
                fs::OpenOptions::new()
                    .write(true)
                    .open(Path::new("/proc").join(&pid).join("fd/1"))
                    .unwrap(),
            );
            fs::write(&release_path, "release").unwrap();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            // Neither the attempt deadline nor the fixture's 30-second sleep
            // may stop the child before this independent watchdog expires.
            tokio::join!(run(&command, Duration::from_secs(60)), setup)
        })
        .await
        .expect("execution and cleanup remain bounded while a writer holds the pipe open");
        drop(held_writer);
        let error = result.unwrap_err();
        let AttemptError::Started {
            reason,
            status,
            stdout,
            cleanup_errors,
            ..
        } = error
        else {
            panic!("command did not start: {error}");
        };
        assert!(matches!(reason, AttemptFailure::OutputLimit), "{reason}");
        assert!(status.is_some(), "the supervised leader was reaped");
        assert_eq!(stdout, vec![b'0'; 1024 * 1024]);
        assert_eq!(cleanup_errors.len(), 1, "{cleanup_errors:?}");
        assert_eq!(cleanup_errors[0].kind(), io::ErrorKind::TimedOut);
        let pid = fs::read_to_string(pid_path).unwrap();
        assert!(!process_exists(&pid), "leader {pid} was not reaped");
    }

    /// Dropping an in-flight attempt stops and reaps its leader and descendant.
    #[tokio::test]
    async fn cancellation_stops_the_leader_and_its_descendant() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("pids");
        let command = shell("sleep 30 & printf '%s %s' \"$$\" \"$!\" > \"$1\"; wait")
            .arg(pid_path.to_str().unwrap());
        let task = tokio::spawn(async move { run(&command, Duration::from_secs(30)).await });
        let pids = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pids) = fs::read_to_string(&pid_path)
                    && pids.split_whitespace().count() == 2
                {
                    break pids;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), async {
            while pids.split_whitespace().any(process_exists) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled leader and descendant were reaped");
    }
}
