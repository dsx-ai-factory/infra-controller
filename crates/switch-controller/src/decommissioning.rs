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

//! Managed-switch decommissioning.

use carbide_redfish::libredfish::RedfishAuth;
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, CredentialWriter};
use carbide_uuid::switch::SwitchId;
use libredfish::model::service_root::RedfishVendor;
use mac_address::MacAddress;
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression};
use model::switch::{Switch, SwitchControllerState, SwitchDecommissioningState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;
use crate::endpoint::resolve_switch_endpoint;

fn decommissioning(state: SwitchDecommissioningState) -> SwitchControllerState {
    SwitchControllerState::Decommissioning {
        decommissioning_state: state,
    }
}

fn missing_data(switch_id: &SwitchId, missing: &'static str) -> StateHandlerError {
    StateHandlerError::MissingData {
        object_id: switch_id.to_string(),
        missing,
    }
}

fn external_error(operation: &str, error: impl std::fmt::Display) -> StateHandlerError {
    StateHandlerError::GenericError(eyre::eyre!("{operation}: {error}"))
}

async fn suppress_dhcp(
    switch_id: &SwitchId,
    mac_address: MacAddress,
    interface: &str,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<sqlx::PgTransaction<'static>, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;
    let suppression = db::bmc_suppression::upsert(
        &mut txn,
        &NewBmcSuppression {
            bmc_mac_address: mac_address,
            subsystem: BmcSuppressionSubsystem::Dhcp,
            source: BmcSuppressionSource::Decommissioning,
            reason: format!(
                "managed switch {switch_id} is being decommissioned; suppressing {interface} DHCP"
            ),
        },
    )
    .await?;

    // this does not actually delete the interface yet,
    // we use this as a signal to the DHCP server to reload its configuration.
    let last_invalidation_time = db::dhcp_record::last_invalidation_time(&mut txn).await?;
    if last_invalidation_time < suppression.requested_at {
        db::machine_interface::record_deletion(&mut txn).await?;
    }

    Ok(txn)
}

async fn dhcp_suppression_acknowledged(
    mac_address: MacAddress,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<bool, StateHandlerError> {
    Ok(db::bmc_suppression::find(
        &ctx.services.db_pool,
        mac_address,
        BmcSuppressionSubsystem::Dhcp,
        BmcSuppressionSource::Decommissioning,
    )
    .await?
    .is_some_and(|suppression| suppression.acknowledged_at.is_some()))
}

pub(super) async fn handle_decommissioning(
    switch_id: &SwitchId,
    switch: &Switch,
    decommissioning_state: &SwitchDecommissioningState,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    match decommissioning_state {
        SwitchDecommissioningState::SuppressingSiteExplorer => {
            handle_suppressing_site_explorer(switch_id, switch, ctx).await
        }
        SwitchDecommissioningState::SuppressingNvosDhcp => {
            handle_suppressing_nvos_dhcp(switch_id, ctx).await
        }
        SwitchDecommissioningState::FactoryResetNvos => {
            handle_factory_reset_nvos(switch_id, ctx).await
        }
        SwitchDecommissioningState::WaitingForNvosDhcpAcknowledgement => {
            handle_waiting_for_nvos_dhcp_acknowledgement(switch_id, ctx).await
        }
        SwitchDecommissioningState::SuppressingBmcDhcp => {
            handle_suppressing_bmc_dhcp(switch_id, switch, ctx).await
        }
        SwitchDecommissioningState::FactoryResetBmc => {
            handle_factory_reset_bmc(switch_id, switch, ctx).await
        }
        SwitchDecommissioningState::WaitingForBmcDhcpAcknowledgement => {
            handle_waiting_for_bmc_dhcp_acknowledgement(switch_id, switch, ctx).await
        }
        SwitchDecommissioningState::DeletingManagedCredentials => {
            handle_deleting_managed_credentials(switch_id, switch, ctx).await
        }
        SwitchDecommissioningState::Decommissioned => Ok(StateHandlerOutcome::do_nothing()),
    }
}

async fn handle_suppressing_site_explorer(
    switch_id: &SwitchId,
    switch: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let bmc_mac = switch
        .bmc_info
        .as_ref()
        .and_then(|info| info.mac)
        .or(switch.bmc_mac_address)
        .ok_or_else(|| missing_data(switch_id, "bmc_mac"))?;
    let mut txn = ctx.services.db_pool.begin().await?;
    let suppression = db::bmc_suppression::upsert(
        &mut txn,
        &NewBmcSuppression {
            bmc_mac_address: bmc_mac,
            subsystem: BmcSuppressionSubsystem::SiteExplorer,
            source: BmcSuppressionSource::Decommissioning,
            reason: format!("managed switch {switch_id} is being decommissioned"),
        },
    )
    .await?;

    let outcome = if suppression.acknowledged_at.is_some() {
        StateHandlerOutcome::transition(decommissioning(
            SwitchDecommissioningState::SuppressingNvosDhcp,
        ))
    } else {
        StateHandlerOutcome::wait(
            "waiting for Site Explorer suppression acknowledgement".to_string(),
        )
    };
    Ok(outcome.with_txn(txn))
}

async fn handle_suppressing_nvos_dhcp(
    switch_id: &SwitchId,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let endpoint = resolve_switch_endpoint(
        switch_id,
        &ctx.services.db_pool,
        &ctx.services.credential_manager,
    )
    .await?;
    let txn = suppress_dhcp(switch_id, endpoint.nvos_mac, "NVOS", ctx).await?;
    Ok(StateHandlerOutcome::transition(decommissioning(
        SwitchDecommissioningState::FactoryResetNvos,
    ))
    .with_txn(txn))
}

async fn handle_factory_reset_nvos(
    switch_id: &SwitchId,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let component_manager = ctx.services.component_manager.clone().ok_or_else(|| {
        StateHandlerError::InvalidState(format!(
            "switch {switch_id} requires the RMS component-manager backend for decommissioning"
        ))
    })?;

    let endpoint = resolve_switch_endpoint(
        switch_id,
        &ctx.services.db_pool,
        &ctx.services.credential_manager,
    )
    .await?;
    let tls_server_domain = endpoint.nvos_host_name.clone();
    component_manager
        .nv_switch
        .batch_reset_switch_factory_default(&[endpoint], tls_server_domain.as_deref())
        .await
        .map_err(|error| external_error("failed to submit NVOS factory reset", error))?;
    Ok(StateHandlerOutcome::transition(decommissioning(
        SwitchDecommissioningState::WaitingForNvosDhcpAcknowledgement,
    )))
}

async fn handle_waiting_for_nvos_dhcp_acknowledgement(
    switch_id: &SwitchId,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let rows = db::switch::find_switch_endpoints_by_ids(
        &ctx.services.db_pool,
        std::slice::from_ref(switch_id),
    )
    .await?;
    let nvos_mac = rows
        .into_iter()
        .next()
        .and_then(|row| row.nvos_mac)
        .ok_or_else(|| missing_data(switch_id, "nvos_mac"))?;
    if !dhcp_suppression_acknowledged(nvos_mac, ctx).await? {
        return Ok(StateHandlerOutcome::wait(
            "waiting for NVOS DHCP suppression acknowledgement".to_string(),
        ));
    }

    Ok(StateHandlerOutcome::transition(decommissioning(
        SwitchDecommissioningState::SuppressingBmcDhcp,
    )))
}

async fn handle_suppressing_bmc_dhcp(
    switch_id: &SwitchId,
    switch: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let bmc_mac_address = switch
        .bmc_info
        .as_ref()
        .and_then(|bmc_info| bmc_info.mac)
        .or(switch.bmc_mac_address)
        .ok_or_else(|| missing_data(switch_id, "bmc_mac"))?;
    let txn = suppress_dhcp(switch_id, bmc_mac_address, "BMC", ctx).await?;
    Ok(StateHandlerOutcome::transition(decommissioning(
        SwitchDecommissioningState::FactoryResetBmc,
    ))
    .with_txn(txn))
}

async fn handle_factory_reset_bmc(
    switch_id: &SwitchId,
    switch: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let bmc_info = switch
        .bmc_info
        .as_ref()
        .ok_or_else(|| missing_data(switch_id, "bmc_info"))?;
    let bmc_ip_address = bmc_info
        .ip
        .ok_or_else(|| missing_data(switch_id, "bmc_ip"))?;
    let bmc_mac_address = bmc_info
        .mac
        .ok_or_else(|| missing_data(switch_id, "bmc_mac"))?;

    let redfish_client = ctx
        .services
        .redfish_client_pool
        .create_client(
            &bmc_ip_address.to_string(),
            bmc_info.port,
            RedfishAuth::for_bmc_mac(bmc_mac_address),
            Some(RedfishVendor::NvidiaGBSwitch),
        )
        .await
        .map_err(|error| external_error("failed to create switch BMC Redfish client", error))?;
    redfish_client
        .bmc_reset_to_defaults()
        .await
        .map_err(|error| external_error("failed to factory reset switch BMC", error))?;

    Ok(StateHandlerOutcome::transition(decommissioning(
        SwitchDecommissioningState::WaitingForBmcDhcpAcknowledgement,
    )))
}

async fn handle_waiting_for_bmc_dhcp_acknowledgement(
    switch_id: &SwitchId,
    switch: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let bmc_mac_address = switch
        .bmc_info
        .as_ref()
        .and_then(|bmc_info| bmc_info.mac)
        .or(switch.bmc_mac_address)
        .ok_or_else(|| missing_data(switch_id, "bmc_mac"))?;
    if !dhcp_suppression_acknowledged(bmc_mac_address, ctx).await? {
        return Ok(StateHandlerOutcome::wait(
            "waiting for BMC DHCP suppression acknowledgement".to_string(),
        ));
    }

    Ok(StateHandlerOutcome::transition(decommissioning(
        SwitchDecommissioningState::DeletingManagedCredentials,
    )))
}

async fn handle_deleting_managed_credentials(
    switch_id: &SwitchId,
    switch: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let bmc_mac_address = switch
        .bmc_info
        .as_ref()
        .and_then(|info| info.mac)
        .or(switch.bmc_mac_address)
        .ok_or_else(|| missing_data(switch_id, "bmc_mac"))?;

    ctx.services
        .credential_manager
        .delete_credentials(&CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
        })
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to delete managed BMC credentials for switch {switch_id}: {error}"
            ))
        })?;
    ctx.services
        .credential_manager
        .delete_credentials(&CredentialKey::SwitchNvosAdmin { bmc_mac_address })
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to delete managed NVOS credentials for switch {switch_id}: {error}"
            ))
        })?;

    let mut txn = ctx.services.db_pool.begin().await?;
    db::credential_rotation::delete_device_converged(
        &mut txn,
        bmc_mac_address,
        db::credential_rotation::CredentialRotationType::Bmc,
    )
    .await?;
    db::credential_rotation::delete_device_converged(
        &mut txn,
        bmc_mac_address,
        db::credential_rotation::CredentialRotationType::Nvos,
    )
    .await?;

    Ok(
        StateHandlerOutcome::transition(decommissioning(
            SwitchDecommissioningState::Decommissioned,
        ))
        .with_txn(txn),
    )
}
