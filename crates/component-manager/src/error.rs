// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, thiserror::Error)]
pub enum ComponentManagerError {
    #[error("backend unavailable: {0}")]
    Unavailable(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// The selected backend does not implement or enable the requested operation.
    #[error("unsupported operation: {0}")]
    Unsupported(String),

    /// The backend rejected a mutating request before accepting any work.
    #[error("operation rejected before dispatch: {0}")]
    RejectedBeforeDispatch(String),

    /// A mutating operation may still be running when local observation ends,
    /// so its outcome cannot be determined yet.
    ///
    /// When a job handle is available, callers must retain it and continue
    /// status observation rather than resubmitting the mutation. Without a job
    /// handle, callers must preserve the staged target until their reconciliation
    /// policy decides whether it is safe to retry.
    ///
    /// For [`crate::nv_switch_manager::NvSwitchManager::ensure_password_rotation`],
    /// callers retain the exact current-to-target request and retry it later.
    #[error("operation outcome unknown: {0}")]
    OperationOutcomeUnknown(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("gRPC status error: {0}")]
    Status(#[from] tonic::Status),

    #[error("RMS error: {0}")]
    Rms(String),
}

impl From<librms::RackManagerError> for ComponentManagerError {
    fn from(err: librms::RackManagerError) -> Self {
        match err {
            librms::RackManagerError::ApiInvocationError(status) => {
                ComponentManagerError::Status(status)
            }
            librms::RackManagerError::TlsError(e) => {
                ComponentManagerError::Unavailable(e.to_string())
            }
        }
    }
}
