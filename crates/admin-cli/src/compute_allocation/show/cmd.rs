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
use ::rpc::forge::FindComputeAllocationsByIdsRequest;

use super::args::Args;
use crate::compute_allocation::common::convert_compute_allocations_to_table;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// Show one or more compute allocations.
/// If only a single allocation is found, verbose output is used
/// automatically.
pub(crate) async fn show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
    verbose: bool,
) -> CarbideCliResult<()> {
    let allocations = if let Some(id) = args.id {
        api_client
            .0
            .find_compute_allocations_by_ids(FindComputeAllocationsByIdsRequest { ids: vec![id] })
            .await?
            .allocations
    } else {
        let all_ids = api_client.0.find_compute_allocation_ids(args).await?.ids;

        let mut allocations = Vec::with_capacity(all_ids.len());

        for ids in all_ids.chunks(page_size) {
            let chunk = api_client
                .0
                .find_compute_allocations_by_ids(FindComputeAllocationsByIdsRequest {
                    ids: ids.to_vec(),
                })
                .await?
                .allocations;
            allocations.extend(chunk);
        }

        allocations
    };

    match output_format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&allocations).map_err(CarbideCliError::JsonError)?
        ),
        OutputFormat::Yaml => println!(
            "{}",
            serde_yaml::to_string(&allocations).map_err(CarbideCliError::YamlError)?
        ),
        OutputFormat::Csv => {
            let verbose = allocations.len() == 1 || verbose;
            convert_compute_allocations_to_table(allocations, verbose)?
                .to_csv(std::io::stdout())
                .map_err(CarbideCliError::CsvError)?
                .flush()?;
        }
        _ => {
            if allocations.len() == 1 {
                convert_compute_allocations_to_table(allocations, true)?.printstd();
            } else {
                convert_compute_allocations_to_table(allocations, verbose)?.printstd();
            }
        }
    }

    Ok(())
}
