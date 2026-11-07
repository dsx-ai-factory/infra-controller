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

Rename an OS image and update its description:
    $ nico-admin-cli os-image update --id 12345678-1234-5678-90ab-cdef01234567 \
    --name ubuntu-22.04 --description \"Ubuntu 22.04 base\"

Rotate the image URL's auth token:
    $ nico-admin-cli os-image update --id 12345678-1234-5678-90ab-cdef01234567 \
    --auth-type Bearer --auth-token <token>

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "uuid of the OS image to update.")]
    id: String,
    #[clap(short = 'n', long, help = "Optional, name of the OS image entry.")]
    name: Option<String>,
    #[clap(
        short = 'd',
        long,
        help = "Optional, description of the OS image entry."
    )]
    description: Option<String>,
    #[clap(
        short = 'y',
        long,
        help = "Optional, Authentication type, usually Bearer."
    )]
    auth_type: Option<String>,
    #[clap(
        short = 'p',
        long,
        help = "Optional, Authentication token, usually in base64."
    )]
    auth_token: Option<String>,
}

/// Parsed update request with a validated UUID.
pub(super) struct UpdateRequest {
    pub(super) id: ::rpc::common::Uuid,
    pub(super) name: Option<String>,
    pub(super) description: Option<String>,
    pub(super) auth_type: Option<String>,
    pub(super) auth_token: Option<String>,
}

impl TryFrom<Args> for UpdateRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let id = str_to_rpc_uuid(&args.id)?;
        Ok(UpdateRequest {
            id,
            name: args.name,
            description: args.description,
            auth_type: args.auth_type,
            auth_token: args.auth_token,
        })
    }
}
