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
use ::rpc::forge::FindInstanceTypesByIdsRequest;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::instance_type::common::convert_itypes_to_table;
use crate::rpc::ApiClient;

/// Show one or more InstanceTypes.
/// If only a single InstanceType is found, verbose output is used
/// automatically.
pub(super) async fn show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
    verbose: bool,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;

    let itypes = if let Some(id) = args.id {
        vec![
            api_client
                .0
                .find_instance_types_by_ids(FindInstanceTypesByIdsRequest {
                    // Admin CLI doesn't need to filter on tenant org.
                    // When the time comes, the API will automatically
                    // filter if the caller is not a provider.
                    tenant_organization_id: None,
                    include_allocation_stats: args.show_stats.unwrap_or_default(),
                    instance_type_ids: vec![id],
                })
                .await?
                .instance_types
                .pop()
                .ok_or(CarbideCliError::Empty)?,
        ]
    } else {
        api_client.get_all_instance_types(page_size).await?
    };

    if is_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&itypes).map_err(CarbideCliError::JsonError)?
        );
    } else if itypes.len() == 1 {
        convert_itypes_to_table(&itypes, true)?.printstd();
    } else {
        convert_itypes_to_table(&itypes, verbose)?.printstd();
    }

    Ok(())
}
