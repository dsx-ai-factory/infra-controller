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

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Enable the BMC proxy and point it at a host:port:
    $ nico-admin-cli set bmc-proxy --enabled true --proxy 192.0.2.10:8080

Disable the BMC proxy:
    $ nico-admin-cli set bmc-proxy --enabled false

")]
pub(crate) struct Args {
    #[clap(
        long,
        action = clap::ArgAction::Set,
        help = "Enable the dynamic site_explorer.bmc_proxy redirect for development and \
                integration environments. This setting is independent of static \
                [bmc_proxy] routing through nico-bmc-proxy."
    )]
    pub(super) enabled: bool,
    #[clap(long, action = clap::ArgAction::Set, help = "host:port string use as a proxy for talking to BMC's")]
    pub(super) proxy: Option<String>,
}
