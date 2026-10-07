/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Handler for PowerShelfControllerState::Ready.

use carbide_uuid::power_shelf::PowerShelfId;
use component_manager::component_common::{PowerStatePollOutcome, interpret_power_state_poll};
use db::power_shelf as db_power_shelf;
use model::power_shelf::{
    PowerShelf, PowerShelfControllerState, PowerShelfDecommissioningState, PowerShelfStatus,
};
use sqlx::PgTransaction;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::PowerShelfStateHandlerContextObjects;
use crate::maintenance::build_power_shelf_endpoint;
use crate::reprovisioning::first_reprovisioning_state;
use crate::rotating_bmc::should_enter_bmc_rotation;

/// Handles the Ready state for a power shelf.
///
/// If the power shelf is marked for deletion, transitions to `Deleting`.
/// If a maintenance request has been posted via
/// `power_shelf_maintenance_requested`, transitions to `Maintenance` with the
/// requested operation (PowerOn / PowerOff). If rack-level reprovisioning has
/// been requested and `rack_firmware_reprovisioning_enabled` is set, transitions
/// to `ReProvisioning`. Otherwise polls the configured component manager backend
/// for the current power state (best-effort observation) and idles.
///
/// TODO: Implement PowerShelf monitoring (health checks, status updates,
/// power consumption / efficiency tracking).
pub async fn handle_ready(
    power_shelf_id: &PowerShelfId,
    state: &mut PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    if state.is_marked_as_deleted() {
        return Ok(StateHandlerOutcome::transition(
            PowerShelfControllerState::Deleting,
        ));
    }

    if state.decommission_requested {
        let mut txn = ctx.services.db_pool.begin().await?;
        db_power_shelf::clear_decommission_requested(txn.as_mut(), *power_shelf_id).await?;
        return Ok(
            StateHandlerOutcome::transition(PowerShelfControllerState::Decommissioning {
                decommissioning_state: PowerShelfDecommissioningState::SuppressingSiteExplorer,
            })
            .with_txn(txn),
        );
    }

    if let Some(req) = state.power_shelf_maintenance_requested.as_ref() {
        tracing::info!(
            operation = ?req.operation,
            initiator = %req.initiator,
            "PowerShelf maintenance requested; transitioning to Maintenance"
        );
        return Ok(StateHandlerOutcome::transition(
            PowerShelfControllerState::Maintenance {
                operation: req.operation,
                request: Some(req.clone()),
            },
        ));
    }

    if let Some(req) = &state.power_shelf_reprovisioning_requested {
        if !ctx.services.rack_firmware_reprovisioning_enabled {
            tracing::info!(
                power_shelf_id = %power_shelf_id,
                initiator = %req.initiator,
                "Rack reprovision request ignored; rack_firmware_reprovisioning_enabled is false"
            );
            let mut txn = ctx.services.db_pool.begin().await?;
            db_power_shelf::clear_power_shelf_reprovisioning_requested(
                txn.as_mut(),
                *power_shelf_id,
            )
            .await?;
            return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
        }

        if !req.initiator.starts_with("rack-") {
            tracing::warn!(
                initiator = %req.initiator,
                "Unknown initiator for power shelf reprovisioning request",
            );
            let cause = format!(
                "unknown initiator for power shelf reprovisioning request: {}",
                req.initiator
            );
            let mut txn = ctx.services.db_pool.begin().await?;
            db_power_shelf::clear_power_shelf_reprovisioning_requested(
                txn.as_mut(),
                *power_shelf_id,
            )
            .await?;
            return Ok(
                StateHandlerOutcome::transition(PowerShelfControllerState::Error { cause })
                    .with_txn(txn),
            );
        }

        let Some(reprovisioning_state) = first_reprovisioning_state(req) else {
            tracing::warn!(
                power_shelf_id = %power_shelf_id,
                initiator = %req.initiator,
                "Rack reprovision request has no power-shelf-relevant activities; clearing request"
            );
            let mut txn = ctx.services.db_pool.begin().await?;
            db_power_shelf::clear_power_shelf_reprovisioning_requested(
                txn.as_mut(),
                *power_shelf_id,
            )
            .await?;
            return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
        };

        tracing::info!(
            ?reprovisioning_state,
            "Rack-level reprovisioning requested — entering ReProvisioning"
        );
        return Ok(StateHandlerOutcome::transition(
            PowerShelfControllerState::ReProvisioning {
                reprovisioning_state,
            },
        ));
    }

    // Lowest precedence: only converge the PMC credential once the power shelf is
    // otherwise idle in Ready, so rotation never contends with maintenance or
    // reprovisioning. The site-flag gate and the operator force-converge override
    // live in `should_enter_bmc_rotation`.
    if should_enter_bmc_rotation(ctx.services, state).await? {
        return Ok(StateHandlerOutcome::transition(
            PowerShelfControllerState::RotatingBmc { retry_count: 0 },
        ));
    }

    let txn = poll_power_state(power_shelf_id, state, ctx).await;

    Ok(StateHandlerOutcome::do_nothing().with_txn_opt(txn))
}

