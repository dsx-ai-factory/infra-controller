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

use carbide_uuid::power_shelf::PowerShelfId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Force delete a power shelf:
    $ nico-admin-cli power-shelf force-delete 12345678-1234-5678-90ab-cdef01234567

Force delete a power shelf and its machine interfaces:
    $ nico-admin-cli power-shelf force-delete 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces

Force delete a power shelf with interface and BMC cleanup:
    $ nico-admin-cli power-shelf force-delete 12345678-1234-5678-90ab-cdef01234567 \
    --delete-interfaces --delete-bmc-suppressions

")]
pub(crate) struct Args {
    #[clap(help = "Power Shelf ID to force delete.")]
    pub(super) power_shelf_id: PowerShelfId,

    #[clap(
        short = 'd',
        long,
        action,
        help = "Delete machine interfaces associated with this power shelf."
    )]
    pub(super) delete_interfaces: bool,

    #[clap(
        long,
        action,
        help = "Delete BMC suppressions (site explorer and DHCP) for this power shelf BMC MAC."
    )]
    pub(super) delete_bmc_suppressions: bool,
}
