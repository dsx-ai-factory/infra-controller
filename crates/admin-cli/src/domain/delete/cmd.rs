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

use rpc::admin_cli::OutputFormat;

use super::args::Args;
use crate::async_writeln;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn delete(
    args: Args,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    api_client.delete_domain(args.domain).await?;
    write_delete_output(args.domain, output_format, output_file).await
}

async fn write_delete_output(
    domain: carbide_uuid::domain::DomainId,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    if output_format == OutputFormat::Json {
        async_writeln!(output_file, "{{\"deleted\":\"{}\"}}", domain)?;
    } else {
        async_writeln!(output_file, "Deleted domain {}", domain)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_write::CapturedOutput;

    #[tokio::test]
    async fn delete_output_uses_configured_writer() {
        let mut captured = CapturedOutput::new();
        let id = "12345678-1234-5678-90ab-cdef01234567".parse().unwrap();

        write_delete_output(id, OutputFormat::Json, captured.writer())
            .await
            .unwrap();

        let output = String::from_utf8(captured.into_bytes().await).unwrap();
        assert_eq!(
            output,
            "{\"deleted\":\"12345678-1234-5678-90ab-cdef01234567\"}\n"
        );
    }
}
