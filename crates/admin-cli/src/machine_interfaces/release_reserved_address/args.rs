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

use clap::{ArgGroup, Parser};
use mac_address::MacAddress;

#[derive(Parser, Debug)]
#[command(
    long_about = "Release parked address reservations, making their addresses available to allocators again.\n\nAt least one of --mac-address or --address must be specified so a release cannot clear every reservation by accident. Only parked reservations are affected; active interface addresses are never released.",
    after_long_help = "\
EXAMPLES:

Release every reservation owned by a MAC:
    $ nico-admin-cli machine-interfaces release-reserved-address --mac-address 00:11:22:33:44:55

Release the reservation for a single address:
    $ nico-admin-cli machine-interfaces release-reserved-address --address 192.0.2.10

Release a specific address only if it is owned by the given MAC:
    $ nico-admin-cli machine-interfaces release-reserved-address --mac-address 00:11:22:33:44:55 \
    --address 192.0.2.10

"
)]
#[clap(group(
    ArgGroup::new("reservation_selector")
        .required(true)
        .multiple(true)
        .args(["mac_address", "address"]),
))]
pub(crate) struct Args {
    #[clap(long, help = "Release reservations owned by this MAC address.")]
    pub(super) mac_address: Option<MacAddress>,

    #[clap(long, help = "Release the reservation for this exact address.")]
    pub(super) address: Option<IpAddr>,
}
