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

use ::rpc::admin_cli::output::OutputFormat;
use rpc::forge;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn create(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    if args.segment_type == forge::NetworkSegmentType::HostInband && args.subdomain_id.is_none() {
        return Err(CarbideCliError::GenericError(
            "host_inband segment require a valid subdomain for working DHCP".to_string(),
        ));
    }

    let segment = api_client.0.create_network_segment(args).await?;

    match output_format {
        OutputFormat::AsciiTable => {
            println!(
                "{}",
                crate::network_segment::show::cmd::convert_network_to_nice_format(
                    segment, None, api_client,
                )
                .await?
            );
        }
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&segment)?),
        _ => println!("{}", serde_yaml::to_string(&segment)?),
    }

    Ok(())
}
