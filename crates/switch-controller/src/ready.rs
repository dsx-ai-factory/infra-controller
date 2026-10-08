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

//! Handler for SwitchControllerState::Ready.

use carbide_uuid::switch::SwitchId;
use component_manager::component_common::{PowerStatePollOutcome, interpret_power_state_poll};
use db::switch as db_switch;
use model::switch::{
    ConfiguringState, Switch, SwitchControllerState, SwitchDecommissioningState, SwitchStatus,
};
use sqlx::PgTransaction;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;
use crate::endpoint::resolve_switch_endpoint;
use crate::nvos_password_rotation::needs_nvos_password_reconciliation;
use crate::reprovisioning::first_reprovisioning_state;
use crate::rotating_bmc::should_enter_bmc_rotation;

/// Handles the Ready state for a switch.
///
/// If the switch is marked for deletion, transitions to `Deleting`.
/// If a maintenance request has been posted via `switch_maintenance_requested`,
/// transitions to `Maintenance` with the requested operation. If rack-level
/// reprovisioning has been requested, transitions to `ReProvisioning`.
/// Otherwise, actionable NVOS convergence work transitions back to `Configuring`.
/// When the switch is otherwise idle, polls the configured component manager
/// backend for the current power state (best-effort observation) and idles.
pub async fn handle_ready(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    if state.is_marked_as_deleted() {
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Deleting,
        ));
    }

    if state.decommission_requested {
        let mut txn = ctx.services.db_pool.begin().await?;
        db_switch::clear_decommission_requested(&mut txn, *switch_id).await?;
        return Ok(
            StateHandlerOutcome::transition(SwitchControllerState::Decommissioning {
                decommissioning_state: SwitchDecommissioningState::SuppressingSiteExplorer,
            })
            .with_txn(txn),
        );
    }

    if let Some(req) = state.switch_maintenance_requested.as_ref() {
        tracing::info!(
            operation = ?req.operation,
            initiator = %req.initiator,
            "Switch maintenance requested; transitioning to Maintenance"
        );
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::maintenance_for_request(req.clone()),
        ));
    }

    if let Some(req) = &state.switch_reprovisioning_requested {
        if !req.initiator.starts_with("rack-") {
            tracing::warn!(
                initiator = %req.initiator,
                "Unknown initiator for switch reprovisioning request",
            );
            let cause = format!(
                "unknown initiator for switch reprovisioning request: {}",
                req.initiator
            );
            let mut txn = ctx.services.db_pool.begin().await?;
            db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id).await?;
            return Ok(
                StateHandlerOutcome::transition(SwitchControllerState::Error { cause })
                    .with_txn(txn),
            );
        }

        let Some(reprovisioning_state) = first_reprovisioning_state(req) else {
            tracing::warn!(
                switch_id = %switch_id,
                initiator = %req.initiator,
                "Rack reprovision request has no switch-relevant activities; clearing request"
            );
            let mut txn = ctx.services.db_pool.begin().await?;
            db_switch::clear_switch_reprovisioning_requested(txn.as_mut(), *switch_id).await?;
            return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
        };

        tracing::info!(
            ?reprovisioning_state,
            "Rack-level reprovisioning requested — entering ReProvisioning"
        );
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::ReProvisioning {
                reprovisioning_state,
            },
        ));
    }

    if needs_nvos_password_reconciliation(switch_id, state, ctx).await? {
        tracing::info!(
            switch_id = ?switch_id,
            "Switch: NVOS password reconciliation pending; transitioning to Configuring",
        );

        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Configuring {
                config_state: ConfiguringState::RotateOsPassword,
            },
        ));
    }

    // Lowest precedence: only converge the BMC credential once the switch is
    // otherwise idle in Ready, so rotation never contends with NVOS
    // reconfiguration, maintenance, or reprovisioning. The site-flag gate and
    // the operator force-converge override live in `should_enter_bmc_rotation`.
    if should_enter_bmc_rotation(ctx.services, state).await? {
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::RotatingBmc { retry_count: 0 },
        ));
    }

    let txn = poll_power_state(switch_id, state, ctx).await;

    Ok(StateHandlerOutcome::do_nothing().with_txn_opt(txn))
}

