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

use carbide_uuid::infiniband::IBPartitionId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all InfiniBand partitions:
    $ nico-admin-cli ib-partition show

Show one InfiniBand partition by ID:
    $ nico-admin-cli ib-partition show 12345678-1234-5678-90ab-cdef01234567

List partitions for one tenant:
    $ nico-admin-cli ib-partition show --tenant-org-id fds34511233a

Find a partition by name:
    $ nico-admin-cli ib-partition show --name my-partition

")]
pub(crate) struct Args {
    #[clap(
        default_value(None),
        help = "The InfiniBand Partition ID to query, leave empty for all (default)"
    )]
    pub(super) id: Option<IBPartitionId>,

    #[clap(short, long, help = "The Tenant Org ID to query")]
    pub(super) tenant_org_id: Option<String>,

    #[clap(short, long, help = "The InfiniBand Partition name to query")]
    pub(super) name: Option<String>,
}
