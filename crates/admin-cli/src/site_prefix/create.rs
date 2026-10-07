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

use carbide_uuid::site_prefix::SitePrefixId;
use clap::Parser;
use ipnet::IpNet;
use rpc::forge::{Metadata, SitePrefixCreationRequest};

use super::output::SitePrefixOutput;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::metadata::parse_rpc_labels;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create a tenant-managed SitePrefix:
    $ nico-admin-cli site-prefix create --tenant-organization-id fds34511233a \
    --prefix 10.0.0.0/16 --name tenant-private-space

Create a SitePrefix with metadata:
    $ nico-admin-cli site-prefix create --tenant-organization-id fds34511233a \
    --prefix 192.168.0.0/20 --name lab-space --description \"Private lab address space\" \
    --label environment:lab --label team:networking

Supply a stable ID for an exact retry or reconciliation workflow:
    $ nico-admin-cli site-prefix create --tenant-organization-id fds34511233a \
    --site-prefix-id 12345678-1234-5678-90ab-cdef01234567 \
    --prefix 172.16.0.0/16 --name tenant-private-space

")]
pub(crate) struct Args {
    #[clap(
        long,
        visible_alias = "tenant-org-id",
        value_name = "TENANT_ORGANIZATION_ID",
        help = "Tenant organization that will own the SitePrefix"
    )]
    tenant_organization_id: String,

    #[clap(
        long,
        value_name = "CIDR",
        help = "Canonical RFC1918 IPv4 prefix with a length from /8 through /31"
    )]
    prefix: IpNet,

    #[clap(long, value_name = "NAME", help = "SitePrefix name")]
    name: String,

    #[clap(long, value_name = "DESCRIPTION", help = "SitePrefix description")]
    description: Option<String>,

    #[clap(
        long = "label",
        value_name = "KEY[:VALUE]",
        action = clap::ArgAction::Append,
        help = "Metadata label; repeat this option to add more than one"
    )]
    labels: Vec<String>,

    #[clap(
        long,
        value_name = "SITE_PREFIX_ID",
        help = "Use this SitePrefix ID instead of generating one locally"
    )]
    site_prefix_id: Option<SitePrefixId>,
}

impl From<Args> for SitePrefixCreationRequest {
    fn from(args: Args) -> Self {
        let id = match args.site_prefix_id {
            Some(id) => id,
            None => SitePrefixId::new(),
        };

        Self {
            id: Some(id),
            tenant_organization_id: args.tenant_organization_id,
            prefix: args.prefix.to_string(),
            metadata: Some(Metadata {
                name: args.name,
                description: args.description.unwrap_or_default(),
                labels: parse_rpc_labels(args.labels),
            }),
        }
    }
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: SitePrefixCreationRequest = self.into();
        let site_prefix = ctx.api_client.0.create_site_prefix(request).await?;

        SitePrefixOutput::one(site_prefix)
            .write(&ctx.config.format, &mut ctx.output_file)
            .await
    }
}
