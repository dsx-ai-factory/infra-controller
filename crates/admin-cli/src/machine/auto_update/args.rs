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
use clap::{ArgGroup, Parser};

#[derive(Parser, Debug, Clone)]
#[clap(group(ArgGroup::new("autoupdate_action").required(true).args(&["enable", "disable", "clear"])))]
#[command(after_long_help = "\
EXAMPLES:

Force-enable firmware auto-update for a host:
    $ nico-admin-cli machine auto-update --machine 12345678-1234-5678-90ab-cdef01234567 --enable

Force-disable it:
    $ nico-admin-cli machine auto-update --machine 12345678-1234-5678-90ab-cdef01234567 --disable

Clear the per-machine override (fall back to global/config):
    $ nico-admin-cli machine auto-update --machine 12345678-1234-5678-90ab-cdef01234567 --clear

")]
pub(crate) struct Args {
    #[clap(long, help = "Machine ID of the host to change")]
    pub(crate) machine: MachineId,
    #[clap(
        short = 'e',
        long,
        action,
        help = "Enable auto updates even if globally disabled or individually disabled by config files"
    )]
    pub(crate) enable: bool,
    #[clap(
        short = 'd',
        long,
        action,
        help = "Disable auto updates even if globally enabled or individually enabled by config files"
    )]
    pub(crate) disable: bool,
    #[clap(
        short = 'c',
        long,
        action,
        help = "Perform auto updates according to config files"
    )]
    clear: bool,
}
