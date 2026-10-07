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

List all instance types:
    $ nico-admin-cli instance-type show

Show a single instance type by id:
    $ nico-admin-cli instance-type show --id 12345678-1234-5678-90ab-cdef01234567

List instance types with allocation counts:
    $ nico-admin-cli instance-type show --show-stats true

")]
pub(crate) struct Args {
    #[clap(
        short = 'i',
        long,
        help = "Optional, instance type ID to restrict the search"
    )]
    pub(super) id: Option<String>,

    #[clap(
        short = 's',
        long,
        help = "Optional, show counts for allocations of instance types"
    )]
    pub(super) show_stats: Option<bool>,
}
