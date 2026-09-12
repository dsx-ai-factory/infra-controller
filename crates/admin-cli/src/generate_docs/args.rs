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

Regenerate the CLI reference in place (the default docs/manuals/nico-admin-cli):
    $ nico-admin-cli generate-cli-docs

Write the generated pages to a different directory:
    $ nico-admin-cli generate-cli-docs --out-dir /tmp/cli-docs

")]
pub(crate) struct Cmd {
    /// Directory to write the generated markdown into. Created if it does not
    /// exist. One `commands/<command>.md` page is written per top-level
    /// command, plus the four domain index pages
    /// (`hardware.md`/`network.md`/`tenant.md`/`admin.md`). The hand-authored
    /// `README.md` overview is left untouched.
    #[clap(long, default_value = "docs/manuals/nico-admin-cli")]
    pub(super) out_dir: PathBuf,
}
