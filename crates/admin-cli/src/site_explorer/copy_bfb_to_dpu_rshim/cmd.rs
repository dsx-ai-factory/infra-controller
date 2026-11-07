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

use ::rpc::forge::{BmcEndpointRequest, CopyBfbToDpuRshimRequest, SshRequest};

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn copy_bfb_to_dpu_rshim(
    api_client: &ApiClient,
    args: Args,
) -> CarbideCliResult<()> {
    api_client
        .0
        .copy_bfb_to_dpu_rshim(CopyBfbToDpuRshimRequest {
            ssh_request: Some(SshRequest {
                endpoint_request: Some(BmcEndpointRequest {
                    ip_address: args.address.to_string(),
                    mac_address: args.mac.map(|m| m.to_string()),
                }),
            }),
            host_bmc_ip: args.host_bmc_ip,
            pre_copy_powercycle: args.pre_copy_powercycle,
        })
        .await?;

    tracing::info!(
        "BFB recovery triggered for {}. Track progress via: site-explorer get-report endpoint {}",
        args.address,
        args.address
    );
    Ok(())
}
