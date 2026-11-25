/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use db::nvlink_domain::members::{self, NvLinkDomainTray};
use model::nvlink_domain::{NvLinkDomainSnapshot, NvLinkDomainState};
use model::rack::{FirmwareUpgradeState, RackMaintenanceState};
use model::switch::SwitchControllerState;
use sqlx::PgConnection;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::NvLinkDomainStateHandlerContext;

pub(crate) async fn handle_discovering(
    txn: &mut PgConnection,
    state: &mut NvLinkDomainSnapshot,
    _ctx: &mut StateHandlerContext<'_, NvLinkDomainStateHandlerContext>,
) -> Result<StateHandlerOutcome<NvLinkDomainState>, StateHandlerError> {
    let members = members::resolve_domain_members(txn, &state.domain).await?;
    if members.is_empty() {
        return Ok(StateHandlerOutcome::wait(
            "waiting for declared domain members".into(),
        ));
    }
    let missing = members
        .iter()
        .filter(|member| member.tray.is_none())
        .count();
    if missing != 0 {
        return Ok(StateHandlerOutcome::wait(format!(
            "waiting for domain members: missing={missing}/{}",
            members.len(),
        )));
    }
    // Compute readiness can depend on fabric bringup, so only declared switches must be Ready.
    let unready_switches = members
        .iter()
        .filter(|member| {
            matches!(
                &member.tray,
                Some(NvLinkDomainTray::Switch { controller_state, .. })
                    if controller_state.value != SwitchControllerState::Ready
            )
        })
        .count();
    if unready_switches != 0 {
        return Ok(StateHandlerOutcome::wait(format!(
            "waiting for domain switches ready: unready={unready_switches}",
        )));
    }
    Ok(StateHandlerOutcome::transition(
        NvLinkDomainState::Maintenance {
            maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                rack_firmware_upgrade: FirmwareUpgradeState::Start,
            },
        },
    ))
}
