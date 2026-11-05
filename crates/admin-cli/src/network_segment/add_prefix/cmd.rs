/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::admin_cli::OutputFormat;

use super::Args;
use crate::async_writeln;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

/// Prints the committed server result, not a reconstruction of the request.
pub(super) async fn add_prefix(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let segment = ctx
        .api_client
        .0
        .add_network_segment_prefix(args)
        .await
        .context("append a network segment prefix")?;
    let output = match ctx.config.format {
        OutputFormat::AsciiTable => {
            crate::network_segment::show::cmd::convert_network_to_nice_format(
                segment,
                None,
                &ctx.api_client,
            )
            .await
            .context("format the updated network segment returned by core")?
        }
        OutputFormat::Json => serde_json::to_string_pretty(&segment)
            .context("serialize the updated network segment as JSON")?,
        OutputFormat::Yaml => serde_yaml::to_string(&segment)
            .context("serialize the updated network segment as YAML")?,
        // Dispatch rejects CSV before the RPC; never serialize it as another format.
        OutputFormat::Csv => {
            return Err(eyre::eyre!("CSV output is not supported for prefix append").into());
        }
    };
    async_writeln!(ctx.output_file, "{output}")
        .context("write the updated network segment returned by core")?;
    Ok(())
}
