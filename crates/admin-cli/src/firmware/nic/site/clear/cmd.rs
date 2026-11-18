/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::set_nic_firmware_site_selection_request::Action;
use rpc::forge::{NicFirmwareHardware, SetNicFirmwareSiteSelectionRequest};

use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn clear(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let error_context = format!(
        "while attempting to clear a site NIC firmware selection for part number {} / PSID {} \
         at selection version {}",
        args.part_number, args.psid, args.if_version_match
    );
    ctx.api_client
        .0
        .set_nic_firmware_site_selection(SetNicFirmwareSiteSelectionRequest {
            hardware: Some(NicFirmwareHardware {
                part_number: args.part_number,
                psid: args.psid,
            }),
            action: Some(Action::Clear(())),
            if_version_match: Some(args.if_version_match.to_string()),
        })
        .await
        .wrap_err(error_context)?;
    Ok(())
}
