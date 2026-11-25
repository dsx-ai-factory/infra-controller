/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use db::TransactionVending as _;
use model::nvlink_domain::{NvLinkDomainSnapshot, NvLinkDomainState};
use sqlx::PgConnection;
use state_controller::CheckApplied;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use uuid::Uuid;

use crate::NvLinkDomainStateHandlerContext;
use crate::created::handle_created;
use crate::deleting::handle_deleting;
use crate::discovering::handle_discovering;
use crate::error_state::handle_error;
use crate::maintenance::handle_maintenance;
use crate::ready::handle_ready;
use crate::validating::handle_validating;

/// Reconciles deletion intent while retaining lifecycle state for phase-specific handlers.
#[derive(Debug, Default)]
pub struct NvLinkDomainStateHandler;

impl NvLinkDomainStateHandler {
    async fn next_outcome(
        txn: &mut PgConnection,
        state: &mut NvLinkDomainSnapshot,
        controller_state: &NvLinkDomainState,
        ctx: &mut StateHandlerContext<'_, NvLinkDomainStateHandlerContext>,
    ) -> Result<StateHandlerOutcome<NvLinkDomainState>, StateHandlerError> {
        if state.deleted.is_some() && !matches!(controller_state, NvLinkDomainState::Deleting) {
            return Ok(StateHandlerOutcome::transition(NvLinkDomainState::Deleting));
        }
        match controller_state {
            NvLinkDomainState::Created => handle_created(txn, state, ctx).await,
            NvLinkDomainState::Discovering => handle_discovering(txn, state, ctx).await,
            NvLinkDomainState::Maintenance { maintenance_state } => {
                handle_maintenance(txn, state, maintenance_state, ctx).await
            }
            NvLinkDomainState::Validating { validating_state } => {
                handle_validating(txn, state, validating_state, ctx).await
            }
            NvLinkDomainState::Ready => handle_ready(txn, state, ctx).await,
            NvLinkDomainState::Error { cause } => handle_error(txn, state, cause, ctx).await,
            NvLinkDomainState::Deleting => handle_deleting(txn, state, ctx).await,
        }
    }
}

#[async_trait::async_trait]
impl StateHandler for NvLinkDomainStateHandler {
    type ObjectId = Uuid;
    type State = NvLinkDomainSnapshot;
    type ControllerState = NvLinkDomainState;
    type ContextObjects = NvLinkDomainStateHandlerContext;

    async fn handle_object_state(
        &self,
        _id: &Uuid,
        state: &mut NvLinkDomainSnapshot,
        controller_state: &NvLinkDomainState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<NvLinkDomainState>, StateHandlerError> {
        let mut txn = ctx.services.db_pool.txn_begin().await?;
        db::nvlink_domain::lifecycle::lock_snapshot(&mut txn, state)
            .await?
            .check_applied()?;
        // Phase handlers run with the domain row locked.
        // External I/O must run outside this transaction.
        Ok(Self::next_outcome(&mut txn, state, controller_state, ctx)
            .await?
            .with_txn(txn))
    }
}
