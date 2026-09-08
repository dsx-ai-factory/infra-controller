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

//! Handler for RackState::Maintenance.

use carbide_instrument::{Event, emit};
use carbide_rack::firmware_object::{
    ANY_RACK_HARDWARE_TYPE, profile_hardware_type_wire_value, rack_maintenance_access_token_key,
    rms_access_token_or_noauth,
};
use carbide_rack::firmware_update::{
    RackFirmwareInventory, RackSwitchFirmwareInventory, build_new_node_info,
    firmware_type_for_profile, load_rack_firmware_inventory, load_rack_switch_firmware_inventory,
};
use carbide_rack::rack_manager_error;
use carbide_rack::rms_node_type::{
    compute_node_identity_for_profile, firmware_object_component_filters_for_node_identities,
    switch_node_identity_for_profile,
};
use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_rack_controller::fabric_manager::{
    observed_primary_switch, persist_fabric_manager_statuses, persist_primary_switch,
    switch_endpoint_from_firmware_device, validate_switch_inventory_for_nmx_cluster,
};
use carbide_rack_controller::validating::strip_rv_labels;
use carbide_secrets::credentials::{CredentialManager, Credentials};
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::NvosUpdateRequest;
use component_manager::component_manager::ComponentManager;
use component_manager::error::ComponentManagerError;
use component_manager::nv_switch_manager::ScaleUpFabricManagerJobStatus;
use db::{
    host_machine_update as db_host_machine_update, machine as db_machine,
    machine_topology as db_machine_topology, power_options as db_power_options,
    power_shelf as db_power_shelf, rack as db_rack, switch as db_switch,
};
use librms::protos::rack_manager as rms;
use model::rack::{
    ConfigureNmxClusterState, FirmwareUpgradeDeviceInfo, FirmwareUpgradeDeviceStatus,
    FirmwareUpgradeState, MaintenanceActivity, MaintenanceScope, NvosUpdateState,
    NvosUpdateSwitchStatus, Rack, RackFirmwareUpgradeState, RackFirmwareUpgradeStatus,
    RackMaintenanceState, RackPowerState, RackState, RackValidationState, SwitchNvosUpdateState,
    SwitchNvosUpdateStatus,
};
use model::rack_type::RackProfile;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate as carbide_rack_controller;

/// Strips all `rv.*` metadata labels from every machine in the rack.
///
/// Called on `Maintenance(Completed)` to ensure machines enter the next
/// validation cycle with a clean slate. RVS is expected to re-populate these
/// labels when it starts a new run.
async fn clear_rv_labels(
    rack: &Rack,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<(), StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;

    let machines = super::get_machines_from_rack(rack, &mut txn).await?;

    for machine in machines.into_iter() {
        let mut metadata = machine.metadata;
        let id = machine.id;
        let ver = machine.version;

        if strip_rv_labels(&mut metadata) {
            db_machine::update_metadata(&mut txn, &id, ver, metadata).await?;
        }
    }

    txn.commit().await?;
    Ok(())
}

fn rack_maintenance_initiator(rack_id: &RackId) -> String {
    format!("rack-{rack_id}")
}

async fn trigger_rack_firmware_reprovisioning_requests(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    machine_ids: &[carbide_uuid::machine::MachineId],
    switch_ids: &[carbide_uuid::switch::SwitchId],
    power_shelf_ids: &[carbide_uuid::power_shelf::PowerShelfId],
    activities: &[MaintenanceActivity],
) -> Result<(), StateHandlerError> {
    let initiator = rack_maintenance_initiator(rack_id);
    for machine_id in machine_ids {
        db_host_machine_update::trigger_host_reprovisioning_request(txn, &initiator, machine_id)
            .await?;
    }
    for switch_id in switch_ids {
        db_switch::set_switch_reprovisioning_requested(
            txn,
            *switch_id,
            &initiator,
            activities.to_vec(),
        )
        .await?;
    }
    for power_shelf_id in power_shelf_ids {
        db_power_shelf::set_power_shelf_reprovisioning_requested(
            txn,
            *power_shelf_id,
            &initiator,
            activities.to_vec(),
        )
        .await?;
    }
    Ok(())
}

async fn clear_rack_firmware_device_statuses(
    txn: &mut sqlx::PgConnection,
    machine_ids: &[carbide_uuid::machine::MachineId],
    switch_ids: &[carbide_uuid::switch::SwitchId],
    power_shelf_ids: &[carbide_uuid::power_shelf::PowerShelfId],
) -> Result<(), StateHandlerError> {
    for machine_id in machine_ids {
        db_machine::update_rack_fw_details(txn, machine_id, None).await?;
    }
    for switch_id in switch_ids {
        db_switch::update_firmware_upgrade_status(txn, *switch_id, None).await?;
    }
    for power_shelf_id in power_shelf_ids {
        db_power_shelf::update_firmware_upgrade_status(txn, *power_shelf_id, None).await?;
    }
    Ok(())
}

async fn clear_nvos_update_statuses(
    txn: &mut sqlx::PgConnection,
    switch_ids: &[carbide_uuid::switch::SwitchId],
) -> Result<(), StateHandlerError> {
    for switch_id in switch_ids {
        db_switch::update_nvos_update_status(txn, *switch_id, None).await?;
    }
    Ok(())
}

#[derive(Debug)]
struct RackMaintenanceTargetIds {
    machine_ids: Vec<carbide_uuid::machine::MachineId>,
    switch_ids: Vec<carbide_uuid::switch::SwitchId>,
    power_shelf_ids: Vec<carbide_uuid::power_shelf::PowerShelfId>,
}

async fn load_rack_maintenance_target_ids(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    scope: &MaintenanceScope,
) -> Result<RackMaintenanceTargetIds, StateHandlerError> {
    let machine_ids = load_scoped_machines(txn, rack_id, scope)
        .await?
        .into_iter()
        .map(|machine| machine.id)
        .collect();

    let mut switch_ids = db_switch::find_ids(
        &mut *txn,
        model::switch::SwitchSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: model::DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;
    if !scope.is_full_rack() {
        let requested: std::collections::HashSet<_> = scope.switch_ids.iter().collect();
        switch_ids.retain(|id| requested.contains(id));
    }

    let mut power_shelf_ids = db_power_shelf::find_ids(
        &mut *txn,
        model::power_shelf::PowerShelfSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: model::DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;
    power_shelf_ids = filter_power_shelf_ids_by_scope(power_shelf_ids, scope);

    Ok(RackMaintenanceTargetIds {
        machine_ids,
        switch_ids,
        power_shelf_ids,
    })
}

/// Unwinds active rack maintenance and transitions the rack to `Error`.
///
/// Requests that have not left the device `Ready` state are cleared here.
/// Device controllers that already started reprovisioning retain ownership of
/// their request and unwind after observing the rack's atomic transition to
/// `Error`.
async fn terminate_active_rack_maintenance(
    rack_id: &RackId,
    state: &mut Rack,
    maintenance_state: &RackMaintenanceState,
    cause: impl Into<String>,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    let cause = cause.into();
    let scope = state
        .config
        .maintenance_requested
        .clone()
        .unwrap_or_default();
    let mut txn = ctx.services.db_pool.begin().await?;
    let targets = load_rack_maintenance_target_ids(txn.as_mut(), rack_id, &scope).await?;
    let initiator = rack_maintenance_initiator(rack_id);

    for machine_id in &targets.machine_ids {
        db_host_machine_update::clear_ready_host_reprovisioning_request(
            txn.as_mut(),
            machine_id,
            &initiator,
        )
        .await?;
    }
    for switch_id in &targets.switch_ids {
        db_switch::clear_ready_switch_reprovisioning_requested(
            txn.as_mut(),
            *switch_id,
            &initiator,
        )
        .await?;
    }
    for power_shelf_id in &targets.power_shelf_ids {
        db_power_shelf::clear_ready_power_shelf_reprovisioning_requested(
            txn.as_mut(),
            *power_shelf_id,
            &initiator,
        )
        .await?;
    }

    let now = chrono::Utc::now();
    match maintenance_state {
        RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade,
        } => {
            clear_rack_firmware_device_statuses(
                txn.as_mut(),
                &targets.machine_ids,
                &targets.switch_ids,
                &targets.power_shelf_ids,
            )
            .await?;
            if matches!(rack_firmware_upgrade, FirmwareUpgradeState::WaitForComplete)
                && let Some(mut job) = state.firmware_upgrade_job.clone()
            {
                job.status = Some("failed".into());
                job.completed_at.get_or_insert(now);
                db_rack::update_firmware_upgrade_job(txn.as_mut(), rack_id, Some(&job)).await?;
                state.firmware_upgrade_job = Some(job);
            }
        }
        RackMaintenanceState::NVOSUpdate { nvos_update } => {
            clear_nvos_update_statuses(txn.as_mut(), &targets.switch_ids).await?;
            if matches!(nvos_update, NvosUpdateState::WaitForComplete)
                && let Some(mut job) = state.nvos_update_job.clone()
            {
                job.status = Some("failed".into());
                job.completed_at.get_or_insert(now);
                db_rack::update_nvos_update_job(txn.as_mut(), rack_id, Some(&job)).await?;
                state.nvos_update_job = Some(job);
            }
        }
        RackMaintenanceState::ConfigureNmxCluster { .. }
        | RackMaintenanceState::PowerSequence { .. }
        | RackMaintenanceState::Completed => {}
    }

    state.config.maintenance_requested = None;
    state.config.maintenance_termination_requested = false;
    db_rack::consume_maintenance_termination_request(txn.as_mut(), rack_id).await?;

    tracing::warn!(
        rack_id = %rack_id,
        %cause,
        machine_count = targets.machine_ids.len(),
        switch_count = targets.switch_ids.len(),
        power_shelf_count = targets.power_shelf_ids.len(),
        "Terminating rack maintenance",
    );

    Ok(StateHandlerOutcome::transition(RackState::Error { cause }).with_txn(txn))
}

/// Aggregated firmware progress for machines, switches, and power shelves
/// participating in a rack firmware job. Advancement out of `WaitForComplete`
/// is based on device controller states, not RMS job strings or firmware status.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeviceFirmwareProgress {
    Waiting {
        pending: usize,
        total: usize,
        completed: usize,
        failed: usize,
    },
    Failed {
        failed: usize,
        total: usize,
    },
    Completed {
        completed: usize,
        total: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceFirmwareOutcome {
    Waiting,
    Failed,
    Completed,
}

async fn desired_off_machine_ids(
    txn: &mut sqlx::PgConnection,
    machine_ids: &[carbide_uuid::machine::MachineId],
) -> Result<Vec<carbide_uuid::machine::MachineId>, StateHandlerError> {
    if machine_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut machine_ids = db_power_options::get_by_ids(machine_ids, txn)
        .await?
        .into_iter()
        .filter(|options| options.desired_power_state == model::power_manager::PowerState::Off)
        .map(|options| options.host_id.into())
        .collect::<Vec<_>>();
    machine_ids.sort_by_key(ToString::to_string);
    Ok(machine_ids)
}

fn format_machine_ids(machine_ids: &[carbide_uuid::machine::MachineId]) -> String {
    machine_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

async fn load_scoped_machines(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    scope: &MaintenanceScope,
) -> Result<Vec<model::machine::Machine>, StateHandlerError> {
    let machine_ids = db_machine::find_machine_ids(
        &mut *txn,
        model::machine::machine_search_config::MachineSearchConfig {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        },
    )
    .await?;
    let machines = if machine_ids.is_empty() {
        Vec::new()
    } else {
        db_machine::find(
            &mut *txn,
            db::ObjectFilter::List(&machine_ids),
            model::machine::machine_search_config::MachineSearchConfig::default(),
        )
        .await?
    };
    Ok(filter_machines_by_scope(machines, scope))
}

async fn power_blocked_rack_firmware_machine_ids(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    scope: &MaintenanceScope,
) -> Result<Vec<carbide_uuid::machine::MachineId>, StateHandlerError> {
    let machines = load_scoped_machines(txn, rack_id, scope).await?;
    let initiator = rack_maintenance_initiator(rack_id);
    let ready_rack_requested_ids = machines
        .iter()
        .filter(|machine| matches!(machine.state.value, model::machine::ManagedHostState::Ready))
        .filter(|machine| {
            machine
                .host_reprovision_requested
                .as_ref()
                .is_some_and(|request| request.initiator == initiator)
        })
        .map(|machine| machine.id)
        .collect::<Vec<_>>();
    desired_off_machine_ids(txn, &ready_rack_requested_ids).await
}

async fn resolve_machine_id_for_firmware_device(
    txn: &mut sqlx::PgConnection,
    device: &FirmwareUpgradeDeviceStatus,
) -> Result<Option<carbide_uuid::machine::MachineId>, StateHandlerError> {
    if !device.node_id.is_empty() {
        return Ok(device
            .node_id
            .parse::<carbide_uuid::machine::MachineId>()
            .ok());
    }
    let mac: mac_address::MacAddress = match device.mac.parse() {
        Ok(mac) => mac,
        Err(_) => return Ok(None),
    };
    Ok(db_machine_topology::find_machine_id_by_bmc_mac(txn, mac).await?)
}

async fn resolve_switch_id_for_firmware_device(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    device: &FirmwareUpgradeDeviceStatus,
) -> Result<Option<carbide_uuid::switch::SwitchId>, StateHandlerError> {
    if !device.node_id.is_empty() {
        return Ok(device
            .node_id
            .parse::<carbide_uuid::switch::SwitchId>()
            .ok());
    }
    let mac: mac_address::MacAddress = match device.mac.parse() {
        Ok(mac) => mac,
        Err(_) => return Ok(None),
    };
    Ok(db_switch::find_ids(
        txn,
        model::switch::SwitchSearchFilter {
            bmc_mac: Some(mac),
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        },
    )
    .await?
    .first()
    .copied())
}

async fn resolve_power_shelf_id_for_firmware_device(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    device: &FirmwareUpgradeDeviceStatus,
) -> Result<Option<carbide_uuid::power_shelf::PowerShelfId>, StateHandlerError> {
    if !device.node_id.is_empty() {
        return Ok(device
            .node_id
            .parse::<carbide_uuid::power_shelf::PowerShelfId>()
            .ok());
    }
    let mac: mac_address::MacAddress = match device.mac.parse() {
        Ok(mac) => mac,
        Err(_) => return Ok(None),
    };
    Ok(db_power_shelf::find_ids(
        txn,
        model::power_shelf::PowerShelfSearchFilter {
            bmc_mac: Some(mac),
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        },
    )
    .await?
    .first()
    .copied())
}

fn machine_firmware_outcome(machine: &model::machine::Machine) -> DeviceFirmwareOutcome {
    match &machine.state.value {
        model::machine::ManagedHostState::HostReprovision {
            reprovision_state: model::machine::HostReprovisionState::WaitingForRackFirmwareUpgrade,
            ..
        } => DeviceFirmwareOutcome::Waiting,
        model::machine::ManagedHostState::HostReprovision {
            reprovision_state: model::machine::HostReprovisionState::FailedFirmwareUpgrade { .. },
            ..
        }
        | model::machine::ManagedHostState::Failed { .. } => DeviceFirmwareOutcome::Failed,
        // Machine has left WaitingForRackFirmwareUpgrade for a later
        // HostReprovision sub-state (success path).
        model::machine::ManagedHostState::HostReprovision { .. } => {
            DeviceFirmwareOutcome::Completed
        }
        // Request posted but controller has not entered the wait state yet.
        _ if machine.host_reprovision_requested.is_some() => DeviceFirmwareOutcome::Waiting,
        _ => DeviceFirmwareOutcome::Completed,
    }
}

fn switch_firmware_outcome(switch: &model::switch::Switch) -> DeviceFirmwareOutcome {
    match &switch.controller_state.value {
        model::switch::SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForRackFirmwareUpgrade,
        } => DeviceFirmwareOutcome::Waiting,
        model::switch::SwitchControllerState::ReProvisioning {
            reprovisioning_state:
                model::switch::ReProvisioningState::WaitingForNVOSUpgrade
                | model::switch::ReProvisioningState::WaitingForNMXCConfigure,
        } => DeviceFirmwareOutcome::Completed,
        model::switch::SwitchControllerState::Error { .. } => DeviceFirmwareOutcome::Failed,
        // Request posted but controller has not entered the wait state yet.
        model::switch::SwitchControllerState::Ready
            if switch.switch_reprovisioning_requested.is_some() =>
        {
            DeviceFirmwareOutcome::Waiting
        }
        _ => DeviceFirmwareOutcome::Completed,
    }
}

fn power_shelf_firmware_outcome(
    power_shelf: &model::power_shelf::PowerShelf,
) -> DeviceFirmwareOutcome {
    match &power_shelf.controller_state.value {
        model::power_shelf::PowerShelfControllerState::ReProvisioning {
            reprovisioning_state:
                model::power_shelf::ReProvisioningState::WaitingForRackFirmwareUpgrade,
        } => DeviceFirmwareOutcome::Waiting,
        model::power_shelf::PowerShelfControllerState::Error { .. } => {
            DeviceFirmwareOutcome::Failed
        }
        // Request posted but controller has not entered the wait state yet
        // (or is about to clear it when rack_firmware_reprovisioning_enabled
        // is false).
        model::power_shelf::PowerShelfControllerState::Ready
            if power_shelf.power_shelf_reprovisioning_requested.is_some() =>
        {
            DeviceFirmwareOutcome::Waiting
        }
        _ => DeviceFirmwareOutcome::Completed,
    }
}

fn summarize_firmware_outcomes(outcomes: &[DeviceFirmwareOutcome]) -> DeviceFirmwareProgress {
    let total = outcomes.len();
    let completed = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, DeviceFirmwareOutcome::Completed))
        .count();
    let failed = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, DeviceFirmwareOutcome::Failed))
        .count();
    let pending = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, DeviceFirmwareOutcome::Waiting))
        .count();
    if pending > 0 {
        return DeviceFirmwareProgress::Waiting {
            pending,
            total,
            completed,
            failed,
        };
    }
    if failed > 0 {
        return DeviceFirmwareProgress::Failed { failed, total };
    }
    DeviceFirmwareProgress::Completed { completed, total }
}

