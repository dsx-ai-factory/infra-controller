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

use carbide_uuid::instance::InstanceId;
use carbide_uuid::vpc::VpcId;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Attach a network security group to a VPC:
    $ nico-admin-cli network-security-group attach --id 12345678-1234-5678-90ab-cdef01234567 \
    --vpc-id abcdef01-2345-6789-abcd-ef0123456789

Attach it to a single instance:
    $ nico-admin-cli network-security-group attach --id 12345678-1234-5678-90ab-cdef01234567 \
    --instance-id abcdef01-2345-6789-abcd-ef0123456789

")]
pub(crate) struct Args {
    #[clap(short = 'n', long, help = "Network security group ID to attach")]
    pub(super) id: String,

    #[clap(
        short = 'v',
        long,
        help = "Optional, VPC ID that should have the network security group applied"
    )]
    pub(super) vpc_id: Option<VpcId>,

    #[clap(
        short = 'i',
        long,
        help = "Optional, Instance ID that should have the network security group applied"
    )]
    pub(super) instance_id: Option<InstanceId>,
}
