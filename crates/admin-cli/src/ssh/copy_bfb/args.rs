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

use super::super::common::SshArgs;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Copy a BFB image to a DPU BMC's RSHIM:
    $ nico-admin-cli ssh copy-bfb 192.0.2.10:22 admin mypassword /path/to/image.bfb

")]
pub(crate) struct Args {
    #[clap(flatten)]
    pub(super) ssh_args: SshArgs,
    #[clap(help = "BFB Path")]
    pub(super) bfb_path: String,
}
