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

//! Observation-only recovery of fabric metadata on recreated ready switches.

use std::collections::HashSet;
use std::time::Duration;

use carbide_uuid::rack::RackId;
use component_manager::nv_switch_manager::ScaleUpFabricResponseStatus;
use db::{ObjectColumnFilter, TransactionVending as _, rack as db_rack, switch as db_switch};
use model::DeletedFilter;
use model::rack::{Rack, RackState};
use model::switch::{
    CONTROL_PLANE_STATE_CONFIGURED, FabricManagerState, Switch, SwitchControllerState,
    SwitchSearchFilter,
};
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::RackStateHandlerContextObjects;
use crate::fabric_manager::{
    load_nmx_fabric_inventory, observed_primary_switch, persist_fabric_manager_statuses,
    switch_endpoint_from_firmware_device, validate_switch_inventory_for_nmx_cluster,
};

// Each status attempt includes backend connection/permit waits and completes
// within thirty seconds; failures retry on a later rack controller iteration.
const STATUS_READ_TIMEOUT: Duration = Duration::from_secs(30);

fn rack_is_idle(rack: &Rack) -> bool {
    rack.deleted.is_none()
        && matches!(rack.controller_state.value, RackState::Ready)
        && !rack.config.topology_changed
        && !rack.config.reprovision_requested
        && rack.config.maintenance_requested.is_none()
}

fn switches_are_idle(switches: &[Switch]) -> bool {
    !switches.is_empty()
        && switches.iter().all(|switch| {
            switch.deleted.is_none()
                && matches!(switch.controller_state.value, SwitchControllerState::Ready)
                && switch.switch_maintenance_requested.is_none()
                && switch.switch_reprovisioning_requested.is_none()
                && !switch.bmc_credential_rotation_requested
                && !switch.decommission_requested
        })
}