/// Reads device controller states for devices in `rack_id`,
/// filtered by `scope`, and decides whether firmware WaitForComplete can
/// advance. Device membership comes from the DB + scope, not the firmware job.
async fn evaluate_firmware_progress_from_devices(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
    scope: &MaintenanceScope,
) -> Result<(DeviceFirmwareProgress, Vec<String>), StateHandlerError> {
    let machines = load_scoped_machines(txn, rack_id, scope).await?;

    let switch_ids = db_switch::find_ids(
        &mut *txn,
        model::switch::SwitchSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: model::DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;
    let switches = if switch_ids.is_empty() {
        Vec::new()
    } else {
        db_switch::find_by(
            txn,
            db::ObjectColumnFilter::List(db_switch::IdColumn, &switch_ids),
        )
        .await?
    };
    let switches = filter_switches_by_scope(switches, scope);

    let power_shelf_ids = db_power_shelf::find_ids(
        &mut *txn,
        model::power_shelf::PowerShelfSearchFilter {
            rack_id: Some(rack_id.clone()),
            deleted: model::DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;
    let power_shelves = if power_shelf_ids.is_empty() {
        Vec::new()
    } else {
        db_power_shelf::find_by(
            txn,
            db::ObjectColumnFilter::List(db_power_shelf::IdColumn, &power_shelf_ids),
        )
        .await?
    };
    let power_shelves = filter_power_shelves_by_scope(power_shelves, scope);

    let mut outcomes = Vec::with_capacity(machines.len() + switches.len() + power_shelves.len());
    let mut pending_device_ids = Vec::new();
    for machine in &machines {
        let outcome = machine_firmware_outcome(machine);
        if outcome == DeviceFirmwareOutcome::Waiting {
            pending_device_ids.push(machine.id.to_string());
        }
        outcomes.push(outcome);
    }
    for switch in &switches {
        let outcome = switch_firmware_outcome(switch);
        if outcome == DeviceFirmwareOutcome::Waiting {
            pending_device_ids.push(switch.id.to_string());
        }
        outcomes.push(outcome);
    }
    for power_shelf in &power_shelves {
        let outcome = power_shelf_firmware_outcome(power_shelf);
        if outcome == DeviceFirmwareOutcome::Waiting {
            pending_device_ids.push(power_shelf.id.to_string());
        }
        outcomes.push(outcome);
    }
    pending_device_ids.sort();
    Ok((summarize_firmware_outcomes(&outcomes), pending_device_ids))
}

fn filter_machines_by_scope(
    mut machines: Vec<model::machine::Machine>,
    scope: &MaintenanceScope,
) -> Vec<model::machine::Machine> {
    if scope.is_full_rack() {
        return machines;
    }
    if scope.machine_ids.is_empty() {
        return Vec::new();
    }
    let allowed: std::collections::HashSet<_> = scope.machine_ids.iter().collect();
    machines.retain(|machine| allowed.contains(&machine.id));
    machines
}

fn filter_switches_by_scope(
    mut switches: Vec<model::switch::Switch>,
    scope: &MaintenanceScope,
) -> Vec<model::switch::Switch> {
    if scope.is_full_rack() {
        return switches;
    }
    if scope.switch_ids.is_empty() {
        return Vec::new();
    }
    let allowed: std::collections::HashSet<_> = scope.switch_ids.iter().collect();
    switches.retain(|switch| allowed.contains(&switch.id));
    switches
}

fn filter_power_shelves_by_scope(
    mut power_shelves: Vec<model::power_shelf::PowerShelf>,
    scope: &MaintenanceScope,
) -> Vec<model::power_shelf::PowerShelf> {
    if scope.is_full_rack() {
        return power_shelves;
    }
    if scope.power_shelf_ids.is_empty() {
        return Vec::new();
    }
    let allowed: std::collections::HashSet<_> = scope.power_shelf_ids.iter().collect();
    power_shelves.retain(|power_shelf| allowed.contains(&power_shelf.id));
    power_shelves
}

fn filter_power_shelf_ids_by_scope(
    mut power_shelf_ids: Vec<carbide_uuid::power_shelf::PowerShelfId>,
    scope: &MaintenanceScope,
) -> Vec<carbide_uuid::power_shelf::PowerShelfId> {
    if scope.is_full_rack() {
        return power_shelf_ids;
    }
    if scope.power_shelf_ids.is_empty() {
        return Vec::new();
    }
    let allowed: std::collections::HashSet<_> = scope.power_shelf_ids.iter().collect();
    power_shelf_ids.retain(|id| allowed.contains(id));
    power_shelf_ids
}

fn skip_firmware_upgrade_outcome(
    rack_id: &RackId,
    reason: impl AsRef<str>,
    scope: &MaintenanceScope,
) -> StateHandlerOutcome<RackState> {
    let next = next_state_after_firmware(scope);
    tracing::info!(
        rack_id = %rack_id,
        reason = %reason.as_ref(),
        next_state = %next,
        "Skipping rack firmware upgrade"
    );
    StateHandlerOutcome::transition(RackState::Maintenance {
        maintenance_state: next,
    })
}

/// Transition the rack to `Error` from a maintenance handler failure.
///
/// Clears `maintenance_requested` (and persists it) so the `Error` handler
/// does not immediately re-enter `Maintenance` and loop on the same failure.
/// The user must explicitly request maintenance again to retry.
async fn transition_to_rack_error(
    rack_id: &RackId,
    state: &mut Rack,
    cause: impl Into<String>,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    let cause = cause.into();
    tracing::warn!(rack_id = %rack_id, %cause, "Rack maintenance failed, transitioning to Error");
    let outcome = StateHandlerOutcome::transition(RackState::Error { cause });
    clear_maintenance_requested_on_error(rack_id, state, outcome, ctx).await
}

async fn transition_to_rack_error_with_firmware_job(
    rack_id: &RackId,
    state: &mut Rack,
    firmware_id: impl Into<String>,
    cause: impl Into<String>,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    let cause = cause.into();
    tracing::warn!(rack_id = %rack_id, %cause, "Rack firmware upgrade failed before polling started");

    let now = chrono::Utc::now();
    let job = model::rack::FirmwareUpgradeJob {
        firmware_id: Some(firmware_id.into()),
        status: Some("failed".into()),
        started_at: Some(now),
        completed_at: Some(now),
        ..Default::default()
    };
    state.firmware_upgrade_job = Some(job.clone());
    state.config.maintenance_requested = None;

    let mut txn = ctx.services.db_pool.begin().await?;
    db_rack::update_firmware_upgrade_job(txn.as_mut(), rack_id, Some(&job)).await?;
    db_rack::update(txn.as_mut(), rack_id, &state.config).await?;

    Ok(StateHandlerOutcome::transition(RackState::Error { cause }).with_txn(txn))
}

/// If `maintenance_requested` is set, clear it and persist the updated config
/// using a fresh transaction attached to the outcome. Used when transitioning
/// from `Maintenance` to `Error` to break the Error → Maintenance loop.
async fn clear_maintenance_requested_on_error(
    rack_id: &RackId,
    state: &mut Rack,
    outcome: StateHandlerOutcome<RackState>,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    if state.config.maintenance_requested.is_none()
        && !state.config.maintenance_termination_requested
    {
        return Ok(outcome);
    }
    state.config.maintenance_requested = None;
    let mut txn = ctx.services.db_pool.begin().await?;
    db_rack::update(txn.as_mut(), rack_id, &state.config).await?;
    Ok(outcome.with_txn(txn))
}

fn nvos_update_requested(scope: &MaintenanceScope) -> bool {
    scope
        .activities
        .iter()
        .any(|activity| matches!(activity, MaintenanceActivity::NvosUpdate { .. }))
}

fn requested_nvos_config_json(scope: &MaintenanceScope) -> Option<String> {
    scope.activities.iter().find_map(|activity| match activity {
        MaintenanceActivity::NvosUpdate { config_json } => {
            (!config_json.trim().is_empty()).then(|| config_json.clone())
        }
        _ => None,
    })
}

fn profile_hardware_type_or_any(profile: Option<&RackProfile>) -> String {
    profile
        .map(profile_hardware_type_wire_value)
        .filter(|hardware_type| !hardware_type.trim().is_empty())
        .unwrap_or_else(|| ANY_RACK_HARDWARE_TYPE.to_string())
}

fn requested_firmware_object_json_upgrade(
    scope: &MaintenanceScope,
) -> Option<(Option<String>, Vec<String>, bool)> {
    scope.activities.iter().find_map(|activity| match activity {
        MaintenanceActivity::FirmwareUpgrade {
            firmware_version,
            components,
            force_update,
        } => Some((firmware_version.clone(), components.clone(), *force_update)),
        _ => None,
    })
}

/// Loads and validates the default firmware object selected by a rack profile.
///
/// A missing or unknown profile, or a profile without a firmware-object
/// source, returns `Ok(None)`. Fetch and JSON validation failures are returned
/// so the state controller can retry `FirmwareUpgrade(Start)`.
async fn configured_ingestion_firmware_object_json(
    rack_id: &RackId,
    rack_profile_id: Option<&RackProfileId>,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<Option<String>, String> {
    let Some(profile) = super::resolve_profile(rack_id, rack_profile_id, ctx) else {
        return Ok(None);
    };

    let Some(firmware_object) = profile.firmware_object.clone() else {
        return Ok(None);
    };

    let config_json = ctx
        .services
        .firmware_object_fetcher
        .fetch(firmware_object.url.as_str(), firmware_object.fetch_timeout)
        .await?;

    serde_json::from_str::<std::collections::HashMap<String, serde::de::IgnoredAny>>(&config_json)
        .map_err(|error| format!("configured SOT firmware object is not a JSON object: {error}"))?;

    Ok(Some(config_json))
}

async fn load_rack_maintenance_access_token(
    credential_manager: &dyn CredentialManager,
    rack_id: &RackId,
) -> Result<String, StateHandlerError> {
    let key = rack_maintenance_access_token_key(rack_id);
    let credentials = credential_manager
        .get_credentials(&key)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to load rack maintenance access token: {}",
                error
            ))
        })?
        .ok_or_else(|| {
            StateHandlerError::GenericError(eyre::eyre!(
                "rack maintenance access token is not available"
            ))
        })?;

    let Credentials::UsernamePassword { password, .. } = credentials;
    Ok(password)
}

