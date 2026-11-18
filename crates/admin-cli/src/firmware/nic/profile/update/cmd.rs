/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::UpdateNicFirmwareProfileRequest;

use super::super::input::read_config;
use super::super::output::write_mutation_result;
use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn update(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let config = read_config(&args.input.config_file)?;
    let response = ctx
        .api_client
        .0
        .update_nic_firmware_profile(UpdateNicFirmwareProfileRequest {
            id: args.id.to_string(),
            config: Some(config.into()),
            if_version_match: Some(args.if_version_match.to_string()),
        })
        .await
        .wrap_err_with(|| {
            format!(
                "while attempting to update NIC firmware profile {} at version {}",
                args.id, args.if_version_match
            )
        })?;
    write_mutation_result(response, &args.id, ctx).await
}
