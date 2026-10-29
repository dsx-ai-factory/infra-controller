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

use thiserror::Error;

use crate::runner::attempt::{AttemptError, AttemptFailure};

// MlxError is a custom error type for Mellanox NIC operations.
#[derive(Error, Debug)]
pub enum MlxError {
    /// A flint process started, but waiting for it failed. Callers must retain
    /// exclusion because the process may still be accessing hardware.
    #[error("flint {operation} process completion is unknown: {source}")]
    ProcessUnsettled {
        /// The operation name, never command arguments or a hardware key.
        operation: &'static str,
        /// The operating-system error returned while waiting for the process.
        source: std::io::Error,
    },

    /// A bounded flint attempt failed, preserving process settlement evidence
    /// while omitting command arguments, tool output, and arbitrary I/O text.
    #[error("bounded flint command failed: {0}")]
    CommandAttempt(#[source] FlintAttemptError),

    /// A flint operation failed; the payload describes the failure.
    /// Key-taking operations omit raw tool output: unsuccessful exits include the
    /// operation and exit status, and execution failures include the operation and
    /// OS error.
    #[error("command execution failed: {0}")]
    CommandFailed(String),

    #[error("device not found: {0}")]
    DeviceNotFound(String),

    #[error("invalid device ID format: {0}")]
    InvalidDeviceId(String),

    #[error("hardware access is already disabled")]
    AlreadyLocked,

    #[error("hardware access is already enabled")]
    AlreadyUnlocked,

    #[error("invalid key format or length")]
    InvalidKey,

    #[error("permission denied - requires root privileges")]
    PermissionDenied,

    #[error("flint tool not found or not executable")]
    FlintNotFound,

    #[error("failed to parse command output: {0}")]
    ParseError(String),

    #[error("dry run - would have executed: {0}")]
    DryRun(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}

// MlxResult is a result type alias for operations that
// can fail with MlxError.
pub type MlxResult<T> = Result<T, MlxError>;

/// A supervisor failure with credentials removed from every error formatter and
/// source. The source retains its [`AttemptError`] variant, exit status, failure
/// category, and cleanup-error count so hardware exclusion remains conservative.
#[derive(Debug)]
pub struct FlintAttemptError(Box<AttemptError>);

impl From<AttemptError> for FlintAttemptError {
    fn from(error: AttemptError) -> Self {
        // Vendor output and nested I/O messages can echo a key in a different
        // representation. Remove those fields instead of matching the input key.
        let command = "flint [arguments redacted]".to_string();
        let error = match error {
            AttemptError::SupervisorNotStarted { source, .. } => {
                AttemptError::SupervisorNotStarted {
                    command,
                    helper: "[redacted]".into(),
                    source: redact_io_error(source),
                }
            }
            AttemptError::NotStarted { source, .. } => AttemptError::NotStarted {
                command,
                source: redact_io_error(source),
            },
            AttemptError::Started {
                status,
                reason,
                cleanup_errors,
                ..
            } => AttemptError::Started {
                command,
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
                reason: match reason {
                    AttemptFailure::Io(source) => AttemptFailure::Io(redact_io_error(source)),
                    reason @ (AttemptFailure::Cancelled
                    | AttemptFailure::Deadline(_)
                    | AttemptFailure::OutputLimit
                    | AttemptFailure::Nonzero) => reason,
                },
                cleanup_errors: cleanup_errors.into_iter().map(redact_io_error).collect(),
            },
        };
        Self(Box::new(error))
    }
}

fn redact_io_error(error: std::io::Error) -> std::io::Error {
    match error.raw_os_error() {
        Some(code) => std::io::Error::from_raw_os_error(code),
        None => std::io::Error::from(error.kind()),
    }
}

impl std::fmt::Display for FlintAttemptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.0.as_ref(), formatter)
    }
}

