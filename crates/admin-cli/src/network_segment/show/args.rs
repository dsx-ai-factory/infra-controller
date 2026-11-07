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
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all network segments:
    $ nico-admin-cli network-segment show

Show one network segment by ID:
    $ nico-admin-cli network-segment show 12345678-1234-5678-90ab-cdef01234567

Filter by tenant org:
    $ nico-admin-cli network-segment show --tenant-org-id fds34511233a

")]
pub(crate) struct Args {
    #[clap(
        default_value(None),
        help = "The network segment to query, leave empty for all (default)"
    )]
    pub(crate) network: Option<NetworkSegmentId>,

    #[clap(short, long, help = "The Tenant Org ID to query")]
    pub(crate) tenant_org_id: Option<String>,

    #[clap(short, long, help = "The VPC name to query")]
    pub(crate) name: Option<String>,
}
