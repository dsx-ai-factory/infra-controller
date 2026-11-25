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

use carbide_secrets::SecretsError;
use libredfish::RedfishError;
use state_controller::state_handler::{ExternalServiceError, StateHandlerError};

#[derive(thiserror::Error, Debug)]
pub enum RedfishClientCreationError {
    #[error("missing credential {key}")]
    MissingCredentials { key: String },
    #[error("missing credential: {cause}")]
    SecretEngineError { cause: SecretsError },
    #[error("failed redfish request {0}")]
    RedfishError(RedfishError),
    #[error("invalid header {0}")]
    InvalidHeader(String),
    #[error("missing arguments: {0}")]
    MissingArgument(String),
    /// The pool cannot serve this request as asked -- e.g. the proxied pool
    /// rejecting explicit credentials or a non-443 BMC port. The payload is
    /// the full operator-facing explanation.
    #[error("{0}")]
    Unsupported(String),
}

impl From<SecretsError> for RedfishClientCreationError {
    fn from(cause: SecretsError) -> Self {
        RedfishClientCreationError::SecretEngineError { cause }
    }
}

impl From<RedfishClientCreationError> for StateHandlerError {
    fn from(error: RedfishClientCreationError) -> StateHandlerError {
        ExternalServiceError::with_source(
            "redfish",
            "create_client",
            error.to_string(),
            "redfish_client_creation_error",
            error,
        )
        .into()
    }
}

pub fn state_handler_redfish_error(
    operation: &'static str,
    error: RedfishError,
) -> StateHandlerError {
    ExternalServiceError::with_source(
        "redfish",
        operation,
        error.to_string(),
        redfish_operation_metric_label(operation),
        error,
    )
    .into()
}

pub(crate) fn redfish_operation_metric_label(operation: &'static str) -> &'static str {
    match operation {
        "restart" => "redfish_restart_error",
        "lockdown" => "redfish_lockdown_error",
        _ => "redfish_other_error",
    }
}