#[derive(Event)]
#[event(
    event_name = "rack_maintenance_access_token_cleanup_failed",
    metric_name = "carbide_rack_maintenance_access_token_cleanup_failures_total",
    component = "rack-controller",
    log = warn,
    metric = counter,
    message = "failed to delete rack maintenance access token",
    describe = "Number of rack maintenance access token cleanup failures"
)]
struct RackMaintenanceAccessTokenCleanupFailed {
    #[context]
    rack_id: RackId,
    #[context]
    error: String,
}

async fn delete_rack_maintenance_access_token(
    credential_manager: &dyn CredentialManager,
    rack_id: &RackId,
) {
    if let Err(error) = credential_manager
        .delete_credentials(&rack_maintenance_access_token_key(rack_id))
        .await
    {
        emit(RackMaintenanceAccessTokenCleanupFailed {
            rack_id: rack_id.clone(),
            error: error.to_string(),
        });
    }
}

fn nvos_update_start_state(_scope: &MaintenanceScope) -> RackMaintenanceState {
    RackMaintenanceState::NVOSUpdate {
        nvos_update: NvosUpdateState::Start,
    }
}

/// Returns the next maintenance sub-state after firmware upgrade, skipping
/// activities not requested in the scope.
fn next_state_after_firmware(scope: &MaintenanceScope) -> RackMaintenanceState {
    if nvos_update_requested(scope) {
        nvos_update_start_state(scope)
    } else {
        next_state_after_nvos(scope)
    }
}

/// Returns the next maintenance sub-state after NVOS update, skipping
/// activities not requested in the scope.
fn next_state_after_nvos(scope: &MaintenanceScope) -> RackMaintenanceState {
    if scope.should_run(&MaintenanceActivity::ConfigureNmxCluster) {
        RackMaintenanceState::ConfigureNmxCluster {
            configure_nmx_cluster: ConfigureNmxClusterState::Start,
        }
    } else {
        next_state_after_configure(scope)
    }
}

/// Returns the next maintenance sub-state after ConfigureNmxCluster, skipping
/// activities not requested in the scope.
fn next_state_after_configure(scope: &MaintenanceScope) -> RackMaintenanceState {
    if scope.should_run(&MaintenanceActivity::PowerSequence) {
        RackMaintenanceState::PowerSequence {
            rack_power: RackPowerState::PoweringOn,
        }
    } else {
        RackMaintenanceState::Completed
    }
}

/// Returns the first maintenance sub-state to enter based on the requested
/// activities in the scope. Called from Ready/Error when entering Maintenance.
pub(crate) fn first_maintenance_state(scope: &MaintenanceScope) -> RackMaintenanceState {
    if scope.should_run(&MaintenanceActivity::FirmwareUpgrade {
        firmware_version: None,
        components: vec![],
        force_update: false,
    }) {
        RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::Start,
        }
    } else {
        next_state_after_firmware(scope)
    }
}

/// Returns the state to advance to when the current maintenance state's
/// activity is not requested. Every [`RackMaintenanceState`] variant is
/// checked here so a persisted or stale state cannot execute outside the
/// current scope.
fn next_state_if_activity_not_requested(
    maintenance_state: &RackMaintenanceState,
    scope: &MaintenanceScope,
) -> Option<RackMaintenanceState> {
    match maintenance_state {
        RackMaintenanceState::FirmwareUpgrade { .. } => {
            (!scope.should_run(&MaintenanceActivity::FirmwareUpgrade {
                firmware_version: None,
                components: vec![],
                force_update: false,
            }))
            .then(|| next_state_after_firmware(scope))
        }
        RackMaintenanceState::NVOSUpdate { .. } => {
            (!nvos_update_requested(scope)).then(|| next_state_after_nvos(scope))
        }
        RackMaintenanceState::ConfigureNmxCluster { .. } => (!scope
            .should_run(&MaintenanceActivity::ConfigureNmxCluster))
        .then(|| next_state_after_configure(scope)),
        RackMaintenanceState::PowerSequence { .. } => (!scope
            .should_run(&MaintenanceActivity::PowerSequence))
        .then_some(RackMaintenanceState::Completed),
        RackMaintenanceState::Completed => None,
    }
}

/// Filters a full-rack firmware inventory down to compute/switch devices listed
/// in the maintenance scope. Power-shelf firmware-object JSON apply is not
/// implemented yet.
fn filter_inventory_by_scope(
    mut inventory: RackFirmwareInventory,
    scope: &MaintenanceScope,
) -> RackFirmwareInventory {
    if scope.is_full_rack() {
        return inventory;
    }

    if scope.machine_ids.is_empty() {
        inventory.machine_ids.clear();
        inventory.machines.clear();
    } else {
        let allowed: std::collections::HashSet<_> = scope.machine_ids.iter().collect();
        inventory.machine_ids.retain(|id| allowed.contains(id));
        inventory.machines.retain(|d| {
            match d.node_id.parse::<carbide_uuid::machine::MachineId>() {
                Ok(ref id) => allowed.contains(id),
                Err(_) => false,
            }
        });
    }

    if scope.switch_ids.is_empty() {
        inventory.switch_ids.clear();
        inventory.switches.clear();
    } else {
        let allowed: std::collections::HashSet<_> = scope.switch_ids.iter().collect();
        inventory.switch_ids.retain(|id| allowed.contains(id));
        inventory.switches.retain(
            |d| match d.node_id.parse::<carbide_uuid::switch::SwitchId>() {
                Ok(ref id) => allowed.contains(id),
                Err(_) => false,
            },
        );
    }

    inventory
}

fn filter_switch_inventory_by_scope(
    mut inventory: RackSwitchFirmwareInventory,
    scope: &MaintenanceScope,
) -> RackSwitchFirmwareInventory {
    if scope.is_full_rack() {
        return inventory;
    }

    if scope.switch_ids.is_empty() {
        inventory.switch_ids.clear();
        inventory.switches.clear();
    } else {
        let allowed: std::collections::HashSet<_> = scope.switch_ids.iter().collect();
        inventory.switch_ids.retain(|id| allowed.contains(id));
        inventory.switches.retain(
            |d| match d.node_id.parse::<carbide_uuid::switch::SwitchId>() {
                Ok(ref id) => allowed.contains(id),
                Err(_) => false,
            },
        );
    }

    inventory
}

fn skip_configure_nmx_cluster_outcome(
    rack_id: &RackId,
    reason: impl AsRef<str>,
    scope: &MaintenanceScope,
) -> StateHandlerOutcome<RackState> {
    let next = next_state_after_configure(scope);
    tracing::info!(
        rack_id = %rack_id,
        reason = %reason.as_ref(),
        next_state = %next,
        "Skipping ConfigureNmxCluster"
    );
    StateHandlerOutcome::transition(RackState::Maintenance {
        maintenance_state: next,
    })
}

fn firmware_device_status(
    device: FirmwareUpgradeDeviceInfo,
    parent_job_id: Option<String>,
    child_jobs: &std::collections::HashMap<String, String>,
    node_errors: &std::collections::HashMap<String, String>,
    batch_error: Option<&str>,
) -> FirmwareUpgradeDeviceStatus {
    let mut status = FirmwareUpgradeDeviceStatus {
        node_id: device.node_id.clone(),
        mac: device.mac,
        bmc_ip: device.bmc_ip,
        status: "in_progress".into(),
        job_id: None,
        parent_job_id,
        error_message: None,
    };

    if let Some(error_message) = node_errors.get(&device.node_id) {
        status.status = "failed".into();
        status.error_message = Some(error_message.clone());
    } else if let Some(job_id) = child_jobs.get(&device.node_id) {
        status.job_id = Some(job_id.clone());
    } else {
        status.status = "failed".into();
        status.error_message = Some(
            batch_error
                .unwrap_or("RMS did not return a child firmware job for this device")
                .to_string(),
        );
    }

    status
}

struct RmsFirmwareObjectJsonApply<'a> {
    rack_id: &'a RackId,
    profile: &'a RackProfile,
    config_json: &'a str,
    access_token: &'a str,
    firmware_type: &'a str,
    hardware_type: &'a str,
    force_update: bool,
    components: &'a [String],
    machines: Vec<FirmwareUpgradeDeviceInfo>,
    switches: Vec<FirmwareUpgradeDeviceInfo>,
}

