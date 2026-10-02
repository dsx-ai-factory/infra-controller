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

//! Handler for SwitchControllerState::Maintenance.

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, Credentials,
};
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::switch::SwitchId;
use component_manager::nv_switch_manager::{SwitchComponentResult, SwitchEndpoint};
use db::switch as db_switch;
use mac_address::MacAddress;
use model::component_manager::PowerAction;
use model::switch::{
    ConfigureCertificateState, Switch, SwitchControllerState, SwitchMaintenanceOperation,
    SwitchMaintenanceRequest,
};
use sqlx::PgPool;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::certificate::{
    ConfigureSwitchCertificateMode, ConfigureSwitchCertificatePollOutcome,
    StartConfigureSwitchCertificateResult, poll_configure_switch_certificate_job,
    start_configure_switch_certificate,
};
use crate::context::SwitchStateHandlerContextObjects;

/// Handles the Maintenance state for a switch, dispatching on the requested
/// operation (`PowerOn` / `PowerOff` / `Reset` / `ReconfigureCertificate`).
pub async fn handle_maintenance(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let SwitchControllerState::Maintenance {
        operation,
        request,
        configure_certificate,
    } = &state.controller_state.value
    else {
        unreachable!("handle_maintenance called with non-Maintenance state");
    };
    let request = request.as_ref();

    match operation {
        SwitchMaintenanceOperation::PowerOn => {
            handle_power_on(switch_id, state, request, ctx).await
        }
        SwitchMaintenanceOperation::PowerOff => {
            handle_power_off(switch_id, state, request, ctx).await
        }
        SwitchMaintenanceOperation::Reset => handle_reset(switch_id, state, request, ctx).await,
        SwitchMaintenanceOperation::ReconfigureCertificate => {
            handle_reconfigure_certificate(
                switch_id,
                state,
                request,
                ctx,
                configure_certificate
                    .clone()
                    .unwrap_or(ConfigureCertificateState::Start),
            )
            .await
        }
    }
}

async fn handle_power_on(
    switch_id: &SwitchId,
    state: &Switch,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    tracing::info!(switch_id = %switch_id, "Switch maintenance: PowerOn");
    invoke_power_operation(
        switch_id,
        state,
        request,
        ctx,
        PowerAction::On,
        "PowerOn",
        SwitchControllerState::Ready,
    )
    .await
}

async fn handle_power_off(
    switch_id: &SwitchId,
    state: &Switch,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    tracing::info!(switch_id = %switch_id, "Switch maintenance: PowerOff");
    invoke_power_operation(
        switch_id,
        state,
        request,
        ctx,
        PowerAction::ForceOff,
        "PowerOff",
        SwitchControllerState::Ready,
    )
    .await
}

async fn handle_reset(
    switch_id: &SwitchId,
    state: &Switch,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    tracing::info!(switch_id = %switch_id, "Switch maintenance: Reset");
    invoke_power_operation(
        switch_id,
        state,
        request,
        ctx,
        PowerAction::ForceRestart,
        "Reset",
        SwitchControllerState::Ready,
    )
    .await
}

async fn handle_reconfigure_certificate(
    switch_id: &SwitchId,
    state: &Switch,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    configure_certificate: ConfigureCertificateState,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    match configure_certificate {
        ConfigureCertificateState::Start => {
            handle_reconfigure_certificate_start(switch_id, state, request, ctx).await
        }
        ConfigureCertificateState::WaitForComplete { job_id } => {
            handle_reconfigure_certificate_wait_for_complete(switch_id, request, ctx, &job_id).await
        }
    }
}

async fn handle_reconfigure_certificate_start(
    switch_id: &SwitchId,
    state: &Switch,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    tracing::info!(switch_id = %switch_id, "Switch maintenance: ReconfigureCertificate");
    match start_configure_switch_certificate(
        switch_id,
        state,
        ctx,
        None,
        ConfigureSwitchCertificateMode::Reconfigure,
    )
    .await?
    {
        StartConfigureSwitchCertificateResult::EarlyTransition(outcome) => {
            finish_maintenance_outcome(switch_id, request, ctx, outcome).await
        }
        StartConfigureSwitchCertificateResult::JobStarted(job_id) => Ok(
            StateHandlerOutcome::transition(SwitchControllerState::Maintenance {
                operation: SwitchMaintenanceOperation::ReconfigureCertificate,
                request: request.cloned(),
                configure_certificate: Some(ConfigureCertificateState::WaitForComplete { job_id }),
            }),
        ),
    }
}

