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

pub(super) async fn set_uefi_password(data: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    api_client.0.set_dpu_uefi_password(data).await?;
    // A DPU stages the change through Redfish BIOS settings and schedules no job;
    // it commits on the next DPU restart, so there is no job id to report.
    println!(
        "successfully staged the site-wide UEFI password on the DPU; it commits on the next DPU restart"
    );
    Ok(())
}
