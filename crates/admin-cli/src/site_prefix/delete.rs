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
use rpc::forge::SitePrefixDeletionRequest;

use super::output::SitePrefixOutput;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Record retirement intent for a tenant-managed SitePrefix:
    $ nico-admin-cli site-prefix delete 12345678-1234-5678-90ab-cdef01234567 \
    --tenant-organization-id fds34511233a

Use the explicit retirement alias for the same operation:
    $ nico-admin-cli site-prefix retire 12345678-1234-5678-90ab-cdef01234567 \
    --tenant-organization-id fds34511233a

")]
pub(crate) struct Args {
    #[clap(
        value_name = "SITE_PREFIX_ID",
        help = "SitePrefix to move into the Deleting state"
    )]
    site_prefix_id: SitePrefixId,

    #[clap(
        long,
        visible_alias = "tenant-org-id",
        value_name = "TENANT_ORGANIZATION_ID",
        help = "Owning tenant organization; Core rejects a different tenant"
    )]
    tenant_organization_id: String,
}

impl From<Args> for SitePrefixDeletionRequest {
    fn from(args: Args) -> Self {
        Self {
            id: Some(args.site_prefix_id),
            tenant_organization_id: args.tenant_organization_id,
        }
    }
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: SitePrefixDeletionRequest = self.into();
        let response = ctx.api_client.0.delete_site_prefix(request).await?;
        let site_prefix = response.site_prefix.ok_or_else(|| {
            CarbideCliError::GenericError(
                "the API did not return the retired SitePrefix".to_string(),
            )
        })?;

        SitePrefixOutput::one(site_prefix)
            .write(&ctx.config.format, &mut ctx.output_file)
            .await
    }
}
