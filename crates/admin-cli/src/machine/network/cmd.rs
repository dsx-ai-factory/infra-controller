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
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn network(
    api_client: &ApiClient,
    cmd: Args,
    format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    page_size: usize,
) -> CarbideCliResult<()> {
    match cmd {
        Args::Status => {
            println!(
                "Deprecated: Use dpu network, instead machine network. machine network will be removed in future."
            );
            crate::dpu::show_dpu_status(api_client, output_file, page_size).await?;
        }
        Args::Config(query) => {
            println!(
                "Deprecated: Use dpu network, instead of machine network. machine network will be removed in future."
            );
            let network_config = api_client
                .0
                .get_managed_host_network_config(query.machine_id)
                .await?;
            if format == OutputFormat::Json {
                println!("{}", serde_json::ser::to_string_pretty(&network_config)?);
            } else {
                // someone might be parsing this output
                println!("{network_config:?}");
            }
        }
    }
    Ok(())
}
