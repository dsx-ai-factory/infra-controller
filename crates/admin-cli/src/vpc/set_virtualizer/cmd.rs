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
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// set_network_virtualization_type is the CLI handler for wrapping
/// a `vpc set-virtualizer` command, taking configuration and doing
/// necessary prep work before handing off to the actual RPC handler
/// to send out an RPC (rpc::set_vpc_network_virtualization_type).
///
/// This is intended for dev use only, and can only be done on a VPC
/// with 0 instances (an error will be returned otherwise).
pub(super) async fn set_network_virtualization_type(
    api_client: &ApiClient,
    args: Args,
) -> CarbideCliResult<()> {
    // TODO(chet): This should probably just be implied
    // and handled as part of set_vpc_network_virtualization_type,
    // and if it's not found, just return that. BUT, since if_version_match
    // comes into play, an `fetch_one` error might be returned if
    // the VPC doesn't exist, OR if there's a version mismatch, so
    // it can be kind of misleading. For now, just do this.
    let mut vpcs = api_client.0.find_vpcs_by_ids(&[args.id]).await?;

    if vpcs.vpcs.len() != 1 {
        return Err(CarbideCliError::GenericError("Unknown VPC ID".to_string()));
    }

    api_client
        .set_vpc_network_virtualization_type(vpcs.vpcs.remove(0), args.virtualizer.into())
        .await
}
