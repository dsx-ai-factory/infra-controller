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

use std::path::PathBuf;
use std::time::Duration;

use carbide_libmlx_model::firmware::result::FirmwareFlashReport;
use thiserror::Error;

pub use crate::runner::attempt::{AttemptError, AttemptFailure};

/// `FirmwarePhase` identifies a supervised tool probe or firmware command.
/// Device queries use `FirmwareError::Discovery` instead of a phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwarePhase {
    /// Resolve a tool before starting its requested operation.
    ToolDiscovery,
    /// Apply the prepared device configuration.
    Configuration,
    /// Write the prepared firmware image.
    Burn,
    /// Request activation through the existing reset command.
    Activation,
    /// Compare device firmware with the prepared image.
    ImageVerification,
}

/// `FirmwareApplyError` preserves progress when a requested phase fails.
/// The error does not authorize replay of any completed or uncertain mutation.
#[derive(Debug, Error)]
#[error("{source}; firmware progress: {report:?}")]
pub struct FirmwareApplyError {
    /// Progress after the burn stage returns successfully. `None` can follow
    /// a successful command whose completion could not be persisted; it does
    /// not prove that the device was unchanged. Requested phases remain
    /// `Some(false)` until both the command and its progress recording succeed.
    pub report: Option<FirmwareFlashReport>,
    /// Original failure, including process status and cleanup diagnostics.
    pub source: Box<FirmwareError>,
}

impl From<FirmwareError> for FirmwareApplyError {
    fn from(source: FirmwareError) -> Self {
        Self {
            report: None,
            source: Box::new(source),
        }
    }
}

/// `FirmwareError` preserves source/staging, device-query, inspection, and
/// firmware-command failures. `PhaseFailed` and `Discovery` retain bounded
/// command diagnostics; neither successful cleanup nor an error proves that a
/// device-internal operation stopped.
#[derive(Debug, Error)]
pub enum FirmwareError {
    /// Staged image bytes differ from the immutable update request.
    #[error("firmware image SHA-256 does not match the requested digest")]
    ImageDigestMismatch,

    /// Staged configuration bytes differ from the immutable update request.
    #[error("firmware configuration SHA-256 does not match the requested digest")]
    ConfigurationDigestMismatch,

    /// The operation owner requested cancellation at a phase boundary.
    /// Earlier completed or uncertain mutations still require reconciliation.
    #[error("firmware operation was canceled")]
    OperationCanceled,

