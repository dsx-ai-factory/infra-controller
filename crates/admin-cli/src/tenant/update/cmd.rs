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

use ::rpc::admin_cli::OutputFormat;
use rpc::forge::{FindTenantRequest, UpdateTenantRequest};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::tenant::show::cmd::convert_tenants_to_table;

/// Update a tenant.
/// On successful update, the details of the
/// tenant will be displayed.
pub(super) async fn update(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let id = args.tenant_org;

    let tenant = api_client
        .0
        .find_tenant(FindTenantRequest {
            tenant_organization_id: id.clone(),
        })
        .await?
        .tenant
        .ok_or(CarbideCliError::TenantNotFound(id.clone()))?;

    // Core treats the profile as a replacement, so an omitted CLI flag must preserve it.
    // Match the fetched version to avoid restoring stale tenant state after a concurrent update.
    let if_version_match = args.version.or(Some(tenant.version));
    let routing_profile_type = args.routing_profile_type.or(tenant.routing_profile_type);
    let mut metadata = tenant.metadata.unwrap_or_default();

    if let Some(n) = args.name {
        metadata.name = n;
    }

    let tenant = api_client
        .0
        .update_tenant(UpdateTenantRequest {
            organization_id: id.clone(),
            metadata: Some(metadata),
            if_version_match,
            routing_profile_type,
        })
        .await?
        .tenant
        .ok_or(CarbideCliError::TenantNotFound(id))?;

    match output_format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&tenant).map_err(CarbideCliError::JsonError)?
        ),
        OutputFormat::Yaml => println!(
            "{}",
            serde_yaml::to_string(&tenant).map_err(CarbideCliError::YamlError)?
        ),
        OutputFormat::Csv => {
            convert_tenants_to_table(&[tenant])?
                .to_csv(std::io::stdout())
                .map_err(CarbideCliError::CsvError)?
                .flush()?;
        }

        _ => convert_tenants_to_table(&[tenant])?.printstd(),
    }

    Ok(())
}
