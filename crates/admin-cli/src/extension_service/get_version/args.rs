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

Get version info for all versions of a service:
    $ nico-admin-cli extension-service get-version --service-id 12345678-1234-5678-90ab-cdef01234567

Get version info for specific versions:
    $ nico-admin-cli extension-service get-version --service-id 12345678-1234-5678-90ab-cdef01234567 \
    --versions 1.0,1.1

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "The extension service ID")]
    service_id: String,

    #[clap(
        short = 'v',
        long,
        help = "Version strings to get (optional, leave empty to get all versions)",
        value_delimiter = ','
    )]
    versions: Vec<String>,
}

impl From<Args> for ::rpc::forge::GetDpuExtensionServiceVersionsInfoRequest {
    fn from(args: Args) -> Self {
        Self {
            service_id: args.service_id,
            versions: args.versions,
        }
    }
}
