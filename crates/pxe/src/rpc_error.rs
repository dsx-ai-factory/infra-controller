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
use std::fmt::{Debug, Display};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_client_ip::Rejection;

pub(super) enum PxeRequestError {
    CarbideApiError(tonic::Status),
    MissingClientConfig,
    MissingIp(Rejection),
    InvalidBuildArch,
    MalformedBuildArch(String),
}

impl IntoResponse for PxeRequestError {
    fn into_response(self) -> Response {
        let response_string = self.to_string();
        let mut response = response_string.into_response();
        *response.status_mut() = StatusCode::BAD_REQUEST;
        response
    }
}

impl Debug for PxeRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self, f)
    }
}

impl Display for PxeRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::CarbideApiError(err) => format!("Error making a carbide API request: {err}"),
                Self::MissingClientConfig =>
                    "Missing client configuration from server config (should not reach this case)"
                        .to_string(),
                Self::InvalidBuildArch =>
                    "Invalid build arch specified in URI parameter buildarch".to_string(),
                Self::MalformedBuildArch(err) => format!("Malformed build arch: {err}"),
                Self::MissingIp(err) => format!("Source IP is missing. Error: {err:?}"),
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum ErrorCase {
        MissingClientConfig,
        InvalidBuildArch,
        MalformedBuildArch,
    }

    fn error_for(case: ErrorCase) -> PxeRequestError {
        match case {
            ErrorCase::MissingClientConfig => PxeRequestError::MissingClientConfig,
            ErrorCase::InvalidBuildArch => PxeRequestError::InvalidBuildArch,
            ErrorCase::MalformedBuildArch => {
                PxeRequestError::MalformedBuildArch("bad arch".to_string())
            }
        }
    }

    fn display_error(case: ErrorCase) -> String {
        error_for(case).to_string()
    }

    fn debug_matches_display(case: ErrorCase) -> bool {
        let error = error_for(case);
        format!("{error:?}") == error.to_string()
    }

    fn response_status(case: ErrorCase) -> StatusCode {
        error_for(case).into_response().status()
    }

    #[test]
    fn formats_pxe_request_errors() {
        value_scenarios!(display_error:
            "missing inputs" {
                ErrorCase::MissingClientConfig => "Missing client configuration from server config (should not reach this case)".to_string(),
                ErrorCase::InvalidBuildArch => "Invalid build arch specified in URI parameter buildarch".to_string(),
            }

            "malformed inputs" {
                ErrorCase::MalformedBuildArch => "Malformed build arch: bad arch".to_string(),
            }
        );
    }

    #[test]
    fn debug_matches_display_for_pxe_request_errors() {
        value_scenarios!(debug_matches_display:
            "debug" {
                ErrorCase::MissingClientConfig => true,
                ErrorCase::InvalidBuildArch => true,
                ErrorCase::MalformedBuildArch => true,
            }
        );
    }

    #[test]
    fn converts_pxe_request_errors_to_bad_request_responses() {
        value_scenarios!(response_status:
            "response status" {
                ErrorCase::MissingClientConfig => StatusCode::BAD_REQUEST,
                ErrorCase::InvalidBuildArch => StatusCode::BAD_REQUEST,
                ErrorCase::MalformedBuildArch => StatusCode::BAD_REQUEST,
            }
        );
    }
}
