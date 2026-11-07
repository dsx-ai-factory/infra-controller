// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

#[derive(Clone, Debug, thiserror::Error)]
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

    /// Certificate submission failed, possibly after accepting some nodes.
    /// The retained job is for reconciliation only; do not resubmit automatically.
    #[error("switch certificate submission failed: {summary}; {failure}")]
    SwitchCertificateSubmission {
        /// Aggregate explanation returned by the backend.
        summary: String,
        /// All returned node failures and the job handle, if supplied.
        failure: Box<model::component_manager::SwitchCertificateFailure>,
    },

    #[error("internal error: {0}")]
    Internal(String),

    /// Shares the transport source so cloning retains the original error.
    #[error("gRPC transport error: {0}")]
    Transport(#[source] Arc<tonic::transport::Error>),

    #[error("gRPC status error: {0}")]
    Status(#[from] tonic::Status),

    #[error("RMS error: {0}")]
    Rms(String),
}

impl From<tonic::transport::Error> for ComponentManagerError {
    fn from(error: tonic::transport::Error) -> Self {
        Self::Transport(Arc::new(error))
    }
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

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn cloned_transport_error_retains_its_source() {
        let source = tonic::transport::Endpoint::new("not a valid uri").unwrap_err();
        let source_message = source.to_string();
        let error = ComponentManagerError::from(source);
        let cloned = error.clone();

        let (ComponentManagerError::Transport(original), ComponentManagerError::Transport(copy)) =
            (&error, &cloned)
        else {
            panic!("cloning must preserve the transport variant");
        };
        assert!(Arc::ptr_eq(original, copy));
        assert_eq!(cloned.to_string(), error.to_string());
        drop(error);
        assert_eq!(cloned.source().unwrap().to_string(), source_message);
    }
}
