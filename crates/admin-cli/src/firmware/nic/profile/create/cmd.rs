/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::CreateNicFirmwareProfileRequest;

use super::super::input::read_config;
use super::super::output::write_mutation_result;
use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn create(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let config = read_config(&args.input.config_file)?;
    let response = ctx
        .api_client
        .0
        .create_nic_firmware_profile(CreateNicFirmwareProfileRequest {
            id: args.id.to_string(),
            config: Some(config.into()),
        })
        .await
        .wrap_err_with(|| {
            format!(
                "while attempting to create NIC firmware profile {}",
                args.id
            )
        })?;
    write_mutation_result(response, &args.id, ctx).await
}
