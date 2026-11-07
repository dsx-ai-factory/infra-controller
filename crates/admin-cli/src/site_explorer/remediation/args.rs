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

Pause remediation actions for an endpoint:
    $ nico-admin-cli site-explorer remediation 192.0.2.10 --pause

Resume remediation actions for an endpoint:
    $ nico-admin-cli site-explorer remediation 192.0.2.10 --resume

")]
pub(crate) struct Args {
    #[clap(help = "BMC IP address of the endpoint")]
    pub(super) address: String,
    #[clap(long, help = "Pause remediation actions", conflicts_with = "resume")]
    pub(super) pause: bool,
    #[clap(long, help = "Resume remediation actions", conflicts_with = "pause")]
    pub(super) resume: bool,
}
