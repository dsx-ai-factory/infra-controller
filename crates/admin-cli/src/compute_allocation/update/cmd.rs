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
use ::rpc::forge::{FindComputeAllocationsByIdsRequest, UpdateComputeAllocationRequest};

use super::args::Args;
use crate::compute_allocation::common::convert_compute_allocations_to_table;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// Update a compute allocation.
/// On successful update, the details of the
/// allocation will be displayed.
pub(super) async fn update(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let allocation = api_client
        .0
        .find_compute_allocations_by_ids(FindComputeAllocationsByIdsRequest { ids: vec![args.id] })
        .await?;
    let allocation = allocation
        .allocations
        .into_iter()
        .next()
        .ok_or(CarbideCliError::Empty)?;

    let mut metadata = allocation.metadata.unwrap_or_default();
    let mut attributes = allocation.attributes.unwrap_or_default();

    if let Some(description) = args.description {
        metadata.description = description;
    }

    if let Some(name) = args.name {
        metadata.name = name;
    }

    if let Some(labels_json) = args.labels {
        metadata.labels = serde_json::from_str(&labels_json)?;
    }

    if let Some(instance_type_id) = args.instance_type_id {
        attributes.instance_type_id = instance_type_id;
    }

    if let Some(count) = args.count {
        attributes.count = count;
    }

    let updated = api_client
        .0
        .update_compute_allocation(UpdateComputeAllocationRequest {
            id: Some(args.id),
            tenant_organization_id: args.tenant_organization_id,
            metadata: Some(metadata),
            attributes: Some(::rpc::forge::ComputeAllocationAttributes {
                instance_type_id: attributes.instance_type_id,
                count: attributes.count,
            }),
            if_version_match: args.version,
            updated_by: None,
        })
        .await?;
    let updated = updated.allocation.ok_or(CarbideCliError::Empty)?;

    match output_format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&updated).map_err(CarbideCliError::JsonError)?
        ),
        OutputFormat::Yaml => println!(
            "{}",
            serde_yaml::to_string(&updated).map_err(CarbideCliError::YamlError)?
        ),
        OutputFormat::Csv => {
            convert_compute_allocations_to_table(vec![updated], true)?
                .to_csv(std::io::stdout())
                .map_err(CarbideCliError::CsvError)?
                .flush()?;
        }
        _ => convert_compute_allocations_to_table(vec![updated], true)?.printstd(),
    }

    Ok(())
}
