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

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show the full network topology (all devices):
    $ nico-admin-cli network-device show

Show one network device by MAC:
    $ nico-admin-cli network-device show mac=00:11:22:33:44:55

")]
pub(crate) struct Args {
    #[clap(
        short,
        long,
        action,
        conflicts_with = "id",
        help = "Show all network devices (DEPRECATED)"
    )]
    all: bool,

    #[clap(
        default_value(""),
        help = "Show data for the given network device (e.g. `mac=<mac>`), leave empty for all (default)"
    )]
    id: String,
}

impl From<Args> for ::rpc::forge::NetworkTopologyRequest {
    fn from(args: Args) -> Self {
        let id = if args.all || args.id.is_empty() {
            None
        } else {
            Some(args.id)
        };
        Self { id }
    }
}
