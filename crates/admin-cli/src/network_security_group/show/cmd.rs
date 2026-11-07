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

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::network_security_group::common::convert_nsgs_to_table;
use crate::rpc::ApiClient;

/// Show one or more NSGs.
/// If only a single NSG is found, verbose output is used
/// automatically.
pub(super) async fn show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
    verbose: bool,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;

    let mut nsgs = Vec::new();
    if let Some(id) = args.id {
        let nsg = api_client.get_single_network_security_group(id).await?;
        nsgs.push(nsg);
    } else {
        nsgs = api_client
            .get_all_network_security_groups(page_size)
            .await?;
    }

    if is_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&nsgs).map_err(CarbideCliError::JsonError)?
        );
    } else if nsgs.len() == 1 {
        convert_nsgs_to_table(&nsgs, true)?.printstd();
    } else {
        convert_nsgs_to_table(&nsgs, verbose)?.printstd();
    }

    Ok(())
}
