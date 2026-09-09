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

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show client and server versions:
    $ nico-admin-cli version

Also display the runtime config:
    $ nico-admin-cli version --show-runtime-config

Show the RMS backend version (requires RMS to be configured in nico-api):
    $ nico-admin-cli version rms

")]
pub(crate) struct Opts {
    #[clap(short, long, action, help = "Display Runtime Config also.")]
    pub(super) show_runtime_config: bool,

    #[clap(subcommand)]
    pub(super) command: Option<Cmd>,
}

/// Optional subcommands for `nico-admin-cli version`.
#[derive(Parser, Debug, Clone)]
#[clap(rename_all = "kebab_case")]
pub(super) enum Cmd {
    #[clap(about = "Show the version of the configured RMS backend via nico-api")]
    Rms,
}
