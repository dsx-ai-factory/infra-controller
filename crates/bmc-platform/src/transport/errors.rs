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

use nv_redfish::bmc_http::reqwest::BmcError;
use nv_redfish::core::ActionError;

use crate::{AuthError, PlatformError};

/// Classifies a transport's failure into the platform error vocabulary.
///
/// Implemented once per HTTP client error type, so a context built over a
/// given `Bmc` always classifies the same way and tests cannot substitute a
/// weaker mapping. Drivers invoke typed Redfish actions, so the transport must
/// also report an action the service does not advertise.
pub trait ClassifyBmcError: ActionError {
    fn classify(self) -> PlatformError;
}

impl ClassifyBmcError for BmcError {
    fn classify(self) -> PlatformError {
        match self {
            Self::ReqwestError(_) => PlatformError::Unreachable,
            Self::InvalidResponse { status, text, .. } => {
                PlatformError::from_http_response(status.as_u16(), &text)
            }
            Self::ActionNotSupported => PlatformError::Unsupported,
            other => PlatformError::InvalidResponse {
                message: other.to_string(),
            },
        }
    }
}

impl PlatformError {
    /// Classifies a non-success HTTP response by status and Redfish error body.
    ///
    /// 401 and 403 are authentication failures the runtime may retry after a
    /// credential refresh, except a 403 whose `@Message.ExtendedInfo` reports
    /// `PasswordChangeRequired`, which a refresh cannot clear. Every other
    /// status, including 409, keeps the first `@Message.ExtendedInfo` entry,
    /// falling back to the top-level `code`/`message`, then the raw body;
    /// drivers that know a status means "already in the requested state"
    /// interpret it themselves.
    pub fn from_http_response(status: u16, body: &str) -> Self {
        match status {
            401 => Self::Auth(AuthError::InvalidCredentials),
            403 => Self::Auth(
                password_change_required(body).unwrap_or(AuthError::InsufficientPrivilege),
            ),
            status => {
                let (message_id, message) = redfish_error_details(body);
                Self::Bmc {
                    status,
                    message_id,
                    message,
                }
            }
        }
    }
}

/// The `PasswordChangeRequired` entry of a 403 body, whose first message
/// argument is the account URI to change.
fn password_change_required(body: &str) -> Option<AuthError> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    let message = error
        .get("@Message.ExtendedInfo")?
        .as_array()?
        .iter()
        .find(|message| {
            message
                .get("MessageId")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id.ends_with("PasswordChangeRequired"))
        })?;
    let account_uri = message
        .get("MessageArgs")
        .and_then(serde_json::Value::as_array)
        .and_then(|arguments| arguments.first())
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Some(AuthError::PasswordChangeRequired { account_uri })
}

fn redfish_error_details(body: &str) -> (Option<String>, String) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return (None, body.to_string());
    };
    let error = value.get("error").unwrap_or(&value);
    let extended = error
        .get("@Message.ExtendedInfo")
        .and_then(serde_json::Value::as_array)
        .and_then(|messages| messages.first());
    let text = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    };
    let message_id = text(extended.and_then(|message| message.get("MessageId")))
        .or_else(|| text(error.get("code")));
    let message = text(extended.and_then(|message| message.get("Message")))
        .or_else(|| text(error.get("message")))
        .unwrap_or_else(|| body.to_string());
    (message_id, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_classification_is_stable() {
        assert_eq!(
            PlatformError::from_http_response(401, ""),
            PlatformError::Auth(AuthError::InvalidCredentials)
        );
        assert_eq!(
            PlatformError::from_http_response(403, ""),
            PlatformError::Auth(AuthError::InsufficientPrivilege)
        );
        let password_change = |args: serde_json::Value| {
            serde_json::json!({
                "error": {
                    "code": "Base.1.8.GeneralError",
                    "@Message.ExtendedInfo": [{
                        "MessageId": "Base.1.18.1.PasswordChangeRequired",
                        "MessageArgs": args
                    }]
                }
            })
            .to_string()
        };
        assert_eq!(
            PlatformError::from_http_response(
                403,
                &password_change(serde_json::json!([
                    "/redfish/v1/AccountService/Accounts/root"
                ]))
            ),
            PlatformError::Auth(AuthError::PasswordChangeRequired {
                account_uri: Some("/redfish/v1/AccountService/Accounts/root".to_string()),
            })
        );
        assert_eq!(
            PlatformError::from_http_response(403, &password_change(serde_json::json!([]))),
            PlatformError::Auth(AuthError::PasswordChangeRequired { account_uri: None })
        );
        let body = serde_json::json!({
            "error": {
                "code": "Base.1.0.GeneralError",
                "message": "general failure",
                "@Message.ExtendedInfo": [{
                    "MessageId": "Base.1.0.ResourceInUse",
                    "Message": "resource is busy"
                }]
            }
        })
        .to_string();
        assert_eq!(
            PlatformError::from_http_response(400, &body),
            PlatformError::Bmc {
                status: 400,
                message_id: Some("Base.1.0.ResourceInUse".to_string()),
                message: "resource is busy".to_string(),
            }
        );
        assert_eq!(
            PlatformError::from_http_response(500, "not JSON"),
            PlatformError::Bmc {
                status: 500,
                message_id: None,
                message: "not JSON".to_string(),
            }
        );
    }
}
