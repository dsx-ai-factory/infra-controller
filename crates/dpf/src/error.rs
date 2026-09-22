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

//! Error types for the DPF SDK.

use thiserror::Error;

/// Error type for DPF operations.
#[derive(Error, Debug)]
pub enum DpfError {
    #[error("kubernetes client error: {0}")]
    KubeError(#[from] kube::Error),

    #[error("resource not found: {kind} '{name}'")]
    NotFound { kind: &'static str, name: String },

    #[error("resource already exists: {kind} '{name}'")]
    AlreadyExists { kind: &'static str, name: String },

    #[error("timeout waiting for {operation}: {details}")]
    Timeout { operation: String, details: String },

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("configuration error: {0}")]
    ConfigError(String),

    #[error("watcher error: {0}")]
    WatcherError(String),

    #[error("JSON serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}

impl DpfError {
    /// Returns whether this error represents a missing DPF resource.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
            || matches!(self, Self::KubeError(kube::Error::Api(status)) if status.is_not_found())
    }

    pub fn not_found(kind: &'static str, name: impl Into<String>) -> Self {
        Self::NotFound {
            kind,
            name: name.into(),
        }
    }

    pub fn already_exists(kind: &'static str, name: impl Into<String>) -> Self {
        Self::AlreadyExists {
            kind,
            name: name.into(),
        }
    }

    pub fn timeout(operation: impl Into<String>, details: impl Into<String>) -> Self {
        Self::Timeout {
            operation: operation.into(),
            details: details.into(),
        }
    }
}

impl From<kube::runtime::watcher::Error> for DpfError {
    fn from(e: kube::runtime::watcher::Error) -> Self {
        Self::WatcherError(e.to_string())
    }
}