/// Best-effort poll of the switch power state while idling in `Ready`.
///
/// On a successful response, the observed power state for this switch is
/// persisted to the `switches.status` column and the in-memory `state` is
/// updated to match. The returned `PgTransaction` (if any) carries that status
/// write so the caller can attach it to the `Ready` outcome and have the
/// state-controller framework commit it alongside the usual outcome
/// bookkeeping.
///
/// Missing prerequisites (no component manager, no rack association, no
/// resolvable endpoint) and transport / backend failures are logged but never
/// transition the controller out of `Ready`, so a transient outage cannot
/// bounce the switch into `Error`.
async fn poll_power_state(
    switch_id: &SwitchId,
    switch: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Option<PgTransaction<'static>> {
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        tracing::debug!(
            switch_id = %switch_id,
            "Switch Ready: skipping power state poll; component manager not configured",
        );
        return None;
    };

    let Some(rack_id) = switch.rack_id.as_ref() else {
        tracing::debug!(
            switch_id = %switch_id,
            "Switch Ready: skipping power state poll; switch has no rack association",
        );
        return None;
    };

    let endpoint = match resolve_switch_endpoint(
        switch_id,
        &ctx.services.db_pool,
        &ctx.services.credential_manager,
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::debug!(
                switch_id = %switch_id,
                rack_id = %rack_id,
                error = %error,
                "Switch Ready: skipping power state poll; unable to resolve endpoint",
            );
            return None;
        }
    };

    let rack_id_str = rack_id.to_string();
    let results = match component_manager
        .nv_switch
        .get_power_state(std::slice::from_ref(&endpoint))
        .await
    {
        Ok(results) => results,
        Err(error) => {
            tracing::warn!(
                switch_id = %switch_id,
                rack_id = %rack_id_str,
                backend = component_manager.nv_switch.name(),
                error = %error,
                "Switch get power state transport error",
            );
            return None;
        }
    };

    match interpret_power_state_poll(results) {
        PowerStatePollOutcome::Observed(observed_power_state) => {
            tracing::info!(
                switch_id = %switch_id,
                rack_id = %rack_id_str,
                backend = component_manager.nv_switch.name(),
                power_state = %observed_power_state,
                "Switch get power state succeeded",
            );
            persist_observed_power_state(switch_id, switch, ctx, &observed_power_state).await
        }
        PowerStatePollOutcome::BackendError(error) => {
            tracing::warn!(
                switch_id = %switch_id,
                rack_id = %rack_id_str,
                backend = component_manager.nv_switch.name(),
                error = %error,
                "Switch get power state returned an error result",
            );
            None
        }
        PowerStatePollOutcome::NoPowerState => {
            tracing::debug!(
                switch_id = %switch_id,
                backend = component_manager.nv_switch.name(),
                "Switch get power state did not return a power state",
            );
            None
        }
        PowerStatePollOutcome::NoResult => {
            tracing::debug!(
                switch_id = %switch_id,
                backend = component_manager.nv_switch.name(),
                "Switch get power state returned no result",
            );
            None
        }
    }
}

/// Stamp the observed power state into `switch.status` and persist it via
/// `db_switch::update`. Returns the open `PgTransaction` so the caller can
/// attach it to the `Ready` outcome.
///
/// Status persistence is best-effort: if the DB write fails, the in-memory
/// switch is left untouched and `None` is returned — `Ready` must stay in
/// `Ready` regardless.
async fn persist_observed_power_state(
    switch_id: &SwitchId,
    switch: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    observed_power_state: &str,
) -> Option<PgTransaction<'static>> {
    let new_status = match switch.status.as_ref() {
        Some(existing) => SwitchStatus {
            switch_name: existing.switch_name.clone(),
            power_state: observed_power_state.to_owned(),
            health_status: existing.health_status.clone(),
        },
        None => SwitchStatus {
            switch_name: switch.config.name.clone(),
            power_state: observed_power_state.to_owned(),
            health_status: String::new(),
        },
    };

    if switch
        .status
        .as_ref()
        .is_some_and(|s| s.power_state == new_status.power_state)
    {
        tracing::debug!(
            switch_id = %switch_id,
            power_state = %new_status.power_state,
            "Switch status power_state unchanged; skipping DB write",
        );
        return None;
    }

    let previous_status = switch.status.replace(new_status);

    let mut txn = match ctx.services.db_pool.begin().await {
        Ok(txn) => txn,
        Err(error) => {
            switch.status = previous_status;
            tracing::warn!(
                switch_id = %switch_id,
                error = %error,
                "Switch Ready: failed to begin txn while persisting observed power state",
            );
            return None;
        }
    };

    if let Err(error) = db_switch::update(switch, txn.as_mut()).await {
        switch.status = previous_status;
        tracing::warn!(
            switch_id = %switch_id,
            error = %error,
            "Switch Ready: failed to persist observed power state to DB",
        );
        return None;
    }

    tracing::info!(
        switch_id = %switch_id,
        power_state = %observed_power_state,
        "Switch Ready: persisted observed power state",
    );

    Some(txn)
}