async fn rms_start_firmware_upgrade_from_json(
    rms_client: &dyn librms::RmsApi,
    request: RmsFirmwareObjectJsonApply<'_>,
) -> Result<model::rack::FirmwareUpgradeJob, StateHandlerError> {
    let started_at = chrono::Utc::now();
    let machine_count = request.machines.len();
    let switch_count = request.switches.len();
    let mut nodes = Vec::with_capacity(machine_count + switch_count);

    // Resolve all required RMS identities before constructing the RMS request
    // so a mixed-device update fails before any partial firmware submission.
    let compute_node_identity = if machine_count > 0 {
        Some(
            compute_node_identity_for_profile(request.profile).map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "failed to resolve RMS compute descriptor: {}",
                    error
                ))
            })?,
        )
    } else {
        None
    };

    let switch_node_identity = if switch_count > 0 {
        Some(
            switch_node_identity_for_profile(request.profile).map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "failed to resolve RMS switch descriptor: {}",
                    error
                ))
            })?,
        )
    } else {
        None
    };

    if let Some(node_identity) = &compute_node_identity {
        nodes.extend(
            request
                .machines
                .iter()
                .map(|device| build_new_node_info(request.rack_id, device, node_identity)),
        );
    }

    if let Some(node_identity) = &switch_node_identity {
        nodes.extend(
            request
                .switches
                .iter()
                .map(|device| build_new_node_info(request.rack_id, device, node_identity)),
        );
    }

    let (component_filters, node_descriptor_component_filters) =
        firmware_object_component_filters_for_node_identities(
            request.components,
            compute_node_identity
                .iter()
                .chain(switch_node_identity.iter()),
        );

    let response = rms_client
        .apply_firmware_object(rms::ApplyFirmwareObjectRequest {
            rack_id: request.rack_id.to_string(),
            config_json: request.config_json.to_string(),
            access_token: Some(rms_access_token_or_noauth(Some(request.access_token))),
            firmware_type: request.firmware_type.to_string(),
            hardware_type: request.hardware_type.to_string(),
            nodes: Some(rms::NodeSet { nodes }),
            force_update: request.force_update,
            component_filters,
            node_descriptor_component_filters,
        })
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to submit firmware object JSON apply to RMS: {}",
                error
            ))
        })?;

    let batch_response = response.response.as_ref();
    let batch_status = batch_response
        .map(|batch_response| batch_response.status)
        .unwrap_or(rms::ReturnCode::Failure as i32);
    let batch_job_id = batch_response
        .map(|batch_response| batch_response.job_id.as_str())
        .unwrap_or_default();
    if batch_status != rms::ReturnCode::Success as i32
        && batch_job_id.is_empty()
        && response.jobs.is_empty()
    {
        let message = batch_response
            .map(|batch_response| batch_response.message.as_str())
            .unwrap_or_default();
        let message = if message.is_empty() {
            "RMS returned failure for ApplyFirmwareObject".to_string()
        } else {
            message.to_string()
        };
        return Err(StateHandlerError::GenericError(eyre::eyre!(message)));
    }

    let parent_job_id = (!batch_job_id.is_empty()).then(|| batch_job_id.to_string());
    let child_jobs = response
        .jobs
        .iter()
        .map(|child| (child.node_id.clone(), child.job_id.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let node_errors = batch_response
        .map(|batch_response| {
            batch_response
                .node_results
                .iter()
                .filter(|result| {
                    result.status != rms::ReturnCode::Success as i32
                        || !result.error_message.is_empty()
                })
                .map(|result| (result.node_id.clone(), result.error_message.clone()))
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();
    let batch_error = batch_response.and_then(|batch_response| {
        if batch_response.status == rms::ReturnCode::Success as i32
            || batch_response.message.is_empty()
        {
            None
        } else {
            Some(batch_response.message.clone())
        }
    });

    let mut job = model::rack::FirmwareUpgradeJob {
        job_id: parent_job_id.clone(),
        firmware_id: Some(response.object_id),
        started_at: Some(started_at),
        batch_job_ids: parent_job_id.iter().cloned().collect(),
        machines: request
            .machines
            .into_iter()
            .map(|device| {
                firmware_device_status(
                    device,
                    parent_job_id.clone(),
                    &child_jobs,
                    &node_errors,
                    batch_error.as_deref(),
                )
            })
            .collect(),
        switches: request
            .switches
            .into_iter()
            .map(|device| {
                firmware_device_status(
                    device,
                    parent_job_id.clone(),
                    &child_jobs,
                    &node_errors,
                    batch_error.as_deref(),
                )
            })
            .collect(),
        ..Default::default()
    };

    tracing::info!(
        rack_id = %request.rack_id,
        parent_job_id = ?job.job_id,
        object_id = ?job.firmware_id,
        machine_count,
        switch_count,
        "RMS firmware object JSON apply submitted",
    );

    let all_devices: Vec<_> = job.all_devices().collect();
    let failed = all_devices
        .iter()
        .filter(|device| device.status == "failed")
        .count();
    let completed = all_devices
        .iter()
        .filter(|device| device.status == "completed")
        .count();
    let total = all_devices.len();
    let terminal = completed + failed;

    job.status = Some(
        if total > 0 && terminal < total {
            "in_progress"
        } else if failed > 0 {
            "failed"
        } else {
            "completed"
        }
        .into(),
    );
    if total > 0 && terminal == total {
        job.completed_at = Some(chrono::Utc::now());
    }

    Ok(job)
}

/// Poll RMS GetFirmwareJobStatus for each tracked child job and update the
/// in-memory rack firmware job with the latest per-device result.
async fn rms_get_firmware_upgrade_status(
    rms_client: &dyn librms::RmsApi,
    job: &model::rack::FirmwareUpgradeJob,
) -> Result<model::rack::FirmwareUpgradeJob, StateHandlerError> {
    let mut updated = job.clone();
    for device in updated.all_devices_mut() {
        if matches!(device.status.as_str(), "completed" | "failed") {
            continue;
        }

        let Some(job_id) = device.job_id.clone() else {
            device.status = "failed".into();
            if device.error_message.is_none() {
                device.error_message = Some("Device has no firmware job ID to poll".into());
            }
            continue;
        };

        let response = rms_client
            .get_firmware_job_status(librms::protos::rack_manager::GetFirmwareJobStatusRequest {
                job_id: job_id.clone(),
            })
            .await;

        match response {
            Ok(response)
                if response.status == librms::protos::rack_manager::ReturnCode::Success as i32 =>
            {
                if !response.node_id.is_empty() {
                    device.node_id = response.node_id.clone();
                }
                match rms::FirmwareJobState::try_from(response.job_state) {
                    Ok(rms::FirmwareJobState::Queued) => {
                        device.status = "pending".into();
                        device.error_message = None;
                    }
                    Ok(rms::FirmwareJobState::Running) => {
                        device.status = "in_progress".into();
                        device.error_message = None;
                    }
                    Ok(rms::FirmwareJobState::Completed) => {
                        device.status = "completed".into();
                        device.error_message = None;
                    }
                    Ok(rms::FirmwareJobState::Failed) => {
                        device.status = "failed".into();
                        device.error_message = Some(if response.error_message.is_empty() {
                            response.state_description
                        } else {
                            response.error_message
                        });
                    }
                    Ok(rms::FirmwareJobState::Unspecified) | Err(_) => {
                        tracing::warn!(
                            job_id = %job_id,
                            job_state = response.job_state,
                            "RMS returned unknown firmware job state; keeping previous device status"
                        );
                        device.error_message = Some(format!(
                            "Unknown RMS firmware job state {}",
                            response.job_state
                        ));
                    }
                }
            }
            Ok(response) => {
                let message = if response.error_message.is_empty() {
                    if response.state_description.is_empty() {
                        format!("RMS could not report status for firmware job {}", job_id)
                    } else {
                        response.state_description
                    }
                } else {
                    response.error_message
                };
                tracing::warn!(
                    job_id = %job_id,
                    job_status = response.status,
                    error = %message,
                    "RMS returned a non-success firmware job status lookup; retrying later"
                );
                device.error_message = Some(message);
            }
            Err(error) => {
                let error = rack_manager_error("get_firmware_job_status", error);
                tracing::warn!(
                    job_id = %job_id,
                    error = %error,
                    "Transient RMS firmware job polling error; retrying later"
                );
                device.error_message = Some(error.to_string());
            }
        }
    }

    let all_devices: Vec<_> = updated.all_devices().collect();
    let failed = all_devices
        .iter()
        .filter(|device| device.status == "failed")
        .count();
    let completed = all_devices
        .iter()
        .filter(|device| device.status == "completed")
        .count();
    let total = all_devices.len();
    let terminal = completed + failed;

    updated.status = Some(
        if total > 0 && terminal < total {
            "in_progress"
        } else if failed > 0 {
            "failed"
        } else {
            "completed"
        }
        .into(),
    );
    updated.completed_at = if total > 0 && terminal == total {
        Some(chrono::Utc::now())
    } else {
        None
    };

    Ok(updated)
}

fn validate_complete_nmx_fabric_inventory(
    switch_inventory: &RackSwitchFirmwareInventory,
) -> Result<(), String> {
    let resolved_switch_ids = switch_inventory
        .switches
        .iter()
        .map(|switch| switch.node_id.as_str())
        .collect::<std::collections::HashSet<_>>();

    let missing_switch_ids = switch_inventory
        .switch_ids
        .iter()
        .filter_map(|switch_id| {
            let switch_id = switch_id.to_string();
            (!resolved_switch_ids.contains(switch_id.as_str())).then_some(switch_id)
        })
        .collect::<Vec<_>>();

    if missing_switch_ids.is_empty() {
        return Ok(());
    }

    Err(format!(
        "missing endpoint info for switches: {}",
        missing_switch_ids.join(", ")
    ))
}

/// Loads the complete rack switch set required by the RMS V2 fabric contract.
///
/// Endpoint resolution must return every rack switch so V2 cannot reconcile a
/// partial fabric.
async fn load_nmx_fabric_inventory(
    rack_id: &RackId,
    operation: &str,
    db_pool: &sqlx::PgPool,
    credential_manager: &dyn CredentialManager,
) -> Result<RackSwitchFirmwareInventory, StateHandlerError> {
    let switch_inventory =
        load_rack_switch_firmware_inventory(db_pool, credential_manager, rack_id)
            .await
            .map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "failed to load rack switch firmware inventory for {}: {}",
                    operation,
                    error
                ))
            })?;

    validate_complete_nmx_fabric_inventory(&switch_inventory).map_err(|error| {
        StateHandlerError::GenericError(eyre::eyre!(
            "failed to load complete rack switch inventory for {}: {}",
            operation,
            error
        ))
    })?;

    Ok(switch_inventory)
}

/// Submits the complete rack fabric topology to the idempotent RMS V2 API.
///
/// RMS selects the primary and ensures its NMX Controller security before
/// reconciling the fabric, so NICo does not run V1 certificate preparation.
/// Submission failures retain the current durable state for retry. A successful
/// response advances only after RMS returns a non-empty job identifier.
async fn configure_scale_up_fabric_manager_v2(
    id: &RackId,
    state: &mut Rack,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
    rack_profile_id: Option<&RackProfileId>,
    scope: &MaintenanceScope,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    if !scope.is_full_rack() && scope.switch_ids.is_empty() {
        return Ok(skip_configure_nmx_cluster_outcome(
            id,
            "maintenance scope contains no switches",
            scope,
        ));
    }

    let Some(component_manager) = ctx.services.component_manager.clone() else {
        return transition_to_rack_error(id, state, "component manager not configured", ctx).await;
    };

    // RMS selects the primary across the fabric, so V2 always receives the full
    // rack inventory rather than a maintenance-scoped subset.
    let switch_inventory = load_nmx_fabric_inventory(
        id,
        "ConfigureScaleUpFabricManager",
        &ctx.services.db_pool,
        ctx.services.credential_manager.as_ref(),
    )
    .await?;

    if switch_inventory.switches.is_empty() {
        return Ok(skip_configure_nmx_cluster_outcome(
            id,
            "rack has no switches in inventory",
            scope,
        ));
    }

    if let Err(cause) = validate_switch_inventory_for_nmx_cluster(&switch_inventory.switches) {
        return transition_to_rack_error(id, state, cause, ctx).await;
    }

    let rack_profile_label = rack_profile_id
        .map(|profile_id| profile_id.to_string())
        .unwrap_or_else(|| "<none>".to_string());

    let Some(profile) = super::resolve_profile(id, rack_profile_id, ctx) else {
        return transition_to_rack_error(
            id,
            state,
            format!(
                "rack profile '{}' is missing or unknown; cannot resolve rack_hardware_topology",
                rack_profile_label
            ),
            ctx,
        )
        .await;
    };

    let Some(rack_hardware_topology) = profile.rack_hardware_topology else {
        return transition_to_rack_error(
            id,
            state,
            format!(
                "rack profile '{}' does not define rack_hardware_topology",
                rack_profile_label
            ),
            ctx,
        )
        .await;
    };

    let endpoints = match switch_inventory
        .switches
        .iter()
        .map(switch_endpoint_from_firmware_device)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(endpoints) => endpoints,
        Err(cause) => return transition_to_rack_error(id, state, cause, ctx).await,
    };

    let topology_type = rack_hardware_topology.to_string();

    tracing::info!(
        rack_id = %id,
        topology_type = %topology_type,
        switch_count = switch_inventory.switches.len(),
        "Submitting RMS v2 NMX cluster configuration"
    );

    let job_id = match component_manager
        .configure_scale_up_fabric_manager(&endpoints, rack_hardware_topology)
        .await
    {
        Ok(job_id) => job_id,
        Err(error @ ComponentManagerError::Unsupported(_)) => {
            return transition_to_rack_error(id, state, error.to_string(), ctx).await;
        }
        Err(error) => {
            // The desired-state RPC is idempotent, so any submission error keeps
            // this state retryable, including a lost response after RMS accepted it.
            tracing::warn!(
                rack_id = %id,
                error = %error,
                "Unable to submit RMS v2 NMX cluster configuration; retrying"
            );

            return Ok(StateHandlerOutcome::wait(format!(
                "Unable to submit RMS v2 NMX cluster configuration: {error}"
            )));
        }
    };

    tracing::info!(
        rack_id = %id,
        topology_type = %topology_type,
        switch_count = switch_inventory.switches.len(),
        job_id,
        "V2 ConfigureScaleUpFabricManager submitted; waiting for RMS job"
    );

    // Persist the returned job ID in controller state before polling so restart
    // recovery resumes the accepted RMS operation instead of submitting a new one.
    Ok(StateHandlerOutcome::transition(RackState::Maintenance {
        maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
            configure_nmx_cluster: ConfigureNmxClusterState::WaitForScaleUpFabricManagerJob {
                job_id,
            },
        },
    }))
}

/// Polls one RMS V2 job and selects its next durable controller state.
///
/// Polling failures retain the job ID. A missing RMS job is resubmitted through
/// the idempotent desired-state call, while failed or invalid jobs stop the rack
/// workflow.
async fn wait_for_scale_up_fabric_manager_job(
    id: &RackId,
    state: &mut Rack,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
    rack_profile_id: Option<&RackProfileId>,
    scope: &MaintenanceScope,
    job_id: &str,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    let Some(component_manager) = ctx.services.component_manager.clone() else {
        return transition_to_rack_error(id, state, "component manager not configured", ctx).await;
    };

    let job = match component_manager
        .get_scale_up_fabric_manager_job_status(job_id)
        .await
    {
        Ok(job) => job,
        Err(error @ ComponentManagerError::Unsupported(_)) => {
            return transition_to_rack_error(id, state, error.to_string(), ctx).await;
        }

        Err(error) => {
            tracing::warn!(
                rack_id = %id,
                job_id,
                error = %error,
                "Unable to poll RMS configuration job; retrying"
            );

            return Ok(StateHandlerOutcome::wait(format!(
                "Unable to poll ConfigureScaleUpFabricManager job {job_id}: {error}"
            )));
        }
    };

    let Some(job) = job else {
        tracing::warn!(
            rack_id = %id,
            job_id,
            "RMS has no state for the configuration job; resubmitting desired state"
        );

        return configure_scale_up_fabric_manager_v2(id, state, ctx, rack_profile_id, scope).await;
    };

    // Job completion proves RMS reconciliation finished, but NICo still reads
    // observed state to discover and persist the RMS-selected primary.
    match job {
        ScaleUpFabricManagerJobStatus::Pending { description } => {
            Ok(StateHandlerOutcome::wait(format!(
                "ConfigureScaleUpFabricManager job {job_id} is {}",
                description
            )))
        }
        ScaleUpFabricManagerJobStatus::Completed => {
            tracing::info!(
                rack_id = %id,
                job_id,
                "ConfigureScaleUpFabricManager job completed; verifying observed primary switch"
            );

            verify_scale_up_fabric_manager_v2(
                id,
                state,
                ctx,
                component_manager.as_ref(),
                rack_profile_id,
                scope,
            )
            .await
        }
        ScaleUpFabricManagerJobStatus::Failed { error } => {
            let cause = error.map_or_else(
                || format!("ConfigureScaleUpFabricManager job {job_id} failed"),
                |error| format!("ConfigureScaleUpFabricManager job {job_id} failed: {error}"),
            );

            transition_to_rack_error(id, state, cause, ctx).await
        }
        ScaleUpFabricManagerJobStatus::Unknown { execution_state } => transition_to_rack_error(
            id,
            state,
            format!(
                "ConfigureScaleUpFabricManager job {job_id} returned invalid execution state {}",
                execution_state
            ),
            ctx,
        )
        .await,
    }
}

