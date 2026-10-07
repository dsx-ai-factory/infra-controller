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

use rpc::forge::CreateRemediationRequest;

use super::args::Args;
use crate::errors::CarbideCliError;
use crate::rpc::ApiClient;

pub(super) async fn create_dpu_remediation(
    create_remediation: Args,
    api_client: &ApiClient,
) -> Result<(), CarbideCliError> {
    let script = tokio::fs::read_to_string(&create_remediation.script_filename)
        .await
        .map_err(|err| {
            tracing::error!("Error reading script file for dpu remediation: {:?}", err);
            CarbideCliError::IOError(err)
        })?;

    let response = api_client
        .0
        .create_remediation(CreateRemediationRequest {
            script,
            retries: create_remediation.retries.unwrap_or_default() as i32,
            metadata: create_remediation.into_metadata(),
        })
        .await?;

    tracing::info!("Created remediation with id: {:?}", response.remediation_id);
    Ok(())
}
