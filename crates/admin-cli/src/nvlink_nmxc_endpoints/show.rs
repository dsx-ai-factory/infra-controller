/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use ::rpc::admin_cli::OutputFormat;
use clap::Parser;
use prettytable::{Table, row};

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all chassis serial to NMX-C endpoint mappings:
    $ nico-admin-cli nvlink-nmxc-endpoints show

Show the mapping for one chassis serial:
    $ nico-admin-cli nvlink-nmxc-endpoints show --chassis-serial 1234567890123

")]
pub(crate) struct Args {
    /// If set, show only this chassis serial
    #[clap(long, value_name = "SERIAL")]
    chassis_serial: Option<String>,
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        run(self, ctx.config.format, &ctx.api_client).await
    }
}

async fn run(args: Args, format: OutputFormat, api_client: &ApiClient) -> CarbideCliResult<()> {
    let list = api_client
        .0
        .list_nvlink_nmxc_endpoints()
        .await
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))?;

    let mut entries = list.entries;
    if let Some(ref s) = args.chassis_serial {
        entries.retain(|e| e.chassis_serial == *s);
    }

    if format == OutputFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&entries).map_err(CarbideCliError::from)?
        );
        return Ok(());
    }

    let mut table = Table::new();
    table.add_row(row!["chassis_serial", "endpoint"]);
    for e in &entries {
        table.add_row(row![e.chassis_serial, e.endpoint]);
    }
    table.printstd();
    Ok(())
}
