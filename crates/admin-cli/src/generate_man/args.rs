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

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Generate man pages into the default ./man directory:
    $ nico-admin-cli generate-man

Generate man pages into a specific directory:
    $ nico-admin-cli generate-man --out-dir /usr/local/share/man/man1

")]
pub(crate) struct Cmd {
    /// Directory to write the generated man pages into. Created if it does
    /// not exist. `clap_mangen` writes one `<command>.1` file per command
    /// in the tree.
    #[clap(long, default_value = "man")]
    pub(super) out_dir: PathBuf,
}
