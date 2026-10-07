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

use thiserror::Error;

// FirmwareError is the error type for firmware management operations,
// encompassing source resolution, flashing, verification, and reset.
#[derive(Debug, Error)]
pub enum FirmwareError {
    #[error("command execution failed: {0}")]
    CommandFailed(String),

    #[error("failed to resolve firmware from '{description}': {reason}")]
    SourceResolution { description: String, reason: String },

    #[error("file not found: {0}")]
    FileNotFound(PathBuf),

    #[error("HTTP download failed: {0}")]
    HttpError(String),

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
