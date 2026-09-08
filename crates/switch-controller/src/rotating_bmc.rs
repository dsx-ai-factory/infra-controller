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

//! Switch-controller BMC credential rotation.
//!
//! The shared [`carbide_credential_rotation`] engine owns the password dance,
//! backoff, and crash-safety; this module is the thin switch-controller adapter,
//! mirroring the machine-controller's `rotation.rs`. A switch has exactly one
//! BMC, so there is no per-device fan-out: one endpoint, one [`rotate_bmc`] call
//! per tick.
//!
//! - *Should we enter rotation?* [`should_enter_bmc_rotation`] honors an operator
//!   force-converge request first (even when the site-wide flag is off), then the
//!   passive gate (`bmc_rotation_enabled` && the cached [`RotationGate`] says
//!   the BMC lags the staged target).
//! - *Do one rotation tick.* [`handle_rotating_bmc`] converges the switch BMC via
//!   [`rotate_bmc`], folds the outcome through the shared state-neutral
//!   [`advance`] retry seam, and returns to `Ready` (clearing a satisfied force
//!   request) or re-enters `RotatingBmc` with an incremented retry budget.
//!
//! A BMC password change never touches the switch data plane, so this is safe in
//! `Ready`.

use carbide_credential_rotation::site_explorer_pause::{self, GateDecision};
use carbide_credential_rotation::{
    BmcEndpoint, BmcRotationTick, RotateOutcome, RotationStep, advance, rotate_bmc,
};
use carbide_uuid::switch::SwitchId;
use libredfish::model::service_root::RedfishVendor;
use model::switch::{Switch, SwitchControllerState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::{SwitchStateHandlerContextObjects, SwitchStateHandlerServices};

/// Whether a Ready switch should enter `SwitchControllerState::RotatingBmc` now.
///
/// An operator force-converge request always wins -- the ops escape hatch is
/// honored even when the site-wide flag is off. Otherwise the passive gate fires
/// only when BMC rotation is enabled site-wide *and* the BMC lags the staged
/// target; the flag is checked first so a disabled site never runs the per-device
/// gate query.
pub async fn should_enter_bmc_rotation(
    services: &SwitchStateHandlerServices,
    switch: &Switch,
) -> Result<bool, StateHandlerError> {
    if switch.bmc_credential_rotation_requested {
        return Ok(true);
    }
    if !services.bmc_rotation_enabled {
        return Ok(false);
    }
    let Some(endpoint) = BmcEndpoint::from_switch(switch) else {
        return Ok(false);
    };
    services
        .bmc_rotation_gate
        .rotation_needed(&services.db_pool, endpoint.device_mac)
        .await
        .map_err(|e| StateHandlerError::GenericError(eyre::eyre!("bmc rotation gate query: {e}")))
}

/// Run one rotation tick over the switch BMC and decide the next state.
///
/// `force` (an operator escape-hatch request on this switch row) bypasses the
/// device's backoff quarantine inside [`rotate_bmc`]. A settled tick returns to
/// `Ready` and clears a satisfied force request; a transient bookkeeping failure
/// re-enters `RotatingBmc` bounded by the shared retry budget; an exhausted
/// budget (`GaveUp`) returns to `Ready` but leaves a pending force request set so
/// the entry guard re-attempts on a later sweep.
pub async fn handle_rotating_bmc(
    switch_id: &SwitchId,
    switch: &Switch,
    retry_count: u32,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let force = switch.bmc_credential_rotation_requested;
    let endpoint = BmcEndpoint::from_switch(switch);

    // Hold site-explorer off the switch BMC and wait until it has acknowledged,
    // before changing the credential. This closes the window in which a probe
    // could hit new-hardware / old-Vault and persist the sticky `AvoidLockout`
    // latch. An unaddressable switch has nothing to suppress (empty scope).
    let bmc_macs: Vec<_> = endpoint
        .as_ref()
        .map(|e| vec![e.device_mac])
        .unwrap_or_default();
    if matches!(
        site_explorer_pause::gate_before_credential_change(
            &ctx.services.db_pool,
            &bmc_macs,
            site_explorer_pause::ROTATION_SUPPRESSION_REASON,
        )
        .await?,
        GateDecision::Wait
    ) {
        return Ok(StateHandlerOutcome::wait(
            "waiting for site-explorer to acknowledge the BMC rotation suppression".to_string(),
        ));
    }

    let tick = match endpoint {
        Some(endpoint) => rotate_switch_bmc(ctx.services, endpoint, force).await,
        // A forced request announces a missing BMC so its one-shot flag still
        // clears; a passive sweep silently settles an unaddressable switch (the
        // entry guard never selects it).
        None => {
            if force {
                tracing::warn!(
                    %switch_id,
                    "force-converge request on a switch with no addressable BMC; clearing the request without action"
                );
            }
            BmcRotationTick::Settled
        }
    };

    match advance(tick, retry_count, switch_id) {
        // The switch BMC changed its hardware password but the per-device secret
        // persist has not yet succeeded: hold in RotatingBmc (site-explorer stays
        // suppressed, and the `Ready`-only allocation gate keeps the switch out
        // of service) and re-tick. Do NOT resume site-explorer and do NOT clear a
        // force request -- the rotation has not finished. The engine leaves the
        // device `needs_rotation`, so a later tick's change-then-verify recovery
        // re-persists and converges; the hold's upper bound is the state's
        // time-in-state SLA.
        RotationStep::WaitForCredentialStoreReconcile => Ok(StateHandlerOutcome::wait(
            "switch BMC hardware changed but the per-device secret persist has not yet \
             succeeded; holding rotation (site-explorer suppressed) until the credential \
             store reconciles"
                .to_string(),
        )),
        step @ (RotationStep::Settled | RotationStep::GaveUp) => {
            // Only a settled tick clears a one-shot force request: the forced
            // attempt genuinely fired. GaveUp exhausted the transient-retry
            // budget without the forced attempt cleanly running, so leave the
            // flag set and let the entry guard re-attempt on a later sweep rather
            // than silently drop the operator's request.
            let mut txn = None;
            if force && matches!(step, RotationStep::Settled) {
                let mut t = ctx.services.db_pool.begin().await?;
                db::switch::clear_bmc_credential_rotation_requested(&mut t, *switch_id).await?;
                txn = Some(t);
            }
            // Resume site-explorer atomically with the return to Ready, so its
            // skip window ends exactly when the rotation does.
            let mut resume_txn = match txn.take() {
                Some(txn) => txn,
                None => ctx.services.db_pool.begin().await?,
            };
            site_explorer_pause::resume_after_credential_change(
                &mut resume_txn,
                &bmc_macs,
                site_explorer_pause::ROTATION_SUPPRESSION_REASON,
            )
            .await?;
            Ok(StateHandlerOutcome::transition(SwitchControllerState::Ready).with_txn(resume_txn))
        }
        RotationStep::Retry { retry_count } => Ok(StateHandlerOutcome::transition(
            SwitchControllerState::RotatingBmc { retry_count },
        )),
    }
}

/// Rotate the switch BMC toward the staged target. `force` bypasses the device's
/// backoff quarantine (operator escape hatch). Returns [`BmcRotationTick::Retry`]
/// only on a transient bookkeeping error; device faults are quarantined inside
/// [`rotate_bmc`] and reported as `Settled`.
async fn rotate_switch_bmc(
    services: &SwitchStateHandlerServices,
    endpoint: BmcEndpoint,
    force: bool,
) -> BmcRotationTick {
    // Every switch BMC in the fleet is an NVIDIA NVLink (MGX) switch today, so
    // the dispatch vendor `set_bmc_root_password` branches on is fixed -- no
    // per-tick probe is needed (unlike the machine path, whose BMC vendors are
    // heterogeneous). If non-NVIDIA switch BMCs ever appear, resolve the vendor
    // here (e.g. via `redfish_client_pool.probe_bmc_vendor`) instead.
    let vendor = RedfishVendor::NvidiaGBSwitch;
    let target = endpoint.into_target(vendor);
    match rotate_bmc(
        &services.db_pool,
        services.credential_manager.as_ref(),
        services.bmc_credential_ops.as_ref(),
        &target,
        force,
    )
    .await
    {
        Ok(RotateOutcome::Converged) => {
            tracing::info!(mac = %target.device_mac, force, "switch BMC converged to site-wide rotation target");
            BmcRotationTick::Settled
        }
        Ok(RotateOutcome::Quarantined { until }) => {
            tracing::warn!(
                mac = %target.device_mac,
                %until,
                "switch BMC rotation attempt failed; quarantined until backoff elapses"
            );
            BmcRotationTick::Settled
        }
        Ok(RotateOutcome::CredentialStoreReconcilePending) => {
            tracing::warn!(
                mac = %target.device_mac,
                "switch BMC hardware changed but the per-device secret persist failed; holding rotation until the credential store reconciles"
            );
            BmcRotationTick::WaitForCredentialStoreReconcile
        }
        Ok(RotateOutcome::NoWork) => BmcRotationTick::Settled,
        Err(e) => {
            tracing::warn!(
                mac = %target.device_mac,
                error = %e,
                "transient switch BMC rotation bookkeeping failure; will retry the tick"
            );
            BmcRotationTick::Retry
        }
    }
}
