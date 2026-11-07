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

use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn show_unmatched_ek(api_client: &ApiClient) -> CarbideCliResult<()> {
    let unmatched_eks = api_client
        .0
        .tpm_show_unmatched_ek_certs()
        .await?
        .tpm_ek_cert_statuses;
    println!("{}", serde_json::to_string_pretty(&unmatched_eks)?);

    Ok(())
}
