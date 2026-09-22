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

use carbide_uuid::rack::RackGroupId;
use clap::Parser;

use crate::expected_rack_group::common::Attributes;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Replace topology, rack membership, devices, and metadata:
    $ nico-admin-cli expected-rack-group update nvl5-gp1-jhb01 --topology gb200_nvl72r1_c2g4 \
    --rack-id rack-01 --member '{\"type\":\"Switch\",\"manufacturer\":\"NVIDIA\",\"id\":\"switch-01\"}' \
    --meta-name nvl5-gp1-jhb01 --label location.datacenter:JHB01

Clear rack/device lists and metadata while retaining the supplied topology:
    $ nico-admin-cli expected-rack-group update nvl5-gp1-jhb01 --topology gb200_nvl72r1_c2g4

This is a full replacement, not a patch. Resubmit every field you want to retain.

")]
pub(crate) struct Args {
    /// Existing external group ID.
    rack_group_id: RackGroupId,
    /// Replacement topology identifier (required).
    #[arg(long)]
    topology: String,
    #[command(flatten)]
    attributes: Attributes,
}
impl From<Args> for rpc::forge::ExpectedRackGroup {
    fn from(args: Args) -> Self {
        args.attributes.into_rpc(args.rack_group_id, args.topology)
    }
}