async fn rack_switches(
    rack_id: &RackId,
    pool: &sqlx::PgPool,
) -> Result<Vec<Switch>, StateHandlerError> {
    let ids = db_switch::find_ids(
        pool,
        SwitchSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut switches =
        db_switch::find_by(pool, ObjectColumnFilter::List(db_switch::IdColumn, &ids)).await?;
    switches.sort_by_key(|switch| switch.id);
    Ok(switches)
}

/// Recovers missing observations without configuring the fabric or its devices.
pub(super) async fn reconcile_missing_fabric_metadata(
    rack: &Rack,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<(), StateHandlerError> {
    if !rack_is_idle(rack) {
        return Ok(());
    }
    let Some(manager) = ctx.services.component_manager.as_ref() else {
        return Ok(());
    };
    let before = rack_switches(&rack.id, &ctx.services.db_pool).await?;
    if !switches_are_idle(&before)
        || (before
            .iter()
            .all(|switch| switch.fabric_manager_status.is_some())
            && before.iter().any(|switch| switch.is_primary))
    {
        return Ok(());
    }

    // The configured hardware topology can differ from the running fabric.
    // Only the two RMS observation methods may be used for this recovery.
    let inventory = match load_nmx_fabric_inventory(
        &rack.id,
        "ObserveFabricMetadata",
        &ctx.services.db_pool,
        ctx.services.credential_manager.as_ref(),
    )
    .await
    {
        Ok(inventory) => inventory,
        Err(error) => {
            tracing::warn!(rack_id = %rack.id, %error, "Unable to resolve fabric observations; retrying");
            return Ok(());
        }
    };
    let before_ids: HashSet<_> = before.iter().map(|switch| switch.id).collect();
    if inventory.switch_ids.iter().copied().collect::<HashSet<_>>() != before_ids {
        return Ok(());
    }
    let observations = async {
        validate_switch_inventory_for_nmx_cluster(&inventory.switches)?;
        let endpoints = inventory
            .switches
            .iter()
            .map(switch_endpoint_from_firmware_device)
            .collect::<Result<Vec<_>, _>>()?;
        let status = tokio::time::timeout(
            STATUS_READ_TIMEOUT,
            manager.get_scale_up_fabric_status(&endpoints),
        )
        .await
        .map_err(|_| "Fabric status read timed out".to_string())?
        .map_err(|error| error.to_string())?;
        let primary = observed_primary_switch(&inventory.switches, &status)?;
        let Some(observed) = status.switches.as_ref() else {
            return Err("Fabric status has no switch observations".to_string());
        };
        let expected: HashSet<_> = inventory
            .switches
            .iter()
            .map(|switch| switch.node_id.as_str())
            .collect();
        if observed.len() != expected.len()
            || observed
                .iter()
                .map(|switch| switch.node_id.as_str())
                .collect::<HashSet<_>>()
                != expected
        {
            return Err(
                "Fabric status does not cover exactly the current rack switches".to_string(),
            );
        }
        let services = tokio::time::timeout(
            STATUS_READ_TIMEOUT,
            manager.batch_get_scale_up_fabric_service_status(&endpoints),
        )
        .await
        .map_err(|_| "Fabric service status read timed out".to_string())?
        .map_err(|error| error.to_string())?;
        if services.status != ScaleUpFabricResponseStatus::Success
            || services
                .service_statuses
                .keys()
                .map(String::as_str)
                .collect::<HashSet<_>>()
                != expected
        {
            return Err(
                "Fabric service status does not cover exactly the current rack switches"
                    .to_string(),
            );
        }
        let primary_status = &services.service_statuses[&primary.to_string()];
        if primary_status.fabric_manager_state != FabricManagerState::Ok
            || primary_status.addition_info.as_deref() != Some(CONTROL_PLANE_STATE_CONFIGURED)
            || primary_status
                .error_message
                .as_deref()
                .is_some_and(|error| !error.is_empty())
        {
            return Err("Observed primary Fabric Manager is not configured".to_string());
        }
        Ok((primary, services, endpoints))
    }
    .await;
    let (primary, services, endpoints) = match observations {
        Ok(observations) => observations,
        Err(error) => {
            tracing::warn!(rack_id = %rack.id, %error, "Unable to verify fabric observations; retrying");
            return Ok(());
        }
    };

    // External reads hold no transaction. Revalidate current lifecycle and
    // ownership under row locks before publishing the paired observations.
    let mut txn = ctx
        .services
        .db_pool
        .txn_begin()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    if !db_rack::lock_for_update(txn.as_mut(), &rack.id).await? {
        return Ok(());
    }
    let current_rack = db_rack::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db_rack::IdColumn, &rack.id),
    )
    .await?;
    let Some(current_rack) = current_rack.first() else {
        return Ok(());
    };
    if !rack_is_idle(current_rack)
        || current_rack.controller_state.version != rack.controller_state.version
    {
        return Ok(());
    }
    let ids = db_switch::lock_ids_for_rack(txn.as_mut(), &rack.id).await?;
    let mut current = db_switch::find_by(
        txn.as_mut(),
        ObjectColumnFilter::List(db_switch::IdColumn, &ids),
    )
    .await?;
    current.sort_by_key(|switch| switch.id);
    if !switches_are_idle(&current)
        || current.len() != before.len()
        || !current.iter().zip(&before).all(|(current, before)| {
            current.id == before.id
                && current.rack_id == before.rack_id
                && current.bmc_mac_address == before.bmc_mac_address
                && current.controller_state.version == before.controller_state.version
                && current.version == before.version
                && current.is_primary == before.is_primary
                && current.fabric_manager_status == before.fabric_manager_status
        })
    {
        return Ok(());
    }
    let current_endpoints = db_switch::find_switch_endpoints_by_ids(txn.as_mut(), &ids).await?;
    if current_endpoints.len() != endpoints.len()
        || !endpoints.iter().all(|endpoint| {
            current_endpoints.iter().any(|current| {
                current.bmc_mac == endpoint.bmc_mac
                    && current.bmc_ip == endpoint.bmc_ip
                    && current.nvos_mac == Some(endpoint.nvos_mac)
                    && current.nvos_ip == Some(endpoint.nvos_ip)
                    && current
                        .nvos_hostname
                        .as_ref()
                        .filter(|value| !value.is_empty())
                        == endpoint.nvos_host_name.as_ref()
            })
        })
    {
        return Ok(());
    }
    persist_fabric_manager_statuses(txn.as_mut(), &rack.id, &inventory.switches, &services)
        .await
        .map_err(|error| StateHandlerError::GenericError(eyre::eyre!(error)))?;
    if !db_switch::set_primary_switch_for_observed_ids(txn.as_mut(), &rack.id, &ids, &primary)
        .await?
    {
        return Ok(());
    }
    let final_ids = db_switch::find_ids(
        txn.as_mut(),
        SwitchSearchFilter {
            rack_id: Some(rack.id.clone()),
            deleted: DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;
    if final_ids.into_iter().collect::<HashSet<_>>() != before_ids {
        return Ok(());
    }
    txn.commit()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    tracing::info!(rack_id = %rack.id, switch_id = %primary, "Recovered missing fabric observations");
    Ok(())
}
