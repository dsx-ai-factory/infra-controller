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

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A typed authentication or authorization failure.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthError {
    #[error("BMC credentials are unavailable")]
    CredentialsUnavailable,
    #[error("BMC rejected the credentials")]
    InvalidCredentials,
    #[error("BMC requires a password change")]
    PasswordChangeRequired,
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
    #[error("BMC vendor is missing")]
    MissingVendor,
    #[error("boot option not found: {description}")]
    MissingBootOption { description: String },
    #[error("DPU is not present")]
    NoDpu,
    #[error("BMC returned no content")]
    NoContent,
    #[error("BMC authentication failed: {0}")]
    Auth(AuthError),
    #[error("BMC request failed with HTTP status {status}: {message}")]
    Bmc {
        status: u16,
        message_id: Option<String>,
        message: String,
    },
    #[error("BMC returned an invalid response: {message}")]
    InvalidResponse { message: String },
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
            PlatformError::MissingVendor,
            PlatformError::MissingBootOption {
                description: "PXE IPv4".to_string(),
            },
            PlatformError::NoDpu,
            PlatformError::NoContent,
            PlatformError::Auth(AuthError::PasswordChangeRequired),
            PlatformError::Bmc {
                status: 409,
                message_id: Some("Base.1.0.ResourceInUse".to_string()),
                message: "resource is busy".to_string(),
            },
            PlatformError::InvalidResponse {
                message: "missing task location".to_string(),
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
            assert!(!error.to_string().is_empty());
        }
    }
}
