/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::{Context, eyre};
use rpc::forge::FindNicFirmwareProfilesByIdsRequest;

use super::super::output::write_profiles;
use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn show(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let response = ctx
        .api_client
        .0
        .find_nic_firmware_profiles_by_ids(FindNicFirmwareProfilesByIdsRequest {
            profile_ids: vec![args.id.to_string()],
        })
        .await
        .wrap_err_with(|| format!("while attempting to read NIC firmware profile {}", args.id))?;
    if response.profiles.is_empty() {
        return Err(eyre!("NIC firmware profile {} not found", args.id).into());
    }
    write_profiles(response.profiles, ctx).await
}
