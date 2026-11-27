/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use model::nvlink_domain::{NvLinkDomainSnapshot, NvLinkDomainState, derive_managed_members};
use sqlx::PgConnection;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::NvLinkDomainStateHandlerContext;

pub(crate) async fn handle_created(
    txn: &mut PgConnection,
    state: &mut NvLinkDomainSnapshot,
    _ctx: &mut StateHandlerContext<'_, NvLinkDomainStateHandlerContext>,
) -> Result<StateHandlerOutcome<NvLinkDomainState>, StateHandlerError> {
    if state.domain.members.is_empty() {
        if !state.config.value.managed {
            return Ok(StateHandlerOutcome::wait(
                "waiting for declared domain members".into(),
            ));
        }
        let group =
            db::expected_rack_group::find_by_rack_group_id(&mut *txn, &state.domain.rack_group_id)
                .await?
                .ok_or_else(|| StateHandlerError::MissingData {
                    object_id: state.domain.id.to_string(),
                    missing: "expected rack group",
                })?;
        let members = derive_managed_members(&group).map_err(StateHandlerError::InvalidState)?;
        if members.is_empty() {
            return Ok(StateHandlerOutcome::wait(
                "rack group has no compute or switch members".into(),
            ));
        }
        db::nvlink_domain::claim_members(txn, &state.domain, &members).await?;
    }
    Ok(StateHandlerOutcome::transition(
        NvLinkDomainState::Discovering,
    ))
}
