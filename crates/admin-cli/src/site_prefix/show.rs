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
use clap::{Parser, ValueEnum};
use ipnet::IpNet;
use rpc::forge::{
    PrefixMatchType, SitePrefixAuthority, SitePrefixLifecycleState, SitePrefixRoutingScope,
    SitePrefixSearchFilter,
};

use super::common::{find_by_ids, find_ids, find_one_by_id};
use super::output::SitePrefixOutput;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all SitePrefixes:
    $ nico-admin-cli site-prefix show

Show one SitePrefix by ID:
    $ nico-admin-cli site-prefix show 12345678-1234-5678-90ab-cdef01234567

List tenant-managed SitePrefixes for one tenant:
    $ nico-admin-cli site-prefix show --tenant-organization-id fds34511233a \
    --authority tenant-managed

List every SitePrefix with the same exact CIDR:
    $ nico-admin-cli site-prefix show --prefix 10.0.0.0/16

Limit an exact CIDR lookup to one tenant:
    $ nico-admin-cli site-prefix show --prefix 10.0.0.0/16 \
    --tenant-organization-id fds34511233a

Find ready SitePrefixes that contain a smaller prefix:
    $ nico-admin-cli site-prefix show --contains 10.0.8.0/24 --lifecycle-state ready

")]
pub(crate) struct Args {
    #[clap(
        value_name = "SITE_PREFIX_ID",
        help = "SitePrefix ID to show; omit to search inventory",
        conflicts_with_all = [
            "tenant_organization_id",
            "authority",
            "routing_scope",
            "lifecycle_state",
            "prefix",
            "contains",
            "contained_by"
        ]
    )]
    site_prefix_id: Option<SitePrefixId>,

    #[clap(
        long,
        visible_alias = "tenant-org-id",
        value_name = "TENANT_ORGANIZATION_ID",
        help = "Return tenant-managed SitePrefixes owned by this tenant"
    )]
    tenant_organization_id: Option<String>,

    #[clap(long, value_enum, help = "Filter by management authority")]
    authority: Option<AuthorityArg>,

    #[clap(long, value_enum, help = "Filter by routing scope")]
    routing_scope: Option<RoutingScopeArg>,

    #[clap(long, value_enum, help = "Filter by lifecycle state")]
    lifecycle_state: Option<LifecycleStateArg>,

    #[clap(
        long,
        value_name = "CIDR",
        help = "Return every SitePrefix with this exact CIDR",
        conflicts_with_all = ["contains", "contained_by"]
    )]
    prefix: Option<IpNet>,

    #[clap(
        long,
        value_name = "CIDR",
        help = "Return SitePrefixes that contain this prefix",
        conflicts_with = "contained_by"
    )]
    contains: Option<IpNet>,

    #[clap(
        long,
        value_name = "CIDR",
        help = "Return SitePrefixes contained by this prefix"
    )]
    contained_by: Option<IpNet>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum AuthorityArg {
    #[value(name = "operator-managed")]
    Operator,
    #[value(name = "tenant-managed")]
    Tenant,
}

impl From<AuthorityArg> for SitePrefixAuthority {
    fn from(value: AuthorityArg) -> Self {
        match value {
            AuthorityArg::Operator => Self::OperatorManaged,
            AuthorityArg::Tenant => Self::TenantManaged,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "kebab_case")]
enum RoutingScopeArg {
    DatacenterOnly,
}

impl From<RoutingScopeArg> for SitePrefixRoutingScope {
    fn from(value: RoutingScopeArg) -> Self {
        match value {
            RoutingScopeArg::DatacenterOnly => Self::DatacenterOnly,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "kebab_case")]
enum LifecycleStateArg {
    Provisioning,
    Ready,
    Deleting,
    Error,
}

impl From<LifecycleStateArg> for SitePrefixLifecycleState {
    fn from(value: LifecycleStateArg) -> Self {
        match value {
            LifecycleStateArg::Provisioning => Self::Provisioning,
            LifecycleStateArg::Ready => Self::Ready,
            LifecycleStateArg::Deleting => Self::Deleting,
            LifecycleStateArg::Error => Self::Error,
        }
    }
}

pub(super) enum ShowMethod {
    Get(SitePrefixId),
    Search(SitePrefixSearchFilter),
}

impl From<Args> for ShowMethod {
    fn from(args: Args) -> Self {
        if let Some(site_prefix_id) = args.site_prefix_id {
            return Self::Get(site_prefix_id);
        }

        let (prefix_match, prefix_match_type) = if let Some(prefix) = args.prefix {
            (
                Some(prefix.to_string()),
                Some(PrefixMatchType::PrefixExact as i32),
            )
        } else if let Some(prefix) = args.contains {
            (
                Some(prefix.to_string()),
                Some(PrefixMatchType::PrefixContains as i32),
            )
        } else if let Some(prefix) = args.contained_by {
            (
                Some(prefix.to_string()),
                Some(PrefixMatchType::PrefixContainedBy as i32),
            )
        } else {
            (None, None)
        };

        Self::Search(SitePrefixSearchFilter {
            tenant_organization_id: args.tenant_organization_id,
            authority: args
                .authority
                .map(|authority| SitePrefixAuthority::from(authority) as i32),
            routing_scope: args
                .routing_scope
                .map(|routing_scope| SitePrefixRoutingScope::from(routing_scope) as i32),
            lifecycle_state: args
                .lifecycle_state
                .map(|lifecycle_state| SitePrefixLifecycleState::from(lifecycle_state) as i32),
            prefix_match,
            prefix_match_type,
        })
    }
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let output = match ShowMethod::from(self) {
            ShowMethod::Get(site_prefix_id) => {
                SitePrefixOutput::one(find_one_by_id(&ctx.api_client, site_prefix_id).await?)
            }
            ShowMethod::Search(filter) => {
                let site_prefix_ids = find_ids(&ctx.api_client, filter).await?;
                let site_prefixes = find_by_ids(
                    &ctx.api_client,
                    ctx.config.page_size,
                    site_prefix_ids.as_slice(),
                )
                .await?;
                SitePrefixOutput::many(site_prefixes)
            }
        };

        output.write(&ctx.config.format, &mut ctx.output_file).await
    }
}
