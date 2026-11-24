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

pub(crate) fn handle_error(
    _txn: &mut PgConnection,
    _state: &mut NvLinkDomainSnapshot,
    cause: &str,
    _ctx: &mut StateHandlerContext<'_, NvLinkDomainStateHandlerContext>,
) -> Ready<Result<StateHandlerOutcome<NvLinkDomainState>, StateHandlerError>> {
    ready(Ok(StateHandlerOutcome::wait(cause.to_owned())))
}
