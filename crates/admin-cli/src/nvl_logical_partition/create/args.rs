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

use ::rpc::forge as forgerpc;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Create a logical partition for a tenant:
    $ nico-admin-cli logical-partition create --name my-partition \
    --tenant-organization-id fds34511233a

")]
pub(crate) struct Args {
    #[clap(short = 'n', long, help = "name of the partition")]
    name: String,
    #[clap(short = 't', long, help = "tenant organization id of the partition")]
    tenant_organization_id: String,
}

impl From<Args> for forgerpc::NvLinkLogicalPartitionCreationRequest {
    fn from(args: Args) -> Self {
        let metadata = forgerpc::Metadata {
            name: args.name,
            labels: vec![forgerpc::Label {
                key: "cloud-unsafe-op".to_string(),
                value: Some("true".to_string()),
            }],
            ..Default::default()
        };
        Self {
            config: Some(forgerpc::NvLinkLogicalPartitionConfig {
                metadata: Some(metadata),
                tenant_organization_id: args.tenant_organization_id,
            }),
            id: None,
        }
    }
}
