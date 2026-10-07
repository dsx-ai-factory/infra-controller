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

use carbide_uuid::machine::MachineInterfaceId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all machine interfaces:
    $ nico-admin-cli machine-interfaces show

Show one machine interface by ID:
    $ nico-admin-cli machine-interfaces show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(
        short,
        long,
        action,
        conflicts_with = "interface_id",
        help = "Show all machine interfaces (DEPRECATED)"
    )]
    pub(crate) all: bool,

    #[clap(
        default_value(None),
        help = "The interface ID to query, leave empty for all (default)"
    )]
    pub(crate) interface_id: Option<MachineInterfaceId>,

    #[clap(long, action)]
    pub(crate) more: bool,
}
