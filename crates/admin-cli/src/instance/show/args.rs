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

/// ShowInstance is used for `cli instance show` configuration,
/// with the ability to filter by a combination of labels, tenant
/// org ID, and VPC ID.
//
// TODO: Possibly add the ability to filter by a list of tenant
// org IDs and/or VPC IDs.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all instances:
    $ nico-admin-cli instance show

Show a single instance by id:
    $ nico-admin-cli instance show 12345678-1234-5678-90ab-cdef01234567

List instances for one tenant org:
    $ nico-admin-cli instance show --tenant-org-id fds34511233a

List instances in a VPC:
    $ nico-admin-cli instance show --vpc-id abcdef01-2345-6789-abcd-ef0123456789

List instances matching a label:
    $ nico-admin-cli instance show --label-key role --label-value training

List instances of an instance type:
    $ nico-admin-cli instance show --instance-type-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(
        default_value(""),
        help = "The instance ID to query, leave empty for all (default)"
    )]
    pub(crate) id: String,

    #[clap(short, long, action)]
    pub(crate) extrainfo: bool,

    #[clap(short, long, help = "The Tenant Org ID to query")]
    pub(crate) tenant_org_id: Option<String>,

    #[clap(short, long, help = "The VPC ID to query.")]
    pub(crate) vpc_id: Option<String>,

    #[clap(long, help = "The key of label instance to query")]
    pub(crate) label_key: Option<String>,

    #[clap(long, help = "The value of label instance to query")]
    pub(crate) label_value: Option<String>,

    #[clap(long, help = "The instance type ID to query.")]
    pub(crate) instance_type_id: Option<String>,
}
