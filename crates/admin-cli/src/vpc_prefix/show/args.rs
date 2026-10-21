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

use carbide_uuid::vpc::VpcId;
use clap::Parser;
use ipnet::IpNet;
use rpc::forge::DeletedFilter;

use crate::vpc_prefix::common::VpcPrefixSelector;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all VPC prefixes:
    $ nico-admin-cli vpc-prefix show

Show one prefix by ID:
    $ nico-admin-cli vpc-prefix show 12345678-1234-5678-90ab-cdef01234567

Show one prefix by its exact CIDR:
    $ nico-admin-cli vpc-prefix show 10.0.0.0/24

List the prefixes belonging to one VPC:
    $ nico-admin-cli vpc-prefix show --vpc-id 12345678-1234-5678-90ab-cdef01234567

Find the prefix that contains an address:
    $ nico-admin-cli vpc-prefix show --contains 10.0.0.5

Find the prefixes contained by a larger prefix:
    $ nico-admin-cli vpc-prefix show --contained-by 10.0.0.0/16

")]
pub(crate) struct Args {
    #[clap(
        name = "VpcPrefixSelector",
        help = "The VPC prefix (by ID or exact unique prefix) to show (omit for all)"
    )]
    pub(super) prefix_selector: Option<VpcPrefixSelector>,

    #[clap(
        long,
        name = "vpc-id",
        value_name = "VpcId",
        help = "Search by VPC ID",
        conflicts_with = "VpcPrefixSelector"
    )]
    pub(super) vpc_id: Option<VpcId>,

    #[clap(
        long,
        name = "contains",
        value_name = "address-or-prefix",
        help = "Search by an address or prefix the VPC prefix contains",
        value_parser = parse_address_or_prefix,
        conflicts_with_all = ["VpcPrefixSelector", "contained-by"],
    )]
    pub(super) contains: Option<IpNet>,

    #[clap(
        long,
        name = "contained-by",
        value_name = "prefix",
        help = "Search by a prefix containing the VPC prefix",
        conflicts_with_all = ["VpcPrefixSelector", "contains"],
    )]
    pub(super) contained_by: Option<IpNet>,

    /// Include soft-deleted VPC prefixes
    #[clap(long, value_enum, default_value = "exclude")]
    pub(super) deleted: DeletedFilter,
}

fn parse_address_or_prefix(value: &str) -> Result<IpNet, ipnet::AddrParseError> {
    value.parse::<IpNet>().or_else(|prefix_error| {
        value
            .parse::<IpAddr>()
            .map(IpNet::from)
            .map_err(|_| prefix_error)
    })
}
