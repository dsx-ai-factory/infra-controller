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
#[command(after_long_help = "\
EXAMPLES:

Start delayed updates for one or more machines now:
    $ nico-admin-cli managed-host start-updates --machines 12345678-1234-5678-90ab-cdef01234567

Schedule the update window for a specific start time:
    $ nico-admin-cli managed-host start-updates --machines 12345678-1234-5678-90ab-cdef01234567 \
    --start 2026-01-02T03:04:05

")]
pub(crate) struct Args {
    #[clap(long, required(true), help = "Machine IDs to update, space separated", num_args = 1.., value_delimiter = ' ')]
    pub(crate) machines: Vec<MachineId>,
    #[clap(
        long,
        help = "Start of the maintenance window for doing the updates (default now) format 2025-01-02T03:04:05+0000 or 2025-01-02T03:04:05 for local time"
    )]
    pub(crate) start: Option<String>,
    #[clap(
        long,
        help = "End of starting new updates (default 24 hours from the start) format 2025-01-02T03:04:05+0000 or 2025-01-02T03:04:05 for local time"
    )]
    pub(crate) end: Option<String>,
    #[arg(long, help = "Cancel any new updates")]
    pub(crate) cancel: bool,
}
