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

use std::net::IpAddr;

use clap::Parser;
use mac_address::MacAddress;

#[derive(Parser, Debug)]
#[command(
    long_about = "List parked address reservations that outlived their interface.\n\nA reservation is an address kept by its interface MAC after the interface row was deleted, so the same MAC can reclaim it on re-ingestion. With no filter, every parked reservation is listed; the filters narrow the listing.",
    after_long_help = "\
EXAMPLES:

List every parked address reservation:
    $ nico-admin-cli machine-interfaces show-reserved-addresses

List reservations owned by one MAC:
    $ nico-admin-cli machine-interfaces show-reserved-addresses --mac-address 00:11:22:33:44:55

Show the reservation for one address:
    $ nico-admin-cli machine-interfaces show-reserved-addresses --address 192.0.2.10

"
)]
pub(crate) struct Args {
    #[clap(long, help = "Only show reservations owned by this MAC address.")]
    pub(super) mac_address: Option<MacAddress>,

    #[clap(long, help = "Only show the reservation for this exact address.")]
    pub(super) address: Option<IpAddr>,
}