async fn handle_reconfigure_certificate_wait_for_complete(
    switch_id: &SwitchId,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    job_id: &str,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    match poll_configure_switch_certificate_job(switch_id, ctx, job_id).await? {
        ConfigureSwitchCertificatePollOutcome::Completed => {
            tracing::info!(
                %job_id,
                switch_id = %switch_id,
                "Switch certificate reconfiguration completed; returning Switch to Ready"
            );
            finish_maintenance(switch_id, request, ctx, SwitchControllerState::Ready).await
        }
        ConfigureSwitchCertificatePollOutcome::Failed(cause) => {
            finish_maintenance_with_error(
                switch_id,
                request,
                ctx,
                format!("Switch {switch_id} maintenance (ReconfigureCertificate): {cause}"),
            )
            .await
        }
        ConfigureSwitchCertificatePollOutcome::InProgress => Ok(StateHandlerOutcome::wait(
            format!("switch certificate reconfiguration job {job_id} in progress"),
        )),
    }
}

async fn finish_maintenance(
    switch_id: &SwitchId,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    next_state: SwitchControllerState,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let Some(request) = request else {
        // Older saved states cannot identify the request that started this
        // operation. Preserve the pending request, even if that repeats work.
        tracing::warn!(
            %switch_id,
            "Maintenance completed without its original request; preserving pending maintenance"
        );
        return Ok(StateHandlerOutcome::transition(next_state));
    };

    let mut txn = ctx.services.db_pool.begin().await?;
    if let db::ConditionalWrite::NotApplied(reason) =
        db_switch::clear_switch_maintenance_requested(&mut txn, *switch_id, request).await?
    {
        tracing::debug!(
            %switch_id,
            ?reason,
            "Completed maintenance request is no longer pending"
        );
    }
    Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
}

async fn finish_maintenance_outcome(
    switch_id: &SwitchId,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    outcome: StateHandlerOutcome<SwitchControllerState>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    match outcome {
        StateHandlerOutcome::Transition { next_state, .. }
            if matches!(next_state, SwitchControllerState::Error { .. }) =>
        {
            let SwitchControllerState::Error { cause } = next_state else {
                unreachable!();
            };
            finish_maintenance_with_error(switch_id, request, ctx, cause).await
        }
        StateHandlerOutcome::Transition {
            next_state: SwitchControllerState::Ready,
            ..
        } => finish_maintenance(switch_id, request, ctx, SwitchControllerState::Ready).await,
        other => Ok(other),
    }
}

