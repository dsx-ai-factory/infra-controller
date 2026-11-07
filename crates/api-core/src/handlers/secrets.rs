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

use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;

const DEFAULT_RE_WRAP_BATCH_SIZE: i64 = 100;

/// Re-wrap batches stay within this range: zero would scan nothing while
/// reporting success, and an enormous batch holds that many rows -- and
/// their sequential KMS round-trips -- in memory between commits.
const RE_WRAP_BATCH_SIZE_RANGE: std::ops::RangeInclusive<i64> = 1..=10_000;

pub(crate) async fn re_wrap_secrets(
    api: &Api,
    request: Request<rpc::forge::ReWrapSecretsRequest>,
) -> Result<Response<rpc::forge::ReWrapSecretsResponse>, Status> {
    crate::api::log_request_data(&request);

    let req = request.into_inner();
    let batch_size = req
        .batch_size
        .map(|b| {
            i64::from(b).clamp(
                *RE_WRAP_BATCH_SIZE_RANGE.start(),
                *RE_WRAP_BATCH_SIZE_RANGE.end(),
            )
        })
        .unwrap_or(DEFAULT_RE_WRAP_BATCH_SIZE);

    let ctx = api.secrets_context.as_ref().ok_or_else(|| {
        CarbideError::FailedPrecondition(
            "secrets backend not configured -- no [secrets] section in config".to_string(),
        )
    })?;

    let result = crate::secrets::re_wrap_stale(
        &api.database_connection,
        &api.work_lock_manager_handle,
        ctx.kms.as_ref(),
        &ctx.routing,
        batch_size,
    )
    .await
    .map_err(|e| match e {
        crate::secrets::PgSecretsError::ReWrapInProgress => {
            CarbideError::FailedPrecondition(e.to_string())
        }
        other => CarbideError::internal(format!("re-wrap failed: {other}")),
    })?;

    tracing::info!(
        rewrapped_secret_count = result.re_wrapped,
        already_current_secret_count = result.already_current,
        remaining_stale_secret_count = result.stale_remaining,
        "secrets re-wrap completed"
    );

    Ok(Response::new(rpc::forge::ReWrapSecretsResponse {
        re_wrapped: result.re_wrapped,
        already_current: result.already_current,
        stale_remaining: result.stale_remaining,
    }))
}
