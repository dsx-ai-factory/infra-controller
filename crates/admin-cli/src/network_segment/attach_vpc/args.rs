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

use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Attach a network segment to a VPC:
    $ nico-admin-cli network-segment attach-vpc --id 12345678-1234-5678-90ab-cdef01234567 \
    --vpc-id abcdef01-2345-6789-abcd-ef0123456789

Reassign a network segment from its current VPC:
    $ nico-admin-cli network-segment attach-vpc --id 12345678-1234-5678-90ab-cdef01234567 \
    --vpc-id abcdef01-2345-6789-abcd-ef0123456789 --force

")]
pub(crate) struct Args {
    #[clap(long, help = "ID of the network segment")]
    id: NetworkSegmentId,

    #[clap(long, help = "ID of the VPC")]
    vpc_id: VpcId,

    #[clap(
        long,
        help = "Allow reassigning a segment that is attached to another VPC"
    )]
    force: bool,
}

impl From<Args> for ::rpc::forge::AttachNetworkSegmentToVpcRequest {
    fn from(args: Args) -> Self {
        Self {
            network_segment_id: Some(args.id),
            vpc_id: Some(args.vpc_id),
            allow_replace: args.force,
        }
    }
}
