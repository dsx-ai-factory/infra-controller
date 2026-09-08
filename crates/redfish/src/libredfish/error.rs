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
}

/// Error from a credential-lifecycle operation ([`super::BmcCredentialOps`]),
/// separating "could not build the Redfish client" from "the operation
/// itself failed". Callers retry `ClientCreation` and treat `Operation` as
/// device-level (quarantine, vendor fallback). Note that client creation is
/// not purely local: besides the credential store and TCP it includes the
/// vendor auto-detect HTTP probe of the BMC, so a persistently broken BMC
/// can surface here too. The operations create their clients internally, so
/// without this split both failure classes would surface identically.
#[derive(thiserror::Error, Debug)]
pub enum CredentialOpError {
    /// The operation's Redfish client could not be built: the credential
    /// store, TCP, or the vendor auto-detect probe failed before the
    /// operation was attempted. Callers retry.
    #[error("creating redfish client: {0}")]
    ClientCreation(#[source] RedfishClientCreationError),
    /// The operation itself failed after the client was built. Callers treat
    /// it as device-level (quarantine, vendor fallback). Only the
    /// [`RedfishClientCreationError::RedfishError`] payload variant is ever
    /// constructed; the wider type is kept so the transparent `Display`
    /// keeps the `failed redfish request` prefix call sites record (e.g. in
    /// quarantine rows).
    #[error(transparent)]
    Operation(RedfishClientCreationError),
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

fn redfish_operation_metric_label(operation: &'static str) -> &'static str {
    match operation {
        "restart" => "redfish_restart_error",
        "lockdown" => "redfish_lockdown_error",
        _ => "redfish_other_error",
    }
}
