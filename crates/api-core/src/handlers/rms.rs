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

use std::time::Duration;

use ::rpc::forge as rpc;
use librms::RackManagerError;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

/// Per-RPC deadline for forwarding `GetVersion` to the RMS backend.
///
/// `librms` applies per-stream connect/read/write timeouts at the transport
/// layer, but those do not bound the total time an active stream can remain
/// pending without completing.  This deadline caps the end-to-end duration of
/// the RPC so a backend that keeps the stream open without responding does not
/// hold the nico-api request indefinitely.
const GET_VERSION_TIMEOUT: Duration = Duration::from_secs(30);

// The `nico-admin-cli backend rms status` classifier
// (`crates/admin-cli/src/backend/rms/status/cmd.rs`) matches on these exact
// strings to tell failures on the nico-api -> RMS leg apart from failures on
// the CLI -> nico-api leg.  Change them only together with that classifier.

/// Message of the `Unavailable` status returned when no RMS client is configured.
const RMS_NOT_CONFIGURED_MESSAGE: &str = "rms is not configured on this API server";

/// Prefix on the message of every status that reports a failure of the call to
/// the RMS backend, whatever its code.
const RMS_ERROR_PREFIX: &str = "rms: ";

/// Status for a `GetVersion` call that did not complete within
/// [`GET_VERSION_TIMEOUT`].
fn get_version_timeout_status() -> Status {
    Status::deadline_exceeded(format!(
        "{RMS_ERROR_PREFIX}get_version timed out after {} seconds",
        GET_VERSION_TIMEOUT.as_secs()
    ))
}

/// Status for a failed `GetVersion` call.
///
/// Preserves the gRPC status code from the RMS backend so the CLI can
/// distinguish Unavailable, Unauthenticated, etc.  Going through
/// `CarbideError` would collapse every variant into Internal.
fn get_version_error_status(error: RackManagerError) -> Status {
    match error {
        RackManagerError::ApiInvocationError(status) => Status::new(
            status.code(),
            format!("{RMS_ERROR_PREFIX}{}", status.message()),
        ),
        // TlsError and any other non-API variant are connectivity failures;
        // surface them as Unavailable so the CLI can classify them as
        // rms-unreachable rather than generic error.
        other => Status::unavailable(format!("{RMS_ERROR_PREFIX}{other}")),
    }
}

/// Forward a `GetVersion` call to the configured RMS backend and return its
/// version string.  Returns `Unavailable` when RMS is not configured on this
/// API server instance.
pub(crate) async fn get_rms_version(
    api: &Api,
    request: Request<rpc::GetRmsVersionRequest>,
) -> Result<Response<rpc::GetRmsVersionResponse>, Status> {
    log_request_data(&request);

    let Some(rms_client) = api.rms_client.as_ref() else {
        return Err(CarbideError::UnavailableError(RMS_NOT_CONFIGURED_MESSAGE.into()).into());
    };

    let resp = tokio::time::timeout(GET_VERSION_TIMEOUT, rms_client.get_version())
        .await
        .map_err(|_elapsed| get_version_timeout_status())?
        .map_err(get_version_error_status)?;

    Ok(Response::new(rpc::GetRmsVersionResponse {
        version: resp.version,
    }))
}

#[cfg(test)]
mod tests {
    use librms::RmsTlsClientError;

    use super::*;

    #[test]
    fn not_configured_status_carries_the_sentinel_the_cli_matches() {
        let status: Status =
            CarbideError::UnavailableError(RMS_NOT_CONFIGURED_MESSAGE.into()).into();

        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "rms is not configured on this API server");
    }

    #[test]
    fn timeout_status_is_prefixed_so_the_cli_attributes_it_to_rms() {
        let status = get_version_timeout_status();

        assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
        assert_eq!(
            status.message(),
            "rms: get_version timed out after 30 seconds"
        );
    }

    #[test]
    fn error_status_preserves_code_and_prefixes_message() {
        let cases = [
            (
                "api error keeps its code",
                RackManagerError::ApiInvocationError(Status::unavailable("connection refused")),
                tonic::Code::Unavailable,
                "rms: connection refused",
            ),
            (
                "api error keeps a non-unavailable code",
                RackManagerError::ApiInvocationError(Status::permission_denied("no role")),
                tonic::Code::PermissionDenied,
                "rms: no role",
            ),
            (
                "tls error becomes unavailable",
                RackManagerError::TlsError(RmsTlsClientError::Connection("tcp reset".into())),
                tonic::Code::Unavailable,
                "rms: TLS client error: ConnectError error: tcp reset",
            ),
        ];

        for (name, error, want_code, want_message) in cases {
            let status = get_version_error_status(error);

            assert_eq!(status.code(), want_code, "{name}: code");
            assert_eq!(status.message(), want_message, "{name}: message");
        }
    }
}
