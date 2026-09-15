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

Rename a network security group:
    $ nico-admin-cli network-security-group update --id 12345678-1234-5678-90ab-cdef01234567 \
    --tenant-organization-id fds34511233a --name web-tier

Replace its rule set from JSON (overwrites all existing rules):
    $ nico-admin-cli network-security-group update --id 12345678-1234-5678-90ab-cdef01234567 \
    --tenant-organization-id fds34511233a --rules '[...]'

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "Network security group ID to update")]
    pub(super) id: String,

    #[clap(
        short = 't',
        long,
        help = "Tenant organization ID of the network security group"
    )]
    pub(super) tenant_organization_id: String,

    #[clap(short = 'n', long, help = "Name of the network security group")]
    pub(super) name: Option<String>,

    #[clap(short = 'd', long, help = "Description of the network security group")]
    pub(super) description: Option<String>,

    #[clap(
        short = 'l',
        long,
        help = "JSON map of simple key:value pairs to be applied as labels to the network security group - will COMPLETELY overwrite any existing labels"
    )]
    pub(super) labels: Option<String>,

    #[clap(
        short = 's',
        long,
        help = "Set whether egress rules are stateful (omitted: unchanged); changing false to true is rejected when site-level stateful ACL support is disabled"
    )]
    pub(super) stateful_egress: Option<bool>,

    #[clap(
        short = 'r',
        long,
        help = "Optional, JSON array containing a defined set of network security group rules - will COMPLETELY overwrite any existing rules"
    )]
    pub(super) rules: Option<String>,

    #[clap(
        short = 'v',
        long,
        help = "Optional, version to use for comparison when performing the update, which will be rejected if the actual version of the record does not match the value of this parameter"
    )]
    pub(super) version: Option<String>,
}
