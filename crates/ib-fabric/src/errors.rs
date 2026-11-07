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

use db::DatabaseError;
use model::errors::ModelError;

#[derive(thiserror::Error, Debug)]
pub enum IbError {
    #[error("database error: {0}")]
    DatabaseError(#[from] DatabaseError),
    #[error("model error: {0}")]
    ModelError(#[from] ModelError),
    #[error("failed to call IBFabricManager: {0}")]
    IBFabricError(String),
    #[error("{kind} not found: {id}")]
    NotFoundError {
        /// The type of the resource that was not found (e.g. Machine)
        kind: &'static str,
        /// The ID of the resource that was not found
        id: String,
    },
    #[error("argument is invalid: {0}")]
    InvalidArgument(String),
    /// An authoritative local UFM credential source is missing or invalid.
    #[error("credential configuration error: {0}")]
    CredentialConfigurationError(String),
    #[error("the function is not implemented")]
    NotImplemented,
    #[error("internal error: {message}")]
    Internal { message: String },
}

impl IbError {
    /// Creates a `Internal` error with the given error message
    pub fn internal(message: String) -> Self {
        Self::Internal { message }
    }
}

pub type IbResult<T> = Result<T, IbError>;

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum ErrorCase {
        Fabric,
        NotFound,
        InvalidArgument,
        CredentialConfiguration,
        NotImplemented,
        Internal,
    }

    fn error_for(case: ErrorCase) -> IbError {
        match case {
            ErrorCase::Fabric => IbError::IBFabricError("ufm failed".to_string()),
            ErrorCase::NotFound => IbError::NotFoundError {
                kind: "Machine",
                id: "machine-1".to_string(),
            },
            ErrorCase::InvalidArgument => IbError::InvalidArgument("bad pkey".to_string()),
            ErrorCase::CredentialConfiguration => {
                IbError::CredentialConfigurationError("missing UFM token".to_string())
            }
            ErrorCase::NotImplemented => IbError::NotImplemented,
            ErrorCase::Internal => IbError::internal("unexpected state".to_string()),
        }
    }

    #[test]
    fn formats_ib_errors() {
        value_scenarios!(
            run = |case| error_for(case).to_string();
            "user facing errors" {
                ErrorCase::Fabric => "failed to call IBFabricManager: ufm failed".to_string(),
                ErrorCase::NotFound => "Machine not found: machine-1".to_string(),
                ErrorCase::InvalidArgument => "argument is invalid: bad pkey".to_string(),
                ErrorCase::CredentialConfiguration =>
                    "credential configuration error: missing UFM token".to_string(),
                ErrorCase::NotImplemented => "the function is not implemented".to_string(),
                ErrorCase::Internal => "internal error: unexpected state".to_string(),
            }
        );
    }
}
