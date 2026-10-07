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

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::os_image::common::str_to_rpc_uuid;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show a single OS image by UUID:
    $ nico-admin-cli os-image show --id 12345678-1234-5678-90ab-cdef01234567

List all OS images for a tenant:
    $ nico-admin-cli os-image show --tenant-org-id fds34511233a

List every OS image:
    $ nico-admin-cli os-image show

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "uuid of the OS image to show.")]
    id: Option<String>,
    #[clap(
        short = 't',
        long,
        help = "Tenant organization identifier to filter OS images listing."
    )]
    tenant_org_id: Option<String>,
}

/// Represents the parsed query for the show command.
pub(super) enum ShowQuery {
    /// Show a single OS image by its UUID.
    Single(::rpc::common::Uuid),
    /// List OS images, optionally filtered by tenant organization ID.
    List(Option<String>),
}

impl TryFrom<Args> for ShowQuery {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        match args.id {
            Some(id) => {
                let uuid = str_to_rpc_uuid(&id)?;
                Ok(ShowQuery::Single(uuid))
            }
            None => Ok(ShowQuery::List(args.tenant_org_id)),
        }
    }
}
