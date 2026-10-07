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

use ::rpc::forge as forgerpc;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn rotate(data: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let response = api_client
        .0
        .rotate_credential(forgerpc::RotateCredentialRequest::try_from(data)?)
        .await?;

    // Print the non-secret result so the operator can see the staged target.
    // Devices converge to it asynchronously; the password itself is never echoed.
    let started_at = response
        .started_at
        .map(|t| t.to_string())
        .unwrap_or_default();
    println!(
        "Staged credential rotation: target version {} (started {started_at}). \
         Devices converge asynchronously; check `credential rotation-status`.",
        response.target_version,
    );
    Ok(())
}
