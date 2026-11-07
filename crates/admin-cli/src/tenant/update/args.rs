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

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Rename a tenant org:
    $ nico-admin-cli tenant update fds34511233a --name \"Acme Corp\"

Apply a routing profile to a tenant:
    $ nico-admin-cli tenant update fds34511233a --routing-profile-type default

Update only if the record is still at a known version (optimistic concurrency):
    $ nico-admin-cli tenant update fds34511233a --name \"Acme Corp\" --version 7

")]
pub(crate) struct Args {
    #[clap(help = "Tenant org ID to update", default_value(None))]
    pub(super) tenant_org: String,

    #[clap(
        short = 'p',
        long,
        help = "Optional, routing profile name to apply to the tenant"
    )]
    pub(super) routing_profile_type: Option<String>,

    #[clap(
        short = 'v',
        long,
        help = "Optional, version to use for comparison when performing the update, which will be rejected if the actual version of the record does not match the value of this parameter"
    )]
    pub(super) version: Option<String>,

    #[clap(short = 'n', long, help = "Organization name of the tenant")]
    pub(super) name: Option<String>,
}
