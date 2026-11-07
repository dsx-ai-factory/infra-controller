/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use eyre::Context;
use rpc::forge::set_machine_nic_firmware_config_request::Action;
use rpc::forge::{NicFirmwareCardSelections, SetMachineNicFirmwareConfigRequest};

use super::Args;
use super::input::read_cards;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

pub(super) async fn set(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let action = match (args.profile, args.clear_profile) {
        (Some(profile), false) => Action::ProfileId(profile.to_string()),
        (None, true) => Action::Clear(()),
        // Clap's required, exclusive `machine_action` group admits only these
        // two cases.
        _ => unreachable!("clap requires exactly one machine profile action"),
    };
    // Absence preserves stored cards; even an empty supplied array must become
    // a present wrapper so Core can distinguish it from a machine-only edit.
    let card_selections = args
        .cards_file
        .as_deref()
        .map(read_cards)
        .transpose()?
        .map(|selections| NicFirmwareCardSelections { selections });
    let request = SetMachineNicFirmwareConfigRequest {
        machine_id: Some(args.machine_id.into()),
        if_version_match: Some(args.if_version_match.to_string()),
        action: Some(action),
        card_selections,
    };
    ctx.api_client
        .0
        .set_machine_nic_firmware_config(request)
        .await
        .wrap_err_with(|| {
            format!(
                "while attempting to change NIC firmware configuration for {} at version {}; \
                 after a connection error, inspect firmware nic machine show before another write",
                args.machine_id, args.if_version_match
            )
        })?;
    Ok(())
}
