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

pub(super) async fn handle_assign_address(
    args: Args,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let resp = api_client
        .0
        .assign_static_address(forgerpc::AssignStaticAddressRequest {
            interface_id: Some(args.interface_id),
            ip_address: args.ip_address.to_string(),
        })
        .await?;

    match resp.status() {
        forgerpc::AssignStaticAddressStatus::Assigned => {
            println!(
                "Assigned static address {} to interface {}",
                resp.ip_address, args.interface_id
            );
        }
        forgerpc::AssignStaticAddressStatus::ReplacedStatic => {
            println!(
                "Replaced existing static address with {} on interface {}",
                resp.ip_address, args.interface_id
            );
        }
        forgerpc::AssignStaticAddressStatus::ReplacedDhcp => {
            println!(
                "Replaced DHCP allocation with static address {} on interface {}",
                resp.ip_address, args.interface_id
            );
        }
    }

    Ok(())
}
