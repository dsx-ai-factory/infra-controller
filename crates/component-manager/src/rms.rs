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

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use carbide_instrument::{Event, emit, red};
use carbide_rack::firmware_object::{
    ANY_RACK_HARDWARE_TYPE, profile_hardware_type_wire_value, rms_access_token_or_noauth,
};
use carbide_rack::firmware_update::{build_new_node_info, firmware_type_for_profile};
use carbide_rack::rms_node_type::{
    RmsNodeIdentity, compute_node_identity_for_profile,
    firmware_object_component_filters_for_node_identities, power_shelf_node_identity_for_profile,
    switch_node_identity_for_profile,
};
use carbide_secrets::credentials::Credentials;
use carbide_uuid::rack::RackProfileId;
use librms::protos::{rack_manager as rms, rack_manager_v2 as rms_v2};
use librms::{RackManagerError, RmsApi};
use mac_address::MacAddress;
use model::component_manager::{
    ComputeTrayComponent, ConfigureSwitchCertificateState, FirmwareState, NvSwitchComponent,
    PowerAction, PowerShelfComponent,
};
use model::rack::{NvosUpdateJob, NvosUpdateSwitchStatus};
use model::rack_type::{RackHardwareTopology, RackProfile, RackProfileConfig};
use model::switch::{FabricManagerState, FabricManagerStatus};
use serde::Deserialize;
use sqlx::PgPool;
use tracing::instrument;

use crate::compute_tray_manager::{
    Backend as ComputeTrayBackend, ComputeTrayEndpoint, ComputeTrayFirmwareUpdateStatus,
    ComputeTrayManager, ComputeTrayResult,
};
use crate::config::ComponentManagerConfig;
use crate::error::ComponentManagerError;
use crate::nv_switch_manager::{
    Backend as NvSwitchBackend, ConfigureSwitchCertificateJobStatus, NvSwitchManager,
    ScaleUpFabricManagerJobStatus, ScaleUpFabricResponseStatus, ScaleUpFabricServiceStatuses,
    ScaleUpFabricStatus, ScaleUpFabricSwitchStatus, SwitchComponentResult, SwitchEndpoint,
    SwitchFactoryResetJobStatus, SwitchFactoryResetState, SwitchFirmwareUpdateStatus,
    SwitchPasswordRotationState, SwitchPowerStateResult, SwitchSlotAndTrayResult,
};
use crate::power_shelf_manager::{
    Backend as PowerShelfBackend, PowerShelfComponentResult, PowerShelfEndpoint,
    PowerShelfFirmwareUpdateStatus, PowerShelfFirmwareVersions, PowerShelfManager,
    PowerShelfPowerStateResult,
};
use crate::types::FirmwareUpdateOptions;
use crate::{NvosUpdateManager, NvosUpdateRequest};

/// Common RMS identity needed to address a device in RMS.
#[derive(Clone)]
struct RmsIdentity {
    node_id: String,
    rack_id: String,
    rack_profile_id: Option<RackProfileId>,
}

struct ResolvedRmsNode<'a> {
    identity: &'a RmsIdentity,
    node_identity: RmsNodeIdentity,
}

/// Role for MAC-keyed switch and power shelf lookups.
///
/// Compute trays use `ComputeTrayRmsIdentity` and `resolve_compute_node`
/// because component-manager addresses compute endpoints by BMC IP and also
/// needs the BMC MAC address when building RMS requests.
#[derive(Clone, Copy)]
enum SwitchOrPowerShelfRole {
    PowerShelf,
    Switch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RmsTrackedFirmwareJob {
    FirmwareObject(String),
    SwitchSystemImage(String),
}

// The direct RMS path matches the rack-maintenance flow and applies production
// firmware artifacts only.
const RMS_FIRMWARE_OBJECT_FIRMWARE_TYPE: &str = "prod";
const RMS_SWITCH_SYSTEM_IMAGE_SOFTWARE_TYPE: &str = "prod";
const RMS_FIRMWARE_OBJECT_HARDWARE_TYPE: &str = "any";
const RMS_IDENTITY_LOOKUP_ERROR: &str = "could not resolve RMS identity from database";

/// Validates rack profile fields required by RMS component-manager backends.
///
/// Descriptor-based RMS requests require a product family and the per-role
/// vendor string. Startup validation checks those descriptor inputs without
/// matching the hardware against a fixed enum.
pub fn validate_rms_backend_rack_profiles(
    config: &ComponentManagerConfig,
    rack_profiles: &RackProfileConfig,
) -> Result<(), ComponentManagerError> {
    let compute_uses_rms = matches!(config.compute_tray_backend, ComputeTrayBackend::Rms);
    let switch_uses_rms = matches!(config.nv_switch_backend, NvSwitchBackend::Rms);
    let power_shelf_uses_rms = matches!(config.power_shelf_backend, PowerShelfBackend::Rms);

    if !(compute_uses_rms || switch_uses_rms || power_shelf_uses_rms) {
        return Ok(());
    }

    if rack_profiles.rack_profiles.is_empty() {
        return Err(ComponentManagerError::InvalidArgument(
            "rack_profiles must contain at least one profile when component_manager uses an RMS backend"
                .into(),
        ));
    }

    for (profile_id, profile) in &rack_profiles.rack_profiles {
        if compute_uses_rms {
            compute_node_identity_for_profile(profile).map_err(|error| {
                rms_node_descriptor_config_error(profile_id, "compute", error.to_string())
            })?;
        }

        if switch_uses_rms {
            switch_node_identity_for_profile(profile).map_err(|error| {
                rms_node_descriptor_config_error(profile_id, "switch", error.to_string())
            })?;
        }

        if power_shelf_uses_rms {
            power_shelf_node_identity_for_profile(profile).map_err(|error| {
                rms_node_descriptor_config_error(profile_id, "power shelf", error.to_string())
            })?;
        }
    }

    Ok(())
}

fn rms_node_descriptor_config_error(
    profile_id: &str,
    role: &str,
    error: String,
) -> ComponentManagerError {
    ComponentManagerError::InvalidArgument(format!(
        "rack profile {profile_id} cannot build RMS {role} node descriptor: {error}"
    ))
}

pub struct RmsBackend {
    client: Arc<dyn RmsApi>,
    switch_system_image_client: Option<Arc<dyn RmsSwitchSystemImageStatusApi>>,
    db: PgPool,
    rack_profiles: Arc<RackProfileConfig>,
    /// Tracks firmware update job IDs keyed by device MAC address.
    firmware_jobs: Mutex<HashMap<MacAddress, Vec<RmsTrackedFirmwareJob>>>,
    nvos_password_rotation_enabled: bool,
}

#[async_trait::async_trait]
pub trait RmsSwitchSystemImageStatusApi: Send + Sync + 'static {
    async fn get_switch_system_image_job_status(
        &self,
        cmd: rms::GetSwitchSystemImageJobStatusRequest,
    ) -> Result<rms::GetSwitchSystemImageJobStatusResponse, RackManagerError>;
}

#[async_trait::async_trait]
impl RmsSwitchSystemImageStatusApi for librms::RackManagerApi {
    async fn get_switch_system_image_job_status(
        &self,
        cmd: rms::GetSwitchSystemImageJobStatusRequest,
    ) -> Result<rms::GetSwitchSystemImageJobStatusResponse, RackManagerError> {
        Ok(self.client.get_switch_system_image_job_status(cmd).await?)
    }
}

/// RMS implementation of durable rack-level NVOS operations.
struct RmsNvosUpdateManager {
    client: Arc<dyn RmsApi>,
}

impl crate::nvos_update_manager::sealed::Sealed for RmsNvosUpdateManager {}

#[async_trait::async_trait]
impl NvosUpdateManager for RmsNvosUpdateManager {
    async fn start_nvos_update(
        &self,
        request: NvosUpdateRequest<'_>,
    ) -> Result<NvosUpdateJob, ComponentManagerError> {
        let started_at = chrono::Utc::now();

        let switch_identity = switch_node_identity_for_profile(request.profile)
            .map_err(|error| ComponentManagerError::InvalidArgument(error.to_string()))?;

        let nodes = request
            .switches
            .iter()
            .map(|switch| build_new_node_info(request.rack_id, switch, &switch_identity))
            .collect();

        let hardware_type = profile_hardware_type_wire_value(request.profile);

        let hardware_type = if hardware_type.trim().is_empty() {
            ANY_RACK_HARDWARE_TYPE.to_string()
        } else {
            hardware_type
        };

        let response = self
            .client
            .apply_switch_system_image(rms::ApplySwitchSystemImageRequest {
                rack_id: request.rack_id.to_string(),
                config_json: request.config_json.to_string(),
                access_token: Some(rms_access_token_or_noauth(Some(request.access_token))),
                software_type: firmware_type_for_profile(request.profile).to_string(),
                hardware_type,
                nodes: Some(rms::NodeSet { nodes }),
            })
            .await
            .map_err(|error| match error {
                // RMS validates node descriptor support before creating a job,
                // so InvalidArgument is terminal for the submitted request.
                RackManagerError::ApiInvocationError(status)
                    if status.code() == tonic::Code::InvalidArgument =>
                {
                    ComponentManagerError::InvalidArgument(format!(
                        "failed to submit NVOS update to RMS: {status}"
                    ))
                }
                RackManagerError::ApiInvocationError(status) => ComponentManagerError::Internal(
                    format!("failed to submit NVOS update to RMS: {status}"),
                ),
                error => ComponentManagerError::Internal(format!(
                    "failed to submit NVOS update to RMS: {error}"
                )),
            })?;

        let batch_response = response.response.as_ref();

        let batch_status = batch_response
            .map(|batch_response| batch_response.status)
            .unwrap_or(rms::ReturnCode::Failure as i32);

        let batch_job_id = batch_response
            .map(|batch_response| batch_response.job_id.as_str())
            .unwrap_or_default();

        // A failed batch can still contain accepted jobs. Reject only responses
        // without a durable handle so accepted work remains pollable.
        if batch_status != rms::ReturnCode::Success as i32
            && batch_job_id.is_empty()
            && response.jobs.is_empty()
        {
            let message = batch_response
                .map(|batch_response| batch_response.message.as_str())
                .unwrap_or_default();

            let message = if message.is_empty() {
                "RMS returned failure for ApplySwitchSystemImage".to_string()
            } else {
                message.to_string()
            };

            return Err(ComponentManagerError::RejectedBeforeDispatch(message));
        }

        // RMS may return child handles, a parent handle, or both. Use the parent
        // when a switch has no child handle so every accepted job can be polled.
        let parent_job_id = (!batch_job_id.is_empty()).then(|| batch_job_id.to_string());

        let child_jobs = response
            .jobs
            .iter()
            .map(|child| (child.node_id.clone(), child.job_id.clone()))
            .collect::<HashMap<_, _>>();

        let switches: Vec<_> = request
            .switches
            .into_iter()
            .map(|switch| {
                let mut status = NvosUpdateSwitchStatus {
                    node_id: switch.node_id.clone(),
                    mac: switch.mac,
                    bmc_ip: switch.bmc_ip,
                    nvos_ip: switch.os_ip.unwrap_or_default(),
                    status: "pending".into(),
                    job_id: child_jobs
                        .get(&switch.node_id)
                        .cloned()
                        .or_else(|| parent_job_id.clone()),
                    error_message: None,
                };

                if status.job_id.is_none() {
                    status.status = "failed".into();
                    status.error_message =
                        Some("RMS did not return a switch system image job for this switch".into());
                }

                status
            })
            .collect();

        let total = switches.len();

        let failed = switches
            .iter()
            .filter(|switch| switch.status == "failed")
            .count();

        let all_failed = total > 0 && failed == total;

        Ok(NvosUpdateJob {
            job_id: parent_job_id,
            firmware_id: response.object_id,
            image_filename: response.image_filename,
            local_file_path: String::new(),
            version: None,
            status: Some(
                if total > failed {
                    "in_progress"
                } else if all_failed {
                    "failed"
                } else {
                    "completed"
                }
                .into(),
            ),
            started_at: Some(started_at),
            completed_at: all_failed.then(chrono::Utc::now),
            switches,
        })
    }

    async fn get_nvos_update_status(
        &self,
        job: &NvosUpdateJob,
    ) -> Result<NvosUpdateJob, ComponentManagerError> {
        let mut updated = job.clone();
        let parent_job_id = updated.job_id.clone();

        for switch in updated.all_switches_mut() {
            if matches!(switch.status.as_str(), "completed" | "failed") {
                continue;
            }

            let Some(job_id) = switch.job_id.clone().or_else(|| parent_job_id.clone()) else {
                switch.status = "failed".into();

                if switch.error_message.is_none() {
                    switch.error_message = Some("Switch has no NVOS job ID to poll".into());
                }

                continue;
            };

            let response = self
                .client
                .get_switch_system_image_job_status(rms::GetSwitchSystemImageJobStatusRequest {
                    job_id: job_id.clone(),
                })
                .await;

            apply_nvos_job_status_response(switch, &job_id, response);
        }

        let total = updated.all_switches().count();

        let completed = updated
            .all_switches()
            .filter(|switch| switch.status == "completed")
            .count();

        let failed = updated
            .all_switches()
            .filter(|switch| switch.status == "failed")
            .count();

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

        if total > 0 && terminal == total {
            updated.completed_at.get_or_insert_with(chrono::Utc::now);
        } else {
            updated.completed_at = None;
        }

        Ok(updated)
    }
}

fn apply_nvos_job_status_response(
    switch: &mut NvosUpdateSwitchStatus,
    job_id: &str,
    response: Result<rms::GetSwitchSystemImageJobStatusResponse, RackManagerError>,
) {
    match response {
        Ok(response) if response.status == rms::ReturnCode::Success as i32 => {
            if !response.node_id.is_empty() {
                switch.node_id = response.node_id.clone();
            }

            match map_rms_switch_system_image_job_state(&response.state) {
                FirmwareState::Queued => {
                    switch.status = "pending".into();
                    switch.error_message = None;
                }
                FirmwareState::InProgress => {
                    switch.status = "in_progress".into();
                    switch.error_message = None;
                }
                FirmwareState::Completed => {
                    switch.status = "completed".into();
                    switch.error_message = None;
                }
                FirmwareState::Failed => {
                    switch.status = "failed".into();

                    switch.error_message = Some(if response.error_message.is_empty() {
                        response.message
                    } else {
                        response.error_message
                    });
                }
                FirmwareState::Unknown | FirmwareState::Verifying | FirmwareState::Cancelled => {
                    let state = response.state.to_ascii_lowercase();

                    tracing::warn!(
                        job_id = %job_id,
                        job_state = %state,
                        "RMS returned unknown switch system image job state; keeping previous status",
                    );

                    switch.error_message =
                        Some(format!("Unknown RMS switch image job state {}", state));
                }
            }
        }
        Ok(response) => {
            let message = if response.error_message.is_empty() {
                if response.message.is_empty() {
                    format!("RMS could not report status for NVOS job {}", job_id)
                } else {
                    response.message
                }
            } else {
                response.error_message
            };

            tracing::warn!(
                job_id = %job_id,
                job_status = response.status,
                error = %message,
                "RMS returned a non-success switch image job status lookup; retrying later",
            );

            switch.error_message = Some(message);
        }
        Err(error) => {
            let cause = match error {
                RackManagerError::ApiInvocationError(status) => status.to_string(),
                error => error.to_string(),
            };

            tracing::warn!(
                job_id = %job_id,
                error = %cause,
                "Transient RMS switch image job polling error; retrying later",
            );

            switch.error_message = Some(cause);
        }
    }
}

/// Creates the RMS implementation of durable rack-level NVOS operations.
pub fn rms_nvos_update_manager(client: Arc<dyn RmsApi>) -> impl NvosUpdateManager {
    RmsNvosUpdateManager { client }
}

impl std::fmt::Debug for RmsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RmsBackend")
            .field("client", &"<RmsApi>")
            .finish()
    }
}

impl RmsBackend {
    pub fn new(
        client: Arc<dyn RmsApi>,
        switch_system_image_client: Option<Arc<dyn RmsSwitchSystemImageStatusApi>>,
        db: PgPool,
        rack_profiles: Arc<RackProfileConfig>,
        nvos_password_rotation_enabled: bool,
    ) -> Self {
        Self {
            client,
            switch_system_image_client,
            db,
            rack_profiles,
            firmware_jobs: Mutex::new(HashMap::new()),
            nvos_password_rotation_enabled,
        }
    }

    fn rack_profile<'a>(
        &'a self,
        identity: &RmsIdentity,
    ) -> Result<&'a RackProfile, ComponentManagerError> {
        let Some(rack_profile_id) = &identity.rack_profile_id else {
            return Err(ComponentManagerError::InvalidArgument(format!(
                "rack {} has no rack_profile_id for RMS node descriptor resolution",
                identity.rack_id
            )));
        };

        self.rack_profiles
            .get(rack_profile_id.as_str())
            .ok_or_else(|| {
                ComponentManagerError::InvalidArgument(format!(
                    "rack profile {} is not configured for RMS node descriptor resolution",
                    rack_profile_id
                ))
            })
    }

    fn resolve_switch_or_power_shelf_node<'a>(
        &self,
        identities: &'a HashMap<MacAddress, RmsIdentity>,
        device_mac: MacAddress,
        role: SwitchOrPowerShelfRole,
    ) -> Result<ResolvedRmsNode<'a>, String> {
        let Some(identity) = identities.get(&device_mac) else {
            return Err(RMS_IDENTITY_LOOKUP_ERROR.to_owned());
        };

        let profile = self
            .rack_profile(identity)
            .map_err(|error| error.to_string())?;

        let node_identity = match role {
            SwitchOrPowerShelfRole::PowerShelf => power_shelf_node_identity_for_profile(profile),
            SwitchOrPowerShelfRole::Switch => switch_node_identity_for_profile(profile),
        }
        .map_err(|error| error.to_string())?;

        Ok(ResolvedRmsNode {
            identity,
            node_identity,
        })
    }

    fn resolve_compute_node<'a>(
        &self,
        identity: &'a ComputeTrayRmsIdentity,
    ) -> Result<ResolvedRmsNode<'a>, String> {
        let profile = self
            .rack_profile(&identity.identity)
            .map_err(|error| error.to_string())?;

        let node_identity =
            compute_node_identity_for_profile(profile).map_err(|error| error.to_string())?;

        Ok(ResolvedRmsNode {
            identity: &identity.identity,
            node_identity,
        })
    }

    async fn resolve_scale_up_fabric_nodes(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<rms::NodeInfo>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|endpoint| endpoint.bmc_mac).collect();
        let identities = resolve_switch_identities(&self.db, &macs).await?;
        let hostnames = resolve_switch_machine_interface_hostnames(&self.db, endpoints).await?;
        let mut nodes = Vec::with_capacity(endpoints.len());

        for endpoint in endpoints {
            let resolved = self
                .resolve_switch_or_power_shelf_node(
                    &identities,
                    endpoint.bmc_mac,
                    SwitchOrPowerShelfRole::Switch,
                )
                .map_err(ComponentManagerError::Internal)?;

            nodes.push(build_switch_node_info(
                endpoint,
                &resolved,
                hostnames.get(&endpoint.nvos_mac).cloned(),
            ));
        }

        Ok(nodes)
    }
}

/// Resolve power shelf MAC addresses to RMS identities via the api-db layer.
async fn resolve_power_shelf_identities(
    db: &PgPool,
    macs: &[MacAddress],
) -> Result<HashMap<MacAddress, RmsIdentity>, ComponentManagerError> {
    let rows = db::power_shelf::find_rms_identities_by_macs(db, macs)
        .await
        .map_err(|e| {
            ComponentManagerError::Internal(format!(
                "failed to resolve power shelf RMS identities: {e}"
            ))
        })?;

    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let Some(rack_id) = row.rack_id else {
            tracing::warn!(bmc_mac_address = %row.bmc_mac_address, "power shelf has no rack_id, skipping");
            continue;
        };
        map.insert(
            row.bmc_mac_address,
            RmsIdentity {
                node_id: row.id,
                rack_id: rack_id.to_string(),
                rack_profile_id: row.rack_profile_id,
            },
        );
    }
    Ok(map)
}

/// Resolved RMS identity for a compute tray, keyed by BMC IP.
struct ComputeTrayRmsIdentity {
    identity: RmsIdentity,
    bmc_mac: MacAddress,
}

/// Resolve compute tray BMC IP addresses to RMS identities via the api-db layer.
async fn resolve_compute_tray_identities(
    db: &PgPool,
    bmc_ips: &[IpAddr],
) -> Result<HashMap<IpAddr, ComputeTrayRmsIdentity>, ComponentManagerError> {
    let rows = db::machine::find_rms_identities_by_bmc_ips(db, bmc_ips)
        .await
        .map_err(|e| {
            ComponentManagerError::Internal(format!(
                "failed to resolve compute tray RMS identities: {e}"
            ))
        })?;

    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let Some(rack_id) = row.rack_id else {
            tracing::warn!(bmc_ip_address = %row.bmc_ip, "compute tray has no rack_id, skipping");
            continue;
        };
        map.insert(
            row.bmc_ip,
            ComputeTrayRmsIdentity {
                identity: RmsIdentity {
                    node_id: row.id,
                    rack_id: rack_id.to_string(),
                    rack_profile_id: row.rack_profile_id,
                },
                bmc_mac: row.bmc_mac_address,
            },
        );
    }
    Ok(map)
}

/// Resolve switch MAC addresses to RMS identities via the api-db layer.
async fn resolve_switch_identities(
    db: &PgPool,
    macs: &[MacAddress],
) -> Result<HashMap<MacAddress, RmsIdentity>, ComponentManagerError> {
    let rows = db::switch::find_rms_identities_by_macs(db, macs)
        .await
        .map_err(|e| {
            ComponentManagerError::Internal(format!("failed to resolve switch RMS identities: {e}"))
        })?;

    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let Some(rack_id) = row.rack_id else {
            tracing::warn!(bmc_mac_address = %row.bmc_mac_address, "switch has no rack_id, skipping");
            continue;
        };
        map.insert(
            row.bmc_mac_address,
            RmsIdentity {
                node_id: row.id,
                rack_id: rack_id.to_string(),
                rack_profile_id: row.rack_profile_id,
            },
        );
    }
    Ok(map)
}

fn to_rms_power_operation(action: PowerAction) -> i32 {
    match action {
        PowerAction::On => rms::PowerOperation::On as i32,
        PowerAction::GracefulShutdown | PowerAction::ForceOff => rms::PowerOperation::Off as i32,
        PowerAction::GracefulRestart | PowerAction::ForceRestart | PowerAction::AcPowercycle => {
            rms::PowerOperation::Reset as i32
        }
    }
}

fn map_rms_firmware_job_state(state: i32) -> FirmwareState {
    match rms::FirmwareJobState::try_from(state) {
        Ok(rms::FirmwareJobState::Queued) => FirmwareState::Queued,
        Ok(rms::FirmwareJobState::Running) => FirmwareState::InProgress,
        Ok(rms::FirmwareJobState::Completed) => FirmwareState::Completed,
        Ok(rms::FirmwareJobState::Failed) => FirmwareState::Failed,
        _ => FirmwareState::Unknown,
    }
}

fn map_rms_switch_system_image_job_state(state: &str) -> FirmwareState {
    match state.to_ascii_lowercase().as_str() {
        "queued" | "pending" => FirmwareState::Queued,
        "running" | "in_progress" | "active" => FirmwareState::InProgress,
        "verifying" | "verify" | "validating" | "validation" => FirmwareState::Verifying,
        "completed" | "success" | "done" => FirmwareState::Completed,
        "failed" | "error" => FirmwareState::Failed,
        "cancelled" | "canceled" => FirmwareState::Cancelled,
        _ => FirmwareState::Unknown,
    }
}

fn aggregate_firmware_job_states(states: &[FirmwareState]) -> FirmwareState {
    if states.is_empty() {
        return FirmwareState::Unknown;
    }
    if states.contains(&FirmwareState::Failed) {
        return FirmwareState::Failed;
    }
    if states.contains(&FirmwareState::Cancelled) {
        return FirmwareState::Cancelled;
    }
    if states.contains(&FirmwareState::InProgress) {
        return FirmwareState::InProgress;
    }
    if states.contains(&FirmwareState::Verifying) {
        return FirmwareState::Verifying;
    }
    if states.contains(&FirmwareState::Queued) {
        return FirmwareState::Queued;
    }
    if states.contains(&FirmwareState::Unknown) {
        return FirmwareState::Unknown;
    }
    if states
        .iter()
        .all(|state| *state == FirmwareState::Completed)
    {
        FirmwareState::Completed
    } else {
        FirmwareState::Unknown
    }
}

/// Default BMC HTTPS port used when populating `rms::Endpoint` for power
/// shelves. Mirrors the value used by `crate::power_shelf_controller::maintenance`.
const POWER_SHELF_BMC_PORT: u32 = 443;

/// Build the `rms::NodeInfo` describing a power shelf for inclusion in a
/// `BatchSetPowerState` request. The caller-supplied variant of the
/// RPC requires the BMC connection details inline rather than relying on
/// RMS's inventory; power shelves do not expose a host endpoint.
fn build_power_shelf_node_info(
    ep: &PowerShelfEndpoint,
    resolved: &ResolvedRmsNode<'_>,
) -> rms::NodeInfo {
    let mut node = rms::NodeInfo {
        node_id: resolved.identity.node_id.clone(),
        rack_id: resolved.identity.rack_id.clone(),
        r#type: None,
        bmc_endpoint: Some(rms::Endpoint {
            interface: Some(rms::NetworkInterface {
                ip_address: ep.pmc_ip.to_string(),
                mac_address: ep.pmc_mac.to_string(),
                host_name: None,
            }),
            port: POWER_SHELF_BMC_PORT,
            credentials: Some(credentials_to_rms(&ep.pmc_credentials)),
        }),
        host_endpoint: None,
        node_descriptor: None,
    };

    resolved.node_identity.apply_to_node_info(&mut node);

    node
}

