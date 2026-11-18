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

use nv_redfish::{Bmc, Error as RedfishError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ClassifyBmcError;

/// A typed authentication or authorization failure.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthError {
    #[error("BMC credentials are unavailable")]
    CredentialsUnavailable,
    #[error("BMC rejected the credentials")]
    InvalidCredentials,
    /// The account must change its password before any other request;
    /// `account_uri` is the account the BMC names for that change.
    #[error("BMC requires a password change")]
    PasswordChangeRequired { account_uri: Option<String> },
    #[error("BMC account lacks the required privilege")]
    InsufficientPrivilege,
}

/// A normalized failure returned by capability and transport implementations.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "details", rename_all = "snake_case")]
pub enum PlatformError {
    #[error("operation is unsupported")]
    Unsupported,
    #[error("BMC is unreachable")]
    Unreachable,
    #[error("BMC operation is blocked by lockdown")]
    LockedDown,
    #[error("BMC user not found: {identifier}")]
    UserNotFound { identifier: String },
    #[error("BMC has no free user slots")]
    TooManyUsers,
    #[error("boot option not found: {description}")]
    MissingBootOption { description: String },
    #[error("DPU is not present")]
    NoDpu,
    #[error("BMC returned no content")]
    NoContent,
    #[error("BMC authentication failed: {0}")]
    Auth(AuthError),
    #[error(
        "BMC request failed with HTTP status {status}{}: {message}",
        message_id_suffix(.message_id)
    )]
    Bmc {
        status: u16,
        message_id: Option<String>,
        message: String,
    },
    #[error("BMC returned an invalid response: {message}")]
    InvalidResponse { message: String },
    /// The operation waits for an operator; `code` names the manual step.
    #[error("operation requires manual intervention: {code}")]
    ManualInterventionRequired { code: String },
    /// The hardware did not reach the state the operation waited for.
    #[error("BMC did not reach the expected state in time: {message}")]
    Timeout { message: String },
    /// The BMC cannot yet answer for something its platform supports, such
    /// as BIOS settings the host has not reported since the BMC restarted;
    /// asking again later can succeed.
    #[error("BMC is not ready: {message}")]
    NotReady { message: String },
}

/// ` (<MessageId>)` when the BMC named one.
fn message_id_suffix(message_id: &Option<String>) -> String {
    message_id
        .as_deref()
        .map(|id| format!(" ({id})"))
        .unwrap_or_default()
}

impl PlatformError {
    /// Maps an `nv-redfish` wrapper failure into the platform vocabulary.
    pub fn from_redfish<B>(error: RedfishError<B>) -> Self
    where
        B: Bmc,
        B::Error: ClassifyBmcError,
    {
        Self::from_redfish_with(error, ClassifyBmcError::classify)
    }

    /// [`Self::from_redfish`] with an explicit transport classifier, for
    /// contexts that captured the classifier instead of carrying the bound.
    pub fn from_redfish_with<B: Bmc>(
        error: RedfishError<B>,
        classify: fn(B::Error) -> Self,
    ) -> Self {
        match error {
            RedfishError::Bmc(error) => classify(error),
            RedfishError::AccountSlotNotAvailable => Self::TooManyUsers,
            RedfishError::ActionNotAvailable => Self::Unsupported,
            other => Self::InvalidResponse {
                message: other.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_errors_round_trip_without_losing_typed_data() {
        let errors = [
            PlatformError::Unsupported,
            PlatformError::Unreachable,
            PlatformError::LockedDown,
            PlatformError::UserNotFound {
                identifier: "operator".to_string(),
            },
            PlatformError::TooManyUsers,
            PlatformError::MissingBootOption {
                description: "PXE IPv4".to_string(),
            },
            PlatformError::NoDpu,
            PlatformError::NoContent,
            PlatformError::Auth(AuthError::PasswordChangeRequired {
                account_uri: Some("/redfish/v1/AccountService/Accounts/2".to_string()),
            }),
            PlatformError::Bmc {
                status: 409,
                message_id: Some("Base.1.0.ResourceInUse".to_string()),
                message: "resource is busy".to_string(),
            },
            PlatformError::InvalidResponse {
                message: "missing task location".to_string(),
            },
            PlatformError::ManualInterventionRequired {
                code: "dpu-host-privilege-restricted".to_string(),
            },
            PlatformError::Timeout {
                message: "host did not power off".to_string(),
            },
            PlatformError::NotReady {
                message: "the BIOS has not reported its attributes".to_string(),
            },
        ];

        for error in errors {
            let encoded = serde_json::to_value(&error).expect("error serializes");
            assert!(
                encoded
                    .get("type")
                    .and_then(|value| value.as_str())
                    .is_some()
            );
            assert_eq!(
                serde_json::from_value::<PlatformError>(encoded).expect("error deserializes"),
                error
            );
        }
    }

    /// The MessageId names which Redfish rule the BMC applied, so it is part
    /// of the message whenever the BMC sends one.
    #[test]
    fn bmc_errors_name_the_message_id_when_given() {
        let cases = [
            (
                Some("Base.1.0.ResourceInUse"),
                "BMC request failed with HTTP status 409 (Base.1.0.ResourceInUse): busy",
            ),
            (None, "BMC request failed with HTTP status 409: busy"),
        ];
        for (message_id, expected) in cases {
            let error = PlatformError::Bmc {
                status: 409,
                message_id: message_id.map(ToString::to_string),
                message: "busy".to_string(),
            };
            assert_eq!(error.to_string(), expected);
        }
    }
}
