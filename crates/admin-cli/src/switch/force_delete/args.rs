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

use carbide_uuid::switch::SwitchId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Force delete a switch:
    $ nico-admin-cli switch force-delete 12345678-1234-5678-90ab-cdef01234567

Force delete a switch and its machine interfaces:
    $ nico-admin-cli switch force-delete 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces

Force delete a switch and its interfaces and BMC suppressions:
    $ nico-admin-cli switch force-delete 12345678-1234-5678-90ab-cdef01234567 \
    --delete-interfaces --delete-bmc-suppressions

")]
pub(crate) struct Args {
    #[clap(help = "Switch ID to force delete.")]
    pub(super) switch_id: SwitchId,

    #[clap(
        short = 'd',
        long,
        action,
        help = "Delete machine interfaces associated with this switch, including interfaces whose MACs match the switch BMC MAC or declared NVOS MACs."
    )]
    pub(super) delete_interfaces: bool,

    #[clap(
        long,
        action,
        help = "Delete BMC suppressions (DHCP and Site Explorer) for the switch BMC MAC and declared NVOS MACs."
    )]
    pub(super) delete_bmc_suppressions: bool,
}
