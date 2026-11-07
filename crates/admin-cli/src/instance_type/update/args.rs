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

Rename an instance type:
    $ nico-admin-cli instance-type update --id 12345678-1234-5678-90ab-cdef01234567 \
    --name dgx-h100-640gb

Replace labels and capability filters (both overwrite completely):
    $ nico-admin-cli instance-type update --id 12345678-1234-5678-90ab-cdef01234567 \
    --labels '{\"tier\":\"premium\"}' \
    --desired-capabilities '[{\"key\":\"gpu_count\",\"value\":\"8\"}]'

Update with optimistic concurrency on the record version:
    $ nico-admin-cli instance-type update --id 12345678-1234-5678-90ab-cdef01234567 \
    --description \"DGX H100 640GB\" --version 3

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "Instance type ID to update")]
    pub(super) id: String,

    #[clap(short = 'n', long, help = "Name of the instance type")]
    pub(super) name: Option<String>,

    #[clap(short = 'd', long, help = "Description of the instance type")]
    pub(super) description: Option<String>,

    #[clap(
        short = 'l',
        long,
        help = "JSON map of simple key:value pairs to be applied as labels to the instance type - will COMPLETELY overwrite any existing labels"
    )]
    pub(super) labels: Option<String>,

    #[clap(
        short = 'f',
        long,
        help = "Optional, JSON array containing a set of instance type capability filters - will COMPLETELY overwrite any existing filters"
    )]
    pub(super) desired_capabilities: Option<String>,

    #[clap(
        short = 'v',
        long,
        help = "Optional, version to use for comparison when performing the update, which will be rejected if the actual version of the record does not match the value of this parameter"
    )]
    pub(super) version: Option<String>,
}
