/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::{FindNicFirmwareSiteSelectionsByIdsRequest, NicFirmwareSiteSelectionSearchFilter};

use super::super::output::write_selections;
use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn list(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let hardware = ctx
        .api_client
        .0
        .find_nic_firmware_site_selection_ids(NicFirmwareSiteSelectionSearchFilter {
            profile_id: args.profile.map(|profile| profile.to_string()),
        })
        .await
        .wrap_err("while attempting to list site NIC firmware selection keys")?
        .hardware;
    let mut selections = Vec::with_capacity(hardware.len());
    if !hardware.is_empty() {
        let page_size = ctx
            .api_client
            .effective_chunk_size(ctx.config.page_size)
            .await
            .wrap_err(
                "while attempting to determine the site NIC firmware selection batch limit",
            )?;
        for hardware in hardware.chunks(page_size) {
            let response = ctx
                .api_client
                .0
                .find_nic_firmware_site_selections_by_ids(
                    FindNicFirmwareSiteSelectionsByIdsRequest {
                        hardware: hardware.to_vec(),
                    },
                )
                .await
                .wrap_err("while attempting to read a batch of site NIC firmware selections")?;
            selections.extend(response.site_selections);
        }
    }
    write_selections(selections, ctx).await
}
