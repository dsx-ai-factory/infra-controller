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
use ::rpc::forge as forgerpc;
use prettytable::{Table, row};
use rpc::forge::TenantByOrganizationIdsRequest;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// Produces a table for printing a non-JSON representation of a
/// tenant to standard out.
///
/// * `tenants`    - A slice of tenants
pub(crate) fn convert_tenants_to_table(
    tenants: &[forgerpc::Tenant],
) -> CarbideCliResult<Box<Table>> {
    let mut table = Box::new(Table::new());
    let default_metadata = Default::default();

    table.set_titles(row![
        "Tenant Organization ID",
        "Name",
        "Description",
        "Version",
        "Routing Profile Type",
        "Labels",
    ]);

    for tenant in tenants {
        let metadata = tenant.metadata.as_ref().unwrap_or(&default_metadata);
        let labels = crate::metadata::fmt_labels_as_kv_pairs(Some(metadata));

        table.add_row(row![
            tenant.organization_id,
            metadata.name,
            metadata.description,
            tenant.version,
            tenant.routing_profile_type.as_deref().unwrap_or("None"),
            labels.join(", "),
        ]);
    }

    Ok(table)
}

/// Show one or more tenants.
pub(super) async fn show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let req: Option<rpc::forge::FindTenantRequest> = (&args).into();

    let tenants = if let Some(req) = req {
        let id = req.tenant_organization_id.clone();
        let tenant = api_client
            .0
            .find_tenant(req)
            .await?
            .tenant
            .ok_or(CarbideCliError::TenantNotFound(id))?;

        vec![tenant]
    } else {
        let all_tenant_orgs = api_client
            .0
            .find_tenant_organization_ids(rpc::forge::TenantSearchFilter {
                tenant_organization_name: None,
            })
            .await?
            .tenant_organization_ids;

        let mut all_tenants = Vec::with_capacity(all_tenant_orgs.len());

        for tenant_ids in all_tenant_orgs.chunks(page_size) {
            let tenants = api_client
                .0
                .find_tenants_by_organization_ids(TenantByOrganizationIdsRequest {
                    organization_ids: tenant_ids.to_vec(),
                })
                .await?
                .tenants;
            all_tenants.extend(tenants);
        }

        all_tenants
    };

    match output_format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&tenants).map_err(CarbideCliError::JsonError)?
        ),
        OutputFormat::Yaml => println!(
            "{}",
            serde_yaml::to_string(&tenants).map_err(CarbideCliError::YamlError)?
        ),
        OutputFormat::Csv => {
            convert_tenants_to_table(&tenants)?
                .to_csv(std::io::stdout())
                .map_err(CarbideCliError::CsvError)?
                .flush()?;
        }

        _ => convert_tenants_to_table(&tenants)?.printstd(),
    }

    Ok(())
}
