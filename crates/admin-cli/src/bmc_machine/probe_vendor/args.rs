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

Probe a BMC's Redfish vendor, targeting the BMC by machine id:
    $ nico-admin-cli bmc-machine probe-vendor --machine 12345678-1234-5678-90ab-cdef01234567

Target the BMC by IP address:
    $ nico-admin-cli bmc-machine probe-vendor --ip-address 192.0.2.20

Target the BMC by MAC address:
    $ nico-admin-cli bmc-machine probe-vendor --mac-address 00:11:22:33:44:55

")]
pub(crate) struct Args {
    #[clap(long, short, help = "IP of the BMC whose vendor to probe")]
    ip_address: Option<String>,
    #[clap(long, help = "MAC of the BMC whose vendor to probe")]
    mac_address: Option<MacAddress>,
    #[clap(long, short, help = "ID of the machine whose BMC vendor to probe")]
    machine: Option<String>,
}

impl From<Args> for forgerpc::ProbeBmcVendorRequest {
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
        }
    }
}
