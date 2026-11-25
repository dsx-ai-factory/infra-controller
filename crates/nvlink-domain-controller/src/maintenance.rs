/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::future::{Ready, ready};

use model::nvlink_domain::{NvLinkDomainSnapshot, NvLinkDomainState};
use model::rack::RackMaintenanceState;
use sqlx::PgConnection;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::NvLinkDomainStateHandlerContext;

pub(crate) fn handle_maintenance(
    _txn: &mut PgConnection,
    _state: &mut NvLinkDomainSnapshot,
    _maintenance_state: &RackMaintenanceState,
    _ctx: &mut StateHandlerContext<'_, NvLinkDomainStateHandlerContext>,
) -> Ready<Result<StateHandlerOutcome<NvLinkDomainState>, StateHandlerError>> {
    ready(Ok(StateHandlerOutcome::wait(
        "waiting for domain maintenance".into(),
    )))
}