#[async_trait::async_trait]
impl PowerShelfManager for RmsBackend {
    fn name(&self) -> &str {
        "rms"
    }

    fn supports_firmware_object_json(&self) -> bool {
        true
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn power_control(
        &self,
        endpoints: &[PowerShelfEndpoint],
        action: PowerAction,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|ep| ep.pmc_mac).collect();
        let ids = resolve_power_shelf_identities(&self.db, &macs).await?;
        let operation = to_rms_power_operation(action);
        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            let resolved = match self.resolve_switch_or_power_shelf_node(
                &ids,
                ep.pmc_mac,
                SwitchOrPowerShelfRole::PowerShelf,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    results.push(PowerShelfComponentResult {
                        pmc_mac: ep.pmc_mac,
                        success: false,
                        error: Some(error),
                    });
                    continue;
                }
            };

            let device = build_power_shelf_node_info(ep, &resolved);

            let request = rms::BatchSetPowerStateRequest {
                nodes: Some(rms::NodeSet {
                    nodes: vec![device],
                }),
                operation,
            };

            match red::instrumented(
                "rms",
                "batch_set_power_state",
                self.client.batch_set_power_state(request),
            )
            .await
            {
                Ok(response) => {
                    let (success, error) =
                        summarize_power_batch(response.response.unwrap_or_default());
                    results.push(PowerShelfComponentResult {
                        pmc_mac: ep.pmc_mac,
                        success,
                        error,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        pmc_mac_address = %ep.pmc_mac,
                        error = %e,
                        "RMS power control failed for power shelf"
                    );
                    results.push(PowerShelfComponentResult {
                        pmc_mac: ep.pmc_mac,
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(results)
    }

    #[instrument(skip(self, target_version, options), fields(backend = "rms", force_update = options.force_update))]
    async fn update_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
        target_version: &str,
        components: &[PowerShelfComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|ep| ep.pmc_mac).collect();
        let ids = resolve_power_shelf_identities(&self.db, &macs).await?;
        let component_filters = power_shelf_firmware_object_component_filters(components);

        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            let resolved = match self.resolve_switch_or_power_shelf_node(
                &ids,
                ep.pmc_mac,
                SwitchOrPowerShelfRole::PowerShelf,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    results.push(PowerShelfComponentResult {
                        pmc_mac: ep.pmc_mac,
                        success: false,
                        error: Some(error),
                    });
                    continue;
                }
            };

            let device = build_power_shelf_node_info(ep, &resolved);

            let request = match apply_firmware_object_request(
                device,
                &resolved,
                target_version,
                options,
                &component_filters,
            ) {
                Ok(request) => request,
                Err(e) => {
                    results.push(PowerShelfComponentResult {
                        pmc_mac: ep.pmc_mac,
                        success: false,
                        error: Some(e.to_string()),
                    });
                    continue;
                }
            };

            match red::instrumented(
                "rms",
                "apply_firmware_object",
                self.client.apply_firmware_object(request),
            )
            .await
            {
                Ok(response) => {
                    let (success, error, job_id) = summarize_firmware_object_apply_response(
                        response,
                        &resolved.identity.node_id,
                    );

                    if success {
                        if let Some(job_id) = job_id {
                            self.firmware_jobs.lock().unwrap().insert(
                                ep.pmc_mac,
                                vec![RmsTrackedFirmwareJob::FirmwareObject(job_id)],
                            );
                        } else {
                            self.firmware_jobs.lock().unwrap().remove(&ep.pmc_mac);
                        }
                    } else {
                        self.firmware_jobs.lock().unwrap().remove(&ep.pmc_mac);
                    }

                    results.push(PowerShelfComponentResult {
                        pmc_mac: ep.pmc_mac,
                        success,
                        error,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        pmc_mac_address = %ep.pmc_mac,
                        error = %e,
                        "RMS firmware update failed for power shelf"
                    );
                    results.push(PowerShelfComponentResult {
                        pmc_mac: ep.pmc_mac,
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(results)
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn get_firmware_status(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareUpdateStatus>, ComponentManagerError> {
        // Snapshot job IDs under the lock, then release it before making
        // async RMS calls (avoids holding a std::sync::Mutex across await).
        let endpoint_jobs: Vec<(MacAddress, Option<String>)> = {
            let jobs = self.firmware_jobs.lock().unwrap();
            endpoints
                .iter()
                .map(|ep| {
                    let job_id = jobs.get(&ep.pmc_mac).and_then(|jobs| {
                        jobs.iter().find_map(|job| match job {
                            RmsTrackedFirmwareJob::FirmwareObject(job_id) => Some(job_id.clone()),
                            RmsTrackedFirmwareJob::SwitchSystemImage(_) => None,
                        })
                    });
                    (ep.pmc_mac, job_id)
                })
                .collect()
        };

        let mut statuses = Vec::with_capacity(endpoints.len());

        for (pmc_mac, job_id) in &endpoint_jobs {
            let Some(job_id) = job_id else {
                statuses.push(PowerShelfFirmwareUpdateStatus {
                    pmc_mac: *pmc_mac,
                    state: FirmwareState::Unknown,
                    target_version: String::new(),
                    error: Some("no firmware job tracked for this power shelf".into()),
                });
                continue;
            };

            let request = rms::GetFirmwareJobStatusRequest {
                job_id: job_id.clone(),
            };

            match red::instrumented(
                "rms",
                "get_firmware_job_status",
                self.client.get_firmware_job_status(request),
            )
            .await
            {
                Ok(response) => {
                    let status_success = response.status == rms::ReturnCode::Success as i32;
                    let state = if status_success {
                        map_rms_firmware_job_state(response.job_state)
                    } else {
                        FirmwareState::Unknown
                    };
                    let error = if response.error_message.is_empty() {
                        (!status_success).then(|| {
                            format!("RMS could not report status for firmware job {job_id}")
                        })
                    } else {
                        Some(response.error_message)
                    };
                    statuses.push(PowerShelfFirmwareUpdateStatus {
                        pmc_mac: *pmc_mac,
                        state,
                        target_version: String::new(),
                        error,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        pmc_mac_address = %pmc_mac,
                        job_id = %job_id,
                        error = %e,
                        "RMS firmware job status query failed"
                    );
                    statuses.push(PowerShelfFirmwareUpdateStatus {
                        pmc_mac: *pmc_mac,
                        state: FirmwareState::Unknown,
                        target_version: String::new(),
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(statuses)
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn list_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareVersions>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|ep| ep.pmc_mac).collect();
        let ids = resolve_power_shelf_identities(&self.db, &macs).await?;
        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            let Some(identity) = ids.get(&ep.pmc_mac) else {
                results.push(PowerShelfFirmwareVersions {
                    pmc_mac: ep.pmc_mac,
                    versions: vec![],
                    error: Some(RMS_IDENTITY_LOOKUP_ERROR.into()),
                });
                continue;
            };

            let request = rms::GetNodeFirmwareInventoryRequest {
                node_id: identity.node_id.clone(),
                rack_id: identity.rack_id.clone(),
            };

            match red::instrumented(
                "rms",
                "get_node_firmware_inventory",
                self.client.get_node_firmware_inventory(request),
            )
            .await
            {
                Ok(response) => {
                    if response.status != rms::ReturnCode::Success as i32 {
                        results.push(PowerShelfFirmwareVersions {
                            pmc_mac: ep.pmc_mac,
                            versions: vec![],
                            error: Some("RMS firmware inventory query failed".into()),
                        });
                        continue;
                    }

                    let versions = response
                        .firmware_list
                        .into_iter()
                        .map(|fi| fi.version)
                        .collect();

                    results.push(PowerShelfFirmwareVersions {
                        pmc_mac: ep.pmc_mac,
                        versions,
                        error: None,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        pmc_mac_address = %ep.pmc_mac,
                        error = %e,
                        "RMS firmware inventory query failed for power shelf"
                    );
                    results.push(PowerShelfFirmwareVersions {
                        pmc_mac: ep.pmc_mac,
                        versions: vec![],
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(results)
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn get_power_state(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfPowerStateResult>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|ep| ep.pmc_mac).collect();
        let ids = resolve_power_shelf_identities(&self.db, &macs).await?;
        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            let resolved = match self.resolve_switch_or_power_shelf_node(
                &ids,
                ep.pmc_mac,
                SwitchOrPowerShelfRole::PowerShelf,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    results.push(PowerShelfPowerStateResult {
                        pmc_mac: ep.pmc_mac,
                        power_state: None,
                        error: Some(error),
                    });
                    continue;
                }
            };

            let device = build_power_shelf_node_info(ep, &resolved);

            let observed = query_rms_power_state(
                self.client.as_ref(),
                device,
                &resolved.identity.node_id,
                ep.pmc_mac,
                "power shelf",
            )
            .await;
            results.push(PowerShelfPowerStateResult {
                pmc_mac: ep.pmc_mac,
                power_state: observed.power_state,
                error: observed.error,
            });
        }

        Ok(results)
    }
}

/// Query all firmware object IDs from RMS.
async fn list_firmware_object_ids(
    client: &dyn RmsApi,
) -> Result<Vec<String>, ComponentManagerError> {
    let response = red::instrumented(
        "rms",
        "list_firmware_objects",
        client.list_firmware_objects(rms::ListFirmwareObjectsRequest {
            only_available: false,
            hardware_type: String::new(),
        }),
    )
    .await
    .map_err(|e| {
        ComponentManagerError::Internal(format!("failed to list firmware objects from RMS: {e}"))
    })?;

    Ok(response.objects.into_iter().map(|fw| fw.id).collect())
}

/// Default BMC HTTPS port used when populating `rms::Endpoint` for
/// switches. Mirrors the value used by `crate::rack::firmware_update`.
const SWITCH_BMC_PORT: u32 = 443;

/// Default BMC HTTPS port used when populating `rms::Endpoint` for compute
/// trays.
const COMPUTE_TRAY_BMC_PORT: u32 = 443;

fn credentials_to_rms(creds: &Credentials) -> rms::Credentials {
    let Credentials::UsernamePassword { username, password } = creds;
    rms::Credentials {
        auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
            username: username.clone(),
            password: password.clone(),
        })),
    }
}

/// Builds the `rms::NodeInfo` used by switch operations that require endpoint
/// details instead of RMS inventory.
fn build_switch_node_info(
    ep: &SwitchEndpoint,
    resolved: &ResolvedRmsNode<'_>,
    nvos_host_name: Option<String>,
) -> rms::NodeInfo {
    let mut node = rms::NodeInfo {
        node_id: resolved.identity.node_id.clone(),
        rack_id: resolved.identity.rack_id.clone(),
        r#type: None,
        bmc_endpoint: Some(rms::Endpoint {
            interface: Some(rms::NetworkInterface {
                ip_address: ep.bmc_ip.to_string(),
                mac_address: ep.bmc_mac.to_string(),
                host_name: None,
            }),
            port: SWITCH_BMC_PORT,
            credentials: Some(credentials_to_rms(&ep.bmc_credentials)),
        }),
        host_endpoint: Some(rms::Endpoint {
            interface: Some(rms::NetworkInterface {
                ip_address: ep.nvos_ip.to_string(),
                mac_address: ep.nvos_mac.to_string(),
                host_name: nvos_host_name,
            }),
            port: 0,
            credentials: Some(credentials_to_rms(&ep.nvos_credentials)),
        }),
        node_descriptor: None,
    };

    resolved.node_identity.apply_to_node_info(&mut node);

    node
}

/// Builds the host-only endpoint description required for password rotation.
///
/// Rotation does not use BMC access, so excluding the BMC endpoint prevents
/// its credentials from crossing this backend boundary.
fn build_switch_password_rotation_node_info(
    ep: &SwitchEndpoint,
    resolved: &ResolvedRmsNode<'_>,
    nvos_host_name: Option<String>,
) -> rms::NodeInfo {
    let node = build_switch_node_info(ep, resolved, nvos_host_name);

    rms::NodeInfo {
        bmc_endpoint: None,
        ..node
    }
}

async fn resolve_switch_machine_interface_hostnames(
    db: &PgPool,
    endpoints: &[SwitchEndpoint],
) -> Result<HashMap<MacAddress, String>, ComponentManagerError> {
    let mut hostnames = HashMap::new();
    let mut macs_to_lookup = Vec::new();

    for ep in endpoints {
        if let Some(name) = ep.nvos_host_name.as_ref().filter(|name| !name.is_empty()) {
            hostnames.insert(ep.nvos_mac, name.clone());
        } else {
            macs_to_lookup.push(ep.nvos_mac);
        }
    }

    macs_to_lookup.sort_unstable();
    macs_to_lookup.dedup();

    if !macs_to_lookup.is_empty() {
        let from_db = db::machine_interface::find_hostnames_by_mac_addresses(db, &macs_to_lookup)
            .await
            .map_err(|e| {
                ComponentManagerError::Internal(format!(
                    "failed to resolve switch machine interface hostnames: {e}"
                ))
            })?;
        hostnames.extend(from_db);
    }

    Ok(hostnames)
}

/// Summarize a `NodeBatchResponse` into a `(success, error)` pair for a
/// single-node `BatchSetPowerState` call. Prefers per-node error
/// messages, then the batch-level message, and finally a generic fallback.
fn summarize_power_batch(batch: rms::NodeBatchResponse) -> (bool, Option<String>) {
    let stats = batch.stats.unwrap_or_default();
    let success = batch.status == rms::ReturnCode::Success as i32 && stats.failed_nodes == 0;

    if success {
        return (true, None);
    }

    let node_error = batch
        .node_results
        .into_iter()
        .find(|r| r.status != rms::ReturnCode::Success as i32 || !r.error_message.is_empty())
        .and_then(|r| {
            if r.error_message.is_empty() {
                None
            } else {
                Some(r.error_message)
            }
        });

    let error = node_error
        .or({
            if batch.message.is_empty() {
                None
            } else {
                Some(batch.message)
            }
        })
        .unwrap_or_else(|| "RMS power control failed".to_owned());

    (false, Some(error))
}

#[derive(Debug, Clone)]
struct RmsObservedPowerState {
    power_state: Option<String>,
    error: Option<String>,
}

async fn query_rms_power_state(
    client: &dyn RmsApi,
    device: rms::NodeInfo,
    node_id: &str,
    device_mac: MacAddress,
    device_kind: &str,
) -> RmsObservedPowerState {
    let request = rms::BatchGetPowerStateRequest {
        nodes: Some(rms::NodeSet {
            nodes: vec![device],
        }),
    };

    match red::instrumented(
        "rms",
        "batch_get_power_state",
        client.batch_get_power_state(request),
    )
    .await
    {
        Ok(response) => {
            let batch = response.response.clone().unwrap_or_default();
            let stats = batch.stats.unwrap_or_default();

            if batch.status != rms::ReturnCode::Success as i32 || stats.failed_nodes != 0 {
                let summary = if batch.message.is_empty() {
                    format!(
                        "batch status {}, failed_nodes {}",
                        batch.status, stats.failed_nodes
                    )
                } else {
                    batch.message
                };
                return RmsObservedPowerState {
                    power_state: None,
                    error: Some(summary),
                };
            }

            let power_state = response
                .node_power_states
                .iter()
                .find(|node| node.node_id == node_id)
                .map(|node| node.pstate.to_lowercase());

            RmsObservedPowerState {
                power_state,
                error: None,
            }
        }
        Err(error) => {
            tracing::warn!(
                device_mac_address = %device_mac,
                error = %error,
                device_kind,
                "RMS get power state failed"
            );
            RmsObservedPowerState {
                power_state: None,
                error: Some(error.to_string()),
            }
        }
    }
}

fn apply_firmware_object_request(
    device: rms::NodeInfo,
    resolved: &ResolvedRmsNode<'_>,
    config_json: &str,
    options: &FirmwareUpdateOptions,
    components: &[String],
) -> Result<rms::ApplyFirmwareObjectRequest, ComponentManagerError> {
    let access_token = Some(rms_access_token_or_noauth(options.access_token.as_deref()));

    if config_json.trim().is_empty() {
        return Err(ComponentManagerError::InvalidArgument(
            "target_version must contain firmware-object JSON for direct RMS updates".into(),
        ));
    }

    let (component_filters, node_descriptor_component_filters) =
        firmware_object_component_filters_for_node_identities(
            components,
            [&resolved.node_identity],
        );

    Ok(rms::ApplyFirmwareObjectRequest {
        rack_id: resolved.identity.rack_id.clone(),
        config_json: config_json.to_owned(),
        access_token,
        firmware_type: RMS_FIRMWARE_OBJECT_FIRMWARE_TYPE.to_owned(),
        hardware_type: RMS_FIRMWARE_OBJECT_HARDWARE_TYPE.to_owned(),
        nodes: Some(rms::NodeSet {
            nodes: vec![device],
        }),
        force_update: options.force_update,
        component_filters,
        node_descriptor_component_filters,
    })
}

fn apply_switch_system_image_request(
    device: rms::NodeInfo,
    identity: &RmsIdentity,
    config_json: &str,
    options: &FirmwareUpdateOptions,
) -> Result<rms::ApplySwitchSystemImageRequest, ComponentManagerError> {
    let access_token = Some(rms_access_token_or_noauth(options.access_token.as_deref()));

    if config_json.trim().is_empty() {
        return Err(ComponentManagerError::InvalidArgument(
            "target_version must contain firmware-object JSON for direct RMS updates".into(),
        ));
    }

    Ok(rms::ApplySwitchSystemImageRequest {
        rack_id: identity.rack_id.clone(),
        config_json: config_json.to_owned(),
        access_token,
        software_type: RMS_SWITCH_SYSTEM_IMAGE_SOFTWARE_TYPE.to_owned(),
        hardware_type: RMS_FIRMWARE_OBJECT_HARDWARE_TYPE.to_owned(),
        nodes: Some(rms::NodeSet {
            nodes: vec![device],
        }),
        // RMS does not expose force_update on switch system-image JSON updates.
    })
}

fn power_shelf_firmware_object_component_filters(
    components: &[PowerShelfComponent],
) -> Vec<String> {
    if components.is_empty() {
        Vec::new()
    } else {
        vec!["PowerShelfFW".to_owned()]
    }
}

fn switch_update_includes_firmware_object(components: &[NvSwitchComponent]) -> bool {
    components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, NvSwitchComponent::Nvos))
}

fn switch_update_includes_system_image(components: &[NvSwitchComponent]) -> bool {
    components.is_empty()
        || components
            .iter()
            .any(|component| matches!(component, NvSwitchComponent::Nvos))
}

fn switch_firmware_object_component_filters(components: &[NvSwitchComponent]) -> Vec<String> {
    components
        .iter()
        .filter_map(|c| match c {
            NvSwitchComponent::Bmc => Some("BMC".to_owned()),
            NvSwitchComponent::Cpld => Some("CPLD".to_owned()),
            NvSwitchComponent::Bios => Some("BIOS".to_owned()),
            NvSwitchComponent::Nvos => None,
        })
        .collect()
}

fn compute_tray_firmware_object_component_filters(
    components: &[ComputeTrayComponent],
) -> Vec<String> {
    if components.is_empty() {
        Vec::new()
    } else {
        components
            .iter()
            .map(|component| component.to_string())
            .collect()
    }
}

fn build_compute_tray_node_info(
    ep: &ComputeTrayEndpoint,
    resolved: &ResolvedRmsNode<'_>,
    bmc_mac: MacAddress,
) -> rms::NodeInfo {
    let mut node = rms::NodeInfo {
        node_id: resolved.identity.node_id.clone(),
        rack_id: resolved.identity.rack_id.clone(),
        r#type: None,
        bmc_endpoint: Some(rms::Endpoint {
            interface: Some(rms::NetworkInterface {
                ip_address: ep.bmc_ip.to_string(),
                mac_address: bmc_mac.to_string(),
                host_name: None,
            }),
            port: COMPUTE_TRAY_BMC_PORT,
            credentials: Some(credentials_to_rms(&ep.bmc_credentials)),
        }),
        host_endpoint: None,
        node_descriptor: None,
    };

    resolved.node_identity.apply_to_node_info(&mut node);

    node
}

fn summarize_firmware_object_apply_response(
    response: rms::ApplyFirmwareObjectResponse,
    node_id: &str,
) -> (bool, Option<String>, Option<String>) {
    let node_job_id = response
        .jobs
        .iter()
        .find(|j| j.node_id == node_id && !j.job_id.is_empty())
        .map(|j| j.job_id.clone());

    summarize_firmware_batch(
        response.response,
        node_job_id,
        node_id,
        "RMS firmware update failed",
    )
}

fn summarize_switch_system_image_apply_response(
    response: rms::ApplySwitchSystemImageResponse,
    node_id: &str,
) -> (bool, Option<String>, Option<String>) {
    let node_job_id = response
        .jobs
        .iter()
        .find(|j| j.node_id == node_id && !j.job_id.is_empty())
        .map(|j| j.job_id.clone());

    summarize_firmware_batch(
        response.response,
        node_job_id,
        node_id,
        "RMS switch system image update failed",
    )
}

fn summarize_firmware_batch(
    batch: Option<rms::NodeBatchResponse>,
    node_job_id: Option<String>,
    node_id: &str,
    default_error: &str,
) -> (bool, Option<String>, Option<String>) {
    let Some(batch) = batch else {
        return (false, Some(default_error.to_owned()), node_job_id);
    };
    let node_failure = batch
        .node_results
        .iter()
        .find(|r| r.node_id == node_id && r.status != rms::ReturnCode::Success as i32)
        .or_else(|| {
            batch
                .node_results
                .iter()
                .find(|r| r.status != rms::ReturnCode::Success as i32)
        });
    let stats = batch.stats.unwrap_or_default();
    let success = batch.status == rms::ReturnCode::Success as i32
        && stats.failed_nodes == 0
        && node_failure.is_none();
    let job_id = node_job_id.or_else(|| (!batch.job_id.is_empty()).then_some(batch.job_id.clone()));

    if success {
        return (true, None, job_id);
    }

    let error = node_failure
        .and_then(|r| {
            if r.error_message.is_empty() {
                None
            } else {
                Some(r.error_message.clone())
            }
        })
        .or({
            if batch.message.is_empty() {
                None
            } else {
                Some(batch.message)
            }
        })
        .unwrap_or_else(|| default_error.to_owned());

    (false, Some(error), job_id)
}

async fn query_tracked_firmware_job_status(
    client: &dyn RmsApi,
    switch_system_image_client: Option<&dyn RmsSwitchSystemImageStatusApi>,
    job: &RmsTrackedFirmwareJob,
) -> (FirmwareState, Option<String>) {
    match job {
        RmsTrackedFirmwareJob::FirmwareObject(job_id) => {
            let request = rms::GetFirmwareJobStatusRequest {
                job_id: job_id.clone(),
            };

            match red::instrumented(
                "rms",
                "get_firmware_job_status",
                client.get_firmware_job_status(request),
            )
            .await
            {
                Ok(response) => {
                    let status_success = response.status == rms::ReturnCode::Success as i32;
                    let state = if status_success {
                        map_rms_firmware_job_state(response.job_state)
                    } else {
                        FirmwareState::Unknown
                    };
                    let error = if response.error_message.is_empty() {
                        (!status_success).then(|| {
                            format!("RMS could not report status for firmware job {job_id}")
                        })
                    } else {
                        Some(response.error_message)
                    };
                    (state, error)
                }
                Err(e) => (FirmwareState::Unknown, Some(e.to_string())),
            }
        }
        RmsTrackedFirmwareJob::SwitchSystemImage(job_id) => {
            let Some(client) = switch_system_image_client else {
                return (
                    FirmwareState::Unknown,
                    Some("RMS switch system-image status client is not configured".to_owned()),
                );
            };
            let request = rms::GetSwitchSystemImageJobStatusRequest {
                job_id: job_id.clone(),
            };

            match red::instrumented(
                "rms",
                "get_switch_system_image_job_status",
                client.get_switch_system_image_job_status(request),
            )
            .await
            {
                Ok(response) if response.status == rms::ReturnCode::Success as i32 => {
                    let state = map_rms_switch_system_image_job_state(&response.state);
                    let error = if response.error_message.is_empty() {
                        (!response.message.is_empty()
                            && matches!(state, FirmwareState::Failed | FirmwareState::Unknown))
                        .then_some(response.message)
                    } else {
                        Some(response.error_message)
                    };
                    (state, error)
                }
                Ok(response) => {
                    let error = if response.error_message.is_empty() {
                        if response.message.is_empty() {
                            format!(
                                "RMS could not report status for switch system-image job {job_id}"
                            )
                        } else {
                            response.message
                        }
                    } else {
                        response.error_message
                    };
                    (FirmwareState::Unknown, Some(error))
                }
                Err(e) => (FirmwareState::Unknown, Some(e.to_string())),
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RmsSwitchSlotAndTrayObservation {
    slot_number: Option<i32>,
    tray_index: Option<i32>,
    error: Option<String>,
}

fn rms_switch_location_value(value: Option<u32>) -> Result<Option<i32>, u32> {
    value
        .map(|value| i32::try_from(value).map_err(|_| value))
        .transpose()
}

/// `classify_rms_switch_slot_and_tray` keeps each usable location field.
/// Missing details and conversion failures become one diagnostic so the
/// switch controller emits one `Event` without dropping the other valid field.
fn classify_rms_switch_slot_and_tray(
    details: Option<&rms::NodeDeviceInfo>,
) -> RmsSwitchSlotAndTrayObservation {
    let Some(details) = details else {
        return RmsSwitchSlotAndTrayObservation {
            slot_number: None,
            tray_index: None,
            error: Some("RMS returned no device info".to_string()),
        };
    };

    let slot_number = rms_switch_location_value(details.slot_number);
    let tray_index = rms_switch_location_value(details.tray_index);
    let slot_number_out_of_range = slot_number.is_err();
    let tray_index_out_of_range = tray_index.is_err();
    let error = match (slot_number_out_of_range, tray_index_out_of_range) {
        (false, false) => None,
        (true, false) => Some("RMS returned slot_number outside the supported range".to_string()),
        (false, true) => Some("RMS returned tray_index outside the supported range".to_string()),
        (true, true) => {
            Some("RMS returned slot_number and tray_index outside the supported range".to_string())
        }
    };

    RmsSwitchSlotAndTrayObservation {
        slot_number: slot_number.ok().flatten(),
        tray_index: tray_index.ok().flatten(),
        error,
    }
}

fn scale_up_fabric_response_status_from_rms(status: i32) -> ScaleUpFabricResponseStatus {
    match rms::ReturnCode::try_from(status) {
        Ok(rms::ReturnCode::Success) => ScaleUpFabricResponseStatus::Success,
        Ok(rms::ReturnCode::Failure) => ScaleUpFabricResponseStatus::Failure,
        Ok(rms::ReturnCode::Unspecified) | Err(_) => ScaleUpFabricResponseStatus::Unknown(status),
    }
}

fn scale_up_fabric_job_status_from_rms(
    job_id: &str,
    response: rms::GetJobStatusResponse,
) -> Option<ScaleUpFabricManagerJobStatus> {
    let job = response
        .job_states
        .into_iter()
        .find(|job| job.job_id == job_id)?;

    Some(
        match rms::JobExecutionState::try_from(job.execution_state) {
            Ok(rms::JobExecutionState::Queued | rms::JobExecutionState::Running) => {
                ScaleUpFabricManagerJobStatus::Pending {
                    description: job.state_description,
                }
            }
            Ok(rms::JobExecutionState::Completed) => ScaleUpFabricManagerJobStatus::Completed,
            Ok(rms::JobExecutionState::Failed) => ScaleUpFabricManagerJobStatus::Failed {
                error: (!job.error_message.trim().is_empty()).then_some(job.error_message),
            },
            Ok(rms::JobExecutionState::Unspecified) | Err(_) => {
                ScaleUpFabricManagerJobStatus::Unknown {
                    execution_state: job.execution_state,
                }
            }
        },
    )
}

fn scale_up_fabric_status_from_rms(
    response: rms::GetScaleUpFabricStatusResponse,
) -> ScaleUpFabricStatus {
    ScaleUpFabricStatus {
        status: scale_up_fabric_response_status_from_rms(response.status),
        switches: response.fabric_status.map(|status| {
            status
                .switches
                .into_iter()
                .map(|switch| ScaleUpFabricSwitchStatus {
                    node_id: switch.node_id,
                    enabled: switch.enabled,
                    error_message: switch.error_message,
                })
                .collect()
        }),
        error_message: response.error_message,
    }
}

fn scale_up_fabric_manager_job_id_from_rms(
    response: rms_v2::ConfigureScaleUpFabricManagerResponse,
) -> Result<String, ComponentManagerError> {
    if response.job_id.trim().is_empty() {
        return Err(ComponentManagerError::OperationOutcomeUnknown(
            "RMS ConfigureScaleUpFabricManagerV2 returned an empty job ID".to_string(),
        ));
    }

    Ok(response.job_id)
}

#[derive(Debug, Deserialize)]
struct RmsFabricManagerStatusPayload {
    status: Option<String>,
    #[serde(rename = "addition-info")]
    addition_info: Option<String>,
    reason: Option<String>,
}

fn fabric_manager_status_from_rms(
    node_id: &str,
    entry: rms::ScaleUpFabricServiceStatusEntry,
) -> FabricManagerStatus {
    // A per-switch RMS error is authoritative; its JSON may be absent or stale.
    if !entry.error_message.trim().is_empty() {
        return FabricManagerStatus {
            fabric_manager_state: FabricManagerState::Unknown,
            addition_info: None,
            reason: None,
            error_message: Some(entry.error_message),
        };
    }

    if entry.status_json.trim().is_empty() {
        return FabricManagerStatus {
            fabric_manager_state: FabricManagerState::Unknown,
            addition_info: None,
            reason: None,
            error_message: None,
        };
    }

    let status_json =
        match serde_json::from_str::<RmsFabricManagerStatusPayload>(&entry.status_json) {
            Ok(status_json) => status_json,
            Err(error) => {
                tracing::warn!(
                    switch_id = %node_id,
                    %error,
                    status_json = %entry.status_json,
                    "Failed to parse RMS fabric-manager status JSON"
                );

                return FabricManagerStatus {
                    fabric_manager_state: FabricManagerState::Unknown,
                    addition_info: None,
                    reason: None,
                    error_message: None,
                };
            }
        };

    let fabric_manager_state = match status_json.status.as_deref().unwrap_or_default() {
        "ok" => FabricManagerState::Ok,
        "not ok" => FabricManagerState::NotOk,
        _ => FabricManagerState::Unknown,
    };

    FabricManagerStatus {
        fabric_manager_state,
        addition_info: status_json.addition_info,
        reason: status_json.reason,
        error_message: None,
    }
}

/// Converts an RMS Fabric Manager service response into typed controller observations.
///
/// The RMS-backed V1 and V2 workflows share this conversion so both persist
/// identical service state. Aggregate statistics are not used by the controller
/// and are discarded.
pub fn scale_up_fabric_service_statuses_from_rms(
    response: rms::BatchGetScaleUpFabricServiceStatusResponse,
) -> ScaleUpFabricServiceStatuses {
    ScaleUpFabricServiceStatuses {
        status: scale_up_fabric_response_status_from_rms(response.status),
        service_statuses: response
            .service_statuses
            .into_iter()
            .map(|(node_id, status)| {
                let observation = fabric_manager_status_from_rms(&node_id, status);
                (node_id, observation)
            })
            .collect(),
    }
}

#[async_trait::async_trait]
impl NvSwitchManager for RmsBackend {
    fn name(&self) -> &str {
        "rms"
    }

    fn supports_password_rotation(&self) -> bool {
        self.nvos_password_rotation_enabled
    }

    fn supports_firmware_object_json(&self) -> bool {
        true
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn power_control(
        &self,
        endpoints: &[SwitchEndpoint],
        action: PowerAction,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|ep| ep.bmc_mac).collect();
        let ids = resolve_switch_identities(&self.db, &macs).await?;
        let operation = to_rms_power_operation(action);
        let mut results = Vec::with_capacity(endpoints.len());
        let hostnames = resolve_switch_machine_interface_hostnames(&self.db, endpoints).await?;

        for ep in endpoints {
            let resolved = match self.resolve_switch_or_power_shelf_node(
                &ids,
                ep.bmc_mac,
                SwitchOrPowerShelfRole::Switch,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    results.push(SwitchComponentResult {
                        bmc_mac: ep.bmc_mac,
                        success: false,
                        error: Some(error),
                    });
                    continue;
                }
            };

            let device =
                build_switch_node_info(ep, &resolved, hostnames.get(&ep.nvos_mac).cloned());

            let request = rms::BatchSetPowerStateRequest {
                nodes: Some(rms::NodeSet {
                    nodes: vec![device],
                }),
                operation,
            };

            match red::instrumented(
                "rms",
                "batch_set_power_state",
                self.client.batch_set_power_state(request),
            )
            .await
            {
                Ok(response) => {
                    let (success, error) =
                        summarize_power_batch(response.response.unwrap_or_default());
                    results.push(SwitchComponentResult {
                        bmc_mac: ep.bmc_mac,
                        success,
                        error,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        bmc_mac_address = %ep.bmc_mac,
                        error = %e,
                        "RMS power control failed for switch"
                    );
                    results.push(SwitchComponentResult {
                        bmc_mac: ep.bmc_mac,
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(results)
    }

    #[instrument(skip(self, bundle_version, options), fields(backend = "rms", force_update = options.force_update))]
    async fn queue_firmware_updates(
        &self,
        endpoints: &[SwitchEndpoint],
        bundle_version: &str,
        components: &[NvSwitchComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|ep| ep.bmc_mac).collect();
        let ids = resolve_switch_identities(&self.db, &macs).await?;
        let include_firmware_object = switch_update_includes_firmware_object(components);
        let include_system_image = switch_update_includes_system_image(components);
        let component_filters = switch_firmware_object_component_filters(components);

        let mut results = Vec::with_capacity(endpoints.len());
        let hostnames = resolve_switch_machine_interface_hostnames(&self.db, endpoints).await?;

        for ep in endpoints {
            let resolved = match self.resolve_switch_or_power_shelf_node(
                &ids,
                ep.bmc_mac,
                SwitchOrPowerShelfRole::Switch,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    results.push(SwitchComponentResult {
                        bmc_mac: ep.bmc_mac,
                        success: false,
                        error: Some(error),
                    });
                    continue;
                }
            };

            let nvos_host_name = hostnames.get(&ep.nvos_mac).cloned();
            let mut success = true;
            let mut errors = Vec::new();
            let mut tracked_jobs = Vec::new();

            if include_firmware_object {
                let device = build_switch_node_info(ep, &resolved, nvos_host_name.clone());
                match apply_firmware_object_request(
                    device,
                    &resolved,
                    bundle_version,
                    options,
                    &component_filters,
                ) {
                    Ok(request) => match red::instrumented(
                        "rms",
                        "apply_firmware_object",
                        self.client.apply_firmware_object(request),
                    )
                    .await
                    {
                        Ok(response) => {
                            let (operation_success, error, job_id) =
                                summarize_firmware_object_apply_response(
                                    response,
                                    &resolved.identity.node_id,
                                );

                            if !operation_success {
                                success = false;
                            }
                            if let Some(error) = error {
                                errors.push(error);
                            }
                            if operation_success {
                                if let Some(job_id) = job_id {
                                    tracked_jobs
                                        .push(RmsTrackedFirmwareJob::FirmwareObject(job_id));
                                }
                            } else if job_id.is_some() {
                                tracing::debug!(
                                    bmc_mac_address = %ep.bmc_mac,
                                    "RMS returned a firmware-object job id for a failed switch update; not tracking it"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                bmc_mac_address = %ep.bmc_mac,
                                error = %e,
                                "RMS firmware-object update failed for switch"
                            );
                            success = false;
                            errors.push(e.to_string());
                        }
                    },
                    Err(e) => {
                        success = false;
                        errors.push(e.to_string());
                    }
                }
            }

            if include_system_image {
                let device = build_switch_node_info(ep, &resolved, nvos_host_name);
                match apply_switch_system_image_request(
                    device,
                    resolved.identity,
                    bundle_version,
                    options,
                ) {
                    Ok(request) => match red::instrumented(
                        "rms",
                        "apply_switch_system_image",
                        self.client.apply_switch_system_image(request),
                    )
                    .await
                    {
                        Ok(response) => {
                            let (operation_success, error, job_id) =
                                summarize_switch_system_image_apply_response(
                                    response,
                                    &resolved.identity.node_id,
                                );

                            if !operation_success {
                                success = false;
                            }
                            if let Some(error) = error {
                                errors.push(error);
                            }
                            if operation_success {
                                if let Some(job_id) = job_id {
                                    tracked_jobs
                                        .push(RmsTrackedFirmwareJob::SwitchSystemImage(job_id));
                                }
                            } else if job_id.is_some() {
                                tracing::debug!(
                                    bmc_mac_address = %ep.bmc_mac,
                                    "RMS returned a switch system-image job id for a failed switch update; not tracking it"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                bmc_mac_address = %ep.bmc_mac,
                                error = %e,
                                "RMS switch system-image update failed for switch"
                            );
                            success = false;
                            errors.push(e.to_string());
                        }
                    },
                    Err(e) => {
                        success = false;
                        errors.push(e.to_string());
                    }
                }
            }

            if !tracked_jobs.is_empty() {
                self.firmware_jobs
                    .lock()
                    .unwrap()
                    .insert(ep.bmc_mac, tracked_jobs);
            } else {
                self.firmware_jobs.lock().unwrap().remove(&ep.bmc_mac);
            }

            results.push(SwitchComponentResult {
                bmc_mac: ep.bmc_mac,
                success,
                error: (!errors.is_empty()).then(|| errors.join("; ")),
            });
        }

        Ok(results)
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn get_firmware_status(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchFirmwareUpdateStatus>, ComponentManagerError> {
        let endpoint_jobs: Vec<(MacAddress, Vec<RmsTrackedFirmwareJob>)> = {
            let jobs = self.firmware_jobs.lock().unwrap();
            endpoints
                .iter()
                .map(|ep| {
                    (
                        ep.bmc_mac,
                        jobs.get(&ep.bmc_mac).cloned().unwrap_or_default(),
                    )
                })
                .collect()
        };

        let mut statuses = Vec::with_capacity(endpoints.len());

        for (bmc_mac, jobs) in &endpoint_jobs {
            if jobs.is_empty() {
                statuses.push(SwitchFirmwareUpdateStatus {
                    bmc_mac: *bmc_mac,
                    state: FirmwareState::Unknown,
                    target_version: String::new(),
                    error: Some("no firmware job tracked for this switch".into()),
                });
                continue;
            }

            let mut states = Vec::with_capacity(jobs.len());
            let mut errors = Vec::new();
            for job in jobs {
                let (state, error) = query_tracked_firmware_job_status(
                    self.client.as_ref(),
                    self.switch_system_image_client.as_deref(),
                    job,
                )
                .await;
                states.push(state);
                if let Some(error) = error {
                    errors.push(error);
                }
            }

            statuses.push(SwitchFirmwareUpdateStatus {
                bmc_mac: *bmc_mac,
                state: aggregate_firmware_job_states(&states),
                target_version: String::new(),
                error: (!errors.is_empty()).then(|| errors.join("; ")),
            });
        }

        Ok(statuses)
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        list_firmware_object_ids(self.client.as_ref()).await
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn get_power_state(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchPowerStateResult>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|ep| ep.bmc_mac).collect();
        let ids = resolve_switch_identities(&self.db, &macs).await?;
        let mut results = Vec::with_capacity(endpoints.len());
        let hostnames = resolve_switch_machine_interface_hostnames(&self.db, endpoints).await?;

        for ep in endpoints {
            let resolved = match self.resolve_switch_or_power_shelf_node(
                &ids,
                ep.bmc_mac,
                SwitchOrPowerShelfRole::Switch,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    results.push(SwitchPowerStateResult {
                        bmc_mac: ep.bmc_mac,
                        power_state: None,
                        error: Some(error),
                    });
                    continue;
                }
            };

            let device =
                build_switch_node_info(ep, &resolved, hostnames.get(&ep.nvos_mac).cloned());

            let observed = query_rms_power_state(
                self.client.as_ref(),
                device,
                &resolved.identity.node_id,
                ep.bmc_mac,
                "switch",
            )
            .await;
            results.push(SwitchPowerStateResult {
                bmc_mac: ep.bmc_mac,
                power_state: observed.power_state,
                error: observed.error,
            });
        }

        Ok(results)
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn get_slot_and_tray(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchSlotAndTrayResult>, ComponentManagerError> {
        let macs: Vec<MacAddress> = endpoints.iter().map(|ep| ep.bmc_mac).collect();
        let ids = resolve_switch_identities(&self.db, &macs).await?;
        let mut results = Vec::with_capacity(endpoints.len());
        let hostnames = resolve_switch_machine_interface_hostnames(&self.db, endpoints).await?;

        for ep in endpoints {
            let resolved = match self.resolve_switch_or_power_shelf_node(
                &ids,
                ep.bmc_mac,
                SwitchOrPowerShelfRole::Switch,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    results.push(SwitchSlotAndTrayResult {
                        bmc_mac: ep.bmc_mac,
                        slot_number: None,
                        tray_index: None,
                        error: Some(error),
                    });
                    continue;
                }
            };

            let device =
                build_switch_node_info(ep, &resolved, hostnames.get(&ep.nvos_mac).cloned());

            let request = rms::BatchGetNodeDeviceInfoRequest {
                nodes: Some(rms::NodeSet {
                    nodes: vec![device],
                }),
            };

            match red::instrumented(
                "rms",
                "batch_get_node_device_info",
                self.client.batch_get_node_device_info(request),
            )
            .await
            {
                Ok(info) => {
                    if info.status != rms::ReturnCode::Success as i32 {
                        let summary = if info.message.is_empty() {
                            format!("status {}", info.status)
                        } else {
                            info.message.clone()
                        };
                        results.push(SwitchSlotAndTrayResult {
                            bmc_mac: ep.bmc_mac,
                            slot_number: None,
                            tray_index: None,
                            error: Some(summary),
                        });
                        continue;
                    }

                    let observation =
                        classify_rms_switch_slot_and_tray(info.node_device_details.first());
                    results.push(SwitchSlotAndTrayResult {
                        bmc_mac: ep.bmc_mac,
                        slot_number: observation.slot_number,
                        tray_index: observation.tray_index,
                        error: observation.error,
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        bmc_mac_address = %ep.bmc_mac,
                        error = %error,
                        "RMS get slot and tray failed for switch"
                    );
                    results.push(SwitchSlotAndTrayResult {
                        bmc_mac: ep.bmc_mac,
                        slot_number: None,
                        tray_index: None,
                        error: Some(error.to_string()),
                    });
                }
            }
        }

        Ok(results)
    }

    #[instrument(skip(self, domain_name), fields(backend = "rms"))]
    async fn configure_switch_certificate(
        &self,
        endpoint: &SwitchEndpoint,
        domain_name: Option<&str>,
        services: Option<&[i32]>,
    ) -> Result<String, ComponentManagerError> {
        let ids =
            resolve_switch_identities(&self.db, std::slice::from_ref(&endpoint.bmc_mac)).await?;

        let resolved = match self.resolve_switch_or_power_shelf_node(
            &ids,
            endpoint.bmc_mac,
            SwitchOrPowerShelfRole::Switch,
        ) {
            Ok(resolved) => resolved,
            Err(error) => {
                return Err(ComponentManagerError::Internal(error));
            }
        };

        let hostnames =
            resolve_switch_machine_interface_hostnames(&self.db, std::slice::from_ref(endpoint))
                .await?;

        let device = build_switch_node_info(
            endpoint,
            &resolved,
            hostnames.get(&endpoint.nvos_mac).cloned(),
        );
        rms_configure_switch_certificate(self.client.as_ref(), device, domain_name, services).await
    }

    #[instrument(skip(self), fields(backend = "rms", job_id))]
    async fn get_configure_switch_certificate_job_status(
        &self,
        job_id: &str,
    ) -> Result<ConfigureSwitchCertificateJobStatus, ComponentManagerError> {
        rms_get_configure_switch_certificate_job_status(self.client.as_ref(), job_id).await
    }

    #[instrument(skip(self, endpoints, tls_server_domain), fields(backend = "rms"))]
    async fn batch_reset_switch_factory_default(
        &self,
        endpoints: &[SwitchEndpoint],
        tls_server_domain: Option<&str>,
    ) -> Result<String, ComponentManagerError> {
        if endpoints.is_empty() {
            return Err(ComponentManagerError::InvalidArgument(
                "switch factory reset requires at least one endpoint".to_string(),
            ));
        }

        let macs: Vec<MacAddress> = endpoints.iter().map(|endpoint| endpoint.bmc_mac).collect();
        let identities = resolve_switch_identities(&self.db, &macs).await?;
        let hostnames = resolve_switch_machine_interface_hostnames(&self.db, endpoints).await?;
        let mut nodes = Vec::with_capacity(endpoints.len());

        for endpoint in endpoints {
            let resolved = self
                .resolve_switch_or_power_shelf_node(
                    &identities,
                    endpoint.bmc_mac,
                    SwitchOrPowerShelfRole::Switch,
                )
                .map_err(ComponentManagerError::Internal)?;

            nodes.push(build_switch_node_info(
                endpoint,
                &resolved,
                hostnames.get(&endpoint.nvos_mac).cloned(),
            ));
        }

        rms_batch_reset_switch_factory_default(self.client.as_ref(), nodes, tls_server_domain).await
    }

    #[instrument(skip(self), fields(backend = "rms", job_id))]
    async fn get_switch_factory_reset_job_status(
        &self,
        job_id: &str,
    ) -> Result<SwitchFactoryResetJobStatus, ComponentManagerError> {
        rms_get_switch_factory_reset_job_status(self.client.as_ref(), job_id).await
    }

    #[instrument(skip(self, endpoints, topology), fields(backend = "rms"))]
    async fn configure_scale_up_fabric_manager(
        &self,
        endpoints: &[SwitchEndpoint],
        topology: RackHardwareTopology,
    ) -> Result<String, ComponentManagerError> {
        let nodes = self.resolve_scale_up_fabric_nodes(endpoints).await?;

        let response = red::instrumented(
            "rms",
            "configure_scale_up_fabric_manager_v2",
            self.client.configure_scale_up_fabric_manager_v2(
                rms_v2::ConfigureScaleUpFabricManagerRequest {
                    nodes: Some(rms::NodeSet { nodes }),
                    // RMS selects the V2 primary across the supplied fabric.
                    primary_switch_node_id: None,
                    domain: None,
                    config: Some(rms_v2::ScaleUpFabricConfig {
                        topology_type: topology.to_string(),
                        extra_static_configs: Vec::new(),
                    }),
                },
            ),
        )
        .await?;

        scale_up_fabric_manager_job_id_from_rms(response)
    }

    #[instrument(skip(self), fields(backend = "rms", job_id))]
    async fn get_scale_up_fabric_manager_job_status(
        &self,
        job_id: &str,
    ) -> Result<Option<ScaleUpFabricManagerJobStatus>, ComponentManagerError> {
        let response = match red::instrumented(
            "rms",
            "get_job_status",
            self.client.get_job_status(rms::GetJobStatusRequest {
                job_id: job_id.to_string(),
                include_child_job_states: false,
            }),
        )
        .await
        {
            Err(RackManagerError::ApiInvocationError(status))
                if status.code() == tonic::Code::NotFound =>
            {
                return Ok(None);
            }
            result => result?,
        };

        Ok(scale_up_fabric_job_status_from_rms(job_id, response))
    }

    #[instrument(skip(self, endpoints), fields(backend = "rms"))]
    async fn get_scale_up_fabric_status(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<ScaleUpFabricStatus, ComponentManagerError> {
        let nodes = self.resolve_scale_up_fabric_nodes(endpoints).await?;

        let response = red::instrumented(
            "rms",
            "get_scale_up_fabric_status",
            self.client
                .get_scale_up_fabric_status(rms::GetScaleUpFabricStatusRequest {
                    nodes: Some(rms::NodeSet { nodes }),
                    domain: None,
                }),
        )
        .await?;

        Ok(scale_up_fabric_status_from_rms(response))
    }

    #[instrument(skip(self, endpoints), fields(backend = "rms"))]
    async fn batch_get_scale_up_fabric_service_status(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<ScaleUpFabricServiceStatuses, ComponentManagerError> {
        let nodes = self.resolve_scale_up_fabric_nodes(endpoints).await?;

        let response = red::instrumented(
            "rms",
            "batch_get_scale_up_fabric_service_status",
            self.client.batch_get_scale_up_fabric_service_status(
                rms::BatchGetScaleUpFabricServiceStatusRequest {
                    nodes: Some(rms::NodeSet { nodes }),
                },
            ),
        )
        .await?;

        Ok(scale_up_fabric_service_statuses_from_rms(response))
    }

    #[instrument(skip(self, endpoint, next_password), fields(backend = "rms", bmc_mac = %endpoint.bmc_mac))]
    async fn ensure_password_rotation(
        &self,
        endpoint: &SwitchEndpoint,
        next_password: &str,
    ) -> Result<String, ComponentManagerError> {
        if !self.supports_password_rotation() {
            return Err(ComponentManagerError::Unsupported(
                "RMS switch password rotation is disabled; enable it only after every RMS server has been upgraded"
                    .to_string(),
            ));
        }

        let identities =
            resolve_switch_identities(&self.db, std::slice::from_ref(&endpoint.bmc_mac)).await?;

        let resolved = self
            .resolve_switch_or_power_shelf_node(
                &identities,
                endpoint.bmc_mac,
                SwitchOrPowerShelfRole::Switch,
            )
            .map_err(ComponentManagerError::Internal)?;

        let hostnames =
            resolve_switch_machine_interface_hostnames(&self.db, std::slice::from_ref(endpoint))
                .await?;

        let device = build_switch_password_rotation_node_info(
            endpoint,
            &resolved,
            hostnames.get(&endpoint.nvos_mac).cloned(),
        );

        rms_ensure_switch_password_rotation(
            self.client.as_ref(),
            device,
            &endpoint.nvos_credentials,
            next_password,
        )
        .await
    }

    #[instrument(skip(self), fields(backend = "rms", job_id))]
    async fn get_password_rotation_job_status(
        &self,
        job_id: &str,
    ) -> Result<SwitchPasswordRotationState, ComponentManagerError> {
        rms_get_switch_password_rotation_job_status(self.client.as_ref(), job_id).await
    }
}

/// Submits one resumable password-convergence attempt.
///
/// A returned job ID is the handle for later reconciliation. A successful RMS
/// response without a job ID is not accepted as convergence evidence because
/// older RMS builds used that shape for non-resumable synchronous work.
async fn rms_ensure_switch_password_rotation(
    client: &dyn RmsApi,
    device: rms::NodeInfo,
    current_credentials: &Credentials,
    next_password: &str,
) -> Result<String, ComponentManagerError> {
    let Credentials::UsernamePassword {
        username,
        password: current_password,
    } = current_credentials;

    if username.is_empty() || current_password.is_empty() || next_password.is_empty() {
        return Err(ComponentManagerError::RejectedBeforeDispatch(
            "switch password rotation requires non-empty username, current password, and next password"
                .to_string(),
        ));
    }

    let request = rms::UpdateSwitchSystemPasswordRequest {
        nodes: Some(rms::NodeSet {
            nodes: vec![device],
        }),
        username: username.clone(),
        password: next_password.to_string(),
    };

    let response = red::instrumented(
        "rms",
        "update_switch_system_password",
        client.update_switch_system_password(request),
    )
    .await
    .map_err(|error| match error {
        RackManagerError::ApiInvocationError(status)
            if status.code() == tonic::Code::InvalidArgument =>
        {
            ComponentManagerError::RejectedBeforeDispatch(
                "RMS rejected the switch password rotation request".to_string(),
            )
        }
        RackManagerError::ApiInvocationError(status)
            if status.code() == tonic::Code::Unimplemented =>
        {
            ComponentManagerError::Unsupported(
                "RMS does not support switch password rotation".to_string(),
            )
        }
        _ => ComponentManagerError::OperationOutcomeUnknown(
            "RMS switch password request returned no durable job ID".to_string(),
        ),
    })?;

    let batch = response.response.ok_or_else(|| {
        ComponentManagerError::OperationOutcomeUnknown(
            "RMS switch password rotation returned no operation response or job ID".to_string(),
        )
    })?;

    // A job ID enables early completion observation. NICo retains the exact
    // current-to-target credential transition because RMS can lose this handle
    // after a restart and safely resume the same request.
    if !batch.job_id.is_empty() {
        return Ok(batch.job_id);
    }

    Err(ComponentManagerError::OperationOutcomeUnknown(format!(
        "RMS switch password rotation returned no job ID (status {}); the operation outcome is unknown",
        batch.status
    )))
}

/// Maps a backend job record to the backend-neutral rotation state.
fn map_rms_password_rotation_state(job: &rms::JobStatus) -> SwitchPasswordRotationState {
    match rms::JobExecutionState::try_from(job.execution_state) {
        Ok(rms::JobExecutionState::Queued | rms::JobExecutionState::Running) => {
            SwitchPasswordRotationState::Pending
        }
        Ok(rms::JobExecutionState::Completed) => SwitchPasswordRotationState::Completed,
        Ok(rms::JobExecutionState::Failed) => SwitchPasswordRotationState::Failed,
        Ok(rms::JobExecutionState::Unspecified) | Err(_) => SwitchPasswordRotationState::Unknown,
    }
}

/// Submits one destructive factory-reset batch and requires a durable job handle.
///
/// The backend-neutral TLS server domain maps to the RMS `domain` field. RMS uses its
/// configured switch domain when this value is absent.
async fn rms_batch_reset_switch_factory_default(
    client: &dyn RmsApi,
    nodes: Vec<rms::NodeInfo>,
    tls_server_domain: Option<&str>,
) -> Result<String, ComponentManagerError> {
    let request = rms::BatchResetSwitchFactoryDefaultRequest {
        nodes: Some(rms::NodeSet { nodes }),
        domain: tls_server_domain.map(str::to_owned),
    };

    let response = red::instrumented(
        "rms",
        "batch_reset_switch_factory_default",
        client.batch_reset_switch_factory_default(request),
    )
    .await
    .map_err(|error| match error {
        // The contract guarantees that an unimplemented RPC accepted no reset work.
        // Other status or transport failures can arrive after dispatch, so they must
        // not invite an automatic retry of this destructive operation.
        RackManagerError::ApiInvocationError(status)
            if status.code() == tonic::Code::Unimplemented =>
        {
            ComponentManagerError::Unsupported(
                "RMS does not support switch factory reset".to_string(),
            )
        }
        _ => ComponentManagerError::OperationOutcomeUnknown(
            "RMS switch factory-reset submission returned no durable job ID".to_string(),
        ),
    })?;

    let batch = response.response.ok_or_else(|| {
        ComponentManagerError::OperationOutcomeUnknown(
            "RMS switch factory-reset submission returned no operation response or job ID"
                .to_string(),
        )
    })?;

    if !batch.job_id.trim().is_empty() {
        return Ok(batch.job_id);
    }

    Err(ComponentManagerError::OperationOutcomeUnknown(format!(
        "RMS switch factory-reset submission returned no job ID (status {}); the operation outcome is unknown",
        batch.status
    )))
}

fn rms_switch_factory_reset_job_failure(job: &rms::JobStatus) -> String {
    let error_code = rms::JobError::try_from(job.error_code).map_or_else(
        |_| format!("unknown({})", job.error_code),
        |error| error.as_str_name().to_string(),
    );

    let node = job
        .node_id
        .as_deref()
        .map(|node_id| format!(" for node {node_id}"))
        .unwrap_or_default();

    let message = if job.error_message.trim().is_empty() {
        "no error message".to_string()
    } else {
        job.error_message.clone()
    };

    format!(
        "switch factory-reset job {}{node} failed with {error_code}: {message}",
        job.job_id
    )
}

/// Aggregates the parent and per-switch RMS jobs into one backend-neutral state.
///
/// A completed parent is not sufficient evidence of success. RMS creates one child
/// job per target switch, so completion requires every child declared by the parent to
/// be present and completed. A missing child remains pending because RMS may expose it
/// on a later poll. A missing parent or unknown execution state cannot establish the
/// outcome and is reported as [`ComponentManagerError::OperationOutcomeUnknown`].
fn summarize_rms_switch_factory_reset_jobs(
    job_id: &str,
    jobs: &[rms::JobStatus],
) -> Result<SwitchFactoryResetJobStatus, ComponentManagerError> {
    let parent = jobs
        .iter()
        .find(|job| job.job_id == job_id)
        .ok_or_else(|| {
            ComponentManagerError::OperationOutcomeUnknown(format!(
                "RMS returned no state for switch factory-reset job {job_id}; the reset outcome is unknown"
            ))
        })?;

    let children: Vec<&rms::JobStatus> = jobs
        .iter()
        .filter(|job| {
            job.job_id != job_id
                && (job.parent_job_id.as_deref() == Some(job_id)
                    || parent
                        .child_job_ids
                        .iter()
                        .any(|child_job_id| child_job_id == &job.job_id))
        })
        .collect();

    // Child failures identify the affected switch and therefore provide more useful
    // diagnostics than the aggregate parent failure.
    if let Some(failed) = children.iter().find(|job| {
        matches!(
            rms::JobExecutionState::try_from(job.execution_state),
            Ok(rms::JobExecutionState::Failed)
        )
    }) {
        return Ok(SwitchFactoryResetJobStatus {
            state: SwitchFactoryResetState::Failed,
            error: Some(rms_switch_factory_reset_job_failure(failed)),
        });
    }

    if matches!(
        rms::JobExecutionState::try_from(parent.execution_state),
        Ok(rms::JobExecutionState::Failed)
    ) {
        return Ok(SwitchFactoryResetJobStatus {
            state: SwitchFactoryResetState::Failed,
            error: Some(rms_switch_factory_reset_job_failure(parent)),
        });
    }

    // A successful reset must include per-switch evidence. Keep polling when RMS has
    // not exposed any children yet or omits a child declared by the parent.
    let mut pending = children.is_empty()
        || parent.child_job_ids.iter().any(|child_job_id| {
            !children
                .iter()
                .any(|job| job.job_id.as_str() == child_job_id)
        });

    for job in std::iter::once(parent).chain(children) {
        match rms::JobExecutionState::try_from(job.execution_state) {
            Ok(rms::JobExecutionState::Queued | rms::JobExecutionState::Running) => {
                pending = true;
            }
            Ok(rms::JobExecutionState::Completed) => {}
            Ok(rms::JobExecutionState::Failed) => {
                return Ok(SwitchFactoryResetJobStatus {
                    state: SwitchFactoryResetState::Failed,
                    error: Some(rms_switch_factory_reset_job_failure(job)),
                });
            }
            Ok(rms::JobExecutionState::Unspecified) | Err(_) => {
                return Err(ComponentManagerError::OperationOutcomeUnknown(format!(
                    "RMS switch factory-reset job {} returned unknown execution state {}; the reset outcome is unknown",
                    job.job_id, job.execution_state
                )));
            }
        }
    }

    let state = if pending {
        SwitchFactoryResetState::Pending
    } else {
        SwitchFactoryResetState::Completed
    };

    Ok(SwitchFactoryResetJobStatus { state, error: None })
}

/// Reads and aggregates the RMS parent and child states for a factory reset.
async fn rms_get_switch_factory_reset_job_status(
    client: &dyn RmsApi,
    job_id: &str,
) -> Result<SwitchFactoryResetJobStatus, ComponentManagerError> {
    if job_id.trim().is_empty() {
        return Err(ComponentManagerError::InvalidArgument(
            "switch factory-reset job ID must be non-empty".to_string(),
        ));
    }

    let request = rms::GetJobStatusRequest {
        job_id: job_id.to_string(),
        include_child_job_states: true,
    };

    match red::instrumented("rms", "get_job_status", client.get_job_status(request)).await {
        Ok(response) => summarize_rms_switch_factory_reset_jobs(job_id, &response.job_states),
        Err(RackManagerError::ApiInvocationError(status)) => match status.code() {
            // Once submission returned a durable handle, a rejected or missing
            // observation does not prove that the destructive job never started.
            tonic::Code::NotFound => Err(ComponentManagerError::OperationOutcomeUnknown(format!(
                "RMS has no state for switch factory-reset job {job_id}; the reset outcome is unknown"
            ))),
            tonic::Code::InvalidArgument => {
                Err(ComponentManagerError::OperationOutcomeUnknown(format!(
                    "RMS rejected observation of switch factory-reset job {job_id}; the reset outcome is unknown"
                )))
            }
            tonic::Code::Unimplemented => Err(ComponentManagerError::Unsupported(
                "RMS does not support switch factory-reset job status".to_string(),
            )),
            tonic::Code::Unavailable
            | tonic::Code::DeadlineExceeded
            | tonic::Code::Cancelled
            | tonic::Code::ResourceExhausted => Err(ComponentManagerError::Unavailable(
                "RMS switch factory-reset job status is temporarily unavailable".to_string(),
            )),
            _ => Err(ComponentManagerError::Rms(format!(
                "RMS could not read switch factory-reset job {job_id}: {status}"
            ))),
        },
        Err(RackManagerError::TlsError(_)) => Err(ComponentManagerError::Unavailable(
            "RMS switch factory-reset job status is temporarily unavailable".to_string(),
        )),
    }
}

/// Summarizes parent and child job observations for one password rotation.
///
/// Child failures are preferred because they describe the individual switch,
/// while an absent job remains an observation rather than proof of no mutation.
fn summarize_password_rotation_jobs(
    job_id: &str,
    jobs: &[rms::JobStatus],
) -> SwitchPasswordRotationState {
    let related_jobs: Vec<&rms::JobStatus> = jobs
        .iter()
        .filter(|job| job.job_id == job_id || job.parent_job_id.as_deref() == Some(job_id))
        .collect();

    if related_jobs.is_empty() {
        return SwitchPasswordRotationState::NotFound;
    }

    // A child carries the device-specific failure. Prefer it over the parent,
    // whose error may only summarize the batch.
    if let Some(failed) = related_jobs.iter().find(|job| {
        job.parent_job_id.as_deref() == Some(job_id)
            && matches!(
                map_rms_password_rotation_state(job),
                SwitchPasswordRotationState::Failed
            )
    }) {
        return map_rms_password_rotation_state(failed);
    }

    if let Some(failed) = related_jobs.iter().find(|job| {
        job.job_id == job_id
            && matches!(
                map_rms_password_rotation_state(job),
                SwitchPasswordRotationState::Failed
            )
    }) {
        return map_rms_password_rotation_state(failed);
    }

    let states: Vec<SwitchPasswordRotationState> = related_jobs
        .iter()
        .map(|job| map_rms_password_rotation_state(job))
        .collect();

    if states
        .iter()
        .all(|state| *state == SwitchPasswordRotationState::Completed)
    {
        SwitchPasswordRotationState::Completed
    } else if states.contains(&SwitchPasswordRotationState::Pending) {
        SwitchPasswordRotationState::Pending
    } else {
        SwitchPasswordRotationState::Unknown
    }
}

/// Reads and classifies the latest password-rotation job observation.
async fn rms_get_switch_password_rotation_job_status(
    client: &dyn RmsApi,
    job_id: &str,
) -> Result<SwitchPasswordRotationState, ComponentManagerError> {
    if job_id.is_empty() {
        return Err(ComponentManagerError::InvalidArgument(
            "switch password rotation job ID must be non-empty".to_string(),
        ));
    }

    let request = rms::GetJobStatusRequest {
        job_id: job_id.to_string(),
        include_child_job_states: true,
    };

    match red::instrumented("rms", "get_job_status", client.get_job_status(request)).await {
        Ok(response) => Ok(summarize_password_rotation_jobs(
            job_id,
            &response.job_states,
        )),
        Err(RackManagerError::ApiInvocationError(status))
            if status.code() == tonic::Code::NotFound =>
        {
            Ok(SwitchPasswordRotationState::NotFound)
        }
        Err(RackManagerError::ApiInvocationError(status))
            if status.code() == tonic::Code::InvalidArgument =>
        {
            Err(ComponentManagerError::InvalidArgument(
                "RMS rejected the switch password-rotation job status request".to_string(),
            ))
        }
        Err(RackManagerError::ApiInvocationError(status))
            if status.code() == tonic::Code::Unimplemented =>
        {
            Err(ComponentManagerError::Unsupported(
                "RMS does not support switch password-rotation job status".to_string(),
            ))
        }
        Err(RackManagerError::ApiInvocationError(status))
            if matches!(
                status.code(),
                tonic::Code::Unavailable
                    | tonic::Code::DeadlineExceeded
                    | tonic::Code::Cancelled
                    | tonic::Code::ResourceExhausted
            ) =>
        {
            Err(ComponentManagerError::Unavailable(
                "RMS switch password-rotation job status is temporarily unavailable".to_string(),
            ))
        }
        Err(RackManagerError::TlsError(_)) => Err(ComponentManagerError::Unavailable(
            "RMS switch password-rotation job status is temporarily unavailable".to_string(),
        )),
        Err(_) => Err(ComponentManagerError::Internal(
            "RMS switch password-rotation job status could not be read".to_string(),
        )),
    }
}

/// RMS may add certificate job states before Carbide knows about them. Count
/// each fallback, but keep it metric-only because polling can see the same
/// state indefinitely while waiting for RMS to move on.
#[derive(Event)]
#[event(
    event_name = "rms_switch_certificate_job_state_unrecognized",
    metric_name = "carbide_rms_switch_certificate_unrecognized_job_states_total",
    component = "component-manager",
    log = off,
    metric = counter,
    describe = "Number of unrecognized RMS switch certificate job states."
)]
struct RmsSwitchCertificateJobStateUnrecognized;

fn map_rms_configure_switch_certificate_job_state(
    state: &str,
) -> Option<ConfigureSwitchCertificateState> {
    match state.to_ascii_lowercase().as_str() {
        "queued" | "pending" => Some(ConfigureSwitchCertificateState::Started),
        "running" | "in_progress" | "active" => Some(ConfigureSwitchCertificateState::InProgress),
        "completed" | "success" | "done" => Some(ConfigureSwitchCertificateState::Completed),
        "failed" | "error" => Some(ConfigureSwitchCertificateState::Failed),
        _ => None,
    }
}

fn summarize_configure_switch_certificate_response(
    response: rms::ConfigureSwitchCertificateResponse,
    node_id: &str,
) -> (bool, Option<String>, Option<String>) {
    let node_job_id = response
        .jobs
        .iter()
        .find(|j| j.node_id == node_id && !j.job_id.is_empty())
        .map(|j| j.job_id.clone());

    summarize_firmware_batch(
        response.response,
        node_job_id,
        node_id,
        "RMS switch certificate configuration failed",
    )
}

async fn rms_configure_switch_certificate(
    client: &dyn RmsApi,
    device: rms::NodeInfo,
    domain_name: Option<&str>,
    services: Option<&[i32]>,
) -> Result<String, ComponentManagerError> {
    let node_id = device.node_id.clone();
    let request = rms::ConfigureSwitchCertificateRequest {
        nodes: Some(rms::NodeSet {
            nodes: vec![device],
        }),
        services: services.map(<[i32]>::to_vec).unwrap_or_default(),
        test_hello: true,
        domain: domain_name.map(str::to_owned),
    };

    let response = red::instrumented(
        "rms",
        "configure_switch_certificate",
        client.configure_switch_certificate(request),
    )
    .await
    .map_err(|e| {
        ComponentManagerError::Internal(format!(
            "failed to start RMS switch certificate configuration: {e}"
        ))
    })?;

    let (success, error, job_id) =
        summarize_configure_switch_certificate_response(response, &node_id);

    if success {
        job_id.ok_or_else(|| {
            ComponentManagerError::Internal(
                "RMS switch certificate configuration succeeded but returned no job id".into(),
            )
        })
    } else {
        Err(ComponentManagerError::Internal(error.unwrap_or_else(
            || "RMS switch certificate configuration failed".to_owned(),
        )))
    }
}

async fn rms_get_configure_switch_certificate_job_status(
    client: &dyn RmsApi,
    job_id: &str,
) -> Result<ConfigureSwitchCertificateJobStatus, ComponentManagerError> {
    let request = rms::GetConfigureSwitchCertificateJobStatusRequest {
        job_id: job_id.to_owned(),
    };

    let response = red::instrumented(
        "rms",
        "get_configure_switch_certificate_job_status",
        client.get_configure_switch_certificate_job_status(request),
    )
    .await
    .map_err(|e| {
        ComponentManagerError::Internal(format!(
            "failed to get RMS switch certificate job status: {e}"
        ))
    })?;

    if response.status != rms::ReturnCode::Success as i32 {
        let error = if response.error_message.is_empty() {
            if response.message.is_empty() {
                format!("RMS could not report status for switch certificate job {job_id}")
            } else {
                response.message
            }
        } else {
            response.error_message
        };
        return Ok(ConfigureSwitchCertificateJobStatus {
            state: ConfigureSwitchCertificateState::Failed,
            error: Some(error),
        });
    }

    let state =
        map_rms_configure_switch_certificate_job_state(&response.state).unwrap_or_else(|| {
            emit(RmsSwitchCertificateJobStateUnrecognized);
            ConfigureSwitchCertificateState::InProgress
        });
    let error = if matches!(state, ConfigureSwitchCertificateState::Failed) {
        Some(
            (!response.error_message.is_empty())
                .then_some(response.error_message)
                .or((!response.message.is_empty()).then_some(response.message))
                .unwrap_or_else(|| "switch certificate configuration failed".to_owned()),
        )
    } else {
        None
    };

    Ok(ConfigureSwitchCertificateJobStatus { state, error })
}

impl RmsBackend {
    /// Resolve RMS identities for pre-ingestion (row-less) compute trays from
    /// the expected inventory, keyed by BMC MAC.
    ///
    /// Only rack-scale trays (those whose expected record declares a rack_id)
    /// resolve; the BMC MAC is used as the RMS node id because no machine id
    /// exists yet.
    async fn resolve_pre_ingestion_compute_identities(
        &self,
        bmc_macs: &[MacAddress],
    ) -> Result<HashMap<MacAddress, ComputeTrayRmsIdentity>, ComponentManagerError> {
        if bmc_macs.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = db::expected_machine::find_rms_identities_by_bmc_macs(&self.db, bmc_macs)
            .await
            .map_err(|e| {
                ComponentManagerError::Internal(format!(
                    "failed to resolve pre-ingestion compute tray RMS identities: {e}"
                ))
            })?;

        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.bmc_mac_address,
                    ComputeTrayRmsIdentity {
                        identity: RmsIdentity {
                            // A row-less tray has no machine id; the BMC MAC is a
                            // stable per-device id and RMS treats node_id as an
                            // opaque string (the request also carries the full
                            // node descriptor and BMC endpoint).
                            node_id: row.bmc_mac_address.to_string(),
                            rack_id: row.rack_id.to_string(),
                            rack_profile_id: row.rack_profile_id,
                        },
                        bmc_mac: row.bmc_mac_address,
                    },
                )
            })
            .collect())
    }

    /// Apply a firmware object to one compute node and record the resulting job.
    ///
    /// Shared by the ingested and pre-ingestion paths of `update_firmware`. The
    /// resulting backend job id is persisted to `compute_firmware_object_jobs`
    /// keyed by BMC MAC so status queries survive a nico-api restart, whether or
    /// not the tray has a `machines` row yet.
    async fn apply_compute_firmware_object(
        &self,
        ep: &ComputeTrayEndpoint,
        identity: &ComputeTrayRmsIdentity,
        target_version: &str,
        options: &FirmwareUpdateOptions,
        component_filters: &[String],
    ) -> ComputeTrayResult {
        let resolved = match self.resolve_compute_node(identity) {
            Ok(resolved) => resolved,
            Err(error) => {
                return ComputeTrayResult {
                    bmc_ip: ep.bmc_ip,
                    bmc_mac: ep.bmc_mac,
                    success: false,
                    error: Some(error),
                };
            }
        };

        let device = build_compute_tray_node_info(ep, &resolved, identity.bmc_mac);

        let request = match apply_firmware_object_request(
            device,
            &resolved,
            target_version,
            options,
            component_filters,
        ) {
            Ok(request) => request,
            Err(e) => {
                return ComputeTrayResult {
                    bmc_ip: ep.bmc_ip,
                    bmc_mac: ep.bmc_mac,
                    success: false,
                    error: Some(e.to_string()),
                };
            }
        };

        match red::instrumented(
            "rms",
            "apply_firmware_object",
            self.client.apply_firmware_object(request),
        )
        .await
        {
            Ok(response) => {
                let (success, error, job_id) =
                    summarize_firmware_object_apply_response(response, &resolved.identity.node_id);

                if success {
                    if let Some(ref job_id) = job_id {
                        // Track both in memory and in the DB keyed by BMC MAC so
                        // status queries survive a nico-api restart, for both
                        // ingested and pre-ingestion trays.
                        self.firmware_jobs.lock().unwrap().insert(
                            ep.bmc_mac,
                            vec![RmsTrackedFirmwareJob::FirmwareObject(job_id.clone())],
                        );
                        if let Err(e) =
                            db::direct_dispatch_firmware_job::save(&self.db, ep.bmc_mac, job_id)
                                .await
                        {
                            tracing::warn!(
                                node_id = %identity.identity.node_id,
                                bmc_ip_address = %ep.bmc_ip,
                                bmc_mac_address = %ep.bmc_mac,
                                job_id = %job_id,
                                error = %e,
                                "failed to persist backend firmware job ID to database"
                            );
                        }
                    } else {
                        self.firmware_jobs.lock().unwrap().remove(&ep.bmc_mac);
                    }
                } else {
                    self.firmware_jobs.lock().unwrap().remove(&ep.bmc_mac);
                }

                ComputeTrayResult {
                    bmc_ip: ep.bmc_ip,
                    bmc_mac: ep.bmc_mac,
                    success,
                    error,
                }
            }
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %ep.bmc_ip,
                    error = %e,
                    "RMS firmware update failed for compute tray"
                );
                ComputeTrayResult {
                    bmc_ip: ep.bmc_ip,
                    bmc_mac: ep.bmc_mac,
                    success: false,
                    error: Some(e.to_string()),
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl ComputeTrayManager for RmsBackend {
    fn name(&self) -> &str {
        "rms"
    }

    fn backend(&self) -> ComputeTrayBackend {
        ComputeTrayBackend::Rms
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        // Ingested trays resolve their RMS identity from the machine row by BMC
        // IP.
        let bmc_ips: Vec<IpAddr> = endpoints.iter().map(|ep| ep.bmc_ip).collect();
        let ingested = resolve_compute_tray_identities(&self.db, &bmc_ips).await?;

        // Trays with no machine row yet (pre-ingestion) fall back to the
        // expected inventory keyed by BMC MAC. Only rack-scale (RMS-managed)
        // trays — those with a declared rack_id — resolve here; the BMC MAC
        // doubles as the RMS node id since no machine id exists.
        let pre_ingestion_macs: Vec<MacAddress> = endpoints
            .iter()
            .filter(|ep| !ingested.contains_key(&ep.bmc_ip))
            .map(|ep| ep.bmc_mac)
            .collect();
        let pre_ingestion = self
            .resolve_pre_ingestion_compute_identities(&pre_ingestion_macs)
            .await?;

        let operation = to_rms_power_operation(action);
        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            let identity = match ingested
                .get(&ep.bmc_ip)
                .or_else(|| pre_ingestion.get(&ep.bmc_mac))
            {
                Some(identity) => identity,
                None => {
                    results.push(ComputeTrayResult {
                        bmc_ip: ep.bmc_ip,
                        bmc_mac: ep.bmc_mac,
                        success: false,
                        error: Some(
                            "could not resolve RMS identity from database or expected inventory"
                                .into(),
                        ),
                    });
                    continue;
                }
            };

            let resolved = match self.resolve_compute_node(identity) {
                Ok(resolved) => resolved,
                Err(error) => {
                    results.push(ComputeTrayResult {
                        bmc_ip: ep.bmc_ip,
                        bmc_mac: ep.bmc_mac,
                        success: false,
                        error: Some(error),
                    });
                    continue;
                }
            };

            let device = build_compute_tray_node_info(ep, &resolved, identity.bmc_mac);

            let request = rms::BatchSetPowerStateRequest {
                nodes: Some(rms::NodeSet {
                    nodes: vec![device],
                }),
                operation,
            };

            match red::instrumented(
                "rms",
                "batch_set_power_state",
                self.client.batch_set_power_state(request),
            )
            .await
            {
                Ok(response) => {
                    let (success, error) =
                        summarize_power_batch(response.response.unwrap_or_default());
                    results.push(ComputeTrayResult {
                        bmc_ip: ep.bmc_ip,
                        bmc_mac: ep.bmc_mac,
                        success,
                        error,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        bmc_ip_address = %ep.bmc_ip,
                        error = %e,
                        "RMS power control failed for compute tray"
                    );
                    results.push(ComputeTrayResult {
                        bmc_ip: ep.bmc_ip,
                        bmc_mac: ep.bmc_mac,
                        success: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(results)
    }

    #[instrument(skip(self, target_version, options), fields(backend = "rms", force_update = options.force_update))]
    async fn update_firmware(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        target_version: &str,
        components: &[ComputeTrayComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        // Ingested trays resolve their RMS identity from the machine row by BMC
        // IP.
        let bmc_ips: Vec<IpAddr> = endpoints.iter().map(|ep| ep.bmc_ip).collect();
        let ingested = resolve_compute_tray_identities(&self.db, &bmc_ips).await?;

        // Trays with no machine row yet (pre-ingestion) fall back to the
        // expected inventory keyed by BMC MAC. Only rack-scale (RMS-managed)
        // trays — those with a declared rack_id — resolve here; the BMC MAC
        // doubles as the RMS node id since no machine id exists.
        let pre_ingestion_macs: Vec<MacAddress> = endpoints
            .iter()
            .filter(|ep| !ingested.contains_key(&ep.bmc_ip))
            .map(|ep| ep.bmc_mac)
            .collect();
        let pre_ingestion = self
            .resolve_pre_ingestion_compute_identities(&pre_ingestion_macs)
            .await?;

        let component_filters = compute_tray_firmware_object_component_filters(components);
        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            // Ingested trays resolve their RMS identity from the machine row by
            // BMC IP; pre-ingestion trays fall back to expected inventory by BMC
            // MAC. Either way the resulting job id is persisted uniformly to
            // compute_firmware_object_jobs (keyed by BMC MAC).
            let identity = match ingested
                .get(&ep.bmc_ip)
                .or_else(|| pre_ingestion.get(&ep.bmc_mac))
            {
                Some(identity) => identity,
                None => {
                    results.push(ComputeTrayResult {
                        bmc_ip: ep.bmc_ip,
                        bmc_mac: ep.bmc_mac,
                        success: false,
                        error: Some(
                            "could not resolve RMS identity from database or expected inventory"
                                .into(),
                        ),
                    });
                    continue;
                }
            };

            results.push(
                self.apply_compute_firmware_object(
                    ep,
                    identity,
                    target_version,
                    options,
                    &component_filters,
                )
                .await,
            );
        }

        Ok(results)
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn get_firmware_status(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError> {
        // Snapshot the in-memory job id per endpoint (keyed by BMC MAC) before
        // any awaits, so the lock is not held across the RMS calls below.
        let in_memory_jobs: Vec<Option<String>> = {
            let jobs = self.firmware_jobs.lock().unwrap();
            endpoints
                .iter()
                .map(|ep| {
                    jobs.get(&ep.bmc_mac).and_then(|jobs| {
                        jobs.iter().find_map(|job| match job {
                            RmsTrackedFirmwareJob::FirmwareObject(job_id) => Some(job_id.clone()),
                            RmsTrackedFirmwareJob::SwitchSystemImage(_) => None,
                        })
                    })
                })
                .collect()
        };

        let mut statuses = Vec::with_capacity(endpoints.len());

        for (ep, in_memory_job) in endpoints.iter().zip(in_memory_jobs) {
            // When the in-memory map has no job (e.g. after a pod restart), fall
            // back to the DB-persisted job id written by update_firmware, keyed
            // by BMC MAC for both ingested and pre-ingestion trays.
            let resolved_job_id: Option<String> = if in_memory_job.is_some() {
                in_memory_job
            } else {
                match db::direct_dispatch_firmware_job::get(&self.db, ep.bmc_mac).await {
                    Ok(db_job_id) => db_job_id,
                    Err(e) => {
                        tracing::warn!(
                            bmc_ip_address = %ep.bmc_ip,
                            bmc_mac_address = %ep.bmc_mac,
                            error = %e,
                            "failed to fetch persisted backend firmware job ID from database"
                        );
                        None
                    }
                }
            };
            let Some(job_id) = resolved_job_id else {
                statuses.push(ComputeTrayFirmwareUpdateStatus {
                    bmc_ip: ep.bmc_ip,
                    bmc_mac: ep.bmc_mac,
                    state: FirmwareState::Unknown,
                    target_version: String::new(),
                    error: Some("no firmware job tracked for this compute tray".into()),
                });
                continue;
            };
            let job_id = &job_id;

            let request = rms::GetFirmwareJobStatusRequest {
                job_id: job_id.clone(),
            };

            match red::instrumented(
                "rms",
                "get_firmware_job_status",
                self.client.get_firmware_job_status(request),
            )
            .await
            {
                Ok(response) => {
                    let status_success = response.status == rms::ReturnCode::Success as i32;
                    let state = if status_success {
                        map_rms_firmware_job_state(response.job_state)
                    } else {
                        FirmwareState::Unknown
                    };
                    let error = if response.error_message.is_empty() {
                        (!status_success).then(|| {
                            format!("RMS could not report status for firmware job {job_id}")
                        })
                    } else {
                        Some(response.error_message)
                    };

                    statuses.push(ComputeTrayFirmwareUpdateStatus {
                        bmc_ip: ep.bmc_ip,
                        bmc_mac: ep.bmc_mac,
                        state,
                        target_version: String::new(),
                        error,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        bmc_ip_address = %ep.bmc_ip,
                        job_id = %job_id,
                        error = %e,
                        "RMS firmware job status query failed"
                    );
                    statuses.push(ComputeTrayFirmwareUpdateStatus {
                        bmc_ip: ep.bmc_ip,
                        bmc_mac: ep.bmc_mac,
                        state: FirmwareState::Unknown,
                        target_version: String::new(),
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(statuses)
    }

    #[instrument(skip(self), fields(backend = "rms"))]
    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        list_firmware_object_ids(self.client.as_ref()).await
    }
}

#[cfg(test)]
mod tests {
    use api_test_helper::mock_rms::MockRmsApi;
    use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
    use carbide_test_support::{Check, check_values, value_scenarios};
    use carbide_uuid::machine::MachineId;
    use carbide_uuid::power_shelf::PowerShelfId;
    use carbide_uuid::rack::RackId;
    use carbide_uuid::switch::SwitchId;
    use model::rack::FirmwareUpgradeDeviceInfo;
    use model::rack_type::{
        RackCapabilitiesSet, RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch,
        RackHardwareTopology, RackProductFamily, RackProfile, RackProfileConfig,
    };

    use super::*;
    use crate::compute_tray_manager::{ComputeTrayManager, ComputeTrayVendor};
    use crate::config::SwitchMtlsService;
    use crate::power_shelf_manager::PowerShelfVendor;

    const KEY_ROLE: &str = "role";
    const ROLE_COMPUTE: &str = "compute";
    const ROLE_POWER_SHELF: &str = "power_shelf";
    const ROLE_SWITCH: &str = "switch";

    #[async_trait::async_trait]
    impl RmsSwitchSystemImageStatusApi for MockRmsApi {
        async fn get_switch_system_image_job_status(
            &self,
            cmd: rms::GetSwitchSystemImageJobStatusRequest,
        ) -> Result<rms::GetSwitchSystemImageJobStatusResponse, RackManagerError> {
            self.get_switch_system_image_job_status_for_test(cmd).await
        }
    }
    use crate::test_support::{
        CT_IP_1, CT_IP_2, CT_MAC_1, CT_MAC_2, PS_MAC_1, PS_MAC_2, SW_MAC_1, SW_MAC_2,
        TEST_RACK_PROFILE_ID, UNKNOWN_MAC, seed_machine, seed_test_data,
    };

    // ---- Mapping unit tests ----

    #[test]
    fn power_action_maps_to_rms_operation() {
        value_scenarios!(to_rms_power_operation:
            "power on" {
                PowerAction::On => rms::PowerOperation::On as i32,
            }

            "power off" {
                PowerAction::GracefulShutdown => rms::PowerOperation::Off as i32,
                PowerAction::ForceOff => rms::PowerOperation::Off as i32,
            }

            "reset" {
                PowerAction::GracefulRestart => rms::PowerOperation::Reset as i32,
                PowerAction::ForceRestart => rms::PowerOperation::Reset as i32,
                PowerAction::AcPowercycle => rms::PowerOperation::Reset as i32,
            }
        );
    }

    #[test]
    fn firmware_job_state_maps_each_variant() {
        value_scenarios!(run = |state: rms::FirmwareJobState| map_rms_firmware_job_state(state as i32);
            "states" {
                rms::FirmwareJobState::Queued => FirmwareState::Queued,
                rms::FirmwareJobState::Running => FirmwareState::InProgress,
                rms::FirmwareJobState::Completed => FirmwareState::Completed,
                rms::FirmwareJobState::Failed => FirmwareState::Failed,
            }
        );
    }

    #[test]
    fn firmware_job_state_unknown_for_unrecognized_value() {
        value_scenarios!(map_rms_firmware_job_state:
            "unrecognized" {
                9999 => FirmwareState::Unknown,
            }
        );
    }

    #[tokio::test]
    async fn nvos_polling_updates_node_id_and_aggregate_status() {
        let mock = Arc::new(MockRmsApi::new());

        mock.enqueue_get_switch_system_image_job_status(Ok(
            rms::GetSwitchSystemImageJobStatusResponse {
                status: rms::ReturnCode::Success as i32,
                state: "RUNNING".into(),
                node_id: "new-node-id".into(),
                ..Default::default()
            },
        ))
        .await;

        let manager = RmsNvosUpdateManager {
            client: mock.clone(),
        };

        let job = NvosUpdateJob {
            job_id: Some("parent-job".into()),
            firmware_id: "firmware-id".into(),
            image_filename: "nvos.img".into(),
            local_file_path: String::new(),
            version: None,
            status: Some("in_progress".into()),
            started_at: Some(chrono::Utc::now()),
            completed_at: Some(chrono::Utc::now()),
            switches: vec![NvosUpdateSwitchStatus {
                node_id: "old-node-id".into(),
                mac: "00:11:22:33:44:55".into(),
                bmc_ip: "10.0.0.10".into(),
                nvos_ip: "192.168.10.10".into(),
                status: "pending".into(),
                job_id: Some("child-job".into()),
                error_message: Some("stale error".into()),
            }],
        };

        let updated = manager
            .get_nvos_update_status(&job)
            .await
            .expect("NVOS polling should succeed");

        assert_eq!(updated.switches[0].node_id, "new-node-id");
        assert_eq!(updated.switches[0].status, "in_progress");
        assert_eq!(updated.switches[0].error_message, None);
        assert_eq!(updated.status.as_deref(), Some("in_progress"));
        assert_eq!(updated.completed_at, None);

        mock.enqueue_get_switch_system_image_job_status(Ok(
            rms::GetSwitchSystemImageJobStatusResponse {
                status: rms::ReturnCode::Success as i32,
                state: "COMPLETED".into(),
                ..Default::default()
            },
        ))
        .await;

        let completed = manager
            .get_nvos_update_status(&updated)
            .await
            .expect("NVOS polling should succeed");

        let repolled = manager
            .get_nvos_update_status(&completed)
            .await
            .expect("completed NVOS polling should succeed");

        assert_eq!(completed.status.as_deref(), Some("completed"));
        assert!(completed.completed_at.is_some());
        assert_eq!(repolled.completed_at, completed.completed_at);

        let calls = mock.get_switch_system_image_job_status_calls().await;

        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| call.job_id == "child-job"));
    }

    #[test]
    fn nvos_polling_maps_failed_state_and_uses_error_message() {
        let mut switch = NvosUpdateSwitchStatus {
            node_id: "node-id".into(),
            mac: "00:11:22:33:44:55".into(),
            bmc_ip: "10.0.0.10".into(),
            nvos_ip: "192.168.10.10".into(),
            status: "in_progress".into(),
            job_id: Some("job-2".into()),
            error_message: None,
        };

        apply_nvos_job_status_response(
            &mut switch,
            "job-2",
            Ok(rms::GetSwitchSystemImageJobStatusResponse {
                status: rms::ReturnCode::Success as i32,
                state: "failed".into(),
                error_message: "image install failed".into(),
                ..Default::default()
            }),
        );

        assert_eq!(switch.status, "failed");

        assert_eq!(
            switch.error_message.as_deref(),
            Some("image install failed")
        );
    }

    #[test]
    fn nvos_polling_unknown_state_preserves_status_and_sets_error() {
        let mut switch = NvosUpdateSwitchStatus {
            node_id: "node-id".into(),
            mac: "00:11:22:33:44:55".into(),
            bmc_ip: "10.0.0.10".into(),
            nvos_ip: "192.168.10.10".into(),
            status: "pending".into(),
            job_id: Some("job-3".into()),
            error_message: None,
        };

        apply_nvos_job_status_response(
            &mut switch,
            "job-3",
            Ok(rms::GetSwitchSystemImageJobStatusResponse {
                status: rms::ReturnCode::Success as i32,
                state: "mystery".into(),
                ..Default::default()
            }),
        );

        assert_eq!(switch.status, "pending");

        assert_eq!(
            switch.error_message.as_deref(),
            Some("Unknown RMS switch image job state mystery")
        );
    }

    #[tokio::test]
    async fn rack_nvos_submission_builds_request_and_preserves_job_handles() {
        let mock = Arc::new(MockRmsApi::new());
        let rack_id = RackId::new("rack-1");
        let child_node_id = "switch-1";
        let parent_node_id = "switch-2";

        let child_switch = FirmwareUpgradeDeviceInfo {
            node_id: child_node_id.to_string(),
            mac: SW_MAC_1.to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            bmc_username: "admin".to_string(),
            bmc_password: "password".to_string(),
            os_mac: Some("11:22:33:44:55:66".to_string()),
            os_ip: Some("192.0.2.20".to_string()),
            os_username: Some("nvos-admin".to_string()),
            os_password: Some("nvos-password".to_string()),
            os_hostname: None,
        };

        let parent_switch = FirmwareUpgradeDeviceInfo {
            node_id: parent_node_id.to_string(),
            mac: SW_MAC_2.to_string(),
            bmc_ip: "192.0.2.11".to_string(),
            os_mac: Some("22:33:44:55:66:77".to_string()),
            os_ip: Some("192.0.2.21".to_string()),
            ..child_switch.clone()
        };

        mock.enqueue_apply_switch_system_image(Ok(rms::ApplySwitchSystemImageResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                job_id: "nvos-parent-job".to_string(),
                ..Default::default()
            }),
            jobs: vec![rms::SwitchSystemImageUpdateJobInfo {
                node_id: child_node_id.to_string(),
                job_id: "nvos-child-job".to_string(),
            }],
            object_id: "fw-json".to_string(),
            image_filename: "nvos.img".to_string(),
        }))
        .await;

        let manager = RmsNvosUpdateManager {
            client: mock.clone(),
        };

        let job = manager
            .start_nvos_update(NvosUpdateRequest {
                rack_id: &rack_id,
                profile: &test_rms_profile(),
                config_json: r#"{"Id":"fw-nvos-default"}"#,
                access_token: "token",
                switches: vec![child_switch, parent_switch],
            })
            .await
            .expect("NVOS submission should succeed");

        assert_eq!(job.job_id.as_deref(), Some("nvos-parent-job"));
        assert_eq!(job.switches[0].job_id.as_deref(), Some("nvos-child-job"));
        assert_eq!(job.switches[1].job_id.as_deref(), Some("nvos-parent-job"));
        assert_eq!(job.status.as_deref(), Some("in_progress"));

        let calls = mock.apply_switch_system_image_calls().await;

        let [call] = calls.as_slice() else {
            panic!("expected one ApplySwitchSystemImage request");
        };

        assert_eq!(call.rack_id, rack_id.to_string());
        assert_eq!(call.config_json, r#"{"Id":"fw-nvos-default"}"#);
        assert_eq!(call.access_token.as_deref(), Some("token"));
        assert_eq!(call.software_type, "prod");
        assert_eq!(call.hardware_type, ANY_RACK_HARDWARE_TYPE);

        let nodes = call.nodes.as_ref().expect("request nodes");

        let [child_node, parent_node] = nodes.nodes.as_slice() else {
            panic!("expected two switch nodes");
        };

        assert_eq!(child_node.node_id, child_node_id);
        assert_eq!(parent_node.node_id, parent_node_id);

        let bmc_endpoint = child_node.bmc_endpoint.as_ref().expect("BMC endpoint");
        let bmc_interface = bmc_endpoint.interface.as_ref().expect("BMC interface");

        assert_eq!(bmc_interface.ip_address, "192.0.2.10");
        assert_eq!(bmc_interface.mac_address, SW_MAC_1);

        assert!(matches!(
            bmc_endpoint
                .credentials
                .as_ref()
                .and_then(|credentials| credentials.auth.as_ref()),
            Some(rms::credentials::Auth::UserPass(credentials))
                if credentials.username == "admin" && credentials.password == "password"
        ));

        let host_endpoint = child_node.host_endpoint.as_ref().expect("NVOS endpoint");
        let host_interface = host_endpoint.interface.as_ref().expect("NVOS interface");

        assert_eq!(host_interface.ip_address, "192.0.2.20");
        assert_eq!(host_interface.mac_address, "11:22:33:44:55:66");

        assert!(matches!(
            host_endpoint
                .credentials
                .as_ref()
                .and_then(|credentials| credentials.auth.as_ref()),
            Some(rms::credentials::Auth::UserPass(credentials))
                if credentials.username == "nvos-admin"
                    && credentials.password == "nvos-password"
        ));
    }

    #[tokio::test]
    async fn rack_nvos_submission_preserves_rms_rpc_error_classification() {
        let mock = Arc::new(MockRmsApi::new());
        let rack_id = RackId::new("rack-1");

        let switch = FirmwareUpgradeDeviceInfo {
            node_id: "switch-1".to_string(),
            mac: SW_MAC_1.to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            bmc_username: "admin".to_string(),
            bmc_password: "password".to_string(),
            os_mac: Some("11:22:33:44:55:66".to_string()),
            os_ip: Some("192.0.2.20".to_string()),
            os_username: Some("nvos-admin".to_string()),
            os_password: Some("nvos-password".to_string()),
            os_hostname: None,
        };

        mock.enqueue_apply_switch_system_image(Err(RackManagerError::ApiInvocationError(
            tonic::Status::invalid_argument("unsupported node descriptor"),
        )))
        .await;

        let manager = RmsNvosUpdateManager {
            client: mock.clone(),
        };

        let result = manager
            .start_nvos_update(NvosUpdateRequest {
                rack_id: &rack_id,
                profile: &test_rms_profile(),
                config_json: r#"{"Id":"fw-nvos-default"}"#,
                access_token: "token",
                switches: vec![switch.clone()],
            })
            .await;

        let Err(ComponentManagerError::InvalidArgument(cause)) = result else {
            panic!("expected RMS InvalidArgument to remain InvalidArgument");
        };

        assert!(cause.contains("unsupported node descriptor"));

        mock.enqueue_apply_switch_system_image(Err(RackManagerError::ApiInvocationError(
            tonic::Status::unavailable("RMS unavailable"),
        )))
        .await;

        let result = manager
            .start_nvos_update(NvosUpdateRequest {
                rack_id: &rack_id,
                profile: &test_rms_profile(),
                config_json: r#"{"Id":"fw-nvos-default"}"#,
                access_token: "token",
                switches: vec![switch],
            })
            .await;

        let Err(ComponentManagerError::Internal(cause)) = result else {
            panic!("expected RMS Unavailable to remain Internal");
        };

        assert!(cause.contains("RMS unavailable"));
    }

    #[test]
    fn switch_system_image_job_state_maps_cancelled_and_verifying() {
        value_scenarios!(map_rms_switch_system_image_job_state:
            "cancelled" {
                "cancelled" => FirmwareState::Cancelled,
            }

            "verifying" {
                "verifying" => FirmwareState::Verifying,
            }
        );
    }

    #[test]
    fn switch_slot_and_tray_response_keeps_partial_values_and_one_diagnostic() {
        let details = |slot_number, tray_index| {
            Some(rms::NodeDeviceInfo {
                slot_number,
                tray_index,
                ..Default::default()
            })
        };
        check_values(
            [
                Check {
                    scenario: "missing device details",
                    input: None,
                    expect: RmsSwitchSlotAndTrayObservation {
                        slot_number: None,
                        tray_index: None,
                        error: Some("RMS returned no device info".to_string()),
                    },
                },
                Check {
                    scenario: "valid values",
                    input: details(Some(12), Some(4)),
                    expect: RmsSwitchSlotAndTrayObservation {
                        slot_number: Some(12),
                        tray_index: Some(4),
                        error: None,
                    },
                },
                Check {
                    scenario: "absent optional field",
                    input: details(Some(12), None),
                    expect: RmsSwitchSlotAndTrayObservation {
                        slot_number: Some(12),
                        tray_index: None,
                        error: None,
                    },
                },
                Check {
                    scenario: "invalid slot keeps tray",
                    input: details(Some(i32::MAX as u32 + 1), Some(4)),
                    expect: RmsSwitchSlotAndTrayObservation {
                        slot_number: None,
                        tray_index: Some(4),
                        error: Some(
                            "RMS returned slot_number outside the supported range".to_string(),
                        ),
                    },
                },
                Check {
                    scenario: "invalid tray keeps slot",
                    input: details(Some(12), Some(u32::MAX)),
                    expect: RmsSwitchSlotAndTrayObservation {
                        slot_number: Some(12),
                        tray_index: None,
                        error: Some(
                            "RMS returned tray_index outside the supported range".to_string(),
                        ),
                    },
                },
                Check {
                    scenario: "invalid fields share one diagnostic",
                    input: details(Some(u32::MAX), Some(u32::MAX)),
                    expect: RmsSwitchSlotAndTrayObservation {
                        slot_number: None,
                        tray_index: None,
                        error: Some(
                            "RMS returned slot_number and tray_index outside the supported range"
                                .to_string(),
                        ),
                    },
                },
            ],
            |details| classify_rms_switch_slot_and_tray(details.as_ref()),
        );
    }

    #[test]
    fn scale_up_fabric_response_status_preserves_unknown_codes() {
        value_scenarios!(scale_up_fabric_response_status_from_rms:
            "known codes" {
                rms::ReturnCode::Success as i32 => ScaleUpFabricResponseStatus::Success,
                rms::ReturnCode::Failure as i32 => ScaleUpFabricResponseStatus::Failure,
            }

            "unknown codes" {
                rms::ReturnCode::Unspecified as i32 => ScaleUpFabricResponseStatus::Unknown(
                    rms::ReturnCode::Unspecified as i32,
                ),
                17 => ScaleUpFabricResponseStatus::Unknown(17),
            }
        );
    }

    #[test]
    fn scale_up_fabric_job_status_maps_lifecycle_and_visibility() {
        let response = |job_id: &str,
                        execution_state: i32,
                        description: &str,
                        error_message: &str| rms::GetJobStatusResponse {
            job_states: vec![rms::JobStatus {
                job_id: job_id.to_string(),
                execution_state,
                state_description: description.to_string(),
                error_message: error_message.to_string(),
                ..Default::default()
            }],
        };

        value_scenarios!(run = |response| scale_up_fabric_job_status_from_rms("job-1", response);
            "job visibility" {
                rms::GetJobStatusResponse::default() => None,
                response("other-job", rms::JobExecutionState::Completed as i32, "", "") => None,
            }

            "pending jobs" {
                response("job-1", rms::JobExecutionState::Queued as i32, "queued", "")
                    => Some(ScaleUpFabricManagerJobStatus::Pending {
                        description: "queued".to_string(),
                    }),
                response("job-1", rms::JobExecutionState::Running as i32, "reconciling", "")
                    => Some(ScaleUpFabricManagerJobStatus::Pending {
                        description: "reconciling".to_string(),
                    }),
            }

            "terminal jobs" {
                response("job-1", rms::JobExecutionState::Completed as i32, "", "")
                    => Some(ScaleUpFabricManagerJobStatus::Completed),
                response("job-1", rms::JobExecutionState::Failed as i32, "", "")
                    => Some(ScaleUpFabricManagerJobStatus::Failed { error: None }),
                response("job-1", rms::JobExecutionState::Failed as i32, "", "fabric rejected")
                    => Some(ScaleUpFabricManagerJobStatus::Failed {
                        error: Some("fabric rejected".to_string()),
                    }),
            }

            "unknown states" {
                response("job-1", rms::JobExecutionState::Unspecified as i32, "", "")
                    => Some(ScaleUpFabricManagerJobStatus::Unknown {
                        execution_state: rms::JobExecutionState::Unspecified as i32,
                    }),
                response("job-1", 17, "", "")
                    => Some(ScaleUpFabricManagerJobStatus::Unknown { execution_state: 17 }),
            }
        );
    }

    #[test]
    fn scale_up_fabric_status_conversion_keeps_required_controller_data() {
        let status = scale_up_fabric_status_from_rms(rms::GetScaleUpFabricStatusResponse {
            status: rms::ReturnCode::Success as i32,
            fabric_status: Some(rms::ScaleUpFabricStatus {
                switches: vec![rms::ScaleUpFabricSwitchStatus {
                    node_id: "switch-1".to_string(),
                    enabled: true,
                    error_message: "inspection warning".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            error_message: "response warning".to_string(),
        });

        assert_eq!(
            status,
            ScaleUpFabricStatus {
                status: ScaleUpFabricResponseStatus::Success,
                switches: Some(vec![ScaleUpFabricSwitchStatus {
                    node_id: "switch-1".to_string(),
                    enabled: true,
                    error_message: "inspection warning".to_string(),
                }]),
                error_message: "response warning".to_string(),
            }
        );

        assert_eq!(
            scale_up_fabric_status_from_rms(rms::GetScaleUpFabricStatusResponse {
                status: rms::ReturnCode::Success as i32,
                fabric_status: None,
                error_message: String::new(),
            })
            .switches,
            None
        );
    }

    #[test]
    fn scale_up_fabric_service_status_conversion_normalizes_rms_payloads() {
        let entry = |status_json: &str, error_message: &str| rms::ScaleUpFabricServiceStatusEntry {
            status_json: status_json.to_string(),
            error_message: error_message.to_string(),
        };

        let status = |fabric_manager_state,
                      addition_info: Option<&str>,
                      reason: Option<&str>,
                      error_message: Option<&str>| FabricManagerStatus {
            fabric_manager_state,
            addition_info: addition_info.map(str::to_string),
            reason: reason.map(str::to_string),
            error_message: error_message.map(str::to_string),
        };

        check_values(
            [
                Check {
                    scenario: "ok preserves service details",
                    input: entry(
                        r#"{"addition-info":"CONTROL_PLANE_STATE_CONFIGURED","reason":"","status":"ok"}"#,
                        "",
                    ),
                    expect: status(
                        FabricManagerState::Ok,
                        Some("CONTROL_PLANE_STATE_CONFIGURED"),
                        Some(""),
                        None,
                    ),
                },
                Check {
                    scenario: "not ok preserves service details",
                    input: entry(
                        r#"{"addition-info":"","reason":"stopped by user","status":"not ok"}"#,
                        "",
                    ),
                    expect: status(
                        FabricManagerState::NotOk,
                        Some(""),
                        Some("stopped by user"),
                        None,
                    ),
                },
                Check {
                    scenario: "unknown status keeps available details",
                    input: entry(
                        r#"{"addition-info":"pending","reason":"new RMS state","status":"unexpected"}"#,
                        "",
                    ),
                    expect: status(
                        FabricManagerState::Unknown,
                        Some("pending"),
                        Some("new RMS state"),
                        None,
                    ),
                },
                Check {
                    scenario: "empty status json is unknown",
                    input: entry("", ""),
                    expect: status(FabricManagerState::Unknown, None, None, None),
                },
                Check {
                    scenario: "error message takes precedence over status json",
                    input: entry(
                        r#"{"addition-info":"CONTROL_PLANE_STATE_CONFIGURED","status":"ok"}"#,
                        "nmx-controller not started",
                    ),
                    expect: status(
                        FabricManagerState::Unknown,
                        None,
                        None,
                        Some("nmx-controller not started"),
                    ),
                },
                Check {
                    scenario: "malformed json is unknown",
                    input: entry("{not-json", ""),
                    expect: status(FabricManagerState::Unknown, None, None, None),
                },
            ],
            |entry| fabric_manager_status_from_rms("switch-1", entry),
        );
    }

    #[test]
    fn scale_up_fabric_configuration_requires_durable_job_id() {
        for job_id in ["", " \t"] {
            let result = scale_up_fabric_manager_job_id_from_rms(
                rms_v2::ConfigureScaleUpFabricManagerResponse {
                    job_id: job_id.to_string(),
                },
            );

            assert!(matches!(
                result,
                Err(ComponentManagerError::OperationOutcomeUnknown(_))
            ));
        }
    }

    #[test]
    fn configure_switch_certificate_job_state_maps_rms_states() {
        value_scenarios!(
            run = map_rms_configure_switch_certificate_job_state;
            "started" {
                "queued" => Some(ConfigureSwitchCertificateState::Started),
                "pending" => Some(ConfigureSwitchCertificateState::Started),
            }

            "in progress" {
                "running" => Some(ConfigureSwitchCertificateState::InProgress),
                "in_progress" => Some(ConfigureSwitchCertificateState::InProgress),
                "active" => Some(ConfigureSwitchCertificateState::InProgress),
            }

            "completed" {
                "completed" => Some(ConfigureSwitchCertificateState::Completed),
                "success" => Some(ConfigureSwitchCertificateState::Completed),
                "done" => Some(ConfigureSwitchCertificateState::Completed),
            }

            "failed" {
                "failed" => Some(ConfigureSwitchCertificateState::Failed),
                "error" => Some(ConfigureSwitchCertificateState::Failed),
            }

            "case insensitive" {
                "RUNNING" => Some(ConfigureSwitchCertificateState::InProgress),
            }

            "unrecognized" {
                "waiting_for_reboot" => None,
                "" => None,
            }
        );
    }

    #[tokio::test]
    async fn unrecognized_switch_certificate_job_state_keeps_polling_and_counts_silently() {
        const METRIC: &str = "carbide_rms_switch_certificate_unrecognized_job_states_total";

        let mock = MockRmsApi::new();
        mock.enqueue_get_configure_switch_certificate_job_status(Ok(
            MockRmsApi::configure_switch_certificate_job_status_ok("waiting_for_reboot"),
        ))
        .await;

        let metrics = MetricsCapture::start();
        let (status, logs) = capture_logs_async(rms_get_configure_switch_certificate_job_status(
            &mock,
            "cert-job-1",
        ))
        .await;
        let status = status.expect("unknown RMS state keeps certificate polling active");

        assert_eq!(status.state, ConfigureSwitchCertificateState::InProgress);
        assert!(status.error.is_none());
        assert!(
            logs.iter().all(|log| log.field("event_name")
                != Some("rms_switch_certificate_job_state_unrecognized")),
            "the polling fallback remains metric-only"
        );
        assert_eq!(metrics.counter_delta(METRIC, &[]), 1.0);

        let calls = mock
            .get_configure_switch_certificate_job_status_calls()
            .await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].job_id, "cert-job-1");
    }

    #[test]
    fn password_rotation_job_state_maps_each_rms_variant() {
        value_scenarios!(run = |state: rms::JobExecutionState| map_rms_password_rotation_state(
            &rms::JobStatus {
                execution_state: state as i32,
                ..Default::default()
            }
        );
            "states" {
                rms::JobExecutionState::Unspecified => SwitchPasswordRotationState::Unknown,
                rms::JobExecutionState::Queued => SwitchPasswordRotationState::Pending,
                rms::JobExecutionState::Running => SwitchPasswordRotationState::Pending,
                rms::JobExecutionState::Completed => SwitchPasswordRotationState::Completed,
                rms::JobExecutionState::Failed => SwitchPasswordRotationState::Failed,
            }
        );
    }

    #[test]
    fn password_rotation_job_summary_handles_eventual_visibility_and_child_failure() {
        let job = |job_id: &str,
                   parent_job_id: Option<&str>,
                   execution_state: rms::JobExecutionState,
                   error_code: rms::JobError| rms::JobStatus {
            job_id: job_id.to_string(),
            parent_job_id: parent_job_id.map(str::to_string),
            execution_state: execution_state as i32,
            error_code: error_code as i32,
            ..Default::default()
        };

        let cases = [
            (
                "job not visible",
                Vec::new(),
                SwitchPasswordRotationState::NotFound,
            ),
            (
                "queued parent",
                vec![job(
                    "password-job",
                    None,
                    rms::JobExecutionState::Queued,
                    rms::JobError::Unspecified,
                )],
                SwitchPasswordRotationState::Pending,
            ),
            (
                "completed child visible before parent",
                vec![job(
                    "child-1",
                    Some("password-job"),
                    rms::JobExecutionState::Completed,
                    rms::JobError::Unspecified,
                )],
                SwitchPasswordRotationState::Completed,
            ),
            (
                "running child",
                vec![
                    job(
                        "password-job",
                        None,
                        rms::JobExecutionState::Queued,
                        rms::JobError::Unspecified,
                    ),
                    job(
                        "child-1",
                        Some("password-job"),
                        rms::JobExecutionState::Running,
                        rms::JobError::Unspecified,
                    ),
                ],
                SwitchPasswordRotationState::Pending,
            ),
            (
                "child failure overrides generic parent",
                vec![
                    job(
                        "password-job",
                        None,
                        rms::JobExecutionState::Failed,
                        rms::JobError::Other,
                    ),
                    job(
                        "child-1",
                        Some("password-job"),
                        rms::JobExecutionState::Failed,
                        rms::JobError::Unauthenticated,
                    ),
                ],
                SwitchPasswordRotationState::Failed,
            ),
        ];

        for (scenario, jobs, expected) in cases {
            assert_eq!(
                summarize_password_rotation_jobs("password-job", &jobs),
                expected,
                "{scenario}"
            );
        }
    }

    #[test]
    fn factory_reset_job_summary_requires_parent_and_all_declared_children() {
        let job = |job_id: &str,
                   parent_job_id: Option<&str>,
                   child_job_ids: &[&str],
                   execution_state: rms::JobExecutionState| rms::JobStatus {
            job_id: job_id.to_string(),
            parent_job_id: parent_job_id.map(str::to_string),
            child_job_ids: child_job_ids.iter().map(|id| (*id).to_string()).collect(),
            execution_state: execution_state as i32,
            ..Default::default()
        };

        let missing_child = vec![job(
            "factory-reset-job",
            None,
            &["child-1"],
            rms::JobExecutionState::Completed,
        )];

        assert_eq!(
            summarize_rms_switch_factory_reset_jobs("factory-reset-job", &missing_child)
                .expect("a missing declared child remains pending")
                .state,
            SwitchFactoryResetState::Pending
        );

        let no_children = vec![job(
            "factory-reset-job",
            None,
            &[],
            rms::JobExecutionState::Completed,
        )];

        assert_eq!(
            summarize_rms_switch_factory_reset_jobs("factory-reset-job", &no_children)
                .expect("a completed parent without per-switch state remains pending")
                .state,
            SwitchFactoryResetState::Pending
        );

        let completed = vec![
            job(
                "factory-reset-job",
                None,
                &["child-1"],
                rms::JobExecutionState::Completed,
            ),
            job(
                "child-1",
                Some("factory-reset-job"),
                &[],
                rms::JobExecutionState::Completed,
            ),
        ];

        assert_eq!(
            summarize_rms_switch_factory_reset_jobs("factory-reset-job", &completed)
                .expect("the complete job graph should succeed")
                .state,
            SwitchFactoryResetState::Completed
        );
    }

    #[test]
    fn factory_reset_job_summary_preserves_child_failure_details() {
        let failed_child = rms::JobStatus {
            job_id: "child-1".to_string(),
            parent_job_id: Some("factory-reset-job".to_string()),
            execution_state: rms::JobExecutionState::Failed as i32,
            error_code: rms::JobError::Unauthenticated as i32,
            error_message: "default login failed".to_string(),
            node_id: Some("switch-1".to_string()),
            ..Default::default()
        };

        let jobs = vec![
            rms::JobStatus {
                job_id: "factory-reset-job".to_string(),
                child_job_ids: vec!["child-1".to_string()],
                execution_state: rms::JobExecutionState::Failed as i32,
                ..Default::default()
            },
            failed_child,
        ];

        let status = summarize_rms_switch_factory_reset_jobs("factory-reset-job", &jobs)
            .expect("a failed job should remain an observed terminal status");

        assert_eq!(status.state, SwitchFactoryResetState::Failed);

        assert!(status.error.as_deref().is_some_and(|error| {
            error.contains("child-1")
                && error.contains("switch-1")
                && error.contains("JOB_ERROR_UNAUTHENTICATED")
                && error.contains("default login failed")
        }));
    }

    #[test]
    fn factory_reset_job_summary_preserves_unknown_outcomes() {
        assert!(matches!(
            summarize_rms_switch_factory_reset_jobs("factory-reset-job", &[]),
            Err(ComponentManagerError::OperationOutcomeUnknown(_))
        ));

        let jobs = vec![rms::JobStatus {
            job_id: "factory-reset-job".to_string(),
            execution_state: i32::MAX,
            ..Default::default()
        }];

        assert!(matches!(
            summarize_rms_switch_factory_reset_jobs("factory-reset-job", &jobs),
            Err(ComponentManagerError::OperationOutcomeUnknown(_))
        ));
    }

    #[test]
    fn aggregate_firmware_job_states_prioritizes_active_over_unknown() {
        value_scenarios!(run = |states| aggregate_firmware_job_states(states);
            "active wins over unknown" {
                &[
                    FirmwareState::Completed,
                    FirmwareState::Unknown,
                    FirmwareState::InProgress,
                ] => FirmwareState::InProgress,
                &[
                    FirmwareState::Completed,
                    FirmwareState::Queued,
                    FirmwareState::Unknown,
                ] => FirmwareState::Queued,
            }
        );
    }

    #[test]
    fn aggregate_firmware_job_states_terminal_failures_win() {
        value_scenarios!(run = |states| aggregate_firmware_job_states(states);
            "terminal failures win" {
                &[
                    FirmwareState::Failed,
                    FirmwareState::InProgress,
                    FirmwareState::Unknown,
                ] => FirmwareState::Failed,
                &[
                    FirmwareState::Cancelled,
                    FirmwareState::InProgress,
                    FirmwareState::Unknown,
                ] => FirmwareState::Cancelled,
            }
        );
    }

    #[test]
    fn power_shelf_firmware_object_filter_collapses_components() {
        let filters = power_shelf_firmware_object_component_filters(&[
            PowerShelfComponent::Pmc,
            PowerShelfComponent::Psu,
        ]);

        assert_eq!(filters, ["PowerShelfFW"]);
    }

    #[test]
    fn switch_firmware_object_filters_map_supported_components() {
        let filters = switch_firmware_object_component_filters(&[
            NvSwitchComponent::Bmc,
            NvSwitchComponent::Cpld,
            NvSwitchComponent::Bios,
        ]);

        assert_eq!(filters, ["BMC", "CPLD", "BIOS"]);
    }

    #[test]
    fn switch_firmware_object_filters_skip_nvos() {
        let filters = switch_firmware_object_component_filters(&[
            NvSwitchComponent::Bmc,
            NvSwitchComponent::Nvos,
        ]);

        assert_eq!(filters, ["BMC"]);
        assert!(switch_update_includes_firmware_object(&[
            NvSwitchComponent::Bmc,
            NvSwitchComponent::Nvos,
        ]));
        assert!(switch_update_includes_system_image(&[
            NvSwitchComponent::Bmc,
            NvSwitchComponent::Nvos,
        ]));
    }

    #[test]
    fn switch_empty_component_list_updates_firmware_object_and_system_image() {
        assert!(switch_update_includes_firmware_object(&[]));
        assert!(switch_update_includes_system_image(&[]));
        assert!(switch_firmware_object_component_filters(&[]).is_empty());
    }

    #[test]
    fn compute_tray_component_filters_map_to_rms_names() {
        assert_eq!(
            compute_tray_firmware_object_component_filters(&[
                ComputeTrayComponent::Bmc,
                ComputeTrayComponent::Bios,
            ]),
            vec!["BMC".to_owned(), "BIOS".to_owned()]
        );
        assert!(compute_tray_firmware_object_component_filters(&[]).is_empty());
    }

    #[test]
    fn firmware_update_missing_batch_response_is_failure() {
        let response = rms::ApplyFirmwareObjectResponse {
            response: None,
            object_id: "fw-json".to_owned(),
            jobs: vec![rms::NodeFirmwareJobInfo {
                node_id: "node-1".to_owned(),
                job_id: "job-1".to_owned(),
            }],
        };

        let (success, error, job_id) = summarize_firmware_object_apply_response(response, "node-1");

        assert!(!success);
        assert_eq!(error.as_deref(), Some("RMS firmware update failed"));
        assert_eq!(job_id.as_deref(), Some("job-1"));
    }

    #[tokio::test]
    async fn rms_calls_record_the_external_call_histogram_by_outcome() {
        use carbide_instrument::testing::MetricsCapture;

        let mock = MockRmsApi::new();
        mock.enqueue_batch_get_power_state(Ok(rms::BatchGetPowerStateResponse::default()))
            .await;
        mock.enqueue_batch_get_power_state(Err(RackManagerError::ApiInvocationError(
            tonic::Status::unavailable("down"),
        )))
        .await;

        let device_mac: MacAddress = PS_MAC_1.parse().unwrap();
        let metrics = MetricsCapture::start();
        let mut observed = Vec::new();
        for _ in 0..2 {
            observed.push(
                query_rms_power_state(
                    &mock,
                    rms::NodeInfo::default(),
                    "node-1",
                    device_mac,
                    "power shelf",
                )
                .await,
            );
        }
        assert!(
            observed[1].error.is_some(),
            "transport failure surfaces as an error"
        );

        for outcome in ["ok", "error"] {
            assert_eq!(
                metrics.histogram_count_delta(
                    "carbide_external_call_duration_milliseconds",
                    &[
                        ("backend", "rms"),
                        ("operation", "batch_get_power_state"),
                        ("outcome", outcome),
                    ],
                ),
                1,
                "{outcome}"
            );
        }
    }

    // ---- Test helpers ----

    fn make_ps_endpoint(mac: &str) -> PowerShelfEndpoint {
        use carbide_secrets::credentials::Credentials;
        PowerShelfEndpoint {
            pmc_ip: "10.0.0.1".parse().unwrap(),
            pmc_mac: mac.parse().unwrap(),
            pmc_vendor: PowerShelfVendor::Liteon,
            pmc_credentials: Credentials::UsernamePassword {
                username: "admin".into(),
                password: "pass".into(),
            },
        }
    }

    fn make_sw_endpoint(mac: &str) -> SwitchEndpoint {
        use carbide_secrets::credentials::Credentials;
        SwitchEndpoint {
            bmc_ip: "10.0.0.1".parse().unwrap(),
            bmc_mac: mac.parse().unwrap(),
            nvos_ip: "10.0.0.2".parse().unwrap(),
            nvos_mac: "11:22:33:44:55:66".parse().unwrap(),
            bmc_credentials: Credentials::UsernamePassword {
                username: "admin".to_string(),
                password: "pass".to_string(),
            },
            nvos_credentials: Credentials::UsernamePassword {
                username: "nvos-admin".to_string(),
                password: "nvos-pass".to_string(),
            },
            nvos_host_name: None,
        }
    }

    fn rack_profile_config() -> RackProfileConfig {
        RackProfileConfig {
            rack_profiles: [(
                TEST_RACK_PROFILE_ID.to_string(),
                RackProfile {
                    product_family: Some(RackProductFamily::Gb200),
                    rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
                    rack_capabilities: RackCapabilitiesSet {
                        compute: RackCapabilityCompute {
                            vendor: Some("NVIDIA".to_string()),
                            ..Default::default()
                        },
                        switch: RackCapabilitySwitch {
                            vendor: Some("NVIDIA".to_string()),
                            ..Default::default()
                        },
                        power_shelf: RackCapabilityPowerShelf {
                            vendor: Some("LiteOn".to_string()),
                            ..Default::default()
                        },
                    },
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn validation_requires_product_family_and_enabled_role_vendors() {
        let mut arbitrary_values = test_rms_profile();
        arbitrary_values.product_family =
            Some(RackProductFamily::Other("test-product-family".to_string()));

        arbitrary_values.rack_capabilities.compute.vendor = Some("test-compute-vendor".to_string());
        arbitrary_values.rack_capabilities.switch.vendor = Some("test-switch-vendor".to_string());
        arbitrary_values.rack_capabilities.power_shelf.vendor =
            Some("test-power-shelf-vendor".to_string());

        let mut missing_vendor = arbitrary_values.clone();
        missing_vendor.rack_capabilities.power_shelf.vendor = None;

        let mut missing_product_family = arbitrary_values.clone();
        missing_product_family.product_family = None;

        let mut blank_product_family = arbitrary_values.clone();
        blank_product_family.product_family = Some(RackProductFamily::Other(" \t ".to_string()));

        value_scenarios!(run = |profile: RackProfile| {
            let profiles = RackProfileConfig {
                rack_profiles: [(TEST_RACK_PROFILE_ID.to_string(), profile)]
                    .into_iter()
                    .collect(),
            };

            validate_rms_backend_rack_profiles(&ComponentManagerConfig::default(), &profiles)
                .is_ok()
            };

            "descriptor validation" {
                arbitrary_values => true,
                missing_vendor => false,
                missing_product_family => false,
                blank_product_family => false,
            }
        );
    }

    /// Create a backend with a real DB pool seeded with test data.
    async fn make_backend(
        pool: &sqlx::PgPool,
    ) -> (
        Arc<MockRmsApi>,
        RmsBackend,
        RackId,
        PowerShelfId,
        PowerShelfId,
        SwitchId,
        SwitchId,
    ) {
        let (rack_id, ps1, ps2, sw1, sw2) = seed_test_data(pool).await;
        let mock = Arc::new(MockRmsApi::new());
        let backend = RmsBackend::new(
            mock.clone(),
            Some(mock.clone()),
            pool.clone(),
            Arc::new(rack_profile_config()),
            true,
        );
        (mock, backend, rack_id, ps1, ps2, sw1, sw2)
    }

    async fn make_compute_tray_backend(
        pool: &sqlx::PgPool,
    ) -> (Arc<MockRmsApi>, RmsBackend, RackId, MachineId, MachineId) {
        let mut txn = pool.begin().await.unwrap();
        let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
        let rack_profile_id = RackProfileId::new(TEST_RACK_PROFILE_ID);
        db::rack::create(
            &mut txn,
            &rack_id,
            Some(&rack_profile_id),
            &model::rack::RackConfig::default(),
            None,
        )
        .await
        .expect("failed to create rack");
        let ct1 = seed_machine(&mut txn, CT_MAC_1, CT_IP_1, "CT-001", &rack_id).await;
        let ct2 = seed_machine(&mut txn, CT_MAC_2, CT_IP_2, "CT-002", &rack_id).await;
        txn.commit().await.unwrap();

        let mock = Arc::new(MockRmsApi::new());
        let backend = RmsBackend::new(
            mock.clone(),
            Some(mock.clone()),
            pool.clone(),
            Arc::new(rack_profile_config()),
            true,
        );
        (mock, backend, rack_id, ct1, ct2)
    }

    /// Seed a rack-scale compute tray that has an expected record and rack but
    /// no machine row, mirroring a device present before ingestion completes.
    /// When `create_rack_row` is false the rack profile is only discoverable
    /// through `expected_racks`, exercising that fallback.
    async fn seed_pre_ingestion_compute_tray(
        txn: &mut sqlx::PgConnection,
        mac: &str,
        rack_id: &RackId,
        create_rack_row: bool,
    ) {
        let rack_profile_id = RackProfileId::new(TEST_RACK_PROFILE_ID);
        db::expected_rack::create(
            &mut *txn,
            &model::expected_rack::ExpectedRack {
                rack_id: rack_id.clone(),
                rack_profile_id: rack_profile_id.clone(),
                metadata: model::metadata::Metadata::default(),
            },
        )
        .await
        .expect("failed to create expected rack");

        if create_rack_row {
            db::rack::create(
                &mut *txn,
                rack_id,
                Some(&rack_profile_id),
                &model::rack::RackConfig::default(),
                None,
            )
            .await
            .expect("failed to create rack");
        }

        db::expected_machine::create(
            &mut *txn,
            model::expected_machine::ExpectedMachine {
                id: None,
                bmc_mac_address: mac.parse().unwrap(),
                data: model::expected_machine::ExpectedMachineData {
                    serial_number: "PRE-CT-001".to_owned(),
                    rack_id: Some(rack_id.clone()),
                    ..Default::default()
                },
            },
        )
        .await
        .expect("failed to create expected machine");
    }

    fn make_ct_endpoint(bmc_ip: &str, bmc_mac: &str) -> ComputeTrayEndpoint {
        use carbide_secrets::credentials::Credentials;
        ComputeTrayEndpoint {
            vendor: ComputeTrayVendor::Nvidia,
            bmc_ip: bmc_ip.parse().unwrap(),
            bmc_mac: bmc_mac.parse().unwrap(),
            bmc_credentials: Credentials::UsernamePassword {
                username: "admin".into(),
                password: "pass".into(),
            },
        }
    }

    fn firmware_update_options() -> FirmwareUpdateOptions {
        FirmwareUpdateOptions {
            access_token: Some("token".to_owned()),
            force_update: true,
        }
    }

    fn test_rms_identity() -> RmsIdentity {
        RmsIdentity {
            node_id: "node-1".to_string(),
            rack_id: "rack-1".to_string(),
            rack_profile_id: None,
        }
    }

    fn test_rms_profile() -> RackProfile {
        let mut profile = RackProfile {
            product_family: Some(RackProductFamily::Gb200),
            ..Default::default()
        };

        profile.rack_capabilities.compute.vendor = Some("NVIDIA".to_string());
        profile.rack_capabilities.switch.vendor = Some("NVIDIA".to_string());
        profile.rack_capabilities.power_shelf.vendor = Some("LiteOn".to_string());

        profile
    }

    fn component_filters_for(request: &rms::ApplyFirmwareObjectRequest) -> &[String] {
        let [filter] = request.node_descriptor_component_filters.as_slice() else {
            panic!("expected one descriptor component filter");
        };

        let descriptor = filter
            .node_descriptor
            .as_ref()
            .expect("filter node descriptor");

        let role = descriptor
            .attributes
            .get(KEY_ROLE)
            .expect("filter node role");

        let node_type = match role.as_str() {
            ROLE_COMPUTE => rms::NodeType::ComputeGb200Nvidia,
            ROLE_SWITCH => rms::NodeType::SwitchGb200Nvidia,
            ROLE_POWER_SHELF => rms::NodeType::PowershelfGb200Liteon,
            role => panic!("unexpected RMS node role {role}"),
        };

        let descriptor_components = filter
            .component_filter
            .as_ref()
            .expect("descriptor component filter")
            .components
            .as_slice();

        assert_eq!(
            request
                .component_filters
                .get(&(node_type as i32))
                .map(|filter| filter.components.as_slice()),
            Some(descriptor_components)
        );

        descriptor_components
    }

    fn assert_descriptor_node(node: &rms::NodeInfo, role: &str) {
        let node_type = match role {
            ROLE_COMPUTE => rms::NodeType::ComputeGb200Nvidia,
            ROLE_SWITCH => rms::NodeType::SwitchGb200Nvidia,
            ROLE_POWER_SHELF => rms::NodeType::PowershelfGb200Liteon,
            role => panic!("unexpected RMS node role {role}"),
        };

        assert_eq!(node.r#type, Some(node_type as i32));

        let descriptor = node.node_descriptor.as_ref().expect("node descriptor");

        assert_eq!(
            descriptor.attributes.get(KEY_ROLE).map(String::as_str),
            Some(role)
        );
    }

    #[test]
    fn direct_rms_power_shelf_node_info_uses_descriptor() {
        let endpoint = make_ps_endpoint(PS_MAC_1);
        let identity = test_rms_identity();
        let profile = test_rms_profile();

        let node_identity = power_shelf_node_identity_for_profile(&profile).unwrap();

        let resolved = ResolvedRmsNode {
            identity: &identity,
            node_identity,
        };

        let node = build_power_shelf_node_info(&endpoint, &resolved);

        assert_descriptor_node(&node, ROLE_POWER_SHELF);
    }

    #[test]
    fn direct_rms_switch_node_info_uses_descriptor() {
        let endpoint = make_sw_endpoint(SW_MAC_1);
        let identity = test_rms_identity();
        let profile = test_rms_profile();

        let node_identity = switch_node_identity_for_profile(&profile).unwrap();

        let resolved = ResolvedRmsNode {
            identity: &identity,
            node_identity,
        };

        let node = build_switch_node_info(&endpoint, &resolved, None);

        assert_descriptor_node(&node, ROLE_SWITCH);
    }

    #[test]
    fn password_rotation_node_info_excludes_bmc_credentials() {
        let endpoint = make_sw_endpoint(SW_MAC_1);
        let identity = test_rms_identity();
        let profile = test_rms_profile();

        let node_identity = switch_node_identity_for_profile(&profile).unwrap();

        let resolved = ResolvedRmsNode {
            identity: &identity,
            node_identity,
        };

        let node = build_switch_password_rotation_node_info(&endpoint, &resolved, None);

        assert!(node.bmc_endpoint.is_none());
        assert_descriptor_node(&node, ROLE_SWITCH);

        assert_eq!(
            node.host_endpoint
                .as_ref()
                .and_then(|endpoint| endpoint.credentials.as_ref())
                .and_then(|credentials| match credentials.auth.as_ref() {
                    Some(rms::credentials::Auth::UserPass(credentials)) =>
                        Some((credentials.username.as_str(), credentials.password.as_str(),)),
                    _ => None,
                }),
            Some(("nvos-admin", "nvos-pass"))
        );
    }

    #[test]
    fn direct_rms_firmware_object_json_request_defaults_missing_access_token_to_noauth() {
        let identity = test_rms_identity();
        let profile = test_rms_profile();

        let node_identity = switch_node_identity_for_profile(&profile).unwrap();

        let resolved = ResolvedRmsNode {
            identity: &identity,
            node_identity,
        };

        let request = apply_firmware_object_request(
            rms::NodeInfo::default(),
            &resolved,
            r#"{"Id":"fw-json"}"#,
            &FirmwareUpdateOptions {
                access_token: None,
                force_update: false,
            },
            &[],
        )
        .unwrap();

        assert_eq!(
            request.access_token.as_deref(),
            Some(carbide_rack::firmware_object::RMS_NOAUTH_ACCESS_TOKEN)
        );
    }

    #[carbide_macros::sqlx_test]
    async fn power_shelf_power_control_request_uses_descriptor(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;

        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &ps1.to_string(),
        )))
        .await;
        let results = PowerShelfManager::power_control(
            &backend,
            &[make_ps_endpoint(PS_MAC_1)],
            PowerAction::On,
        )
        .await?;

        assert!(results[0].success);

        let calls = mock.batch_set_power_state_calls().await;

        let [call] = calls.as_slice() else {
            panic!("expected one BatchSetPowerState request");
        };

        let [node] = call.nodes.as_ref().expect("request nodes").nodes.as_slice() else {
            panic!("expected one node");
        };

        assert_descriptor_node(node, ROLE_POWER_SHELF);

        Ok(())
    }

    #[test]
    fn direct_rms_switch_system_image_request_defaults_empty_access_token_to_noauth() {
        let request = apply_switch_system_image_request(
            rms::NodeInfo::default(),
            &RmsIdentity {
                node_id: "node-1".to_string(),
                rack_id: "rack-1".to_string(),
                rack_profile_id: None,
            },
            r#"{"Id":"fw-json"}"#,
            &FirmwareUpdateOptions {
                access_token: Some(String::new()),
                force_update: false,
            },
        )
        .unwrap();

        assert_eq!(
            request.access_token.as_deref(),
            Some(carbide_rack::firmware_object::RMS_NOAUTH_ACCESS_TOKEN)
        );
    }

    // ---- PowerShelfManager tests ----

    #[carbide_macros::sqlx_test]
    async fn ps_power_control_success(pool: sqlx::PgPool) {
        let (mock, backend, rack_id, ps1, ps2, _, _) = make_backend(&pool).await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &ps1.to_string(),
        )))
        .await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &ps2.to_string(),
        )))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1), make_ps_endpoint(PS_MAC_2)];
        let results = PowerShelfManager::power_control(&backend, &eps, PowerAction::On)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].success);
        assert!(results[1].success);

        let calls = mock.batch_set_power_state_calls().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].operation, rms::PowerOperation::On as i32);
        let dev0 = &calls[0].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev0.node_id, ps1.to_string());
        assert_eq!(dev0.rack_id, rack_id.to_string());
        assert_descriptor_node(dev0, ROLE_POWER_SHELF);
        assert!(dev0.bmc_endpoint.is_some());
        assert!(dev0.host_endpoint.is_none());
        let dev1 = &calls[1].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev1.node_id, ps2.to_string());
    }

    #[carbide_macros::sqlx_test]
    async fn ps_power_control_partial_failure(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, ps2, _, _) = make_backend(&pool).await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &ps1.to_string(),
        )))
        .await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_fail(
            &ps2.to_string(),
            "rms reported failure",
        )))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1), make_ps_endpoint(PS_MAC_2)];
        let results = PowerShelfManager::power_control(&backend, &eps, PowerAction::On)
            .await
            .unwrap();

        assert!(results[0].success);
        assert!(!results[1].success);
        assert!(results[1].error.is_some());
    }

    #[carbide_macros::sqlx_test]
    async fn ps_power_control_transport_error(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &ps1.to_string(),
        )))
        .await;
        mock.enqueue_batch_set_power_state(Err(librms::RackManagerError::ApiInvocationError(
            tonic::Status::unavailable("connection refused"),
        )))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1), make_ps_endpoint(PS_MAC_2)];
        let results = PowerShelfManager::power_control(&backend, &eps, PowerAction::On)
            .await
            .unwrap();

        assert!(results[0].success);
        assert!(!results[1].success);
        assert!(
            results[1]
                .error
                .as_ref()
                .unwrap()
                .contains("connection refused")
        );
    }

    #[carbide_macros::sqlx_test]
    async fn ps_power_control_unknown_mac(pool: sqlx::PgPool) {
        let (mock, backend, _, _, ps2, _, _) = make_backend(&pool).await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &ps2.to_string(),
        )))
        .await;

        let eps = vec![make_ps_endpoint(UNKNOWN_MAC), make_ps_endpoint(PS_MAC_2)];
        let results =
            PowerShelfManager::power_control(&backend, &eps, PowerAction::GracefulShutdown)
                .await
                .unwrap();

        assert!(!results[0].success);
        assert!(results[1].success);

        let calls = mock.batch_set_power_state_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].operation, rms::PowerOperation::Off as i32);
    }

    #[carbide_macros::sqlx_test]
    async fn ps_update_firmware_success(pool: sqlx::PgPool) {
        let (mock, backend, rack_id, ps1, _ps2, _, _) = make_backend(&pool).await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ps1.to_string(),
            "job-aaa",
        )))
        .await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &_ps2.to_string(),
            "job-bbb",
        )))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1), make_ps_endpoint(PS_MAC_2)];
        let results = PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        assert!(results[0].success);
        assert!(results[1].success);

        let calls = mock.apply_firmware_object_calls().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].config_json, r#"{"Id":"fw-json"}"#);
        assert_eq!(calls[0].access_token.as_deref(), Some("token"));
        assert_eq!(calls[0].firmware_type, "prod");
        assert_eq!(calls[0].hardware_type, "any");
        assert!(calls[0].force_update);
        let filters = component_filters_for(&calls[0]);
        assert_eq!(filters, ["PowerShelfFW"]);
        let dev0 = &calls[0].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev0.node_id, ps1.to_string());
        assert_eq!(dev0.rack_id, rack_id.to_string());
        assert_descriptor_node(dev0, ROLE_POWER_SHELF);
        assert!(dev0.bmc_endpoint.is_some());

        let jobs = backend.firmware_jobs.lock().unwrap();
        assert_eq!(
            jobs.get(&PS_MAC_1.parse::<MacAddress>().unwrap()),
            Some(&vec![RmsTrackedFirmwareJob::FirmwareObject(
                "job-aaa".to_string()
            )])
        );
        assert_eq!(
            jobs.get(&PS_MAC_2.parse::<MacAddress>().unwrap()),
            Some(&vec![RmsTrackedFirmwareJob::FirmwareObject(
                "job-bbb".to_string()
            )])
        );
    }

    #[carbide_macros::sqlx_test]
    async fn ps_update_firmware_multiple_components(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ps1.to_string(),
            "job-1",
        )))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        let results = PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc, PowerShelfComponent::Psu],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        assert!(results[0].success);

        let calls = mock.apply_firmware_object_calls().await;
        let filters = component_filters_for(&calls[0]);
        assert_eq!(filters, ["PowerShelfFW"]);
    }

    #[carbide_macros::sqlx_test]
    async fn ps_update_firmware_failure(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_fail(
            &ps1.to_string(),
            "bad firmware file",
        )))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        let results = PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        assert!(!results[0].success);
        assert_eq!(results[0].error.as_deref(), Some("bad firmware file"));
    }

    #[carbide_macros::sqlx_test]
    async fn ps_update_firmware_failure_clears_tracked_job(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;
        let eps = vec![make_ps_endpoint(PS_MAC_1)];

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ps1.to_string(),
            "job-old",
        )))
        .await;
        PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_fail(
            &ps1.to_string(),
            "bad firmware file",
        )))
        .await;
        PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        let jobs = backend.firmware_jobs.lock().unwrap();
        assert!(!jobs.contains_key(&PS_MAC_1.parse::<MacAddress>().unwrap()));
    }

    #[carbide_macros::sqlx_test]
    async fn ps_firmware_status_running(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ps1.to_string(),
            "job-xyz",
        )))
        .await;
        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        mock.enqueue_get_firmware_job_status(Ok(MockRmsApi::firmware_job_status_ok(
            rms::FirmwareJobState::Running,
        )))
        .await;

        let statuses = PowerShelfManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();

        assert_eq!(statuses[0].state, FirmwareState::InProgress);
        assert!(statuses[0].error.is_none());

        let calls = mock.get_firmware_job_status_calls().await;
        assert_eq!(calls[0].job_id, "job-xyz");
    }

    #[carbide_macros::sqlx_test]
    async fn ps_firmware_status_no_job(pool: sqlx::PgPool) {
        let (_mock, backend, _, _, _, _, _) = make_backend(&pool).await;

        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        let statuses = PowerShelfManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();

        assert_eq!(statuses[0].state, FirmwareState::Unknown);
        assert!(
            statuses[0]
                .error
                .as_ref()
                .unwrap()
                .contains("no firmware job")
        );
    }

    #[carbide_macros::sqlx_test]
    async fn ps_firmware_status_completed(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ps1.to_string(),
            "job-done",
        )))
        .await;
        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        mock.enqueue_get_firmware_job_status(Ok(MockRmsApi::firmware_job_status_ok(
            rms::FirmwareJobState::Completed,
        )))
        .await;

        let statuses = PowerShelfManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();
        assert_eq!(statuses[0].state, FirmwareState::Completed);
    }

    #[carbide_macros::sqlx_test]
    async fn ps_firmware_status_failed(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ps1.to_string(),
            "job-fail",
        )))
        .await;
        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        mock.enqueue_get_firmware_job_status(Ok(rms::GetFirmwareJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            job_state: rms::FirmwareJobState::Failed as i32,
            error_message: "checksum mismatch".into(),
            ..Default::default()
        }))
        .await;

        let statuses = PowerShelfManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();
        assert_eq!(statuses[0].state, FirmwareState::Failed);
        assert_eq!(statuses[0].error.as_deref(), Some("checksum mismatch"));
    }

    #[carbide_macros::sqlx_test]
    async fn ps_firmware_status_non_success_without_error_has_diagnostic(pool: sqlx::PgPool) {
        let (mock, backend, _, ps1, _, _, _) = make_backend(&pool).await;

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ps1.to_string(),
            "job-status-error",
        )))
        .await;
        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        PowerShelfManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[PowerShelfComponent::Pmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        mock.enqueue_get_firmware_job_status(Ok(rms::GetFirmwareJobStatusResponse {
            status: rms::ReturnCode::Failure as i32,
            ..Default::default()
        }))
        .await;

        let statuses = PowerShelfManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();
        assert_eq!(statuses[0].state, FirmwareState::Unknown);
        assert!(
            statuses[0]
                .error
                .as_deref()
                .unwrap()
                .contains("job-status-error")
        );
    }

    #[carbide_macros::sqlx_test]
    async fn ps_list_firmware_success(pool: sqlx::PgPool) {
        let (mock, backend, rack_id, ps1, _, _, _) = make_backend(&pool).await;
        mock.enqueue_get_node_firmware_inventory(Ok(MockRmsApi::firmware_inventory_ok(&[
            ("PMC", "1.2.3"),
            ("PSU", "4.5.6"),
        ])))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        let results = backend.list_firmware(&eps).await.unwrap();

        assert_eq!(results[0].versions, vec!["1.2.3", "4.5.6"]);
        assert!(results[0].error.is_none());

        let calls = mock.get_node_firmware_inventory_calls().await;
        assert_eq!(calls[0].node_id, ps1.to_string());
        assert_eq!(calls[0].rack_id, rack_id.to_string());
    }

    #[carbide_macros::sqlx_test]
    async fn ps_list_firmware_rms_failure(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, _, _) = make_backend(&pool).await;
        mock.enqueue_get_node_firmware_inventory(Ok(rms::GetNodeFirmwareInventoryResponse {
            status: rms::ReturnCode::Failure as i32,
            ..Default::default()
        }))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        let results = backend.list_firmware(&eps).await.unwrap();

        assert!(results[0].versions.is_empty());
        assert!(results[0].error.is_some());
    }

    #[carbide_macros::sqlx_test]
    async fn ps_list_firmware_transport_error(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, _, _) = make_backend(&pool).await;
        mock.enqueue_get_node_firmware_inventory(Err(
            librms::RackManagerError::ApiInvocationError(tonic::Status::unavailable("down")),
        ))
        .await;

        let eps = vec![make_ps_endpoint(PS_MAC_1)];
        let results = backend.list_firmware(&eps).await.unwrap();

        assert!(results[0].versions.is_empty());
        assert!(results[0].error.as_ref().unwrap().contains("down"));
    }

    #[carbide_macros::sqlx_test]
    async fn ps_list_firmware_unknown_mac(pool: sqlx::PgPool) {
        let (_mock, backend, _, _, _, _, _) = make_backend(&pool).await;

        let eps = vec![make_ps_endpoint(UNKNOWN_MAC)];
        let results = backend.list_firmware(&eps).await.unwrap();

        assert!(results[0].versions.is_empty());
        assert!(results[0].error.is_some());
    }

    // ---- NvSwitchManager tests ----

    #[tokio::test]
    async fn sw_factory_reset_job_status_includes_children_and_returns_domain_status() {
        let mock = MockRmsApi::new();

        let response = rms::GetJobStatusResponse {
            job_states: vec![rms::JobStatus {
                job_id: "factory-reset-job-1".to_string(),
                child_job_ids: vec!["factory-reset-child-1".to_string()],
                execution_state: rms::JobExecutionState::Running as i32,
                ..Default::default()
            }],
        };

        mock.enqueue_get_job_status(Ok(response)).await;

        let status = rms_get_switch_factory_reset_job_status(&mock, "factory-reset-job-1")
            .await
            .expect("factory-reset job status should be readable");

        assert_eq!(status.state, SwitchFactoryResetState::Pending);
        assert!(status.error.is_none());

        let calls = mock.get_job_status_calls().await;

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].job_id, "factory-reset-job-1");
        assert!(calls[0].include_child_job_states);
    }

    #[tokio::test]
    async fn sw_factory_reset_job_status_unobservable_states_preserve_unknown_outcome() {
        let cases = [
            tonic::Status::not_found("expired factory-reset-job-1"),
            tonic::Status::invalid_argument("factory-reset-job-1 is not observable"),
        ];

        for status in cases {
            let mock = MockRmsApi::new();

            mock.enqueue_get_job_status(Err(RackManagerError::ApiInvocationError(status)))
                .await;

            let error = rms_get_switch_factory_reset_job_status(&mock, "factory-reset-job-1")
                .await
                .unwrap_err();

            assert!(matches!(
                error,
                ComponentManagerError::OperationOutcomeUnknown(_)
            ));

            assert!(
                mock.batch_reset_switch_factory_default_calls()
                    .await
                    .is_empty()
            );
        }
    }

    #[carbide_macros::sqlx_test]
    async fn sw_batch_reset_switch_factory_default_maps_endpoints_and_returns_job_id(
        pool: sqlx::PgPool,
    ) {
        let (mock, backend, _, _, _, sw1, _) = make_backend(&pool).await;

        let expected_response = rms::BatchResetSwitchFactoryDefaultResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                job_id: "factory-reset-job-1".to_string(),
                ..Default::default()
            }),
        };

        mock.enqueue_batch_reset_switch_factory_default(Ok(expected_response))
            .await;

        let endpoint = make_sw_endpoint(SW_MAC_1);

        let tls_server_domain = "switches.example.test";

        let job_id = NvSwitchManager::batch_reset_switch_factory_default(
            &backend,
            std::slice::from_ref(&endpoint),
            Some(tls_server_domain),
        )
        .await
        .unwrap();

        assert_eq!(job_id, "factory-reset-job-1");

        let calls = mock.batch_reset_switch_factory_default_calls().await;

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].domain.as_deref(), Some(tls_server_domain));

        let nodes = calls[0]
            .nodes
            .as_ref()
            .expect("the RMS request should include target nodes");

        assert_eq!(nodes.nodes.len(), 1);
        assert_eq!(nodes.nodes[0].node_id, sw1.to_string());
    }

    #[tokio::test]
    async fn sw_batch_reset_switch_factory_default_requires_durable_job_id() {
        let cases = [
            (
                "missing response",
                rms::BatchResetSwitchFactoryDefaultResponse::default(),
            ),
            (
                "empty job ID",
                rms::BatchResetSwitchFactoryDefaultResponse {
                    response: Some(rms::NodeBatchResponse::default()),
                },
            ),
        ];

        for (scenario, response) in cases {
            let mock = MockRmsApi::new();
            mock.enqueue_batch_reset_switch_factory_default(Ok(response))
                .await;

            let error =
                rms_batch_reset_switch_factory_default(&mock, vec![rms::NodeInfo::default()], None)
                    .await
                    .expect_err(scenario);

            assert!(matches!(
                error,
                ComponentManagerError::OperationOutcomeUnknown(_)
            ));
        }
    }

    #[tokio::test]
    async fn sw_batch_reset_switch_factory_default_preserves_ambiguous_transport_failure() {
        let mock = MockRmsApi::new();

        mock.enqueue_batch_reset_switch_factory_default(Err(RackManagerError::ApiInvocationError(
            tonic::Status::unavailable("connection lost"),
        )))
        .await;

        let error =
            rms_batch_reset_switch_factory_default(&mock, vec![rms::NodeInfo::default()], None)
                .await
                .unwrap_err();

        assert!(matches!(
            error,
            ComponentManagerError::OperationOutcomeUnknown(_)
        ));
    }

    #[tokio::test]
    async fn sw_batch_reset_switch_factory_default_preserves_unproven_rejections() {
        let invalid_argument = MockRmsApi::new();
        invalid_argument
            .enqueue_batch_reset_switch_factory_default(Err(RackManagerError::ApiInvocationError(
                tonic::Status::invalid_argument("invalid target"),
            )))
            .await;

        let error = rms_batch_reset_switch_factory_default(
            &invalid_argument,
            vec![rms::NodeInfo::default()],
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            ComponentManagerError::OperationOutcomeUnknown(_)
        ));

        let unimplemented = MockRmsApi::new();
        unimplemented
            .enqueue_batch_reset_switch_factory_default(Err(RackManagerError::ApiInvocationError(
                tonic::Status::unimplemented("unsupported"),
            )))
            .await;

        let error = rms_batch_reset_switch_factory_default(
            &unimplemented,
            vec![rms::NodeInfo::default()],
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ComponentManagerError::Unsupported(_)));
    }

    #[carbide_macros::sqlx_test]
    async fn sw_batch_reset_switch_factory_default_rejects_empty_targets_before_dispatch(
        pool: sqlx::PgPool,
    ) {
        let (mock, backend, _, _, _, _, _) = make_backend(&pool).await;

        let error = NvSwitchManager::batch_reset_switch_factory_default(&backend, &[], None)
            .await
            .unwrap_err();

        assert!(matches!(error, ComponentManagerError::InvalidArgument(_)));

        assert!(
            mock.batch_reset_switch_factory_default_calls()
                .await
                .is_empty()
        );
    }

    #[carbide_macros::sqlx_test]
    async fn sw_configure_switch_certificate_success(pool: sqlx::PgPool) {
        let (mock, backend, rack_id, _, _, sw1, _) = make_backend(&pool).await;
        mock.enqueue_configure_switch_certificate(Ok(MockRmsApi::configure_switch_certificate_ok(
            &sw1.to_string(),
            "cert-job-1",
        )))
        .await;

        let endpoint = make_sw_endpoint(SW_MAC_1);
        let job_id = NvSwitchManager::configure_switch_certificate(
            &backend,
            &endpoint,
            Some(rack_id.as_ref()),
            Some(&crate::config::switch_mtls_services_as_i32(
                &SwitchMtlsService::default_services(),
            )),
        )
        .await
        .unwrap();

        assert_eq!(job_id, "cert-job-1");

        let calls = mock.configure_switch_certificate_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].domain, Some(rack_id.to_string()));
        assert_eq!(
            calls[0].nodes.as_ref().unwrap().nodes[0].node_id,
            sw1.to_string()
        );
        assert_eq!(
            calls[0].services,
            crate::config::switch_mtls_services_as_i32(&SwitchMtlsService::default_services())
        );
    }

    #[carbide_macros::sqlx_test]
    async fn sw_configure_switch_certificate_job_status_completed(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, _, _) = make_backend(&pool).await;
        mock.enqueue_get_configure_switch_certificate_job_status(Ok(
            MockRmsApi::configure_switch_certificate_job_status_ok("completed"),
        ))
        .await;

        let status =
            NvSwitchManager::get_configure_switch_certificate_job_status(&backend, "cert-job-1")
                .await
                .unwrap();

        assert_eq!(status.state, ConfigureSwitchCertificateState::Completed);
        assert!(status.error.is_none());

        let calls = mock
            .get_configure_switch_certificate_job_status_calls()
            .await;
        assert_eq!(calls[0].job_id, "cert-job-1");
    }

    #[carbide_macros::sqlx_test]
    async fn sw_password_rotation_submits_current_and_next_passwords(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, sw1, _) = make_backend(&pool).await;

        mock.enqueue_update_switch_system_password(Ok(rms::UpdateSwitchSystemPasswordResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                job_id: "password-job-1".to_string(),
                ..Default::default()
            }),
        }))
        .await;

        let endpoint = make_sw_endpoint(SW_MAC_1);

        let started =
            NvSwitchManager::ensure_password_rotation(&backend, &endpoint, "next-password")
                .await
                .expect("password rotation should start");

        assert_eq!(started, "password-job-1");

        let calls = mock.update_switch_system_password_calls().await;

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].username, "nvos-admin");
        assert_eq!(calls[0].password, "next-password");

        let node = &calls[0]
            .nodes
            .as_ref()
            .expect("password request should include its node")
            .nodes[0];

        assert_eq!(node.node_id, sw1.to_string());
        assert!(node.bmc_endpoint.is_none());

        assert_eq!(
            node.host_endpoint
                .as_ref()
                .and_then(|endpoint| endpoint.credentials.as_ref())
                .and_then(|credentials| match credentials.auth.as_ref() {
                    Some(rms::credentials::Auth::UserPass(credentials)) => {
                        Some(credentials.password.as_str())
                    }
                    _ => None,
                }),
            Some("nvos-pass"),
            "endpoint must retain the current password until RMS accepts the job"
        );
    }

    #[tokio::test]
    async fn sw_password_rotation_success_without_job_has_unknown_outcome() {
        let mock = MockRmsApi::new();

        mock.enqueue_update_switch_system_password(Ok(rms::UpdateSwitchSystemPasswordResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                ..Default::default()
            }),
        }))
        .await;

        let credentials = Credentials::UsernamePassword {
            username: "admin".to_string(),
            password: "current-password".to_string(),
        };

        let result = rms_ensure_switch_password_rotation(
            &mock,
            rms::NodeInfo::default(),
            &credentials,
            "next-password",
        )
        .await;

        assert!(matches!(
            result,
            Err(ComponentManagerError::OperationOutcomeUnknown(_))
        ));
    }

    #[tokio::test]
    async fn sw_password_rotation_unspecified_without_job_has_unknown_outcome() {
        let mock = MockRmsApi::new();

        mock.enqueue_update_switch_system_password(Ok(rms::UpdateSwitchSystemPasswordResponse {
            response: Some(rms::NodeBatchResponse::default()),
        }))
        .await;

        let credentials = Credentials::UsernamePassword {
            username: "admin".to_string(),
            password: "current-password".to_string(),
        };

        let result = rms_ensure_switch_password_rotation(
            &mock,
            rms::NodeInfo::default(),
            &credentials,
            "next-password",
        )
        .await;

        assert!(matches!(
            result,
            Err(ComponentManagerError::OperationOutcomeUnknown(message))
                if message.contains("returned no job ID")
        ));
    }

    #[tokio::test]
    async fn sw_password_rotation_job_id_wins_over_admission_status() {
        let mock = MockRmsApi::new();

        mock.enqueue_update_switch_system_password(Ok(rms::UpdateSwitchSystemPasswordResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Failure as i32,
                job_id: "password-job-1".to_string(),
                ..Default::default()
            }),
        }))
        .await;

        let credentials = Credentials::UsernamePassword {
            username: "admin".to_string(),
            password: "current-password".to_string(),
        };

        let started = rms_ensure_switch_password_rotation(
            &mock,
            rms::NodeInfo::default(),
            &credentials,
            "next-password",
        )
        .await
        .expect("durable job ID should permit reconciliation");

        assert_eq!(started, "password-job-1");

        let calls = mock.update_switch_system_password_calls().await;

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].username, "admin");
        assert_eq!(calls[0].password, "next-password");
    }

    #[tokio::test]
    async fn sw_password_rotation_rpc_failure_does_not_expose_rpc_error_text() {
        let mock = MockRmsApi::new();

        mock.enqueue_update_switch_system_password(Err(RackManagerError::ApiInvocationError(
            tonic::Status::unavailable("current-password next-password"),
        )))
        .await;

        let credentials = Credentials::UsernamePassword {
            username: "admin".to_string(),
            password: "current-password".to_string(),
        };

        let result = rms_ensure_switch_password_rotation(
            &mock,
            rms::NodeInfo::default(),
            &credentials,
            "next-password",
        )
        .await;

        assert!(matches!(
            result,
            Err(ComponentManagerError::OperationOutcomeUnknown(message))
                if message == "RMS switch password request returned no durable job ID"
        ));
    }

    #[tokio::test]
    async fn sw_password_rotation_invalid_request_is_rejected_before_dispatch() {
        let mock = MockRmsApi::new();

        mock.enqueue_update_switch_system_password(Err(RackManagerError::ApiInvocationError(
            tonic::Status::invalid_argument("request rejected"),
        )))
        .await;

        let credentials = Credentials::UsernamePassword {
            username: "admin".to_string(),
            password: "current-password".to_string(),
        };

        let result = rms_ensure_switch_password_rotation(
            &mock,
            rms::NodeInfo::default(),
            &credentials,
            "next-password",
        )
        .await;

        assert!(matches!(
            result,
            Err(ComponentManagerError::RejectedBeforeDispatch(message))
                if message == "RMS rejected the switch password rotation request"
        ));
    }

    #[tokio::test]
    async fn sw_password_rotation_unimplemented_is_unsupported() {
        let mock = MockRmsApi::new();

        mock.enqueue_update_switch_system_password(Err(RackManagerError::ApiInvocationError(
            tonic::Status::unimplemented("upgrade RMS"),
        )))
        .await;

        let credentials = Credentials::UsernamePassword {
            username: "admin".to_string(),
            password: "current-password".to_string(),
        };

        let result = rms_ensure_switch_password_rotation(
            &mock,
            rms::NodeInfo::default(),
            &credentials,
            "next-password",
        )
        .await;

        assert!(matches!(
            result,
            Err(ComponentManagerError::Unsupported(message))
                if message == "RMS does not support switch password rotation"
        ));
    }

    #[tokio::test]
    async fn sw_password_rotation_poll_includes_child_jobs() {
        let mock = MockRmsApi::new();

        mock.enqueue_get_job_status(Ok(rms::GetJobStatusResponse {
            job_states: vec![rms::JobStatus {
                job_id: "password-job-1".to_string(),
                execution_state: rms::JobExecutionState::Completed as i32,
                ..Default::default()
            }],
        }))
        .await;

        let status = rms_get_switch_password_rotation_job_status(&mock, "password-job-1")
            .await
            .expect("password job should be readable");

        assert_eq!(status, SwitchPasswordRotationState::Completed);

        let calls = mock.get_job_status_calls().await;

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].job_id, "password-job-1");
        assert!(calls[0].include_child_job_states);
    }

    #[tokio::test]
    async fn sw_password_rotation_poll_not_found_is_an_observation() {
        let mock = MockRmsApi::new();

        mock.enqueue_get_job_status(Err(RackManagerError::ApiInvocationError(
            tonic::Status::not_found("expired password-job-1"),
        )))
        .await;

        let status = rms_get_switch_password_rotation_job_status(&mock, "password-job-1")
            .await
            .expect("missing jobs are observations, not polling errors");

        assert_eq!(status, SwitchPasswordRotationState::NotFound);
    }

    #[tokio::test]
    async fn sw_password_rotation_poll_transport_failure_is_retryable() {
        let mock = MockRmsApi::new();

        mock.enqueue_get_job_status(Err(RackManagerError::ApiInvocationError(
            tonic::Status::unavailable("transient failure"),
        )))
        .await;

        let result = rms_get_switch_password_rotation_job_status(&mock, "password-job-1").await;

        assert!(matches!(
            result,
            Err(ComponentManagerError::Unavailable(message))
                if message == "RMS switch password-rotation job status is temporarily unavailable"
        ));
    }

    #[tokio::test]
    async fn sw_password_rotation_poll_rejects_empty_job_id_without_rpc() {
        let mock = MockRmsApi::new();

        let result = rms_get_switch_password_rotation_job_status(&mock, "").await;

        assert!(matches!(
            result,
            Err(ComponentManagerError::InvalidArgument(message))
                if message == "switch password rotation job ID must be non-empty"
        ));

        assert!(mock.get_job_status_calls().await.is_empty());
    }

    #[tokio::test]
    async fn sw_password_rotation_poll_preserves_definitive_server_rejection() {
        let mock = MockRmsApi::new();

        mock.enqueue_get_job_status(Err(RackManagerError::ApiInvocationError(
            tonic::Status::invalid_argument("malformed job ID"),
        )))
        .await;

        let result = rms_get_switch_password_rotation_job_status(&mock, "password-job-1").await;

        assert!(matches!(
            result,
            Err(ComponentManagerError::InvalidArgument(message))
                if message == "RMS rejected the switch password-rotation job status request"
        ));
    }

    #[carbide_macros::sqlx_test]
    async fn sw_power_control_success(pool: sqlx::PgPool) {
        let (mock, backend, rack_id, _, _, sw1, sw2) = make_backend(&pool).await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &sw1.to_string(),
        )))
        .await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &sw2.to_string(),
        )))
        .await;

        let eps = vec![make_sw_endpoint(SW_MAC_1), make_sw_endpoint(SW_MAC_2)];
        let results = NvSwitchManager::power_control(&backend, &eps, PowerAction::On)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].success);
        assert!(results[1].success);

        let calls = mock.batch_set_power_state_calls().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].operation, rms::PowerOperation::On as i32);
        let dev0 = &calls[0].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev0.node_id, sw1.to_string());
        assert_eq!(dev0.rack_id, rack_id.to_string());
        assert_descriptor_node(dev0, ROLE_SWITCH);
        assert!(dev0.bmc_endpoint.is_some());
        let dev1 = &calls[1].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev1.node_id, sw2.to_string());
    }

    #[carbide_macros::sqlx_test]
    async fn sw_power_control_unknown_mac(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, _, sw2) = make_backend(&pool).await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &sw2.to_string(),
        )))
        .await;

        let eps = vec![make_sw_endpoint(UNKNOWN_MAC), make_sw_endpoint(SW_MAC_2)];
        let results = NvSwitchManager::power_control(&backend, &eps, PowerAction::ForceOff)
            .await
            .unwrap();

        assert!(!results[0].success);
        assert!(results[1].success);

        let calls = mock.batch_set_power_state_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].operation, rms::PowerOperation::Off as i32);
    }

    #[carbide_macros::sqlx_test]
    async fn sw_queue_firmware_updates_success(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, sw1, _) = make_backend(&pool).await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &sw1.to_string(),
            "sw-job-1",
        )))
        .await;

        let eps = vec![make_sw_endpoint(SW_MAC_1)];
        let results = backend
            .queue_firmware_updates(
                &eps,
                r#"{"Id":"fw-json"}"#,
                &[NvSwitchComponent::Bmc, NvSwitchComponent::Bios],
                &firmware_update_options(),
            )
            .await
            .unwrap();

        assert!(results[0].success);

        let calls = mock.apply_firmware_object_calls().await;
        assert_eq!(calls[0].config_json, r#"{"Id":"fw-json"}"#);
        assert_eq!(calls[0].access_token.as_deref(), Some("token"));
        assert!(calls[0].force_update);
        let filters = component_filters_for(&calls[0]);
        assert_eq!(filters, ["BMC", "BIOS"]);
        let dev0 = &calls[0].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev0.node_id, sw1.to_string());
        assert_descriptor_node(dev0, ROLE_SWITCH);
        assert!(dev0.bmc_endpoint.is_some());
        assert!(dev0.host_endpoint.is_some());

        let jobs = backend.firmware_jobs.lock().unwrap();
        assert_eq!(
            jobs.get(&SW_MAC_1.parse::<MacAddress>().unwrap()),
            Some(&vec![RmsTrackedFirmwareJob::FirmwareObject(
                "sw-job-1".to_string()
            )])
        );
    }

    #[carbide_macros::sqlx_test]
    async fn sw_queue_firmware_updates_failure_clears_tracked_jobs(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, sw1, _) = make_backend(&pool).await;
        let eps = vec![make_sw_endpoint(SW_MAC_1)];

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &sw1.to_string(),
            "sw-job-old",
        )))
        .await;
        backend
            .queue_firmware_updates(
                &eps,
                r#"{"Id":"fw-json"}"#,
                &[NvSwitchComponent::Bmc],
                &firmware_update_options(),
            )
            .await
            .unwrap();

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_fail(
            &sw1.to_string(),
            "bad firmware file",
        )))
        .await;
        let results = backend
            .queue_firmware_updates(
                &eps,
                r#"{"Id":"fw-json"}"#,
                &[NvSwitchComponent::Bmc],
                &firmware_update_options(),
            )
            .await
            .unwrap();

        assert!(!results[0].success);
        let jobs = backend.firmware_jobs.lock().unwrap();
        assert!(!jobs.contains_key(&SW_MAC_1.parse::<MacAddress>().unwrap()));
    }

    #[carbide_macros::sqlx_test]
    async fn sw_queue_firmware_updates_nvos_uses_switch_system_image_json(pool: sqlx::PgPool) {
        let (mock, backend, rack_id, _, _, sw1, _) = make_backend(&pool).await;
        mock.enqueue_apply_switch_system_image(Ok(MockRmsApi::switch_system_image_apply_ok(
            &sw1.to_string(),
            "nvos-job-1",
        )))
        .await;

        let eps = vec![make_sw_endpoint(SW_MAC_1)];
        let results = backend
            .queue_firmware_updates(
                &eps,
                r#"{"Id":"fw-json"}"#,
                &[NvSwitchComponent::Nvos],
                &firmware_update_options(),
            )
            .await
            .unwrap();

        assert!(results[0].success);
        assert!(mock.apply_firmware_object_calls().await.is_empty());

        let calls = mock.apply_switch_system_image_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].config_json, r#"{"Id":"fw-json"}"#);
        assert_eq!(calls[0].access_token.as_deref(), Some("token"));
        assert_eq!(calls[0].software_type, "prod");
        assert_eq!(calls[0].hardware_type, "any");
        assert_eq!(calls[0].rack_id, rack_id.to_string());
        let dev0 = &calls[0].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev0.node_id, sw1.to_string());
        assert_descriptor_node(dev0, ROLE_SWITCH);
        assert!(dev0.bmc_endpoint.is_some());
        assert!(dev0.host_endpoint.is_some());

        let jobs = backend.firmware_jobs.lock().unwrap();
        assert_eq!(
            jobs.get(&SW_MAC_1.parse::<MacAddress>().unwrap()),
            Some(&vec![RmsTrackedFirmwareJob::SwitchSystemImage(
                "nvos-job-1".to_string()
            )])
        );
    }

    #[carbide_macros::sqlx_test]
    async fn sw_queue_firmware_updates_mixed_tracks_both_jobs(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, sw1, _) = make_backend(&pool).await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &sw1.to_string(),
            "sw-fw-job",
        )))
        .await;
        mock.enqueue_apply_switch_system_image(Ok(MockRmsApi::switch_system_image_apply_ok(
            &sw1.to_string(),
            "sw-nvos-job",
        )))
        .await;

        let eps = vec![make_sw_endpoint(SW_MAC_1)];
        let results = backend
            .queue_firmware_updates(
                &eps,
                r#"{"Id":"fw-json"}"#,
                &[NvSwitchComponent::Bmc, NvSwitchComponent::Nvos],
                &firmware_update_options(),
            )
            .await
            .unwrap();

        assert!(results[0].success);
        assert_eq!(mock.apply_firmware_object_calls().await.len(), 1);
        assert_eq!(mock.apply_switch_system_image_calls().await.len(), 1);

        {
            let jobs = backend.firmware_jobs.lock().unwrap();
            assert_eq!(
                jobs.get(&SW_MAC_1.parse::<MacAddress>().unwrap()),
                Some(&vec![
                    RmsTrackedFirmwareJob::FirmwareObject("sw-fw-job".to_string()),
                    RmsTrackedFirmwareJob::SwitchSystemImage("sw-nvos-job".to_string()),
                ])
            );
        }

        mock.enqueue_get_firmware_job_status(Ok(MockRmsApi::firmware_job_status_ok(
            rms::FirmwareJobState::Completed,
        )))
        .await;
        mock.enqueue_get_switch_system_image_job_status(Ok(
            MockRmsApi::switch_system_image_job_status_ok("running"),
        ))
        .await;

        let statuses = NvSwitchManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();

        assert_eq!(statuses[0].state, FirmwareState::InProgress);
        assert_eq!(
            mock.get_firmware_job_status_calls().await[0].job_id,
            "sw-fw-job"
        );
        let status_calls = mock.get_switch_system_image_job_status_calls().await;
        assert_eq!(status_calls[0].job_id, "sw-nvos-job");
    }

    #[carbide_macros::sqlx_test]
    async fn sw_queue_firmware_updates_mixed_failure_keeps_submitted_job(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, sw1, _) = make_backend(&pool).await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &sw1.to_string(),
            "sw-fw-job",
        )))
        .await;
        mock.enqueue_apply_switch_system_image(Ok(MockRmsApi::switch_system_image_apply_fail(
            &sw1.to_string(),
            "bad system image",
        )))
        .await;

        let eps = vec![make_sw_endpoint(SW_MAC_1)];
        let results = backend
            .queue_firmware_updates(
                &eps,
                r#"{"Id":"fw-json"}"#,
                &[NvSwitchComponent::Bmc, NvSwitchComponent::Nvos],
                &firmware_update_options(),
            )
            .await
            .unwrap();

        assert!(!results[0].success);

        let jobs = backend.firmware_jobs.lock().unwrap();
        assert_eq!(
            jobs.get(&SW_MAC_1.parse::<MacAddress>().unwrap()),
            Some(&vec![RmsTrackedFirmwareJob::FirmwareObject(
                "sw-fw-job".to_string()
            )])
        );
    }

    #[carbide_macros::sqlx_test]
    async fn sw_firmware_status(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, sw1, _) = make_backend(&pool).await;

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &sw1.to_string(),
            "sw-job-2",
        )))
        .await;
        let eps = vec![make_sw_endpoint(SW_MAC_1)];
        backend
            .queue_firmware_updates(
                &eps,
                r#"{"Id":"fw-json"}"#,
                &[NvSwitchComponent::Bmc],
                &firmware_update_options(),
            )
            .await
            .unwrap();

        mock.enqueue_get_firmware_job_status(Ok(MockRmsApi::firmware_job_status_ok(
            rms::FirmwareJobState::Completed,
        )))
        .await;

        let statuses = NvSwitchManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();

        assert_eq!(statuses[0].state, FirmwareState::Completed);

        let calls = mock.get_firmware_job_status_calls().await;
        assert_eq!(calls[0].job_id, "sw-job-2");
    }

    #[carbide_macros::sqlx_test]
    async fn sw_firmware_object_status_non_success_without_error_has_diagnostic(
        pool: sqlx::PgPool,
    ) {
        let (mock, backend, _, _, _, sw1, _) = make_backend(&pool).await;

        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &sw1.to_string(),
            "sw-job-status-error",
        )))
        .await;
        let eps = vec![make_sw_endpoint(SW_MAC_1)];
        backend
            .queue_firmware_updates(
                &eps,
                r#"{"Id":"fw-json"}"#,
                &[NvSwitchComponent::Bmc],
                &firmware_update_options(),
            )
            .await
            .unwrap();

        mock.enqueue_get_firmware_job_status(Ok(rms::GetFirmwareJobStatusResponse {
            status: rms::ReturnCode::Failure as i32,
            ..Default::default()
        }))
        .await;

        let statuses = NvSwitchManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();

        assert_eq!(statuses[0].state, FirmwareState::Unknown);
        assert!(
            statuses[0]
                .error
                .as_deref()
                .unwrap()
                .contains("sw-job-status-error")
        );
    }

    #[carbide_macros::sqlx_test]
    async fn sw_firmware_status_no_job(pool: sqlx::PgPool) {
        let (_mock, backend, _, _, _, _, _) = make_backend(&pool).await;

        let eps = vec![make_sw_endpoint(SW_MAC_1)];
        let statuses = NvSwitchManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();

        assert_eq!(statuses[0].state, FirmwareState::Unknown);
        assert!(
            statuses[0]
                .error
                .as_ref()
                .unwrap()
                .contains("no firmware job")
        );
    }

    #[carbide_macros::sqlx_test]
    async fn list_firmware_bundles_empty_rms(pool: sqlx::PgPool) {
        let (mock, backend, _, _, _, _, _) = make_backend(&pool).await;
        mock.enqueue_list_firmware_objects(Ok(rms::ListFirmwareObjectsResponse {
            objects: Vec::new(),
        }))
        .await;

        let bundles = NvSwitchManager::list_firmware_bundles(&backend)
            .await
            .unwrap();

        assert!(bundles.is_empty());
    }

    // ---- ComputeTrayManager tests ----

    #[carbide_macros::sqlx_test]
    async fn ct_power_control_success(pool: sqlx::PgPool) {
        let (mock, backend, rack_id, ct1, ct2) = make_compute_tray_backend(&pool).await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &ct1.to_string(),
        )))
        .await;
        mock.enqueue_batch_set_power_state(Ok(MockRmsApi::batch_set_power_state_ok(
            &ct2.to_string(),
        )))
        .await;

        let eps = vec![
            make_ct_endpoint(CT_IP_1, CT_MAC_1),
            make_ct_endpoint(CT_IP_2, CT_MAC_2),
        ];
        let results = ComputeTrayManager::power_control(&backend, &eps, PowerAction::On)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].success);
        assert!(results[1].success);

        let calls = mock.batch_set_power_state_calls().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].operation, rms::PowerOperation::On as i32);
        let dev0 = &calls[0].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev0.node_id, ct1.to_string());
        assert_eq!(dev0.rack_id, rack_id.to_string());
        assert_descriptor_node(dev0, ROLE_COMPUTE);
        assert!(dev0.bmc_endpoint.is_some());
        assert!(dev0.host_endpoint.is_none());
    }

    #[carbide_macros::sqlx_test]
    async fn ct_update_firmware_success(pool: sqlx::PgPool) {
        let (mock, backend, rack_id, ct1, _) = make_compute_tray_backend(&pool).await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ct1.to_string(),
            "ct-job-1",
        )))
        .await;

        let eps = vec![make_ct_endpoint(CT_IP_1, CT_MAC_1)];
        let results = ComputeTrayManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[ComputeTrayComponent::Bmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        assert!(results[0].success);

        let calls = mock.apply_firmware_object_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].rack_id, rack_id.to_string());
        let filters = component_filters_for(&calls[0]);
        assert_eq!(filters, &["BMC".to_owned()]);
        let dev0 = &calls[0].nodes.as_ref().unwrap().nodes[0];
        assert_descriptor_node(dev0, ROLE_COMPUTE);

        let jobs = backend.firmware_jobs.lock().unwrap();
        assert_eq!(
            jobs.get(&CT_MAC_1.parse::<MacAddress>().unwrap()),
            Some(&vec![RmsTrackedFirmwareJob::FirmwareObject(
                "ct-job-1".to_string()
            )])
        );
    }

    #[carbide_macros::sqlx_test]
    async fn ct_update_firmware_pre_ingestion_dispatches_via_expected_rack(pool: sqlx::PgPool) {
        // No machine row and no live racks row: the RMS identity must resolve
        // entirely from the expected inventory, with the node id synthesized
        // from the BMC MAC.
        let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
        let mut txn = pool.begin().await.unwrap();
        seed_pre_ingestion_compute_tray(&mut txn, CT_MAC_1, &rack_id, false).await;
        txn.commit().await.unwrap();

        let mock = Arc::new(MockRmsApi::new());
        let backend = RmsBackend::new(
            mock.clone(),
            Some(mock.clone()),
            pool.clone(),
            Arc::new(rack_profile_config()),
            true,
        );

        let bmc_mac: MacAddress = CT_MAC_1.parse().unwrap();
        // Row-less trays use the BMC MAC directly as the RMS node id.
        let node_id = bmc_mac.to_string();
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &node_id,
            "ct-pre-job-1",
        )))
        .await;

        // No machine row exists for CT_IP_1, so update_firmware falls back to the
        // expected inventory keyed by BMC MAC.
        let eps = vec![make_ct_endpoint(CT_IP_1, CT_MAC_1)];
        let results = ComputeTrayManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[ComputeTrayComponent::Bmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        assert!(results[0].success);

        let calls = mock.apply_firmware_object_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].rack_id, rack_id.to_string());
        assert_eq!(calls[0].config_json, r#"{"Id":"fw-json"}"#);
        let dev0 = &calls[0].nodes.as_ref().unwrap().nodes[0];
        assert_eq!(dev0.node_id, node_id);
        assert_descriptor_node(dev0, ROLE_COMPUTE);
        assert!(dev0.bmc_endpoint.is_some());

        // In-memory tracking is keyed by MAC even without a machine row.
        let jobs = backend.firmware_jobs.lock().unwrap();
        assert_eq!(
            jobs.get(&bmc_mac),
            Some(&vec![RmsTrackedFirmwareJob::FirmwareObject(
                "ct-pre-job-1".to_string()
            )])
        );
    }

    #[carbide_macros::sqlx_test]
    async fn ct_update_firmware_pre_ingestion_without_expected_rack_errors(pool: sqlx::PgPool) {
        // A MAC with no rack-scale expected record has no descriptor to build,
        // so the backend reports an error rather than dispatching to RMS.
        let mock = Arc::new(MockRmsApi::new());
        let backend = RmsBackend::new(
            mock.clone(),
            Some(mock.clone()),
            pool.clone(),
            Arc::new(rack_profile_config()),
            true,
        );

        let eps = vec![make_ct_endpoint(CT_IP_1, CT_MAC_1)];
        let results = ComputeTrayManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[ComputeTrayComponent::Bmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        assert!(!results[0].success);
        assert!(
            results[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("expected inventory"))
        );
        assert!(mock.apply_firmware_object_calls().await.is_empty());
    }

    #[carbide_macros::sqlx_test]
    async fn ct_firmware_status_tracks_job(pool: sqlx::PgPool) {
        let (mock, backend, _, ct1, _) = make_compute_tray_backend(&pool).await;
        mock.enqueue_apply_firmware_object(Ok(MockRmsApi::firmware_object_apply_ok(
            &ct1.to_string(),
            "ct-job-status",
        )))
        .await;

        let eps = vec![make_ct_endpoint(CT_IP_1, CT_MAC_1)];
        ComputeTrayManager::update_firmware(
            &backend,
            &eps,
            r#"{"Id":"fw-json"}"#,
            &[ComputeTrayComponent::Bmc],
            &firmware_update_options(),
        )
        .await
        .unwrap();

        mock.enqueue_get_firmware_job_status(Ok(MockRmsApi::firmware_job_status_ok(
            rms::FirmwareJobState::Completed,
        )))
        .await;

        let statuses = ComputeTrayManager::get_firmware_status(&backend, &eps)
            .await
            .unwrap();

        assert_eq!(statuses[0].state, FirmwareState::Completed);
        assert!(statuses[0].error.is_none());
    }
}
