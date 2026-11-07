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

use carbide_uuid::machine::MachineId;
use clap::Parser;

#[derive(Parser, Debug)]
#[clap(disable_help_flag = true)]
#[command(after_long_help = "\
EXAMPLES:

List all machines:
    $ nico-admin-cli machine show

Show one machine by ID:
    $ nico-admin-cli machine show 12345678-1234-5678-90ab-cdef01234567

Show only DPUs (or only hosts):
    $ nico-admin-cli machine show --dpus
    $ nico-admin-cli machine show --hosts

Limit every column to 20 characters so long values don't wrap the table:
    $ nico-admin-cli machine show --max-width 20

Limit a column whose header contains spaces (quote the value):
    $ nico-admin-cli machine show --max-width \"State Version=20\"

Show only some columns, in the order given:
    $ nico-admin-cli machine show --columns state,id,\"attached dpus\"

")]
pub(crate) struct Args {
    #[clap(long, action = clap::ArgAction::HelpLong)]
    pub(crate) help: Option<bool>,

    #[clap(
        short,
        long,
        action,
        conflicts_with = "machine",
        help = "Show all machines (DEPRECATED)"
    )]
    pub(crate) all: bool,

    #[clap(
        short,
        long,
        action,
        conflicts_with = "machine",
        help = "Show only DPUs"
    )]
    pub(crate) dpus: bool,

    #[clap(
        short,
        long,
        action,
        conflicts_with = "machine",
        help = "Show only hosts"
    )]
    pub(crate) hosts: bool,

    #[clap(
        short = 't',
        long,
        action,
        // DPUs don't get associated with instance types.
        // Wouldn't hurt to allow the query, but might as well
        // be helpful here.
        conflicts_with = "dpus",
        help = "Show only machines for this instance type"
    )]
    pub(crate) instance_type_id: Option<String>,

    #[clap(
        default_value(None),
        conflicts_with = "max_width",
        conflicts_with = "columns",
        help = "The machine ID to query. Omit to show all machines. Cannot be combined with \
                --max-width or --columns."
    )]
    pub(crate) machine: Option<MachineId>,

    #[clap(
        short = 'c',
        long,
        default_value("5"),
        help = "History count. Valid if `machine` argument is passed."
    )]
    pub(crate) history_count: u32,

    #[clap(flatten)]
    pub(crate) width: crate::table_utils::MaxWidthArgs,

    #[clap(flatten)]
    pub(crate) columns: crate::table_utils::ColumnsArgs,
}
