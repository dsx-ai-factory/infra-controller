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

Create a BMC user, targeting the BMC by machine id:
    $ nico-admin-cli bmc-machine create-bmc-user \
    --machine 12345678-1234-5678-90ab-cdef01234567 --username admin --password mynewpassword

Target the BMC by IP address:
    $ nico-admin-cli bmc-machine create-bmc-user \
    --ip-address 192.0.2.20 --username admin --password mynewpassword

Target the BMC by MAC address:
    $ nico-admin-cli bmc-machine create-bmc-user \
    --mac-address 00:11:22:33:44:55 --username admin --password mynewpassword

Create a read-only user by setting an explicit role:
    $ nico-admin-cli bmc-machine create-bmc-user \
    --machine 12345678-1234-5678-90ab-cdef01234567 --username admin --password mynewpassword \
    --role-id readonly

")]
pub(crate) struct Args {
    #[clap(long, short, help = "IP of the BMC where we want to create a new user")]
    ip_address: Option<String>,
    #[clap(long, help = "MAC of the BMC where we want to create a new user")]
    mac_address: Option<MacAddress>,
    #[clap(
        long,
        short,
        help = "ID of the machine where we want to create a new user"
    )]
    machine: Option<String>,

    #[clap(long, short, help = "Username of new BMC account")]
    username: String,
    #[clap(long, short, help = "Password of new BMC account")]
    password: String,
    #[clap(
        long,
        short,
        value_enum,
        help = "Role of new BMC account (default: administrator)"
    )]
    role_id: Option<crate::bmc_role::BmcRole>,
}

impl From<Args> for forgerpc::CreateBmcUserRequest {
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
            create_username: args.username,
            create_password: args.password,
            create_role_id: args.role_id.map(|role| role.as_api_str().to_string()),
        }
    }
}
