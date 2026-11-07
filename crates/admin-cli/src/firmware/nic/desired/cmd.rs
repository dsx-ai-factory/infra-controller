/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::GetDesiredNicFirmwareRequest;

use super::Args;
use super::output::write_desired;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn desired(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let response = ctx
        .api_client
        .0
        .get_desired_nic_firmware(GetDesiredNicFirmwareRequest {
            machine_id: Some(args.host.into()),
            allocation_profile_id: args.allocation_profile.map(|id| id.to_string()),
        })
        .await
        .wrap_err_with(|| {
            format!(
                "while attempting to read desired NIC firmware for {}",
                args.host
            )
        })?;
    write_desired(response, ctx).await
}
