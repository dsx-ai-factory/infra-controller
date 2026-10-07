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
use rpc::forge::{Metadata, SitePrefixUpdateRequest};

use super::output::SitePrefixOutput;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::metadata::parse_rpc_labels;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Replace the metadata on a tenant-managed SitePrefix:
    $ nico-admin-cli site-prefix update 12345678-1234-5678-90ab-cdef01234567 \
    --tenant-organization-id fds34511233a --name production-space \
    --description \"Production private address space\" --label environment:production

Reject the update if the SitePrefix changed since it was read:
    $ nico-admin-cli site-prefix update 12345678-1234-5678-90ab-cdef01234567 \
    --tenant-organization-id fds34511233a --name production-space \
    --if-version-match V4-T1750000000000000

")]
pub(crate) struct Args {
    #[clap(value_name = "SITE_PREFIX_ID", help = "SitePrefix to update")]
    site_prefix_id: SitePrefixId,

    #[clap(
        long,
        visible_alias = "tenant-org-id",
        value_name = "TENANT_ORGANIZATION_ID",
        help = "Owning tenant organization; Core rejects a different tenant"
    )]
    tenant_organization_id: String,

    #[clap(long, value_name = "NAME", help = "Replacement SitePrefix name")]
    name: String,

    #[clap(
        long,
        value_name = "DESCRIPTION",
        help = "Replacement description; omit this option to clear the description"
    )]
    description: Option<String>,

    #[clap(
        long = "label",
        value_name = "KEY[:VALUE]",
        action = clap::ArgAction::Append,
        help = "Replacement metadata label; repeat for more than one and omit all labels to clear them"
    )]
    labels: Vec<String>,

    #[clap(
        long,
        value_name = "VERSION",
        help = "Update only when the stored SitePrefix has this V<number>-T<Unix-epoch-microseconds> version"
    )]
    if_version_match: Option<String>,
}

impl From<Args> for SitePrefixUpdateRequest {
    fn from(args: Args) -> Self {
        Self {
            id: Some(args.site_prefix_id),
            tenant_organization_id: args.tenant_organization_id,
            metadata: Some(Metadata {
                name: args.name,
                description: args.description.unwrap_or_default(),
                labels: parse_rpc_labels(args.labels),
            }),
            if_version_match: args.if_version_match,
        }
    }
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: SitePrefixUpdateRequest = self.into();
        let site_prefix = ctx.api_client.0.update_site_prefix(request).await?;

        SitePrefixOutput::one(site_prefix)
            .write(&ctx.config.format, &mut ctx.output_file)
            .await
    }
}
