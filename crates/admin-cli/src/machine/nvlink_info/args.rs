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
use clap::{Parser, Subcommand};

#[derive(Subcommand, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show existing NVLink info for a machine:
    $ nico-admin-cli machine nvlink-info show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum Args {
    #[clap(about = "Show existing NVLink info")]
    Show(NvlinkInfoArgs),
    #[clap(
        about = "Deprecated compatibility command; NVLink info is populated automatically by NICo"
    )]
    Populate(NvlinkInfoPopulateArgs),
}

#[derive(Parser, Debug)]
pub(crate) struct NvlinkInfoArgs {
    #[clap(help = "Machine ID to query")]
    pub(super) machine_id: MachineId,
}

#[derive(Parser, Debug)]
#[command(
    long_about = "Deprecated compatibility command. The NICo NVLink partition manager populates and repairs the NVLink info of a managed machine automatically, so manual population is no longer required. This command always returns an unsupported-operation error and does not contact Redfish, NMX-C, or the database. MACHINE_ID, --update-db, --extended, and --sort-by are all ignored and retained only for command-line compatibility. Use `nico-admin-cli machine nvlink-info show` to inspect the current NVLink info.",
    after_long_help = "\
EXAMPLES:

Invoke the retained compatibility command (returns an unsupported error):
    $ nico-admin-cli machine nvlink-info populate fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

"
)]
pub(crate) struct NvlinkInfoPopulateArgs {
    #[clap(help = "Machine ID (ignored)")]
    pub(super) machine_id: MachineId,

    #[clap(
        long,
        action,
        help = "Ignored; retained for command-line compatibility"
    )]
    pub(super) update_db: bool,
}
