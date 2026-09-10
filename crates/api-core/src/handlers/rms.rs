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

/// Forward a `GetVersion` call to the configured RMS backend and return its
/// version string.  Returns `Unavailable` when RMS is not configured on this
/// API server instance.
pub(crate) async fn get_rms_version(
    api: &Api,
    request: Request<rpc::GetRmsVersionRequest>,
) -> Result<Response<rpc::GetRmsVersionResponse>, Status> {
    log_request_data(&request);

    let Some(rms_client) = api.rms_client.as_ref() else {
        return Err(CarbideError::UnavailableError(
            "rms is not configured on this API server".into(),
        )
        .into());
    };

    let resp = tokio::time::timeout(GET_VERSION_TIMEOUT, rms_client.get_version())
        .await
        .map_err(|_elapsed| {
            Status::deadline_exceeded("rms get_version timed out after 30 seconds")
        })?
        .map_err(|e| -> Status {
            // Preserve the gRPC status code from the RMS backend so the CLI
            // can distinguish Unavailable, Unauthenticated, etc.  Going through
            // CarbideError would collapse every variant into Internal.
            match e {
                RackManagerError::ApiInvocationError(status) => {
                    Status::new(status.code(), format!("rms: {}", status.message()))
                }
                other => Status::internal(format!("rms: {other}")),
            }
        })?;

    Ok(Response::new(rpc::GetRmsVersionResponse {
        version: resp.version,
    }))
}
