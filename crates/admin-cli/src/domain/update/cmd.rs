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
use crate::domain::show::cmd::convert_domain_to_nice_format;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn update(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    // UpdateDomain keeps the stored name when it is empty and the stored TTL
    // when it is absent, so send only what the caller asked to change rather
    // than reading the domain first and racing a concurrent rename.
    let updated = api_client
        .update_domain(::rpc::protos::dns::Domain {
            id: Some(args.domain),
            default_ttl: Some(args.default_ttl),
            ..Default::default()
        })
        .await?;

    match output_format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&updated)?),
        _ => println!("{}", convert_domain_to_nice_format(&updated)?),
    }

    Ok(())
}
