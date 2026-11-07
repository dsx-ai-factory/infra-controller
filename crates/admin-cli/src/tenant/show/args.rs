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
use rpc::forge::FindTenantRequest;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

List all tenants:
    $ nico-admin-cli tenant show

Show details for one tenant org:
    $ nico-admin-cli tenant show fds34511233a

")]
pub(crate) struct Args {
    #[clap(help = "Optional, tenant org ID to restrict the search")]
    tenant_org: Option<String>,
}

impl From<&Args> for Option<FindTenantRequest> {
    fn from(args: &Args) -> Self {
        args.tenant_org.as_ref().map(|id| FindTenantRequest {
            tenant_organization_id: id.clone(),
        })
    }
}
