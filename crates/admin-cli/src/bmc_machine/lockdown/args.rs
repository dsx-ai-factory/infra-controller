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

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Enable lockdown on a machine:
    $ nico-admin-cli bmc-machine lockdown \
    --machine 12345678-1234-5678-90ab-cdef01234567 --enable

Disable lockdown on a machine:
    $ nico-admin-cli bmc-machine lockdown \
    --machine 12345678-1234-5678-90ab-cdef01234567 --disable

Enable lockdown and reboot to apply the change:
    $ nico-admin-cli bmc-machine lockdown \
    --machine 12345678-1234-5678-90ab-cdef01234567 --enable --reboot

")]
pub(crate) struct Args {
    #[clap(long, help = "ID of the machine to enable/disable lockdown")]
    pub(super) machine: MachineId,
    #[clap(short, long, help = "Issue reboot to apply lockdown change")]
    pub(super) reboot: bool,
    #[clap(
        long,
        conflicts_with = "disable",
        required_unless_present = "disable",
        help = "Enable lockdown"
    )]
    pub(super) enable: bool,
    #[clap(
        long,
        conflicts_with = "enable",
        required_unless_present = "enable",
        help = "Disable lockdown"
    )]
    pub(super) disable: bool,
}
