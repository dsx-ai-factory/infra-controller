/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::future::{Ready, ready};

use model::nvlink_domain::{NvLinkDomainSnapshot, NvLinkDomainState};
use sqlx::PgConnection;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::NvLinkDomainStateHandlerContext;

pub(crate) fn handle_created(
    _txn: &mut PgConnection,
    state: &mut NvLinkDomainSnapshot,
    _ctx: &mut StateHandlerContext<'_, NvLinkDomainStateHandlerContext>,
) -> Ready<Result<StateHandlerOutcome<NvLinkDomainState>, StateHandlerError>> {
    if state.domain.members.is_empty() {
        return ready(Ok(StateHandlerOutcome::wait(
            "waiting for declared domain members".into(),
        )));
    }
    ready(Ok(StateHandlerOutcome::transition(
        NvLinkDomainState::Discovering,
    )))
}
