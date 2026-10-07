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
use ::rpc::forge::IpType;
use serde::Serialize;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

#[derive(Serialize)]
struct IpFindResult {
    ip_type: String,
    owner_id: Option<String>,
    message: String,
}

#[derive(Serialize)]
struct IpFindOutput {
    results: Vec<IpFindResult>,
    errors: Vec<String>,
}

pub(super) async fn find(
    args: Args,
    format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let resp = api_client.0.find_ip_address(args).await?;

    let output = IpFindOutput {
        results: resp
            .matches
            .into_iter()
            .map(|m| {
                let ip_type = IpType::try_from(m.ip_type)
                    .map(|t| t.as_str_name().to_string())
                    .unwrap_or_else(|_| format!("Unknown({})", m.ip_type));
                IpFindResult {
                    ip_type,
                    owner_id: m.owner_id,
                    message: m.message,
                }
            })
            .collect(),
        errors: resp.errors,
    };

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Yaml => {
            println!("{}", serde_yaml::to_string(&output)?);
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            for r in &output.results {
                println!("{}", r.message);
            }
            if !output.errors.is_empty() {
                eprintln!("These matchers failed:");
                for err in &output.errors {
                    eprintln!("\t{err}");
                }
            }
        }
    }
    Ok(())
}
