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
use rpc::forge as forgerpc;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Rotate the BMC root password, targeting the BMC by machine id:
    $ nico-admin-cli bmc-machine set-root-password \
    --machine 12345678-1234-5678-90ab-cdef01234567 --new-password mynewpassword

Target the BMC by IP address:
    $ nico-admin-cli bmc-machine set-root-password \
    --ip-address 192.0.2.20 --new-password mynewpassword

Target the BMC by MAC address:
    $ nico-admin-cli bmc-machine set-root-password \
    --mac-address 00:11:22:33:44:55 --new-password mynewpassword

")]
pub(crate) struct Args {
    #[clap(long, short, help = "IP of the BMC whose root password to set")]
    ip_address: Option<String>,
    #[clap(long, help = "MAC of the BMC whose root password to set")]
    mac_address: Option<MacAddress>,
    #[clap(long, short, help = "ID of the machine whose BMC root password to set")]
    machine: Option<String>,

    #[clap(long, help = "New BMC root password to set")]
    new_password: String,
}

impl From<Args> for forgerpc::SetBmcRootPasswordRequest {
    fn from(args: Args) -> Self {
        let bmc_endpoint_request = if args.ip_address.is_some() || args.mac_address.is_some() {
            Some(forgerpc::BmcEndpointRequest {
                ip_address: args.ip_address.unwrap_or_default(),
                mac_address: args.mac_address.map(|mac| mac.to_string()),
            })
        } else {
            None
        };

        Self {
            bmc_endpoint_request,
            machine_id: args.machine,
            new_password: args.new_password,
        }
    }
}
