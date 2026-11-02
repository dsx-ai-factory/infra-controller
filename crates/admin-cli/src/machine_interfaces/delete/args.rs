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

use carbide_uuid::machine::MachineInterfaceId;
use clap::{ArgGroup, Parser};
use mac_address::MacAddress;

#[derive(Parser, Debug)]
#[command(
    long_about = "Delete a machine interface.\n\nExactly one deletion selector must be specified: INTERFACE_ID or --mac-address. Providing both selectors is rejected.",
    after_long_help = "\
EXAMPLES:

Delete a machine interface by ID (redeploy kea afterward):
    $ nico-admin-cli machine-interfaces delete 12345678-1234-5678-90ab-cdef01234567

Delete a leftover interface when you only have the BMC MAC (e.g. a replacement host
whose ingestion is blocked by a stale interface record):
    $ nico-admin-cli machine-interfaces delete --mac-address 00:11:22:33:44:55

"
)]
#[clap(group(
    ArgGroup::new("interface_selector")
        .required(true)
        .args(["interface_id", "mac_address"]),
))]
pub(crate) struct Args {
    #[clap(help = "The interface ID to delete.")]
    pub(super) interface_id: Option<MachineInterfaceId>,

    #[clap(
        long,
        help = "Delete every interface carrying this MAC address instead of selecting by ID."
    )]
    pub(super) mac_address: Option<MacAddress>,
}