async fn invoke_power_operation(
    switch_id: &SwitchId,
    state: &Switch,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    action: PowerAction,
    operation_label: &'static str,
    success_state: SwitchControllerState,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return finish_maintenance_with_error(
            switch_id,
            request,
            ctx,
            format!(
                "Switch {} maintenance ({}): component manager not configured",
                switch_id, operation_label
            ),
        )
        .await;
    };

    let Some(rack_id) = state.rack_id.as_ref() else {
        return finish_maintenance_with_error(
            switch_id,
            request,
            ctx,
            format!(
                "Switch {} maintenance ({}): switch has no rack association",
                switch_id, operation_label
            ),
        )
        .await;
    };

    let endpoint = match build_switch_endpoint(
        switch_id,
        state,
        &ctx.services.db_pool,
        ctx.services.credential_manager.as_ref(),
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(cause) => {
            return finish_maintenance_with_error(
                switch_id,
                request,
                ctx,
                format!(
                    "Switch {} maintenance ({}): {}",
                    switch_id, operation_label, cause
                ),
            )
            .await;
        }
    };

    match component_manager
        .nv_switch
        .power_control(std::slice::from_ref(&endpoint), action)
        .await
    {
        Ok(results) => {
            let result = results.into_iter().next().unwrap_or(SwitchComponentResult {
                bmc_mac: endpoint.bmc_mac,
                success: false,
                error: Some("component manager returned no result".into()),
            });

            if result.success {
                tracing::info!(
                    switch_id = %switch_id,
                    rack_id = %rack_id,
                    operation = operation_label,
                    backend = component_manager.nv_switch.name(),
                    "Switch power control succeeded; returning Switch to Ready"
                );
                return finish_maintenance(switch_id, request, ctx, success_state).await;
            }

            let summary = result
                .error
                .unwrap_or_else(|| "power control failed".into());
            tracing::warn!(
                switch_id = %switch_id,
                rack_id = %rack_id,
                operation = operation_label,
                backend = component_manager.nv_switch.name(),
                summary = %summary,
                "Switch power control returned a non-success result",
            );
            let cause = format!(
                "Switch {} maintenance ({}): power control failed: {}",
                switch_id, operation_label, summary
            );
            finish_maintenance_with_error(switch_id, request, ctx, cause).await
        }
        Err(error) => {
            let cause = format!(
                "Switch {} maintenance ({}): power control failed: {}",
                switch_id, operation_label, error
            );
            tracing::warn!(
                switch_id = %switch_id,
                rack_id = %rack_id,
                operation = operation_label,
                backend = component_manager.nv_switch.name(),
                error = %error,
                "Switch power control transport error",
            );
            finish_maintenance_with_error(switch_id, request, ctx, cause).await
        }
    }
}

pub(super) async fn build_switch_endpoint(
    switch_id: &SwitchId,
    state: &Switch,
    db_pool: &PgPool,
    credential_manager: &dyn CredentialManager,
) -> Result<SwitchEndpoint, String> {
    let bmc_mac = state
        .bmc_mac_address
        .ok_or_else(|| format!("switch {} has no BMC MAC address recorded", switch_id))?;

    let rows = db_switch::find_switch_endpoints_by_ids(db_pool, &[*switch_id])
        .await
        .map_err(|error| format!("failed to look up switch endpoints: {}", error))?;

    let endpoint = rows
        .into_iter()
        .find(|row| row.switch_id == *switch_id)
        .ok_or_else(|| format!("no endpoint info found for switch {}", switch_id))?;

    let (Some(nvos_mac), Some(nvos_ip)) = (endpoint.nvos_mac, endpoint.nvos_ip) else {
        return Err(format!(
            "switch {} is missing NVOS MAC or IP required for power control",
            switch_id
        ));
    };

    let bmc_credentials = lookup_bmc_credentials(credential_manager, bmc_mac).await?;
    let nvos_credentials = lookup_nvos_credentials(credential_manager, bmc_mac).await?;

    Ok(SwitchEndpoint {
        bmc_ip: endpoint.bmc_ip,
        bmc_mac,
        nvos_ip,
        nvos_mac,
        bmc_credentials,
        nvos_credentials,
        nvos_host_name: endpoint.nvos_hostname.none_if_empty(),
    })
}

/// Resolve the per-device BMC root credentials for the given MAC.
///
/// Per-device secrets are authoritative; there is deliberately no site-wide
/// fallback. A missing per-MAC secret means the switch has not been
/// (re)ingested, and falling back to the rotating site-wide credential would
/// mask that and break the moment the site rotates.
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

async fn lookup_nvos_credentials(
    credential_manager: &dyn CredentialManager,
    bmc_mac: MacAddress,
) -> Result<Credentials, String> {
    let key = CredentialKey::SwitchNvosAdmin {
        bmc_mac_address: bmc_mac,
    };
    credential_manager
        .get_credentials(&key)
        .await
        .map_err(|error| format!("failed to read NVOS credentials for {}: {}", bmc_mac, error))?
        .ok_or_else(|| format!("no NVOS admin credentials configured for {}", bmc_mac))
}

async fn finish_maintenance_with_error(
    switch_id: &SwitchId,
    request: Option<&SwitchMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    cause: String,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    finish_maintenance(
        switch_id,
        request,
        ctx,
        SwitchControllerState::Error { cause },
    )
    .await
}
