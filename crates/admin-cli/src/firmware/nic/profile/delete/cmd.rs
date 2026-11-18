/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::DeleteNicFirmwareProfileRequest;

use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn delete(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    ctx.api_client
        .0
        .delete_nic_firmware_profile(DeleteNicFirmwareProfileRequest {
            id: args.id.to_string(),
            if_version_match: Some(args.if_version_match.to_string()),
        })
        .await
        .wrap_err_with(|| {
            format!(
                "while attempting to delete NIC firmware profile {} at version {}",
                args.id, args.if_version_match
            )
        })?;
    Ok(())
}