impl std::error::Error for FlintAttemptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // Expose the attempt itself: skipping to its source would hide the
        // exit status and cleanup evidence used to retain hardware exclusion.
        Some(self.0.as_ref())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::error::Error;
    use std::io::{self, ErrorKind};
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use carbide_test_support::value_scenarios;

    use super::*;

    /// Error chains must retain settlement evidence without retaining a key in
    /// command text, output, helper paths, or nested I/O diagnostics.
    #[test]
    fn bounded_flint_errors_redact_sources_without_losing_settlement() {
        const KEY: &str = "0123aBcD4567eF89";
        value_scenarios!(run = |attempt: AttemptError| {
            let error = MlxError::CommandAttempt(attempt.into());
            let mut source: Option<&(dyn Error + 'static)> = Some(&error);
            while let Some(current) = source {
                for rendered in [current.to_string(), format!("{current:?}")] {
                    assert!(!rendered.contains(KEY), "{rendered}");
                    assert!(!rendered.contains(&KEY.to_uppercase()), "{rendered}");
                }
                source = current.source();
            }
            let attempt = error
                .source().unwrap()
                .source().unwrap()
                .downcast_ref::<AttemptError>()
                .unwrap();
            match attempt {
                AttemptError::SupervisorNotStarted { source, .. } => (
                    "supervisor", None, Some(source.kind()), Vec::new(), source.raw_os_error(),
                ),
                AttemptError::NotStarted { source, .. } => (
                    "tool", None, Some(source.kind()), Vec::new(), source.raw_os_error(),
                ),
                AttemptError::Started {
                    status, stdout, stderr, reason, cleanup_errors, ..
                } => {
                    assert!(stdout.is_empty() && stderr.is_empty());
                    let AttemptFailure::Io(source) = reason else {
                        panic!("I/O failure category was lost: {reason:?}");
                    };
                    (
                        "started", *status, Some(source.kind()),
                        cleanup_errors.iter().map(io::Error::kind).collect(), source.raw_os_error(),
                    )
                }
            }
        };
            // Helper paths and custom I/O errors must not bypass redaction.
            "helper launch failure" {
                AttemptError::SupervisorNotStarted {
                    command: format!("flint {KEY}"),
                    helper: KEY.into(),
                    source: io::Error::new(ErrorKind::NotFound, KEY),
                } => ("supervisor", None, Some(ErrorKind::NotFound), vec![], None),
            }
            // An OS error number remains useful after command redaction.
            "tool launch failure" {
                AttemptError::NotStarted {
                    command: format!("flint {KEY}"),
                    source: io::Error::from_raw_os_error(13),
                } => ("tool", None, Some(ErrorKind::PermissionDenied), vec![], Some(13)),
            }
            // An unreaped leader remains uncertain even without cleanup errors.
            "started command without a leader exit" {
                AttemptError::Started {
                    command: format!("flint {KEY}"),
                    status: None,
                    stdout: KEY.as_bytes().to_vec(),
                    stderr: KEY.to_uppercase().into_bytes(),
                    reason: AttemptFailure::Io(io::Error::new(ErrorKind::InvalidData, KEY)),
                    cleanup_errors: Vec::new(),
                } => ("started", None, Some(ErrorKind::InvalidData), vec![], None),
            }
            // A reaped leader does not override failed descendant cleanup.
            "started command with uncertain cleanup" {
                AttemptError::Started {
                    command: format!("flint {KEY}"),
                    status: Some(ExitStatus::from_raw(0)),
                    stdout: KEY.as_bytes().to_vec(),
                    stderr: KEY.to_uppercase().into_bytes(),
                    reason: AttemptFailure::Io(io::Error::new(ErrorKind::InvalidData, KEY)),
                    cleanup_errors: vec![io::Error::new(ErrorKind::TimedOut, KEY)],
                } => (
                    "started", Some(ExitStatus::from_raw(0)), Some(ErrorKind::InvalidData),
                    vec![ErrorKind::TimedOut], None,
                ),
            }
        );
    }
}
