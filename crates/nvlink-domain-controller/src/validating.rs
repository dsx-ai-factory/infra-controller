/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::future::{Ready, ready};

use model::nvlink_domain::{NvLinkDomainSnapshot, NvLinkDomainState};
use model::rack::RackValidationState;
use sqlx::PgConnection;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::NvLinkDomainStateHandlerContext;

pub(crate) fn handle_validating(
    _txn: &mut PgConnection,
    _state: &mut NvLinkDomainSnapshot,
    _validating_state: &RackValidationState,
    _ctx: &mut StateHandlerContext<'_, NvLinkDomainStateHandlerContext>,
) -> Ready<Result<StateHandlerOutcome<NvLinkDomainState>, StateHandlerError>> {
    ready(Ok(StateHandlerOutcome::wait(
        "waiting for domain validation".into(),
    )))
}
