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
use rpc::forge::{
    self as forgerpc, CreateNetworkSecurityGroupRequest, NetworkSecurityGroupAttributes,
};

use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Create a network security group for a tenant:
    $ nico-admin-cli network-security-group create --tenant-organization-id fds34511233a \
    --name web-tier

Create one with stateful egress and labels (requires site-level support):
    $ nico-admin-cli network-security-group create --tenant-organization-id fds34511233a \
    --name web-tier --stateful-egress --labels '{\"env\":\"prod\"}'

")]
pub(crate) struct Args {
    #[clap(
        short = 'i',
        long,
        help = "Optional, unique ID to use when creating the network security group"
    )]
    id: Option<String>,

    #[clap(
        short = 't',
        long,
        help = "Tenant organization ID of the network security group"
    )]
    tenant_organization_id: String,

    #[clap(short = 'n', long, help = "Name of the network security group")]
    name: Option<String>,

    #[clap(short = 'd', long, help = "Description of the network security group")]
    description: Option<String>,

    #[clap(
        short = 'l',
        long,
        help = "JSON map of simple key:value pairs to be applied as labels to the network security group"
    )]
    labels: Option<String>,

    #[clap(
        short = 's',
        long,
        help = "Enable stateful egress (default: false); rejected when site-level stateful ACL support is disabled"
    )]
    stateful_egress: bool,

    #[clap(
        short = 'r',
        long,
        help = "Optional, JSON array containing a defined set of network security group rules"
    )]
    rules: Option<String>,
}

impl TryFrom<Args> for CreateNetworkSecurityGroupRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let labels = if let Some(l) = args.labels {
            serde_json::from_str(&l)?
        } else {
            vec![]
        };

        let metadata = forgerpc::Metadata {
            name: args.name.unwrap_or_default(),
            description: args.description.unwrap_or_default(),
            labels,
        };

        let rules = if let Some(r) = args.rules {
            serde_json::from_str(&r)?
        } else {
            vec![]
        };

        Ok(CreateNetworkSecurityGroupRequest {
            id: args.id,
            tenant_organization_id: args.tenant_organization_id,
            metadata: Some(metadata),
            network_security_group_attributes: Some(NetworkSecurityGroupAttributes {
                stateful_egress: args.stateful_egress,
                rules,
            }),
        })
    }
}
