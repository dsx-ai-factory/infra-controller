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

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show what is attached to a network security group:
    $ nico-admin-cli network-security-group show-attachments --id 12345678-1234-5678-90ab-cdef01234567

Include objects inheriting the group from a parent:
    $ nico-admin-cli network-security-group show-attachments --id 12345678-1234-5678-90ab-cdef01234567 \
    --include-indirect

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "network security group ID to query")]
    pub(super) id: String,

    #[clap(
        short = 'a',
        long,
        help = "include indirect relationships (objects that are inheriting the NSG from a parent object)"
    )]
    pub(super) include_indirect: bool,
}
