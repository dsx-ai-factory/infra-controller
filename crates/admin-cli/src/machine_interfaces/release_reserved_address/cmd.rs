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

pub(super) async fn handle_release_reserved_address(
    args: Args,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    // Clap's ArgGroup guarantees at least one selector is set.
    let resp = api_client
        .0
        .admin_release_reserved_addresses(forgerpc::AdminReleaseReservedAddressesRequest {
            reserved_by_mac: args.mac_address.map(|mac| mac.to_string()),
            address: args.address.map(|address| address.to_string()),
        })
        .await?;

    if resp.released_addresses.is_empty() {
        println!("No matching reserved addresses to release");
    } else {
        println!(
            "Released {} reserved address(es):",
            resp.released_addresses.len()
        );
        for address in &resp.released_addresses {
            println!("  {address}");
        }
    }

    Ok(())
}