    /// Recovery progress could not be stored. The source preserves the storage
    /// failure; a preceding command may already have changed the device.
    #[error("failed to persist firmware progress: {0}")]
    ProgressPersistence(#[source] std::io::Error),

    /// A command failed before launch or has an uncertain result after dispatch.
    /// A mutation that started, or may have started, requires reconciliation.
    #[error("firmware phase {phase:?} failed: {source}")]
    PhaseFailed {
        /// Phase whose command attempt failed. A persistence error uses
        /// `ProgressPersistence` and may follow a command that already succeeded.
        phase: FirmwarePhase,
        /// Process status, diagnostics, and cleanup evidence.
        source: AttemptError,
    },

    /// A local input or live device could not be validated.
    #[error("firmware inspection failed: {0}")]
    Inspection(#[from] crate::firmware::preflight::FirmwarePreflightError),

    /// The bounded query failed before usable device facts were available.
    #[error("firmware device query failed: {0}")]
    Discovery(#[from] crate::device::discovery::DeviceDiscoveryError),

    #[error("command execution failed: {0}")]
    CommandFailed(String),

    #[error("failed to resolve firmware from '{description}': {reason}")]
    SourceResolution { description: String, reason: String },

    #[error("file not found: {0}")]
    FileNotFound(PathBuf),

    #[error("HTTP download failed: {0}")]
    HttpError(String),

    /// An artifact's connection, authentication, and response exceeded one deadline.
    #[error("firmware source transfer exceeded its {timeout:?} deadline")]
    SourceTransferTimeout {
        /// Cumulative network budget for this artifact, excluding disk writes.
        timeout: Duration,
    },

    #[error("SSH transfer failed: {0}")]
    SshError(#[from] forge_ssh::SshError),

    #[error("flint error: {0}")]
    FlintError(#[from] crate::lockdown::error::MlxError),

    #[error("mlxconfig error: {0}")]
    MlxConfigError(#[from] crate::runner::error::MlxRunnerError),

    #[error("firmware reset failed: {0}")]
    ResetFailed(String),

    #[error("firmware verification failed: {0}")]
    VerificationFailed(String),

    /// Requested verification did not finish within the shared post-reset
    /// budget. Burn and reset are not repeated.
    #[error("post-reset firmware verification exceeded its {timeout:?} budget")]
    PostResetVerificationTimeout {
        /// Shared allowance for tool-lookup probes, verification reads, and
        /// retry waits after the reset completion checkpoint.
        timeout: Duration,
        /// Most recent failed read since the last successful check, if any.
        source: Option<Box<FirmwareError>>,
    },

    #[error("mlxfwreset tool not found or not executable")]
    MlxFwResetNotFound,

    #[error("permission denied - requires root privileges")]
    PermissionDenied,

    #[error("device not found: {0}")]
    DeviceNotFound(String),

    #[error("dry run - would have executed: {0}")]
    DryRun(String),

    #[error("configuration error: {0}")]
    ConfigError(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl FirmwareError {
    /// Retries settled nonzero exits and command deadlines, unusable query
    /// reports, and version mismatches. A settled nonzero exit is treated as
    /// possible device unreadiness even if its underlying cause is persistent.
    /// Launch or tool-discovery failures, unconfirmed process settlement,
    /// output limits, and receipt-persistence failures stop work.
    pub(super) fn can_retry_verification(&self) -> bool {
        use crate::device::discovery::DeviceDiscoveryError;
        use crate::firmware::preflight::FirmwarePreflightError;

        match self {
            Self::PhaseFailed {
                phase: FirmwarePhase::ImageVerification,
                source,
            }
            | Self::Discovery(DeviceDiscoveryError::QueryAttempt(source)) => matches!(
                source,
                AttemptError::Started {
                    status: Some(_),
                    reason: AttemptFailure::Nonzero | AttemptFailure::Deadline(_),
                    cleanup_errors,
                    ..
                } if cleanup_errors.is_empty()
            ),
            Self::Discovery(
                DeviceDiscoveryError::InvalidReport(_)
                | DeviceDiscoveryError::ReportEncoding { .. },
            )
            | Self::Inspection(FirmwarePreflightError::UnknownFirmwareVersion)
            | Self::VerificationFailed(_) => true,
            _ => false,
        }
    }
}

// FirmwareResult is a result type alias for firmware operations.
pub type FirmwareResult<T> = Result<T, FirmwareError>;

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::device::discovery::DeviceDiscoveryError;
    use crate::firmware::preflight::FirmwarePreflightError;

    /// Read retry safety depends on both the failure and process settlement;
    /// a completed vendor rejection must not hide an unresolved descendant.
    #[test]
    fn verification_retries_require_settled_reads() {
        let failed_attempt = |status, reason, cleanup_errors| AttemptError::Started {
            command: "fixture".into(),
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
            reason,
            cleanup_errors,
        };
        let failed_phase = |phase, reason, cleanup_errors| FirmwareError::PhaseFailed {
            phase,
            source: failed_attempt(Some(ExitStatus::from_raw(1 << 8)), reason, cleanup_errors),
        };
        check_values(
            [
                Check {
                    scenario: "settled image comparison can recover after reset",
                    input: failed_phase(
                        FirmwarePhase::ImageVerification,
                        AttemptFailure::Nonzero,
                        Vec::new(),
                    ),
                    expect: true,
                },
                Check {
                    scenario: "settled read deadline can recover within the overall budget",
                    input: failed_phase(
                        FirmwarePhase::ImageVerification,
                        AttemptFailure::Deadline(Duration::from_secs(1)),
                        Vec::new(),
                    ),
                    expect: true,
                },
                Check {
                    scenario: "settled discovery deadline can recover after reset",
                    input: FirmwareError::Discovery(DeviceDiscoveryError::QueryAttempt(
                        failed_attempt(
                            Some(ExitStatus::from_raw(1 << 8)),
                            AttemptFailure::Deadline(Duration::from_secs(1)),
                            Vec::new(),
                        ),
                    )),
                    expect: true,
                },
                Check {
                    scenario: "missing exit status prevents retry despite empty cleanup errors",
                    input: FirmwareError::Discovery(DeviceDiscoveryError::QueryAttempt(
                        failed_attempt(
                            None,
                            AttemptFailure::Deadline(Duration::from_secs(1)),
                            Vec::new(),
                        ),
                    )),
                    expect: false,
                },
                Check {
                    scenario: "missing device report can recover after reset",
                    input: FirmwareError::Discovery(DeviceDiscoveryError::InvalidReport(
                        "device unavailable".into(),
                    )),
                    expect: true,
                },
                Check {
                    scenario: "invalid report encoding can recover after reset",
                    input: FirmwareError::Discovery(DeviceDiscoveryError::ReportEncoding {
                        source: String::from_utf8(vec![0xff]).expect_err("invalid UTF-8 fixture"),
                        stderr: String::new(),
                    }),
                    expect: true,
                },
                Check {
                    scenario: "unknown firmware version can become available after reset",
                    input: FirmwareError::Inspection(
                        FirmwarePreflightError::UnknownFirmwareVersion,
                    ),
                    expect: true,
                },
                Check {
                    scenario: "version mismatch may converge without another mutation",
                    input: FirmwareError::VerificationFailed("old version".into()),
                    expect: true,
                },
                Check {
                    scenario: "a failed mutation is never retried",
                    input: failed_phase(
                        FirmwarePhase::Activation,
                        AttemptFailure::Nonzero,
                        Vec::new(),
                    ),
                    expect: false,
                },
                Check {
                    scenario: "broken tool discovery is not a device readiness signal",
                    input: failed_phase(
                        FirmwarePhase::ToolDiscovery,
                        AttemptFailure::Nonzero,
                        Vec::new(),
                    ),
                    expect: false,
                },
                Check {
                    scenario: "unresolved cleanup overrides retryable vendor failure",
                    input: failed_phase(
                        FirmwarePhase::ImageVerification,
                        AttemptFailure::Nonzero,
                        vec![std::io::Error::other("descendant still running")],
                    ),
                    expect: false,
                },
                Check {
                    scenario: "output limits stop rather than repeating noisy tools",
                    input: failed_phase(
                        FirmwarePhase::ImageVerification,
                        AttemptFailure::OutputLimit,
                        Vec::new(),
                    ),
                    expect: false,
                },
                Check {
                    scenario: "missing tool is an installation failure",
                    input: FirmwareError::Discovery(DeviceDiscoveryError::QueryAttempt(
                        AttemptError::NotStarted {
                            command: "fixture".into(),
                            source: std::io::Error::from(std::io::ErrorKind::NotFound),
                        },
                    )),
                    expect: false,
                },
                Check {
                    scenario: "failed receipt persistence prevents more reads",
                    input: FirmwareError::ProgressPersistence(std::io::Error::other(
                        "journal fenced",
                    )),
                    expect: false,
                },
            ],
            |error| error.can_retry_verification(),
        );
    }
}
