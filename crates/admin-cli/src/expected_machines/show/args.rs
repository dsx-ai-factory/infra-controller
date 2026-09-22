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

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use mac_address::MacAddress;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all expected machines:
    $ nico-admin-cli expected-machine show

Show one expected machine by BMC MAC address:
    $ nico-admin-cli expected-machine show 00:11:22:33:44:55

Show one expected machine by id:
    $ nico-admin-cli expected-machine show --id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(
        default_value(None),
        help = "BMC MAC address of the expected machine to show. Omit both this address and --id to list all expected machines."
    )]
    bmc_mac_address: Option<MacAddress>,

    #[clap(
        long,
        help = "ID (UUID) of the expected machine to show. Cannot be combined with a BMC MAC address."
    )]
    id: Option<Uuid>,
}

impl TryFrom<Args> for Option<::rpc::forge::ExpectedMachineRequest> {
    type Error = clap::Error;

    fn try_from(args: Args) -> Result<Self, Self::Error> {
        match (args.bmc_mac_address, args.id) {
            (Some(_), Some(_)) => Err(Args::command()
                .bin_name("nico-admin-cli expected-machine show")
                .error(
                    ErrorKind::ArgumentConflict,
                    "cannot specify both a BMC MAC address and --id; provide only one",
                )),
            (None, Some(id)) => Ok(Some(::rpc::forge::ExpectedMachineRequest {
                bmc_mac_address: String::new(),
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
            })),
            (Some(mac), None) => Ok(Some(::rpc::forge::ExpectedMachineRequest {
                bmc_mac_address: mac.to_string(),
                id: None,
            })),
            (None, None) => Ok(None),
        }
    }
}
