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

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn trim_measured_boot(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let request = ::rpc::forge::TrimTableRequest {
        target: ::rpc::forge::TrimTableTarget::MeasuredBoot.into(),
        keep_entries: args.keep_entries,
    };

    let response = api_client.0.trim_table(request).await?;

    println!(
        "Trimmed {} reports from Measured Boot",
        response.total_deleted
    );
    Ok(())
}