///
/// On a successful response, the observed power state for this power shelf is
/// persisted to the `power_shelves.status` column and the in-memory `state`
/// is updated to match. The returned `PgTransaction` (if any) carries that
/// status write so the caller can attach it to the `Ready` outcome and have
/// the state-controller framework commit it alongside the usual outcome
/// bookkeeping.
async fn poll_power_state(
    power_shelf_id: &PowerShelfId,
    state: &mut PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Option<PgTransaction<'static>> {
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        tracing::debug!(
            power_shelf_id = %power_shelf_id,
            "PowerShelf Ready: skipping power state poll; component manager not configured",
        );
        return None;
    };

    let Some(rack_id) = state.rack_id.as_ref() else {
        tracing::debug!(
            power_shelf_id = %power_shelf_id,
            "PowerShelf Ready: skipping power state poll; power shelf has no rack association",
        );
        return None;
    };

    let endpoint = match build_power_shelf_endpoint(
        power_shelf_id,
        state,
        ctx.services.credential_manager.as_ref(),
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(cause) => {
            tracing::debug!(
                power_shelf_id = %power_shelf_id,
                rack_id = %rack_id,
                cause = %cause,
                "PowerShelf Ready: skipping power state poll; unable to build endpoint",
            );
            return None;
        }
    };

    let rack_id_str = rack_id.to_string();
    let results = match component_manager
        .power_shelf
        .get_power_state(std::slice::from_ref(&endpoint))
        .await
    {
        Ok(results) => results,
        Err(error) => {
            tracing::warn!(
                power_shelf_id = %power_shelf_id,
                rack_id = %rack_id_str,
                backend = component_manager.power_shelf.name(),
                error = %error,
                "Power shelf get power state transport error",
            );
            return None;
        }
    };

    match interpret_power_state_poll(results) {
        PowerStatePollOutcome::Observed(observed_power_state) => {
            tracing::info!(
                power_shelf_id = %power_shelf_id,
                rack_id = %rack_id_str,
                backend = component_manager.power_shelf.name(),
                power_state = %observed_power_state,
                "Power shelf get power state succeeded",
            );
            persist_observed_power_state(power_shelf_id, state, ctx, &observed_power_state).await
        }
        PowerStatePollOutcome::BackendError(error) => {
            tracing::warn!(
                power_shelf_id = %power_shelf_id,
                rack_id = %rack_id_str,
                backend = component_manager.power_shelf.name(),
                error = %error,
                "Power shelf get power state returned an error result",
            );
            None
        }
        PowerStatePollOutcome::NoPowerState => {
            tracing::debug!(
                power_shelf_id = %power_shelf_id,
                backend = component_manager.power_shelf.name(),
                "Power shelf get power state did not return a power state",
            );
            None
        }
        PowerStatePollOutcome::NoResult => {
            tracing::debug!(
                power_shelf_id = %power_shelf_id,
                backend = component_manager.power_shelf.name(),
                "Power shelf get power state returned no result",
            );
            None
        }
    }
}

/// Stamp the observed power state into `state.status` and persist it via
/// `db_power_shelf::update`. Returns the open `PgTransaction` so the caller
/// can attach it to the `Ready` outcome.
///
/// Status persistence is best-effort: if the DB write fails, the in-memory
/// state is left untouched and `None` is returned — `Ready` must stay in
/// `Ready` regardless.
async fn persist_observed_power_state(
    power_shelf_id: &PowerShelfId,
    state: &mut PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
    observed_power_state: &str,
) -> Option<PgTransaction<'static>> {
    let new_status = match state.status.as_ref() {
        Some(existing) => PowerShelfStatus {
            shelf_name: existing.shelf_name.clone(),
            power_state: observed_power_state.to_owned(),
            health_status: existing.health_status.clone(),
        },
        None => PowerShelfStatus {
            shelf_name: state.config.name.clone(),
            power_state: observed_power_state.to_owned(),
            health_status: String::new(),
        },
    };

    if state
        .status
        .as_ref()
        .is_some_and(|s| s.power_state == new_status.power_state)
    {
        tracing::debug!(
            power_shelf_id = %power_shelf_id,
            power_state = %new_status.power_state,
            "PowerShelf status power_state unchanged; skipping DB write",
        );
        return None;
    }

    let previous_status = state.status.replace(new_status);

    let mut txn = match ctx.services.db_pool.begin().await {
        Ok(txn) => txn,
        Err(error) => {
            state.status = previous_status;
            tracing::warn!(
                power_shelf_id = %power_shelf_id,
                error = %error,
                "PowerShelf Ready: failed to begin txn while persisting observed power state",
            );
            return None;
        }
    };

    if let Err(error) = db_power_shelf::update(state, &mut txn).await {
        state.status = previous_status;
        tracing::warn!(
            power_shelf_id = %power_shelf_id,
            error = %error,
            "PowerShelf Ready: failed to persist observed power state to DB",
        );
        return None;
    }

    tracing::info!(
        power_shelf_id = %power_shelf_id,
        power_state = %observed_power_state,
        "PowerShelf Ready: persisted observed power state",
    );

    Some(txn)
}
