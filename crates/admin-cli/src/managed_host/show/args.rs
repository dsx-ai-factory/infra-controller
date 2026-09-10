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

List all managed hosts:
    $ nico-admin-cli managed-host show

Show details for one host (by host or DPU machine ID):
    $ nico-admin-cli managed-host show 12345678-1234-5678-90ab-cdef01234567

Show the summary with IP details:
    $ nico-admin-cli managed-host show --ips

Limit the State column so a long error message doesn't wrap every row:
    $ nico-admin-cli managed-host show --max-width State=40

Limit a column whose header contains spaces (quote the value):
    $ nico-admin-cli managed-host show --max-width \"Machine IDs (H/D)=40\"

")]
pub(crate) struct Args {
    #[clap(long, action = clap::ArgAction::HelpLong)]
    help: Option<bool>,

    #[clap(
        short,
        long,
        action,
        help = "Show all managed hosts (DEPRECATED)",
        conflicts_with = "machine"
    )]
    pub(super) all: bool,

    #[clap(
        default_value(None),
        help = "Show managed host specific details (using host or dpu machine id), leave empty for all"
    )]
    pub(super) machine: Option<MachineId>,

    #[clap(
        short,
        long,
        action,
        help = "Show IP details in summary",
        conflicts_with = "machine"
    )]
    pub(super) ips: bool,

    #[clap(
        short = 't',
        long,
        action,
        help = "Show only hosts for this instance type"
    )]
    instance_type_id: Option<String>,

    #[clap(
        short,
        long,
        action,
        help = "Show GPU and memory details in summary",
        conflicts_with = "machine"
    )]
    pub(super) more: bool,

    #[clap(long, action, help = "Show only hosts in maintenance mode")]
    pub(super) fix: bool,

    #[clap(long, action, help = "Show only hosts in quarantine")]
    pub(super) quarantine: bool,

    #[clap(flatten)]
    pub(super) width: crate::table_utils::MaxWidthArgs,
}
