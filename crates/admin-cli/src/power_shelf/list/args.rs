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

use clap::Parser;
use mac_address::MacAddress;
use rpc::forge::DeletedFilter;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all live power shelves:
    $ nico-admin-cli power-shelf list

Include deleted power shelves:
    $ nico-admin-cli power-shelf list --deleted include

Filter by controller state:
    $ nico-admin-cli power-shelf list --controller-state ready

Find a power shelf by its BMC MAC address:
    $ nico-admin-cli power-shelf list --bmc-mac 00:11:22:33:44:55

")]
pub(crate) struct Args {
    /// Include deleted power shelves
    #[clap(long, value_enum, default_value = "exclude")]
    pub(super) deleted: DeletedFilter,

    /// Filter by controller state (e.g. "ready", "initializing", "error")
    #[clap(long)]
    pub(super) controller_state: Option<String>,

    /// Filter by BMC MAC address
    #[clap(long)]
    pub(super) bmc_mac: Option<MacAddress>,
}
