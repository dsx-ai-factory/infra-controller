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

//! Managed power-shelf decommissioning handlers.

use carbide_credential_rotation::BmcEndpoint;
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, CredentialWriter};
use carbide_utils::redfish::BmcAccessInfo;
use carbide_uuid::power_shelf::PowerShelfId;
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression};
use model::power_shelf::{PowerShelf, PowerShelfControllerState, PowerShelfDecommissioningState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::PowerShelfStateHandlerContextObjects;

pub(super) async fn handle_decommissioning(
    power_shelf_id: &PowerShelfId,
    power_shelf: &PowerShelf,
    decommissioning_state: &PowerShelfDecommissioningState,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    match decommissioning_state {
        PowerShelfDecommissioningState::SuppressingSiteExplorer => {
            handle_suppressing_site_explorer(power_shelf_id, power_shelf, ctx).await
        }
        PowerShelfDecommissioningState::SuppressingBmcDhcp => {
            handle_suppressing_bmc_dhcp(power_shelf_id, power_shelf, ctx).await
        }
        PowerShelfDecommissioningState::FactoryResetBmc => {
            handle_factory_reset_bmc(power_shelf_id, power_shelf, ctx).await
        }
        PowerShelfDecommissioningState::WaitingForBmcDhcpAcknowledgement => {
            handle_waiting_for_bmc_dhcp_acknowledgement(power_shelf_id, power_shelf, ctx).await
        }
        PowerShelfDecommissioningState::DeletingManagedCredentials => {
            handle_deleting_managed_credentials(power_shelf_id, power_shelf, ctx).await
        }
        PowerShelfDecommissioningState::Decommissioned => Ok(StateHandlerOutcome::do_nothing()),
    }
}

async fn handle_suppressing_site_explorer(
    power_shelf_id: &PowerShelfId,
    power_shelf: &PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let bmc_mac = power_shelf
        .bmc_info
        .as_ref()
        .and_then(|info| info.mac)
        .or(power_shelf.bmc_mac_address)
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: power_shelf_id.to_string(),
            missing: "bmc_mac",
        })?;
    let mut txn = ctx.services.db_pool.begin().await?;
    let suppression = db::bmc_suppression::upsert(
        &mut txn,
        &NewBmcSuppression {
            bmc_mac_address: bmc_mac,
            subsystem: BmcSuppressionSubsystem::SiteExplorer,
            source: BmcSuppressionSource::Decommissioning,
            reason: format!("power shelf {power_shelf_id} is being decommissioned"),
        },
    )
    .await?;

    let outcome = if suppression.acknowledged_at.is_some() {
        StateHandlerOutcome::transition(PowerShelfControllerState::Decommissioning {
            decommissioning_state: PowerShelfDecommissioningState::SuppressingBmcDhcp,
        })
    } else {
        StateHandlerOutcome::wait(
            "waiting for Site Explorer suppression acknowledgement".to_string(),
        )
    };
    Ok(outcome.with_txn(txn))
}

async fn handle_suppressing_bmc_dhcp(
    power_shelf_id: &PowerShelfId,
    power_shelf: &PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let bmc_mac = power_shelf
        .bmc_info
        .as_ref()
        .and_then(|info| info.mac)
        .or(power_shelf.bmc_mac_address)
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: power_shelf_id.to_string(),
            missing: "bmc_mac",
        })?;
    let mut txn = ctx.services.db_pool.begin().await?;
    let suppression = db::bmc_suppression::upsert(
        &mut txn,
        &NewBmcSuppression {
            bmc_mac_address: bmc_mac,
            subsystem: BmcSuppressionSubsystem::Dhcp,
            source: BmcSuppressionSource::Decommissioning,
            reason: format!(
                "power shelf {power_shelf_id} is being decommissioned; suppressing BMC DHCP"
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

    Ok(
        StateHandlerOutcome::transition(PowerShelfControllerState::Decommissioning {
            decommissioning_state: PowerShelfDecommissioningState::FactoryResetBmc,
        })
        .with_txn(txn),
    )
}

async fn handle_factory_reset_bmc(
    power_shelf_id: &PowerShelfId,
    power_shelf: &PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let endpoint = BmcEndpoint::from_power_shelf(power_shelf).ok_or_else(|| {
        StateHandlerError::MissingData {
            object_id: power_shelf_id.to_string(),
            missing: "addressable_bmc",
        }
    })?;
    let access = BmcAccessInfo {
        host: endpoint.host,
        port: endpoint.port,
        mac_address: endpoint.device_mac,
    };
    ctx.services
        .redfish_client_pool
        .client_by_info(&access)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to create redfish client for power shelf {power_shelf_id}: {error}"
            ))
        })?
        .bmc_reset_to_defaults()
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to factory reset BMC for power shelf {power_shelf_id}: {error}"
            ))
        })?;

    Ok(StateHandlerOutcome::transition(
        PowerShelfControllerState::Decommissioning {
            decommissioning_state: PowerShelfDecommissioningState::WaitingForBmcDhcpAcknowledgement,
        },
    ))
}

async fn handle_waiting_for_bmc_dhcp_acknowledgement(
    power_shelf_id: &PowerShelfId,
    power_shelf: &PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let bmc_mac = power_shelf
        .bmc_info
        .as_ref()
        .and_then(|info| info.mac)
        .or(power_shelf.bmc_mac_address)
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: power_shelf_id.to_string(),
            missing: "bmc_mac",
        })?;
    let suppression = db::bmc_suppression::find(
        &ctx.services.db_pool,
        bmc_mac,
        BmcSuppressionSubsystem::Dhcp,
        BmcSuppressionSource::Decommissioning,
    )
    .await?;

    if suppression.is_some_and(|suppression| suppression.acknowledged_at.is_some()) {
        Ok(StateHandlerOutcome::transition(
            PowerShelfControllerState::Decommissioning {
                decommissioning_state: PowerShelfDecommissioningState::DeletingManagedCredentials,
            },
        ))
    } else {
        Ok(StateHandlerOutcome::wait(
            "waiting for BMC DHCP suppression acknowledgement".to_string(),
        ))
    }
}

async fn handle_deleting_managed_credentials(
    power_shelf_id: &PowerShelfId,
    power_shelf: &PowerShelf,
    ctx: &mut StateHandlerContext<'_, PowerShelfStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<PowerShelfControllerState>, StateHandlerError> {
    let bmc_mac = power_shelf
        .bmc_info
        .as_ref()
        .and_then(|info| info.mac)
        .or(power_shelf.bmc_mac_address)
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: power_shelf_id.to_string(),
            missing: "bmc_mac",
        })?;
    let credential_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: bmc_mac,
        },
    };

    ctx.services
        .credential_manager
        .delete_credentials(&credential_key)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to delete managed BMC credentials for power shelf {power_shelf_id}: {error}"
            ))
        })?;

    let mut txn = ctx.services.db_pool.begin().await?;
    db::credential_rotation::delete_device_converged(
        &mut txn,
        bmc_mac,
        db::credential_rotation::CredentialRotationType::Bmc,
    )
    .await?;

    Ok(
        StateHandlerOutcome::transition(PowerShelfControllerState::Decommissioning {
            decommissioning_state: PowerShelfDecommissioningState::Decommissioned,
        })
        .with_txn(txn),
    )
}
