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

use bmc_platform::{AuthError, PlatformError};
use libredfish::RedfishError;

/// Keeps libredfish's message wherever the platform error can carry one.
pub(crate) fn platform_error(error: RedfishError) -> PlatformError {
    match error {
        RedfishError::NotSupported(_) => PlatformError::Unsupported,
        RedfishError::Lockdown => PlatformError::LockedDown,
        RedfishError::NoDpu => PlatformError::NoDpu,
        RedfishError::NoContent => PlatformError::NoContent,
        RedfishError::TooManyUsers => PlatformError::TooManyUsers,
        RedfishError::UserNotFound(identifier) => PlatformError::UserNotFound { identifier },
        RedfishError::MissingBootOption(description) => {
            PlatformError::MissingBootOption { description }
        }
        RedfishError::PasswordChangeRequired { account_uri } => {
            PlatformError::Auth(AuthError::PasswordChangeRequired { account_uri })
        }
        // Classified as the drivers classify responses, so callers branching
        // on `Auth` behave the same on either backend.
        RedfishError::HTTPErrorCode {
            status_code,
            response_body,
            ..
        } => PlatformError::from_http_response(status_code.as_u16(), &response_body),
        RedfishError::NetworkError { .. } | RedfishError::ReqwestError(_) => {
            PlatformError::Unreachable
        }
        RedfishError::JsonDeserializeError { .. }
        | RedfishError::JsonSerializeError { .. }
        | RedfishError::InvalidValue { .. }
        | RedfishError::GenericError { .. }
        | RedfishError::NoHeader
        | RedfishError::MissingVendor
        | RedfishError::FileError(_)
        | RedfishError::UnnecessaryOperation
        | RedfishError::MissingKey { .. }
        | RedfishError::InvalidKeyType { .. }
        | RedfishError::TypeMismatch { .. } => PlatformError::InvalidResponse {
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn libredfish_errors_keep_the_cases_callers_branch_on() {
        value_scenarios!(platform_error:
            "typed" {
                RedfishError::NotSupported("AC power cycle".to_string()) => PlatformError::Unsupported,
                RedfishError::Lockdown => PlatformError::LockedDown,
                RedfishError::NoDpu => PlatformError::NoDpu,
            }
            "classified like driver responses" {
                RedfishError::HTTPErrorCode {
                    url: "https://192.0.2.10/redfish/v1/Systems/1".to_string(),
                    status_code: http::StatusCode::UNAUTHORIZED,
                    response_body: String::new(),
                } => PlatformError::Auth(AuthError::InvalidCredentials),
                RedfishError::HTTPErrorCode {
                    url: "https://192.0.2.10/redfish/v1/Systems/1".to_string(),
                    status_code: http::StatusCode::FORBIDDEN,
                    response_body: String::new(),
                } => PlatformError::Auth(AuthError::InsufficientPrivilege),
            }
            "message preserved" {
                RedfishError::GenericError { error: "BMC busy".to_string() } => PlatformError::InvalidResponse {
                    message: RedfishError::GenericError { error: "BMC busy".to_string() }.to_string(),
                },
            }
        );
    }
}
