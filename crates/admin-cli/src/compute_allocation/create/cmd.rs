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
use ::rpc::forge::CreateComputeAllocationRequest;

use super::args::Args;
use crate::compute_allocation::common::convert_compute_allocations_to_table;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// Create a compute allocation.
/// On successful creation, the details of the
/// new allocation will be displayed.
pub(super) async fn create(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let req: CreateComputeAllocationRequest = args.try_into()?;
    let allocation = api_client.0.create_compute_allocation(req).await?;
    let allocation = allocation.allocation.ok_or(CarbideCliError::Empty)?;

    match output_format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&allocation).map_err(CarbideCliError::JsonError)?
        ),
        OutputFormat::Yaml => println!(
            "{}",
            serde_yaml::to_string(&allocation).map_err(CarbideCliError::YamlError)?
        ),
        OutputFormat::Csv => {
            convert_compute_allocations_to_table(vec![allocation], true)?
                .to_csv(std::io::stdout())
                .map_err(CarbideCliError::CsvError)?
                .flush()?;
        }
        _ => convert_compute_allocations_to_table(vec![allocation], true)?.printstd(),
    }

    Ok(())
}
