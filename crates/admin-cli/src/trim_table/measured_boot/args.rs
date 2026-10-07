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

Keep the 1000 most recent measured-boot reports, deleting the rest:
    $ nico-admin-cli trim-table measured-boot --keep-entries 1000

Trim down to the latest report only:
    $ nico-admin-cli trim-table measured-boot --keep-entries 1

")]
pub(crate) struct Args {
    #[clap(help = "Number of entries to keep")]
    #[arg(long)]
    pub(super) keep_entries: u32,
}
