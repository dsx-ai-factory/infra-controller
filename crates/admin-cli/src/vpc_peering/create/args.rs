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
use rpc::forge::VpcPeeringCreationRequest;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Peer two VPCs:
    $ nico-admin-cli vpc-peering create 12345678-1234-5678-90ab-cdef01234567 \
    abcdef01-2345-6789-abcd-ef0123456789

Peer two VPCs with a chosen peering ID:
    $ nico-admin-cli vpc-peering create 12345678-1234-5678-90ab-cdef01234567 \
    abcdef01-2345-6789-abcd-ef0123456789 --id 0fedcba9-8765-4321-0fed-cba987654321

")]
pub(crate) struct Args {
    #[clap(help = "The ID of one VPC ID to peer")]
    vpc1_id: VpcId,

    #[clap(help = "The ID of other VPC ID to peer")]
    vpc2_id: VpcId,

    #[clap(long, help = "Optional desired ID for the VPC peering")]
    id: Option<VpcPeeringId>,
}

impl From<Args> for VpcPeeringCreationRequest {
    fn from(args: Args) -> Self {
        VpcPeeringCreationRequest {
            vpc_id: Some(args.vpc1_id),
            peer_vpc_id: Some(args.vpc2_id),
            id: args.id,
        }
    }
}
