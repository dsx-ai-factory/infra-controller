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

//! Handler for PowerShelfControllerState::Maintenance.

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, Credentials,
};
use carbide_uuid::power_shelf::PowerShelfId;
use component_manager::power_shelf_manager::{
    PowerShelfComponentResult, PowerShelfEndpoint, PowerShelfVendor,
};
use db::power_shelf as db_power_shelf;
use mac_address::MacAddress;
use model::component_manager::PowerAction;
use model::power_shelf::{
    PowerShelf, PowerShelfControllerState, PowerShelfMaintenanceOperation,
    PowerShelfMaintenanceRequest,
};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::PowerShelfStateHandlerContextObjects;
/// Handles the Maintenance state for a power shelf, dispatching on the
/// requested operation (`PowerOn` / `PowerOff`).
pub async fn handle_maintenance(
    power_shelf_id: &PowerShelfId,
    state: &mut PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let PowerShelfControllerState::Maintenance { operation, request } =
        &state.controller_state.value
    else {
        unreachable!("handle_maintenance called with non-Maintenance state");
    };
    let request = request.as_ref();

    match operation {
        PowerShelfMaintenanceOperation::PowerOn => {
            handle_power_on(power_shelf_id, state, request, ctx).await
        }
        PowerShelfMaintenanceOperation::PowerOff => {
            handle_power_off(power_shelf_id, state, request, ctx).await
        }
    }
}

async fn handle_power_on(
    power_shelf_id: &PowerShelfId,
    state: &PowerShelf,
    request: Option<&PowerShelfMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    tracing::info!(
        power_shelf_id = %power_shelf_id,
        "PowerShelf maintenance: PowerOn"
    );
    invoke_power_operation(
        power_shelf_id,
        state,
        request,
        ctx,
        PowerAction::On,
        "PowerOn",
    )
    .await
}

async fn handle_power_off(
    power_shelf_id: &PowerShelfId,
    state: &PowerShelf,
    request: Option<&PowerShelfMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    tracing::info!(
        power_shelf_id = %power_shelf_id,
        "PowerShelf maintenance: PowerOff"
    );
    invoke_power_operation(
        power_shelf_id,
        state,
        request,
        ctx,
        PowerAction::ForceOff,
        "PowerOff",
    )
    .await
}

/// Common driver for component-manager-backed power maintenance operations.
/// Builds a `PowerShelfEndpoint` with the power shelf's BMC connection details
/// and dispatches `power_control` against the configured backend. Returns to
/// `Ready` on success or transitions to `Error` on failure. In both terminal
/// cases only the completed request is cleared. A replacement remains pending.
async fn invoke_power_operation(
    power_shelf_id: &PowerShelfId,
    state: &PowerShelf,
    request: Option<&PowerShelfMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
    action: PowerAction,
    operation_label: &'static str,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return finish_maintenance_with_error(
            power_shelf_id,
            request,
            ctx,
            format!(
                "PowerShelf {} maintenance ({}): component manager not configured",
                power_shelf_id, operation_label
            ),
        )
        .await;
    };

    let Some(rack_id) = state.rack_id.as_ref() else {
        return finish_maintenance_with_error(
            power_shelf_id,
            request,
            ctx,
            format!(
                "PowerShelf {} maintenance ({}): power shelf has no rack association",
                power_shelf_id, operation_label
            ),
        )
        .await;
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
            return finish_maintenance_with_error(
                power_shelf_id,
                request,
                ctx,
                format!(
                    "PowerShelf {} maintenance ({}): {}",
                    power_shelf_id, operation_label, cause
                ),
            )
            .await;
        }
    };

    match component_manager
        .power_shelf
        .power_control(std::slice::from_ref(&endpoint), action)
        .await
    {
        Ok(results) => {
            let result = results
                .into_iter()
                .next()
                .unwrap_or(PowerShelfComponentResult {
                    pmc_mac: endpoint.pmc_mac,
                    success: false,
                    error: Some("component manager returned no result".into()),
                });

            if result.success {
                tracing::info!(
                    power_shelf_id = %power_shelf_id,
                    rack_id = %rack_id,
                    operation = operation_label,
                    backend = component_manager.power_shelf.name(),
                    "Power shelf power control succeeded; returning PowerShelf to Ready"
                );
                return finish_maintenance(
                    power_shelf_id,
                    request,
                    ctx,
                    PowerShelfControllerState::Ready,
                )
                .await;
            }

            let summary = result
                .error
                .unwrap_or_else(|| "power control failed".into());
            tracing::warn!(
                power_shelf_id = %power_shelf_id,
                rack_id = %rack_id,
                operation = operation_label,
                backend = component_manager.power_shelf.name(),
                summary = %summary,
                "Power shelf power control returned a non-success result",
            );
            let cause = format!(
                "PowerShelf {} maintenance ({}): power control failed: {}",
                power_shelf_id, operation_label, summary
            );
            finish_maintenance_with_error(power_shelf_id, request, ctx, cause).await
        }
        Err(error) => {
            let cause = format!(
                "PowerShelf {} maintenance ({}): power control failed: {}",
                power_shelf_id, operation_label, error
            );
            tracing::warn!(
                power_shelf_id = %power_shelf_id,
                rack_id = %rack_id,
                operation = operation_label,
                backend = component_manager.power_shelf.name(),
                error = %error,
                "Power shelf power control transport error",
            );
            finish_maintenance_with_error(power_shelf_id, request, ctx, cause).await
        }
    }
}

