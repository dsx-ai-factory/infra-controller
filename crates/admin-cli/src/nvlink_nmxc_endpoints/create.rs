/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use ::rpc::forge::NvlinkNmxcEndpoint;
use clap::Parser;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Insert a chassis serial to NMX-C endpoint mapping:
    $ nico-admin-cli nvlink-nmxc-endpoints create --chassis-serial 1234567890123 \
    --endpoint https://192.0.2.10:50051

")]
pub(crate) struct Args {
    #[clap(long, value_name = "SERIAL")]
    chassis_serial: String,

    /// NMX-C gRPC base URL (e.g. https://host:50051)
    #[clap(long)]
    endpoint: String,
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        if self.chassis_serial.is_empty() {
            return Err(CarbideCliError::GenericError(
                "chassis_serial must not be empty".to_string(),
            ));
        }
        let req = NvlinkNmxcEndpoint {
            chassis_serial: self.chassis_serial,
            endpoint: self.endpoint,
        };
        let created = ctx
            .api_client
            .0
            .create_nvlink_nmxc_endpoint(req)
            .await
            .map_err(|e| CarbideCliError::GenericError(e.to_string()))?;
        println!("created {} -> {}", created.chassis_serial, created.endpoint);
        Ok(())
    }
}
