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
use crate::async_writeln;
use crate::domain::show::cmd::convert_domain_to_nice_format;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn create(
    args: Args,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let domain = api_client
        .create_domain(args.name, args.vpc_id, args.default_ttl)
        .await?;

    write_create_output(&domain, output_format, output_file).await
}

async fn write_create_output(
    domain: &::rpc::protos::dns::Domain,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    match output_format {
        OutputFormat::Json => {
            async_writeln!(output_file, "{}", serde_json::to_string_pretty(domain)?)?
        }
        _ => async_writeln!(output_file, "{}", convert_domain_to_nice_format(domain)?)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_write::CapturedOutput;

    #[tokio::test]
    async fn create_output_uses_configured_writer() {
        let mut captured = CapturedOutput::new();
        let domain = ::rpc::protos::dns::Domain {
            name: "tenant.example.com".to_string(),
            ..Default::default()
        };

        write_create_output(&domain, OutputFormat::Json, captured.writer())
            .await
            .unwrap();

        let output = String::from_utf8(captured.into_bytes().await).unwrap();
        assert!(output.contains("tenant.example.com"));
    }
}