/// Build the `PowerShelfEndpoint` describing this power shelf for component
/// manager power operations. The BMC MAC/IP come from the `bmc_info` carried on
/// the loaded `PowerShelf` model (resolved by the power-shelf load query); BMC
/// credentials are resolved via the credential manager.
pub(super) async fn build_power_shelf_endpoint(
    power_shelf_id: &PowerShelfId,
    state: &PowerShelf,
    credential_manager: &dyn CredentialManager,
) -> Result<PowerShelfEndpoint, String> {
    let bmc_mac = state.bmc_mac_address.ok_or_else(|| {
        format!(
            "power shelf {} has no BMC MAC address recorded",
            power_shelf_id
        )
    })?;

    let pmc_ip = state
        .bmc_info
        .as_ref()
        .and_then(|info| info.ip)
        .ok_or_else(|| {
            format!(
                "no BMC IP found for power shelf {} (bmc_mac {})",
                power_shelf_id, bmc_mac
            )
        })?;
    let credentials = lookup_bmc_credentials(credential_manager, bmc_mac).await?;

    Ok(PowerShelfEndpoint {
        pmc_ip,
        pmc_mac: bmc_mac,
        pmc_vendor: PowerShelfVendor::DEFAULT,
        pmc_credentials: credentials,
    })
}

/// Resolve the per-device BMC root credentials for the given MAC.
///
/// Per-device secrets are the authoritative source of truth for a device's BMC
/// password (every device NICo owns is recorded on the site-wide credential at
/// ingestion and stamped into its own per-MAC secret). There is deliberately no
/// site-wide fallback: a missing per-MAC secret means the device has not been
/// (re)ingested, and silently authenticating with the rotating site-wide
/// credential would mask that and break once the site rotates.
async fn lookup_bmc_credentials(
    credential_manager: &dyn CredentialManager,
    bmc_mac: MacAddress,
) -> Result<Credentials, String> {
    let bmc_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: bmc_mac,
        },
    };
    match credential_manager.get_credentials(&bmc_key).await {
        Ok(Some(creds)) => Ok(creds),
        Ok(None) => Err(format!(
            "no per-device BMC credentials configured for {bmc_mac}; the device must be (re)ingested"
        )),
        Err(error) => Err(format!(
            "failed to read BMC credentials for {bmc_mac}: {error}"
        )),
    }
}

/// Acknowledge the failed request so it does not retry indefinitely. A newer
/// request remains available to the `Error` state's maintenance admission.
async fn finish_maintenance_with_error(
    power_shelf_id: &PowerShelfId,
    request: Option<&PowerShelfMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
    cause: String,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    finish_maintenance(
        power_shelf_id,
        request,
        ctx,
        PowerShelfControllerState::Error { cause },
    )
    .await
}

async fn finish_maintenance(
    power_shelf_id: &PowerShelfId,
    request: Option<&PowerShelfMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
    next_state: PowerShelfControllerState,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let Some(request) = request else {
        // Older saved states cannot identify the request that started this
        // operation. Preserve the pending request, even if that repeats work.
        tracing::warn!(
            %power_shelf_id,
            "Maintenance completed without its original request; preserving pending maintenance"
        );
        return Ok(StateHandlerOutcome::transition(next_state));
    };

    let mut txn = ctx.services.db_pool.begin().await?;
    if let db::ConditionalWrite::NotApplied(reason) =
        db_power_shelf::clear_power_shelf_maintenance_requested(&mut txn, *power_shelf_id, request)
            .await?
    {
        tracing::debug!(
            %power_shelf_id,
            ?reason,
            "Completed maintenance request is no longer pending"
        );
    }
    Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
}
