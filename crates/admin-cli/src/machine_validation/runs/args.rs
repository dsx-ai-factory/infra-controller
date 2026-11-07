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

use carbide_uuid::machine::MachineId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show all validation runs:
    $ nico-admin-cli machine-validation runs show

Show runs for one machine, including history:
    $ nico-admin-cli machine-validation runs show --machine 12345678-1234-5678-90ab-cdef01234567 \
    --history

")]
pub(crate) enum Args {
    #[clap(about = "Show Runs")]
    Show(ShowRunsOptions),
}

#[derive(Parser, Debug)]
pub(crate) struct ShowRunsOptions {
    #[clap(short = 'm', long, help = "Show machine validation runs of a machine")]
    pub(super) machine: Option<MachineId>,

    #[clap(long, default_value = "false", help = "run history")]
    pub(super) history: bool,
}
