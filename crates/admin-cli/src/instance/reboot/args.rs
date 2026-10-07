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

use carbide_uuid::instance::InstanceId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Reboot an instance:
    $ nico-admin-cli instance reboot --instance 12345678-1234-5678-90ab-cdef01234567

Reboot and apply any pending firmware updates:
    $ nico-admin-cli instance reboot --instance 12345678-1234-5678-90ab-cdef01234567 \
    --apply-updates-on-reboot

Reboot into the custom PXE flow:
    $ nico-admin-cli instance reboot --instance 12345678-1234-5678-90ab-cdef01234567 \
    --custom-pxe

")]
pub(crate) struct Args {
    #[clap(short, long)]
    pub(super) instance: InstanceId,

    #[clap(short, long, action)]
    pub(super) custom_pxe: bool,

    #[clap(short, long, action)]
    pub(super) apply_updates_on_reboot: bool,
}
