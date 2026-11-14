/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use model::nvlink_domain::{NvLinkDomainSnapshot, NvLinkDomainState};
use state_controller::CheckApplied;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use uuid::Uuid;

use crate::NvLinkDomainStateHandlerContext;

/// Reconciles deletion intent while retaining lifecycle state for phase-specific handlers.
#[derive(Debug, Default)]
pub struct NvLinkDomainStateHandler;

impl NvLinkDomainStateHandler {
    fn next_outcome(
        state: &NvLinkDomainSnapshot,
        controller_state: &NvLinkDomainState,
    ) -> StateHandlerOutcome<NvLinkDomainState> {
        if state.deleted.is_some() && !matches!(controller_state, NvLinkDomainState::Deleting) {
            return StateHandlerOutcome::transition(NvLinkDomainState::Deleting);
        }
        let reason = match controller_state {
            NvLinkDomainState::Created | NvLinkDomainState::Discovering => {
                "waiting for domain ingestion"
            }
            NvLinkDomainState::Maintenance { .. } => "waiting for domain maintenance",
            NvLinkDomainState::Validating { .. } => "waiting for domain validation",
            NvLinkDomainState::Ready => "domain is ready",
            NvLinkDomainState::Error { cause } => return StateHandlerOutcome::wait(cause.clone()),
            NvLinkDomainState::Deleting => "domain is awaiting teardown",
        };
        StateHandlerOutcome::wait(reason.into())
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
        let mut txn = ctx.services.begin().await?;
        db::nvlink_domain::lifecycle::lock_snapshot(&mut txn, state)
            .await?
            .check_applied()?;
        Ok(Self::next_outcome(state, controller_state).with_txn(txn))
    }
}
