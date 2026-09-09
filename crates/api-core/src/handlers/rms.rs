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

use ::rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

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
            "RMS is not configured on this API server".into(),
        )
        .into());
    };

    let resp = rms_client
        .get_version()
        .await
        .map_err(|e| -> Status { CarbideError::from(e).into() })?;

    Ok(Response::new(rpc::GetRmsVersionResponse {
        version: resp.version,
    }))
}
