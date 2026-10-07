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

// MlxError is a custom error type for Mellanox NIC operations.
#[derive(Error, Debug)]
pub enum MlxError {
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