/// Verifies and persists the primary switch and per-switch Fabric Manager
/// status from RMS V2.
///
/// Transient read failures retain the completed-job state for retry. A missing
/// rack profile is terminal because the backend cannot reconstruct the submitted
/// switch identities. Primary selection and Fabric Manager status are committed
/// together before maintenance advances.
async fn verify_scale_up_fabric_manager_v2(
    id: &RackId,
    state: &mut Rack,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
    component_manager: &ComponentManager,
    rack_profile_id: Option<&RackProfileId>,
    scope: &MaintenanceScope,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    // Re-resolve the complete fabric after the asynchronous job so status reads
    // use current switch endpoints without weakening the rack-wide contract.
    let switch_inventory = load_nmx_fabric_inventory(
        id,
        "GetScaleUpFabricStatusV2",
        &ctx.services.db_pool,
        ctx.services.credential_manager.as_ref(),
    )
    .await?;

    if switch_inventory.switches.is_empty() {
        return Ok(skip_configure_nmx_cluster_outcome(
            id,
            "rack has no switches in inventory",
            scope,
        ));
    }

    if let Err(cause) = validate_switch_inventory_for_nmx_cluster(&switch_inventory.switches) {
        return transition_to_rack_error(id, state, cause, ctx).await;
    }

    if super::resolve_profile(id, rack_profile_id, ctx).is_none() {
        return transition_to_rack_error(
            id,
            state,
            "rack profile is missing or unknown; cannot verify RMS ScaleUp Fabric status",
            ctx,
        )
        .await;
    }

    let endpoints = match switch_inventory
        .switches
        .iter()
        .map(switch_endpoint_from_firmware_device)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(endpoints) => endpoints,
        Err(cause) => return transition_to_rack_error(id, state, cause, ctx).await,
    };

    // RMS owns primary selection in V2. Read the observed switch state instead
    // of reproducing RMS tray-selection policy in NICo.
    let response = match component_manager
        .get_scale_up_fabric_status(&endpoints)
        .await
    {
        Ok(response) => response,
        Err(error @ ComponentManagerError::Unsupported(_)) => {
            return transition_to_rack_error(id, state, error.to_string(), ctx).await;
        }
        Err(error) => {
            tracing::warn!(
                rack_id = %id,
                error = %error,
                "Unable to verify RMS v2 primary switch; retrying"
            );

            return Ok(StateHandlerOutcome::wait(format!(
                "Unable to verify RMS v2 primary switch: {error}"
            )));
        }
    };

    let observed_primary = match observed_primary_switch(&switch_inventory.switches, &response) {
        Ok(primary_switch) => primary_switch,
        Err(cause) => {
            tracing::warn!(
                rack_id = %id,
                cause,
                "RMS v2 primary switch is not yet verifiable; retrying"
            );

            return Ok(StateHandlerOutcome::wait(cause));
        }
    };

    // GetScaleUpFabricStatus identifies the primary, while existing NICo
    // consumers require the richer per-switch Fabric Manager status payload.
    let fabric_manager_status_response = match component_manager
        .batch_get_scale_up_fabric_service_status(&endpoints)
        .await
    {
        Ok(response) => response,
        Err(error @ ComponentManagerError::Unsupported(_)) => {
            return transition_to_rack_error(id, state, error.to_string(), ctx).await;
        }
        Err(error) => {
            let cause = error.to_string();

            tracing::warn!(
                rack_id = %id,
                cause,
                "Unable to read RMS Fabric Manager status; retrying"
            );

            return Ok(StateHandlerOutcome::wait(cause));
        }
    };

    let observed_primary_node_id = observed_primary.to_string();
    let mut txn = ctx.services.db_pool.begin().await?;

    // Keep the primary and per-switch status atomic so readers cannot observe a
    // newly selected primary paired with stale Fabric Manager state.
    if let Err(cause) = persist_fabric_manager_statuses(
        txn.as_mut(),
        id,
        &switch_inventory.switches,
        &fabric_manager_status_response,
    )
    .await
    {
        drop(txn);
        return transition_to_rack_error(id, state, cause, ctx).await;
    }

    if let Err(cause) = persist_primary_switch(txn.as_mut(), id, &observed_primary_node_id).await {
        drop(txn);
        return transition_to_rack_error(id, state, cause, ctx).await;
    }

    txn.commit().await?;

    let next = next_state_after_configure(scope);

    tracing::info!(
        rack_id = %id,
        observed_primary_switch = %observed_primary,
        switch_count = switch_inventory.switches.len(),
        next_state = %next,
        "Verified and persisted RMS v2 fabric status; advancing"
    );

    Ok(StateHandlerOutcome::transition(RackState::Maintenance {
        maintenance_state: next,
    }))
}

