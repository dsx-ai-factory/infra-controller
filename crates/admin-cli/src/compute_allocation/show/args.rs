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

use ::rpc::forge::FindComputeAllocationIdsRequest;
use carbide_uuid::compute_allocation::ComputeAllocationId;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show all compute allocations:
    $ nico-admin-cli compute-allocation show

Show one compute allocation by ID:
    $ nico-admin-cli compute-allocation show --id 12345678-1234-5678-90ab-cdef01234567

Show allocations for one tenant:
    $ nico-admin-cli compute-allocation show --tenant-organization-id fds34511233a

Filter by instance type:
    $ nico-admin-cli compute-allocation show --instance-type-id DGX-H100-640GB

")]
pub(crate) struct Args {
    #[clap(
        short = 'i',
        long,
        help = "Optional, compute allocation ID to restrict the search"
    )]
    pub(crate) id: Option<ComputeAllocationId>,

    #[clap(
        short = 't',
        long,
        help = "Optional, tenant organization ID used to filter results"
    )]
    pub(crate) tenant_organization_id: Option<String>,

    #[clap(short = 'n', long, help = "Optional, name used to filter results")]
    pub(crate) name: Option<String>,

    #[clap(long, help = "Optional, instance type ID used to filter results")]
    pub(crate) instance_type_id: Option<String>,
}

impl From<Args> for FindComputeAllocationIdsRequest {
    fn from(args: Args) -> Self {
        FindComputeAllocationIdsRequest {
            name: args.name,
            tenant_organization_id: args.tenant_organization_id,
            instance_type_id: args.instance_type_id,
        }
    }
}
