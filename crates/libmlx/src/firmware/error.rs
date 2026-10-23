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

// FirmwareResult is a result type alias for firmware operations.
pub type FirmwareResult<T> = Result<T, FirmwareError>;
