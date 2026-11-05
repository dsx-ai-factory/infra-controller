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

use eyre::WrapErr;

use crate::attestation::spdm::job::waive::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn waive(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    // The controller records the waiver after evidence collection cleanup.
    api_client
        .0
        .waive_attestation(::rpc::common::Uuid {
            value: args.job_id.to_string(),
        })
        .await
        .context("waive the SPDM attestation job")?;
    println!(
        "Waiver requested; terminal results count as Waived and unfinished work will resolve after BMC cleanup"
    );
    Ok(())
}
