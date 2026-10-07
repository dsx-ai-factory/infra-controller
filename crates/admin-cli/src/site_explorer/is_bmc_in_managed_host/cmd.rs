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

use ::rpc::forge::BmcEndpointRequest;
use mac_address::MacAddress;

use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn is_bmc_in_managed_host(
    api_client: &ApiClient,
    address: &str,
    mac: Option<MacAddress>,
) -> CarbideCliResult<()> {
    let is_bmc_in_managed_host = api_client
        .0
        .is_bmc_in_managed_host(BmcEndpointRequest {
            ip_address: address.to_string(),
            mac_address: mac.map(|m| m.to_string()),
        })
        .await?;
    println!(
        "Is {} in a managed host?: {}",
        address, is_bmc_in_managed_host.in_managed_host
    );
    Ok(())
}