/// Advances the rack's current maintenance substate.
///
/// At firmware-upgrade start, an explicit maintenance activity takes
/// precedence over the optional rack-profile firmware object. When neither
/// source exists, the firmware step is skipped.
///
/// # Errors
///
/// Returns an error when a required database, credential, firmware-source, or
/// backend operation fails.
pub async fn handle_maintenance(
    id: &RackId,
    state: &mut Rack,
    rack_profile_id: Option<&RackProfileId>,
    maintenance_state: &RackMaintenanceState,
    ctx: &mut StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<RackState>, StateHandlerError> {
    if state.config.maintenance_termination_requested {
        return terminate_active_rack_maintenance(
            id,
            state,
            maintenance_state,
            format!("rack maintenance terminated by operator while in {maintenance_state}"),
            ctx,
        )
        .await;
    }

    let scope = state
        .config
        .maintenance_requested
        .clone()
        .unwrap_or_default();
    let scope = &scope;

    if let Some(next) = next_state_if_activity_not_requested(maintenance_state, scope) {
        tracing::info!(
            rack_id = %id,
            current_state = %maintenance_state,
            next_state = %next,
            "Skipping rack maintenance state not requested by scope activities"
        );
        return Ok(StateHandlerOutcome::transition(RackState::Maintenance {
            maintenance_state: next,
        }));
    }

    match maintenance_state {
        RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade,
        } => match rack_firmware_upgrade {
            FirmwareUpgradeState::Start => {
                // A stored access token exists only for explicit maintenance requests
                // and must be cleaned up by the branches that consume those requests.
                let requested_source = requested_firmware_object_json_upgrade(scope);
                let uses_stored_token = requested_source.is_some();

                let (config_json, components, force_update) = match requested_source {
                    Some(requested_source) => requested_source,
                    None => {
                        let config_json =
                            configured_ingestion_firmware_object_json(id, rack_profile_id, ctx)
                                .await
                                .map_err(|error| {
                                    StateHandlerError::GenericError(eyre::eyre!(error))
                                })?;

                        (config_json, Vec::new(), false)
                    }
                };

                // Defensive: older persisted maintenance state may predate API-side JSON
                // validation.
                let Some(config_json) = config_json.filter(|json| !json.trim().is_empty()) else {
                    if uses_stored_token {
                        return transition_to_rack_error(
                            id,
                            state,
                            "firmware-upgrade rack maintenance requires SOT JSON and access token",
                            ctx,
                        )
                        .await;
                    }

                    return Ok(skip_firmware_upgrade_outcome(
                        id,
                        "firmware object JSON source is not configured for rack maintenance; skipping firmware update",
                        scope,
                    ));
                };

                let nvos_json_pending = requested_nvos_config_json(scope).is_some();

                let desired_off_machine_ids = {
                    let mut conn = ctx.services.db_pool.acquire().await?;
                    let machine_ids = load_scoped_machines(conn.as_mut(), id, scope)
                        .await?
                        .into_iter()
                        .map(|machine| machine.id)
                        .collect::<Vec<_>>();
                    desired_off_machine_ids(conn.as_mut(), &machine_ids).await?
                };
                if !desired_off_machine_ids.is_empty() {
                    if uses_stored_token {
                        delete_rack_maintenance_access_token(
                            ctx.services.credential_manager.as_ref(),
                            id,
                        )
                        .await;
                    }
                    return transition_to_rack_error(
                        id,
                        state,
                        format!(
                            "rack firmware upgrade cannot target machines whose desired power state is Off: {}",
                            format_machine_ids(&desired_off_machine_ids)
                        ),
                        ctx,
                    )
                    .await;
                }

                let Some(rms_client) = ctx.services.rms_client.as_ref() else {
                    if uses_stored_token {
                        delete_rack_maintenance_access_token(
                            ctx.services.credential_manager.as_ref(),
                            id,
                        )
                        .await;
                    }

                    return transition_to_rack_error(id, state, "RMS client not configured", ctx)
                        .await;
                };

                // Profile-driven ingestion has no caller token, so it uses the RMS
                // NOAUTH sentinel.
                let access_token = if uses_stored_token {
                    match load_rack_maintenance_access_token(
                        ctx.services.credential_manager.as_ref(),
                        id,
                    )
                    .await
                    {
                        Ok(access_token) => access_token,
                        Err(error) => {
                            let message = error.to_string();
                            return transition_to_rack_error(id, state, &message, ctx).await;
                        }
                    }
                } else {
                    rms_access_token_or_noauth(None)
                };
                let profile = super::resolve_profile(id, rack_profile_id, ctx);
                let rack_hardware_type = profile_hardware_type_or_any(profile);
                let firmware_type = profile
                    .map(firmware_type_for_profile)
                    .unwrap_or("prod")
                    .to_string();
                let inventory = load_rack_firmware_inventory(
                    &ctx.services.db_pool,
                    ctx.services.credential_manager.as_ref(),
                    id,
                )
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to load rack firmware inventory: {}",
                        error
                    ))
                })?;
                let inventory = filter_inventory_by_scope(inventory, scope);

                if inventory.machines.is_empty() && inventory.switches.is_empty() {
                    if uses_stored_token && !nvos_json_pending {
                        delete_rack_maintenance_access_token(
                            ctx.services.credential_manager.as_ref(),
                            id,
                        )
                        .await;
                    }
                    return Ok(skip_firmware_upgrade_outcome(
                        id,
                        "no compute or switch devices require rack firmware updates",
                        scope,
                    ));
                }

                let Some(profile) = profile else {
                    // Keep this aligned with the NVOS missing-profile path.
                    // Startup validation deliberately does not scan rack rows,
                    // so this call-time error still owns token cleanup.
                    if uses_stored_token {
                        delete_rack_maintenance_access_token(
                            ctx.services.credential_manager.as_ref(),
                            id,
                        )
                        .await;
                    }

                    return transition_to_rack_error(
                        id,
                        state,
                        "rack profile is missing or unknown; cannot build RMS node descriptors",
                        ctx,
                    )
                    .await;
                };

                tracing::info!(
                    rack_id = %id,
                    firmware_type = %firmware_type,
                    hardware_type = %rack_hardware_type,
                    force_update,
                    machine_count = inventory.machines.len(),
                    switch_count = inventory.switches.len(),
                    "Rack firmware object JSON apply starting",
                );

                let submit_result = rms_start_firmware_upgrade_from_json(
                    rms_client.as_ref(),
                    RmsFirmwareObjectJsonApply {
                        rack_id: id,
                        profile,
                        config_json: &config_json,
                        access_token: &access_token,
                        firmware_type: &firmware_type,
                        hardware_type: &rack_hardware_type,
                        force_update,
                        components: &components,
                        machines: inventory.machines.clone(),
                        switches: inventory.switches.clone(),
                    },
                )
                .await;

                if uses_stored_token && (submit_result.is_err() || !nvos_json_pending) {
                    delete_rack_maintenance_access_token(
                        ctx.services.credential_manager.as_ref(),
                        id,
                    )
                    .await;
                }

                let mut job = match submit_result {
                    Ok(job) => job,
                    Err(error) => {
                        return transition_to_rack_error_with_firmware_job(
                            id,
                            state,
                            "firmware-object-json",
                            error.to_string(),
                            ctx,
                        )
                        .await;
                    }
                };

                let mut txn = ctx.services.db_pool.begin().await?;
                let power_shelf_ids = db_power_shelf::find_ids(
                    txn.as_mut(),
                    model::power_shelf::PowerShelfSearchFilter {
                        rack_id: Some(id.clone()),
                        deleted: model::DeletedFilter::Exclude,
                        ..Default::default()
                    },
                )
                .await?;
                let power_shelf_ids = filter_power_shelf_ids_by_scope(power_shelf_ids, scope);
                trigger_rack_firmware_reprovisioning_requests(
                    txn.as_mut(),
                    id,
                    &inventory.machine_ids,
                    &inventory.switch_ids,
                    &power_shelf_ids,
                    &scope.activities,
                )
                .await?;
                clear_rack_firmware_device_statuses(
                    txn.as_mut(),
                    &inventory.machine_ids,
                    &inventory.switch_ids,
                    &power_shelf_ids,
                )
                .await?;
                job.started_at = Some(chrono::Utc::now());
                db_rack::update_firmware_upgrade_job(txn.as_mut(), id, Some(&job)).await?;
                state.firmware_upgrade_job = Some(job);

                Ok(StateHandlerOutcome::transition(RackState::Maintenance {
                    maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                        rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
                    },
                })
                .with_txn(txn))
            }
            FirmwareUpgradeState::WaitForComplete => {
                if state.firmware_upgrade_job.is_none() {
                    return Ok(StateHandlerOutcome::wait(
                        "firmware upgrade: no job recorded yet".into(),
                    ));
                }

                let power_blocked_machine_ids = {
                    let mut conn = ctx.services.db_pool.acquire().await?;
                    power_blocked_rack_firmware_machine_ids(conn.as_mut(), id, scope).await?
                };
                if !power_blocked_machine_ids.is_empty() {
                    let now = chrono::Utc::now();
                    let mut job = state.firmware_upgrade_job.clone().unwrap();
                    job.status = Some("failed".into());
                    if job.completed_at.is_none() {
                        job.completed_at = Some(now);
                    }
                    let cause = format!(
                        "rack firmware upgrade cannot progress because target machines are Ready with desired power state Off: {}",
                        format_machine_ids(&power_blocked_machine_ids)
                    );

                    // Keep the persisted maintenance scope intact while deleting credentials so
                    // a concurrent termination can still unwind only the originally requested
                    // devices.
                    delete_rack_maintenance_access_token(
                        ctx.services.credential_manager.as_ref(),
                        id,
                    )
                    .await;

                    let mut recovery_txn = ctx.services.db_pool.begin().await?;
                    db_rack::update_firmware_upgrade_job(recovery_txn.as_mut(), id, Some(&job))
                        .await?;
                    state.firmware_upgrade_job = Some(job);

                    let initiator = rack_maintenance_initiator(id);
                    for machine_id in &power_blocked_machine_ids {
                        db_host_machine_update::clear_ready_host_reprovisioning_request(
                            recovery_txn.as_mut(),
                            machine_id,
                            &initiator,
                        )
                        .await?;
                    }

                    if state.config.maintenance_requested.is_some() {
                        state.config.maintenance_requested = None;
                        db_rack::update(recovery_txn.as_mut(), id, &state.config).await?;
                    }
                    return Ok(StateHandlerOutcome::transition(RackState::Error { cause })
                        .with_txn(recovery_txn));
                }

                let Some(rms_client) = ctx.services.rms_client.as_ref() else {
                    if requested_nvos_config_json(scope).is_some() {
                        delete_rack_maintenance_access_token(
                            ctx.services.credential_manager.as_ref(),
                            id,
                        )
                        .await;
                    }
                    return transition_to_rack_error(id, state, "RMS client not configured", ctx)
                        .await;
                };
                let current_job = state.firmware_upgrade_job.as_ref().unwrap();
                let mut job =
                    rms_get_firmware_upgrade_status(rms_client.as_ref(), current_job).await?;

                let mut txn = ctx.services.db_pool.begin().await?;

                let build_status =
                    |device: &FirmwareUpgradeDeviceStatus| -> RackFirmwareUpgradeStatus {
                        let state = match device.status.as_str() {
                            "completed" => RackFirmwareUpgradeState::Completed,
                            "failed" => RackFirmwareUpgradeState::Failed {
                                cause: format!("RMS reported failure for {}", device.mac),
                            },
                            "in_progress" => RackFirmwareUpgradeState::InProgress,
                            _ => RackFirmwareUpgradeState::Started,
                        };
                        RackFirmwareUpgradeStatus {
                            task_id: device
                                .job_id
                                .clone()
                                .or_else(|| device.parent_job_id.clone())
                                .or_else(|| job.job_id.clone())
                                .unwrap_or_else(|| "unknown".to_string()),
                            status: state,
                            started_at: job.started_at,
                            ended_at: if device.status == "completed" || device.status == "failed" {
                                job.completed_at.or(Some(chrono::Utc::now()))
                            } else {
                                None
                            },
                        }
                    };

                for device in job.machines.iter() {
                    if let Some(machine_id) =
                        resolve_machine_id_for_firmware_device(txn.as_mut(), device).await?
                    {
                        let fw_status = build_status(device);
                        db_machine::update_rack_fw_details(
                            txn.as_mut(),
                            &machine_id,
                            Some(&fw_status),
                        )
                        .await?;
                    }
                }

                for device in job.switches.iter() {
                    if let Some(switch_id) =
                        resolve_switch_id_for_firmware_device(txn.as_mut(), id, device).await?
                    {
                        let fw_status = build_status(device);
                        db_switch::update_firmware_upgrade_status(
                            txn.as_mut(),
                            switch_id,
                            Some(&fw_status),
                        )
                        .await?;
                    }
                }

                for device in job.power_shelves.iter() {
                    if let Some(power_shelf_id) =
                        resolve_power_shelf_id_for_firmware_device(txn.as_mut(), id, device).await?
                    {
                        let fw_status = build_status(device);
                        db_power_shelf::update_firmware_upgrade_status(
                            txn.as_mut(),
                            power_shelf_id,
                            Some(&fw_status),
                        )
                        .await?;
                    }
                }

                // When RMS does not yet report power-shelf devices, stamp
                // Completed for scoped shelves so enabled controllers can leave
                // WaitingForRackFirmwareUpgrade without hanging.
                if job.power_shelves.is_empty() {
                    let power_shelf_ids = db_power_shelf::find_ids(
                        txn.as_mut(),
                        model::power_shelf::PowerShelfSearchFilter {
                            rack_id: Some(id.clone()),
                            deleted: model::DeletedFilter::Exclude,
                            ..Default::default()
                        },
                    )
                    .await?;
                    let power_shelf_ids = filter_power_shelf_ids_by_scope(power_shelf_ids, scope);
                    if !power_shelf_ids.is_empty() {
                        let fw_status = RackFirmwareUpgradeStatus {
                            task_id: job.job_id.clone().unwrap_or_else(|| "unknown".to_string()),
                            status: RackFirmwareUpgradeState::Completed,
                            started_at: job.started_at,
                            ended_at: Some(chrono::Utc::now()),
                        };
                        for power_shelf_id in power_shelf_ids {
                            db_power_shelf::update_firmware_upgrade_status(
                                txn.as_mut(),
                                power_shelf_id,
                                Some(&fw_status),
                            )
                            .await?;
                        }
                    }
                }

                // Advancement is driven by machine/switch/power-shelf controller
                // states for devices in this rack that are selected by the
                // maintenance scope.
                let (progress, pending_device_ids) =
                    evaluate_firmware_progress_from_devices(txn.as_mut(), id, scope).await?;

                match progress {
                    DeviceFirmwareProgress::Waiting {
                        pending,
                        total,
                        completed,
                        failed,
                    } => {
                        db_rack::update_firmware_upgrade_job(txn.as_mut(), id, Some(&job)).await?;
                        state.firmware_upgrade_job = Some(job);
                        Ok(StateHandlerOutcome::wait(format!(
                            "firmware upgrade: waiting for machine/switch/power-shelf controllers (completed={}, failed={}, pending={}/{}; pending devices: [{}])",
                            completed,
                            failed,
                            pending,
                            total,
                            pending_device_ids.join(", ")
                        ))
                        .with_txn(txn))
                    }
                    DeviceFirmwareProgress::Failed { failed, total } => {
                        let should_cleanup_token = requested_nvos_config_json(scope).is_some();
                        let now = chrono::Utc::now();
                        job.status = Some("failed".into());
                        if job.completed_at.is_none() {
                            job.completed_at = Some(now);
                        }
                        let cause = format!(
                            "firmware upgrade failed: {}/{} devices failed",
                            failed, total
                        );

                        // Persist the device status updates already accumulated in this
                        // transaction, but leave the maintenance scope intact while deleting
                        // credentials so a concurrent termination retains the original scope.
                        txn.commit().await?;
                        if should_cleanup_token {
                            delete_rack_maintenance_access_token(
                                ctx.services.credential_manager.as_ref(),
                                id,
                            )
                            .await;
                        }

                        let mut recovery_txn = ctx.services.db_pool.begin().await?;
                        db_rack::update_firmware_upgrade_job(recovery_txn.as_mut(), id, Some(&job))
                            .await?;
                        state.firmware_upgrade_job = Some(job);
                        if state.config.maintenance_requested.is_some() {
                            state.config.maintenance_requested = None;
                            db_rack::update(recovery_txn.as_mut(), id, &state.config).await?;
                        }
                        Ok(StateHandlerOutcome::transition(RackState::Error { cause })
                            .with_txn(recovery_txn))
                    }
                    DeviceFirmwareProgress::Completed { completed, total } => {
                        let now = chrono::Utc::now();
                        job.status = Some("completed".into());
                        if job.completed_at.is_none() {
                            job.completed_at = Some(now);
                        }
                        db_rack::update_firmware_upgrade_job(txn.as_mut(), id, Some(&job)).await?;
                        state.firmware_upgrade_job = Some(job);

                        let next_maintenance_state = if nvos_update_requested(scope) {
                            let next = next_state_after_firmware(scope);
                            tracing::info!(
                                rack_id = %id,
                                completed_device_count = completed,
                                total_device_count = total,
                                next_state = %next,
                                "Rack firmware upgrade complete on machine/switch controllers; advancing to explicitly requested next activity"
                            );
                            next
                        } else {
                            let next = next_state_after_nvos(scope);
                            tracing::info!(
                                rack_id = %id,
                                completed_device_count = completed,
                                total_device_count = total,
                                next_state = %next,
                                "Rack firmware upgrade complete on machine/switch controllers; no explicit NVOS update requested, advancing"
                            );
                            next
                        };

                        Ok(StateHandlerOutcome::transition(RackState::Maintenance {
                            maintenance_state: next_maintenance_state,
                        })
                        .with_txn(txn))
                    }
                }
            }
        },
        RackMaintenanceState::NVOSUpdate { nvos_update } => match nvos_update {
            NvosUpdateState::Start => {
                let Some(config_json) = requested_nvos_config_json(scope) else {
                    return transition_to_rack_error(
                        id,
                        state,
                        "nvos-update rack maintenance requires SOT JSON and access token",
                        ctx,
                    )
                    .await;
                };

                let Some(nvos_update_manager) = ctx.services.nvos_update_manager.as_deref() else {
                    delete_rack_maintenance_access_token(
                        ctx.services.credential_manager.as_ref(),
                        id,
                    )
                    .await;

                    return transition_to_rack_error(id, state, "RMS client not configured", ctx)
                        .await;
                };

                let access_token = match load_rack_maintenance_access_token(
                    ctx.services.credential_manager.as_ref(),
                    id,
                )
                .await
                {
                    Ok(access_token) => access_token,
                    Err(error) => {
                        let message = error.to_string();
                        return transition_to_rack_error(id, state, &message, ctx).await;
                    }
                };

                let profile = super::resolve_profile(id, rack_profile_id, ctx);

                let switch_inventory = load_rack_switch_firmware_inventory(
                    &ctx.services.db_pool,
                    ctx.services.credential_manager.as_ref(),
                    id,
                )
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to load rack switch firmware inventory for NVOS update: {}",
                        error
                    ))
                })?;
                let switch_inventory = filter_switch_inventory_by_scope(switch_inventory, scope);

                if switch_inventory.switches.is_empty() {
                    delete_rack_maintenance_access_token(
                        ctx.services.credential_manager.as_ref(),
                        id,
                    )
                    .await;
                    let next = next_state_after_nvos(scope);
                    tracing::info!(
                        rack_id = %id,
                        next_state = %next,
                        "No switches selected for NVOS update, advancing"
                    );
                    return Ok(StateHandlerOutcome::transition(RackState::Maintenance {
                        maintenance_state: next,
                    }));
                }
                let Some(profile) = profile else {
                    delete_rack_maintenance_access_token(
                        ctx.services.credential_manager.as_ref(),
                        id,
                    )
                    .await;
                    return transition_to_rack_error(
                        id,
                        state,
                        "rack profile is missing or unknown; cannot build RMS switch node descriptor",
                        ctx,
                    )
                    .await;
                };

                // Profile errors are terminal for this maintenance request and
                // take precedence over retryable switch credential errors.
                if let Err(error) = switch_node_identity_for_profile(profile) {
                    delete_rack_maintenance_access_token(
                        ctx.services.credential_manager.as_ref(),
                        id,
                    )
                    .await;

                    return transition_to_rack_error(id, state, error.to_string(), ctx).await;
                }

                let rack_hardware_type = profile_hardware_type_or_any(Some(profile));
                let software_type = firmware_type_for_profile(profile);

                tracing::info!(
                    rack_id = %id,
                    software_type,
                    hardware_type = %rack_hardware_type,
                    switch_count = switch_inventory.switches.len(),
                    "Rack switch system image FromJSON update starting",
                );

                for switch in &switch_inventory.switches {
                    switch.os_ip.as_ref().ok_or_else(|| {
                        StateHandlerError::GenericError(eyre::eyre!(
                            "switch {} has no NVOS IP for rack NVOS update",
                            switch.mac
                        ))
                    })?;
                    switch.os_username.as_ref().ok_or_else(|| {
                        StateHandlerError::GenericError(eyre::eyre!(
                            "switch {} has no NVOS username for rack NVOS update",
                            switch.mac
                        ))
                    })?;
                    switch.os_password.as_ref().ok_or_else(|| {
                        StateHandlerError::GenericError(eyre::eyre!(
                            "switch {} has no NVOS password for rack NVOS update",
                            switch.mac
                        ))
                    })?;
                }

                let submit_result = nvos_update_manager
                    .start_nvos_update(NvosUpdateRequest {
                        rack_id: id,
                        profile,
                        config_json: &config_json,
                        access_token: &access_token,
                        switches: switch_inventory.switches,
                    })
                    .await;

                // Invalid requests cannot succeed on retry. Backend submission
                // failures remain handler errors, with the token retained so
                // the Start state can resubmit the request.
                let job = match submit_result {
                    Ok(job) => job,
                    Err(ComponentManagerError::InvalidArgument(cause)) => {
                        delete_rack_maintenance_access_token(
                            ctx.services.credential_manager.as_ref(),
                            id,
                        )
                        .await;

                        return transition_to_rack_error(id, state, &cause, ctx).await;
                    }
                    Err(ComponentManagerError::Internal(cause))
                    | Err(ComponentManagerError::RejectedBeforeDispatch(cause)) => {
                        return Err(StateHandlerError::GenericError(eyre::eyre!(cause)));
                    }
                    Err(error) => return Err(StateHandlerError::GenericError(eyre::eyre!(error))),
                };

                delete_rack_maintenance_access_token(ctx.services.credential_manager.as_ref(), id)
                    .await;

                let mut txn = ctx.services.db_pool.begin().await?;
                clear_nvos_update_statuses(txn.as_mut(), &switch_inventory.switch_ids).await?;
                db_rack::update_nvos_update_job(txn.as_mut(), id, Some(&job)).await?;
                state.nvos_update_job = Some(job);

                Ok(StateHandlerOutcome::transition(RackState::Maintenance {
                    maintenance_state: RackMaintenanceState::NVOSUpdate {
                        nvos_update: NvosUpdateState::WaitForComplete,
                    },
                })
                .with_txn(txn))
            }
            NvosUpdateState::WaitForComplete => {
                let current_job = match &state.nvos_update_job {
                    Some(job) => job,
                    None => {
                        return Ok(StateHandlerOutcome::wait(
                            "nvos update: no job recorded yet".into(),
                        ));
                    }
                };

                let Some(nvos_update_manager) = ctx.services.nvos_update_manager.as_deref() else {
                    return transition_to_rack_error(id, state, "RMS client not configured", ctx)
                        .await;
                };

                let job = nvos_update_manager
                    .get_nvos_update_status(current_job)
                    .await
                    .map_err(|error| StateHandlerError::GenericError(eyre::eyre!(error)))?;

                let mut txn = ctx.services.db_pool.begin().await?;

                let build_status = |switch: &NvosUpdateSwitchStatus| -> SwitchNvosUpdateStatus {
                    let status = match switch.status.as_str() {
                        "completed" => SwitchNvosUpdateState::Completed,
                        "failed" => SwitchNvosUpdateState::Failed {
                            cause: switch.error_message.clone().unwrap_or_else(|| {
                                format!("RMS reported NVOS failure for {}", switch.mac)
                            }),
                        },
                        "in_progress" => SwitchNvosUpdateState::InProgress,
                        _ => SwitchNvosUpdateState::Started,
                    };

                    SwitchNvosUpdateStatus {
                        task_id: switch
                            .job_id
                            .clone()
                            .or_else(|| job.job_id.clone())
                            .unwrap_or_else(|| "unknown".to_string()),
                        firmware_id: job.firmware_id.clone(),
                        image_filename: job.image_filename.clone(),
                        status,
                        started_at: job.started_at,
                        ended_at: if switch.status == "completed" || switch.status == "failed" {
                            Some(chrono::Utc::now())
                        } else {
                            None
                        },
                    }
                };

                for switch in job.switches.iter() {
                    let switch_id = if !switch.node_id.is_empty() {
                        switch
                            .node_id
                            .parse::<carbide_uuid::switch::SwitchId>()
                            .ok()
                    } else {
                        let mac: mac_address::MacAddress = match switch.mac.parse() {
                            Ok(mac) => mac,
                            Err(_) => continue,
                        };
                        db_switch::find_ids(
                            txn.as_mut(),
                            model::switch::SwitchSearchFilter {
                                bmc_mac: Some(mac),
                                rack_id: Some(id.clone()),
                                ..Default::default()
                            },
                        )
                        .await?
                        .first()
                        .copied()
                    };
                    if let Some(switch_id) = switch_id {
                        let nvos_status = build_status(switch);
                        db_switch::update_nvos_update_status(
                            txn.as_mut(),
                            switch_id,
                            Some(&nvos_status),
                        )
                        .await?;
                    } else {
                        tracing::error!(
                            mac_address = %switch.mac,
                            "switch not found in database for NVOS update",
                        );
                    }
                }

                let total = job.all_switches().count();
                let completed = job
                    .all_switches()
                    .filter(|switch| switch.status == "completed")
                    .count();
                let failed = job
                    .all_switches()
                    .filter(|switch| switch.status == "failed")
                    .count();

                if failed > 0 {
                    db_rack::update_nvos_update_job(txn.as_mut(), id, Some(&job)).await?;
                    state.nvos_update_job = Some(job);
                    if state.config.maintenance_requested.is_some() {
                        state.config.maintenance_requested = None;
                        db_rack::update(txn.as_mut(), id, &state.config).await?;
                    }
                    return Ok(StateHandlerOutcome::transition(RackState::Error {
                        cause: format!("NVOS update failed: {}/{} switches failed", failed, total),
                    })
                    .with_txn(txn));
                }

                if completed < total {
                    db_rack::update_nvos_update_job(txn.as_mut(), id, Some(&job)).await?;
                    state.nvos_update_job = Some(job);
                    return Ok(StateHandlerOutcome::wait(format!(
                        "nvos update: {}/{} switches completed",
                        completed, total
                    ))
                    .with_txn(txn));
                }

                let next = next_state_after_nvos(scope);
                tracing::info!(
                    rack_id = %id,
                    completed_switch_count = completed,
                    total_switch_count = total,
                    next_state = %next,
                    "Rack NVOS update complete, advancing"
                );
                db_rack::update_nvos_update_job(txn.as_mut(), id, Some(&job)).await?;
                state.nvos_update_job = Some(job);
                Ok(StateHandlerOutcome::transition(RackState::Maintenance {
                    maintenance_state: next,
                })
                .with_txn(txn))
            }
        },
        RackMaintenanceState::ConfigureNmxCluster {
            configure_nmx_cluster,
        } => match configure_nmx_cluster {
            ConfigureNmxClusterState::Start => {
                configure_scale_up_fabric_manager_v2(id, state, ctx, rack_profile_id, scope).await
            }
            ConfigureNmxClusterState::WaitForScaleUpFabricManagerJob { job_id } => {
                wait_for_scale_up_fabric_manager_job(id, state, ctx, rack_profile_id, scope, job_id)
                    .await
            }
            retired @ (ConfigureNmxClusterState::ConfigureCertificates {}
            | ConfigureNmxClusterState::DisableScaleUpFabricState
            | ConfigureNmxClusterState::ConfigureScaleUpFabricManager
            | ConfigureNmxClusterState::WaitForFabricStatus) => {
                transition_to_rack_error(
                    id,
                    state,
                    format!(
                        "rack is persisted in ConfigureNmxCluster({retired}), a sub-state this \
                         version does not run; maintenance cannot resume and the rack requires \
                         manual intervention"
                    ),
                    ctx,
                )
                .await
            }
        },
        RackMaintenanceState::PowerSequence { rack_power } => match rack_power {
            RackPowerState::PoweringOn => {
                tracing::info!(
                    rack_id = %id,
                    "Rack power sequence (on) - stubbed",
                );

                Ok(StateHandlerOutcome::transition(RackState::Maintenance {
                    maintenance_state: RackMaintenanceState::Completed,
                }))
            }
            RackPowerState::PoweringOff => {
                tracing::info!(
                    rack_id = %id,
                    "Rack power sequence (off) - stubbed",
                );
                Ok(StateHandlerOutcome::wait(
                    "power sequence (off) in progress".into(),
                ))
            }
            RackPowerState::PowerReset => {
                tracing::info!(
                    rack_id = %id,
                    "Rack power sequence (reset) - stubbed",
                );
                Ok(StateHandlerOutcome::wait(
                    "power sequence (reset) in progress".into(),
                ))
            }
        },
        RackMaintenanceState::Completed => {
            tracing::info!(
                rack_id = %id,
                "Maintenance completed, clearing rv.* labels and entering Validating(Pending)"
            );
            clear_rv_labels(state, ctx).await?;

            let mut outcome = StateHandlerOutcome::transition(RackState::Validating {
                validating_state: RackValidationState::Pending,
            });

            if state.config.maintenance_requested.is_some() {
                state.config.maintenance_requested = None;
                let mut txn = ctx.services.db_pool.begin().await?;
                db_rack::update(txn.as_mut(), id, &state.config).await?;
                outcome = outcome.with_txn(txn);
            }

            Ok(outcome)
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_rack::firmware_update::{RackFirmwareInventory, RackSwitchFirmwareInventory};
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::{Check, check_values};
    use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
    use carbide_uuid::rack::RackId;
    use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
    use model::rack::{
        ConfigureNmxClusterState, FirmwareUpgradeDeviceInfo, FirmwareUpgradeState,
        MaintenanceActivity, MaintenanceScope, NvosUpdateState, RackMaintenanceState,
        RackPowerState,
    };
    use model::rack_type::{RackHardwareType, RackProfile};

    use super::{
        DeviceFirmwareOutcome, DeviceFirmwareProgress, delete_rack_maintenance_access_token,
        filter_inventory_by_scope, firmware_device_status, first_maintenance_state,
        next_state_after_configure, next_state_after_firmware, next_state_after_nvos,
        next_state_if_activity_not_requested, profile_hardware_type_or_any,
        summarize_firmware_outcomes, validate_complete_nmx_fabric_inventory,
    };

    fn test_machine_id(seed: u8) -> MachineId {
        let mut hash = [0u8; 32];
        hash[0] = seed;
        MachineId::new(MachineIdSource::Tpm, hash, MachineType::Host)
    }

    fn test_switch_id(seed: u8) -> SwitchId {
        let mut hash = [0u8; 32];
        hash[0] = seed;
        SwitchId::new(SwitchIdSource::Tpm, hash, SwitchType::NvLink)
    }

    fn test_device_info(node_id: impl ToString) -> FirmwareUpgradeDeviceInfo {
        FirmwareUpgradeDeviceInfo {
            node_id: node_id.to_string(),
            mac: "00:11:22:33:44:55".to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            bmc_username: "admin".to_string(),
            bmc_password: "password".to_string(),
            os_mac: None,
            os_ip: None,
            os_username: None,
            os_password: None,
            os_hostname: None,
        }
    }

    fn sample_inventory() -> RackFirmwareInventory {
        let machine_a = test_machine_id(1);
        let machine_b = test_machine_id(2);
        let switch_a = test_switch_id(3);
        let switch_b = test_switch_id(4);

        RackFirmwareInventory {
            machine_ids: vec![machine_a, machine_b],
            machines: vec![test_device_info(machine_a), test_device_info(machine_b)],
            switch_ids: vec![switch_a, switch_b],
            switches: vec![test_device_info(switch_a), test_device_info(switch_b)],
        }
    }

    #[test]
    fn nmx_fabric_inventory_requires_endpoint_info_for_every_rack_switch() {
        let switch_a = test_switch_id(1);
        let switch_b = test_switch_id(2);

        check_values(
            [
                Check {
                    scenario: "empty rack",
                    input: RackSwitchFirmwareInventory {
                        switch_ids: Vec::new(),
                        switches: Vec::new(),
                    },
                    expect: Ok(()),
                },
                Check {
                    scenario: "complete endpoint inventory",
                    input: RackSwitchFirmwareInventory {
                        switch_ids: vec![switch_a, switch_b],
                        switches: vec![test_device_info(switch_a), test_device_info(switch_b)],
                    },
                    expect: Ok(()),
                },
                Check {
                    scenario: "one endpoint missing",
                    input: RackSwitchFirmwareInventory {
                        switch_ids: vec![switch_a, switch_b],
                        switches: vec![test_device_info(switch_a)],
                    },
                    expect: Err(format!("missing endpoint info for switches: {switch_b}")),
                },
                Check {
                    scenario: "all endpoints missing",
                    input: RackSwitchFirmwareInventory {
                        switch_ids: vec![switch_a, switch_b],
                        switches: Vec::new(),
                    },
                    expect: Err(format!(
                        "missing endpoint info for switches: {switch_a}, {switch_b}"
                    )),
                },
            ],
            |inventory| validate_complete_nmx_fabric_inventory(&inventory),
        );
    }

    #[derive(Debug, PartialEq)]
    struct AccessTokenCleanupObservation {
        counter_delta: f64,
        log_count: usize,
        level: Option<tracing::Level>,
        metadata_name: Option<String>,
        message: Option<String>,
        event_name: Option<String>,
        metric_name: Option<String>,
        rack_id: Option<String>,
        error: Option<String>,
    }

    #[test]
    fn rack_maintenance_access_token_cleanup_emits_only_on_failure() {
        const METRIC_NAME: &str = "carbide_rack_maintenance_access_token_cleanup_failures_total";

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let rack_id = RackId::from("rack-1");

        carbide_test_support::value_scenarios!(
            run = |delete_fails: bool| {
                let credential_manager = TestCredentialManager::default();
                credential_manager.set_delete_credentials_failure(delete_fails);
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    runtime.block_on(delete_rack_maintenance_access_token(
                        &credential_manager,
                        &rack_id,
                    ));
                })
                .into_iter()
                .filter(|log| {
                    log.field("event_name")
                        == Some("rack_maintenance_access_token_cleanup_failed")
                })
                .collect::<Vec<_>>();
                let log = logs.first();

                AccessTokenCleanupObservation {
                    counter_delta: metrics.counter_delta(METRIC_NAME, &[]),
                    log_count: logs.len(),
                    level: log.map(|log| log.level),
                    metadata_name: log.map(|log| log.metadata_name.clone()),
                    message: log.map(|log| log.message.clone()),
                    event_name: log
                        .and_then(|log| log.field("event_name"))
                        .map(str::to_string),
                    metric_name: log
                        .and_then(|log| log.field("metric_name"))
                        .map(str::to_string),
                    rack_id: log
                        .and_then(|log| log.field("rack_id"))
                        .map(str::to_string),
                    error: log.and_then(|log| log.field("error")).map(str::to_string),
                }
            };
            "credential cleanup outcome" {
                false => AccessTokenCleanupObservation {
                    counter_delta: 0.0,
                    log_count: 0,
                    level: None,
                    metadata_name: None,
                    message: None,
                    event_name: None,
                    metric_name: None,
                    rack_id: None,
                    error: None,
                },
                true => AccessTokenCleanupObservation {
                    counter_delta: 1.0,
                    log_count: 1,
                    level: Some(tracing::Level::WARN),
                    metadata_name: Some(
                        "rack_maintenance_access_token_cleanup_failed".to_string(),
                    ),
                    message: Some(
                        "failed to delete rack maintenance access token".to_string(),
                    ),
                    event_name: Some(
                        "rack_maintenance_access_token_cleanup_failed".to_string(),
                    ),
                    metric_name: Some(METRIC_NAME.to_string()),
                    rack_id: Some("rack-1".to_string()),
                    error: Some(
                        "Secrets operation failed: test credential delete failure".to_string(),
                    ),
                },
            }
        );
    }

    #[test]
    fn profile_hardware_type_or_any_defaults_missing_values_to_any() {
        assert_eq!(profile_hardware_type_or_any(None), "any");
        assert_eq!(
            profile_hardware_type_or_any(Some(&RackProfile::default())),
            "any"
        );

        let profile = RackProfile {
            rack_hardware_type: Some(RackHardwareType::from("gb200")),
            ..Default::default()
        };

        assert_eq!(profile_hardware_type_or_any(Some(&profile)), "gb200");
    }

    #[test]
    fn filter_inventory_by_scope_full_rack_keeps_all_devices() {
        let inventory = sample_inventory();
        let filtered = filter_inventory_by_scope(inventory, &MaintenanceScope::default());

        assert_eq!(filtered.machine_ids.len(), 2);
        assert_eq!(filtered.machines.len(), 2);
        assert_eq!(filtered.switch_ids.len(), 2);
        assert_eq!(filtered.switches.len(), 2);
    }

    #[test]
    fn filter_inventory_by_scope_machines_only() {
        let machine_id = test_machine_id(1);
        let scope = MaintenanceScope {
            machine_ids: vec![machine_id],
            ..Default::default()
        };
        let filtered = filter_inventory_by_scope(sample_inventory(), &scope);

        assert_eq!(filtered.machine_ids, vec![machine_id]);
        assert_eq!(filtered.machines.len(), 1);
        assert_eq!(filtered.machines[0].node_id, machine_id.to_string());
        assert!(filtered.switch_ids.is_empty());
        assert!(filtered.switches.is_empty());
    }

    #[test]
    fn filter_inventory_by_scope_switches_only() {
        let switch_id = test_switch_id(3);
        let scope = MaintenanceScope {
            switch_ids: vec![switch_id],
            ..Default::default()
        };
        let filtered = filter_inventory_by_scope(sample_inventory(), &scope);

        assert!(filtered.machine_ids.is_empty());
        assert!(filtered.machines.is_empty());
        assert_eq!(filtered.switch_ids, vec![switch_id]);
        assert_eq!(filtered.switches.len(), 1);
        assert_eq!(filtered.switches[0].node_id, switch_id.to_string());
    }

    #[test]
    fn filter_inventory_by_scope_machines_and_switches() {
        let machine_id = test_machine_id(2);
        let switch_id = test_switch_id(4);
        let scope = MaintenanceScope {
            machine_ids: vec![machine_id],
            switch_ids: vec![switch_id],
            ..Default::default()
        };
        let filtered = filter_inventory_by_scope(sample_inventory(), &scope);

        assert_eq!(filtered.machine_ids, vec![machine_id]);
        assert_eq!(filtered.machines.len(), 1);
        assert_eq!(filtered.machines[0].node_id, machine_id.to_string());
        assert_eq!(filtered.switch_ids, vec![switch_id]);
        assert_eq!(filtered.switches.len(), 1);
        assert_eq!(filtered.switches[0].node_id, switch_id.to_string());
    }

    #[test]
    fn filter_inventory_by_scope_excludes_unknown_device_ids() {
        let machine_id = test_machine_id(1);
        let switch_id = test_switch_id(3);
        let scope = MaintenanceScope {
            machine_ids: vec![machine_id],
            switch_ids: vec![switch_id],
            ..Default::default()
        };
        let mut inventory = sample_inventory();
        inventory
            .machines
            .push(test_device_info("not-a-machine-id"));
        inventory.switches.push(test_device_info("not-a-switch-id"));

        let filtered = filter_inventory_by_scope(inventory, &scope);

        assert_eq!(filtered.machine_ids, vec![machine_id]);
        assert_eq!(filtered.machines.len(), 1);
        assert_eq!(filtered.machines[0].node_id, machine_id.to_string());
        assert_eq!(filtered.switch_ids, vec![switch_id]);
        assert_eq!(filtered.switches.len(), 1);
        assert_eq!(filtered.switches[0].node_id, switch_id.to_string());
    }

    #[test]
    fn firmware_device_status_uses_batch_error_when_child_job_missing() {
        let status = firmware_device_status(
            test_device_info("node-1"),
            Some("parent-job".to_string()),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            Some("invalid SOT JSON"),
        );

        assert_eq!(status.status, "failed");
        assert_eq!(status.error_message.as_deref(), Some("invalid SOT JSON"));
    }

    #[test]
    fn test_summarize_firmware_outcomes_from_controller_states() {
        check_values(
            [
                Check {
                    scenario: "any waiting keeps rack waiting",
                    input: vec![
                        DeviceFirmwareOutcome::Completed,
                        DeviceFirmwareOutcome::Waiting,
                        DeviceFirmwareOutcome::Failed,
                    ],
                    expect: DeviceFirmwareProgress::Waiting {
                        pending: 1,
                        total: 3,
                        completed: 1,
                        failed: 1,
                    },
                },
                Check {
                    scenario: "all completed advances",
                    input: vec![
                        DeviceFirmwareOutcome::Completed,
                        DeviceFirmwareOutcome::Completed,
                    ],
                    expect: DeviceFirmwareProgress::Completed {
                        completed: 2,
                        total: 2,
                    },
                },
                Check {
                    scenario: "any failed without waiting errors",
                    input: vec![
                        DeviceFirmwareOutcome::Completed,
                        DeviceFirmwareOutcome::Failed,
                    ],
                    expect: DeviceFirmwareProgress::Failed {
                        failed: 1,
                        total: 2,
                    },
                },
            ],
            |outcomes| summarize_firmware_outcomes(&outcomes),
        );
    }

    /// A firmware-upgrade activity with no version/components/force, the form
    /// used by the maintenance-state transition tables.
    fn firmware_upgrade() -> MaintenanceActivity {
        MaintenanceActivity::FirmwareUpgrade {
            firmware_version: None,
            components: vec![],
            force_update: false,
        }
    }

    fn nvos_update() -> MaintenanceActivity {
        MaintenanceActivity::NvosUpdate {
            config_json: r#"{"Id":"fw-nvos"}"#.into(),
        }
    }

    fn scope_of(activities: Vec<MaintenanceActivity>) -> MaintenanceScope {
        MaintenanceScope {
            activities,
            ..Default::default()
        }
    }

    fn firmware_start() -> RackMaintenanceState {
        RackMaintenanceState::FirmwareUpgrade {
            rack_firmware_upgrade: FirmwareUpgradeState::Start,
        }
    }

    fn configure_start() -> RackMaintenanceState {
        RackMaintenanceState::ConfigureNmxCluster {
            configure_nmx_cluster: ConfigureNmxClusterState::Start,
        }
    }

    fn nvos_start() -> RackMaintenanceState {
        RackMaintenanceState::NVOSUpdate {
            nvos_update: NvosUpdateState::Start,
        }
    }

    fn powering_on() -> RackMaintenanceState {
        RackMaintenanceState::PowerSequence {
            rack_power: RackPowerState::PoweringOn,
        }
    }

    // ── current-state scope enforcement ─────────────────────────────────

    #[test]
    fn test_next_state_if_activity_not_requested() {
        check_values(
            [
                Check {
                    scenario: "requested firmware continues",
                    input: (firmware_start(), scope_of(vec![firmware_upgrade()])),
                    expect: None,
                },
                Check {
                    scenario: "unrequested firmware skips to configure",
                    input: (
                        firmware_start(),
                        scope_of(vec![MaintenanceActivity::ConfigureNmxCluster]),
                    ),
                    expect: Some(configure_start()),
                },
                Check {
                    scenario: "requested nvos continues",
                    input: (nvos_start(), scope_of(vec![nvos_update()])),
                    expect: None,
                },
                Check {
                    scenario: "unrequested nvos skips to configure",
                    input: (
                        nvos_start(),
                        scope_of(vec![MaintenanceActivity::ConfigureNmxCluster]),
                    ),
                    expect: Some(configure_start()),
                },
                Check {
                    scenario: "requested configure continues",
                    input: (
                        configure_start(),
                        scope_of(vec![MaintenanceActivity::ConfigureNmxCluster]),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "unrequested configure skips to power",
                    input: (
                        configure_start(),
                        scope_of(vec![MaintenanceActivity::PowerSequence]),
                    ),
                    expect: Some(powering_on()),
                },
                Check {
                    scenario: "requested power continues",
                    input: (
                        powering_on(),
                        scope_of(vec![MaintenanceActivity::PowerSequence]),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "unrequested power skips to completed",
                    input: (powering_on(), scope_of(vec![firmware_upgrade()])),
                    expect: Some(RackMaintenanceState::Completed),
                },
                Check {
                    scenario: "completed always continues",
                    input: (
                        RackMaintenanceState::Completed,
                        scope_of(vec![firmware_upgrade()]),
                    ),
                    expect: None,
                },
            ],
            |(maintenance_state, scope)| {
                next_state_if_activity_not_requested(&maintenance_state, &scope)
            },
        );
    }

    // ── first_maintenance_state ─────────────────────────────────────────

    #[test]
    fn test_first_maintenance_state() {
        check_values(
            [
                Check {
                    scenario: "all activities -> firmware first",
                    input: MaintenanceScope::default(),
                    expect: firmware_start(),
                },
                Check {
                    scenario: "only firmware -> firmware",
                    input: scope_of(vec![firmware_upgrade()]),
                    expect: firmware_start(),
                },
                Check {
                    scenario: "only configure -> configure",
                    input: scope_of(vec![MaintenanceActivity::ConfigureNmxCluster]),
                    expect: configure_start(),
                },
                Check {
                    scenario: "only nvos -> nvos",
                    input: scope_of(vec![nvos_update()]),
                    expect: nvos_start(),
                },
                Check {
                    scenario: "only power sequence -> power",
                    input: scope_of(vec![MaintenanceActivity::PowerSequence]),
                    expect: powering_on(),
                },
                Check {
                    scenario: "configure and power -> configure first",
                    input: scope_of(vec![
                        MaintenanceActivity::ConfigureNmxCluster,
                        MaintenanceActivity::PowerSequence,
                    ]),
                    expect: configure_start(),
                },
            ],
            |scope| first_maintenance_state(&scope),
        );
    }

    // ── next_state_after_firmware ───────────────────────────────────────

    #[test]
    fn test_next_state_after_firmware() {
        check_values(
            [
                // All activities: no explicit NVOS JSON, so NVOS is skipped and
                // the scope falls through to ConfigureNmxCluster.
                Check {
                    scenario: "all activities skips nvos without explicit json -> configure",
                    input: MaintenanceScope::default(),
                    expect: configure_start(),
                },
                Check {
                    scenario: "firmware and power, no configure -> power",
                    input: scope_of(vec![firmware_upgrade(), MaintenanceActivity::PowerSequence]),
                    expect: powering_on(),
                },
                Check {
                    scenario: "only firmware -> completed",
                    input: scope_of(vec![firmware_upgrade()]),
                    expect: RackMaintenanceState::Completed,
                },
                Check {
                    scenario: "explicit nvos preserves requested firmware -> nvos",
                    input: scope_of(vec![firmware_upgrade(), nvos_update()]),
                    expect: nvos_start(),
                },
            ],
            |scope| next_state_after_firmware(&scope),
        );
    }

    // ── next_state_after_nvos ──────────────────────────────────────────

    #[test]
    fn test_next_state_after_nvos() {
        check_values(
            [
                Check {
                    scenario: "all activities -> configure",
                    input: MaintenanceScope::default(),
                    expect: configure_start(),
                },
                Check {
                    scenario: "nvos and power, no configure -> power",
                    input: scope_of(vec![nvos_update(), MaintenanceActivity::PowerSequence]),
                    expect: powering_on(),
                },
            ],
            |scope| next_state_after_nvos(&scope),
        );
    }

    // ── next_state_after_configure ──────────────────────────────────────

    #[test]
    fn test_next_state_after_configure() {
        check_values(
            [
                Check {
                    scenario: "all activities -> power",
                    input: MaintenanceScope::default(),
                    expect: powering_on(),
                },
                Check {
                    scenario: "firmware and configure, no power -> completed",
                    input: scope_of(vec![
                        firmware_upgrade(),
                        MaintenanceActivity::ConfigureNmxCluster,
                    ]),
                    expect: RackMaintenanceState::Completed,
                },
            ],
            |scope| next_state_after_configure(&scope),
        );
    }
}
