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

use carbide_uuid::extension_service::ExtensionServiceId;
use clap::Parser;
use config_version::ConfigVersion;

#[derive(Parser, Debug)]
#[command(
    long_about = "Show the selected service VPC and attachment ID for each matching instance. Without a version filter the active attachment is preferred. If only terminating attachments remain, Core can return any retained version. Removing indicates asynchronous cleanup."
)]
#[command(after_long_help = "\
EXAMPLES:

Show all instances using an extension service:
    $ nico-admin-cli extension-service show-instances --service-id 12345678-1234-5678-90ab-cdef01234567

Show instances using a specific version of the service:
    $ nico-admin-cli extension-service show-instances --service-id 12345678-1234-5678-90ab-cdef01234567 \
    --version V1-T1720000000000000

")]
pub(crate) struct Args {
    #[clap(short = 'i', long, help = "The extension service ID")]
    service_id: ExtensionServiceId,

    #[clap(
        short = 'v',
        long,
        help = "Exact service version (V<number>-T<timestamp>)"
    )]
    version: Option<ConfigVersion>,
}

impl From<Args> for ::rpc::forge::FindInstancesByDpuExtensionServiceRequest {
    fn from(args: Args) -> Self {
        Self {
            service_id: args.service_id.to_string(),
            version: args.version.map(|version| version.to_string()),
        }
    }
}
