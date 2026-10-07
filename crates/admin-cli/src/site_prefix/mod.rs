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

mod common;
mod create;
mod delete;
mod output;
mod show;
mod state_history;
mod update;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

List SitePrefixes:
    $ nico-admin-cli site-prefix show

Show one SitePrefix by ID:
    $ nico-admin-cli site-prefix show 12345678-1234-5678-90ab-cdef01234567

Create a tenant-managed SitePrefix:
    $ nico-admin-cli site-prefix create --tenant-organization-id fds34511233a \
    --prefix 10.0.0.0/16 --name tenant-private-space

Retire a tenant-managed SitePrefix:
    $ nico-admin-cli site-prefix delete 12345678-1234-5678-90ab-cdef01234567 \
    --tenant-organization-id fds34511233a

")]
#[clap(rename_all = "kebab_case")]
pub(crate) enum Cmd {
    /// List SitePrefixes or show one by ID
    Show(show::Args),
    /// Create a tenant-managed, datacenter-only SitePrefix in Provisioning
    Create(create::Args),
    /// Replace the complete metadata document on a tenant-managed SitePrefix
    ///
    /// Tenant ownership, CIDR, authority, and routing scope are immutable.
    Update(update::Args),
    /// Record retirement intent and return the retained Deleting SitePrefix
    ///
    /// This is not a force-delete path. The CIDR and quota slot remain in use
    /// until child resources and the dataplane have drained.
    #[clap(visible_alias = "retire")]
    Delete(delete::Args),
    /// Show all lifecycle state history for a SitePrefix, or an empty collection
    StateHistory(state_history::Args),
}
