/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::{
    FindNicFirmwareProfileIdsRequest, FindNicFirmwareProfilesByIdsRequest, NicFirmwareHardware,
};

use super::super::output::write_profiles;
use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn list(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let hardware = args
        .part_number
        .zip(args.psid)
        .map(|(part_number, psid)| NicFirmwareHardware { part_number, psid });
    let ids = ctx
        .api_client
        .0
        .find_nic_firmware_profile_ids(FindNicFirmwareProfileIdsRequest { hardware })
        .await
        .wrap_err("while attempting to list NIC firmware profile IDs")?
        .profile_ids;
    let mut profiles = Vec::with_capacity(ids.len());
    if !ids.is_empty() {
        let page_size = ctx
            .api_client
            .effective_chunk_size(ctx.config.page_size)
            .await
            .wrap_err("while attempting to determine the NIC firmware profile batch limit")?;
        for profile_ids in ids.chunks(page_size) {
            let response = ctx
                .api_client
                .0
                .find_nic_firmware_profiles_by_ids(FindNicFirmwareProfilesByIdsRequest {
                    profile_ids: profile_ids.to_vec(),
                })
                .await
                .wrap_err("while attempting to read a batch of NIC firmware profiles")?;
            profiles.extend(response.profiles);
        }
    }
    write_profiles(profiles, ctx).await
}
