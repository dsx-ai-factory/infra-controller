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

use ::rpc::forge::InterfaceDeleteQuery;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn handle_delete(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    // Clap's ArgGroup guarantees exactly one of these is set.
    api_client
        .0
        .delete_interface(InterfaceDeleteQuery {
            id: args.interface_id,
            mac_address: args.mac_address.map(|mac| mac.to_string()),
        })
        .await?;
    Ok(())
}
