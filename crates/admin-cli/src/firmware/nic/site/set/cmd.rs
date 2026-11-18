/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::set_nic_firmware_site_selection_request::Action;
use rpc::forge::{NicFirmwareHardware, SetNicFirmwareSiteSelectionRequest};

use super::super::output::write_mutation_result;
use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn set(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let version_context = match &args.if_version_match {
        Some(version) => format!("at selection version {version}"),
        None => "without an existing selection version".into(),
    };
    let error_context = format!(
        "while attempting to set a site NIC firmware selection for part number {} / PSID {} \
         to profile {} {version_context}",
        args.part_number, args.psid, args.profile
    );
    let response = ctx
        .api_client
        .0
        .set_nic_firmware_site_selection(SetNicFirmwareSiteSelectionRequest {
            hardware: Some(NicFirmwareHardware {
                part_number: args.part_number,
                psid: args.psid,
            }),
            action: Some(Action::ProfileId(args.profile.to_string())),
            if_version_match: args.if_version_match.map(|version| version.to_string()),
        })
        .await
        .wrap_err(error_context)?;
    write_mutation_result(response, ctx).await
}
