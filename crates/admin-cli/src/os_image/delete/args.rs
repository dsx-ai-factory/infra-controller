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

use clap::Parser;
use rpc::forge::DeleteOsImageRequest;

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::os_image::common::str_to_rpc_uuid;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Delete an OS image (must be unused by any instance):
    $ nico-admin-cli os-image delete --id 12345678-1234-5678-90ab-cdef01234567 \
    --tenant-org-id fds34511233a

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "uuid of the OS image to delete.")]
    id: String,
    #[clap(
        short = 't',
        long,
        help = "Tenant organization identifier of OS image to delete."
    )]
    tenant_org_id: String,
}

impl TryFrom<Args> for DeleteOsImageRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let id = str_to_rpc_uuid(&args.id)?;
        Ok(DeleteOsImageRequest {
            id: Some(id),
            tenant_organization_id: args.tenant_org_id,
        })
    }
}
