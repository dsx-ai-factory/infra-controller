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

use carbide_uuid::vpc::VpcId;
use carbide_uuid::vpc_peering::VpcPeeringId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all VPC peerings:
    $ nico-admin-cli vpc-peering show

Show details for one VPC peering:
    $ nico-admin-cli vpc-peering show --id 12345678-1234-5678-90ab-cdef01234567

List the peerings for one VPC:
    $ nico-admin-cli vpc-peering show --vpc-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(
        long,
        conflicts_with = "vpc_id",
        help = "The ID of the VPC peering to show"
    )]
    pub(super) id: Option<VpcPeeringId>,

    #[clap(
        long,
        conflicts_with = "id",
        help = "The ID of the VPC to show VPC peerings for"
    )]
    pub(super) vpc_id: Option<VpcId>,
}
