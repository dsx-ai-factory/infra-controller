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

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Copy a BFB to a DPU's rshim via its BMC, power-cycling the host afterward:
    $ nico-admin-cli site-explorer copy-bfb-to-dpu-rshim 192.0.2.10 \
    --host-bmc-ip 192.0.2.20

Power-cycle the host first to release rshim control to the DPU BMC:
    $ nico-admin-cli site-explorer copy-bfb-to-dpu-rshim 192.0.2.10 \
    --host-bmc-ip 192.0.2.20 --pre-copy-powercycle

")]
pub(crate) struct Args {
    #[clap(help = "BMC IP address or hostname with optional port")]
    pub(super) address: String,
    #[clap(long, help = "The MAC address the BMC sent DHCP from")]
    pub(super) mac: Option<MacAddress>,
    #[clap(
        long,
        help = "Host BMC IP address. Required for the mandatory post-copy host power-cycle \
                that applies the new BFB image to the DPU."
    )]
    pub(super) host_bmc_ip: String,
    #[clap(
        long,
        help = "Power-cycle the host before the BFB copy to release rshim control to the DPU BMC."
    )]
    pub(super) pre_copy_powercycle: bool,
}
