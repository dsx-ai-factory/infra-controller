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
use std::fmt::Display;

use carbide_uuid::machine::HostMachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::StateSla;
use crate::component_manager::PowerAction;
use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::health::HealthReportSources;
use crate::metadata::Metadata;

// Well-known label keys!
//
// For rack chassis and location, there are currently a few labels that we
// are passing through from the NICo REST API into NICo. These labels get
// used by orchestration systems and tooling who want to work on, and have
// awareness of, racks based on their physical location.
//
// At the time of this writing, these are all new and optional, but it made
// the most sense to put them in as labels due to their optional and flexible
// nature as we smooth things out. In the interim, it seemed to make sense
// to at least have some "well known" defs for now, which may very well
// change over time.
//
// These labels apply to both ExpectedRack AND Rack metadata labels, which
// are applied from Expected -> Managed at promition time.
//
// First, rack chassis info labels, which physically identifies the rack
// hardware itself.
pub const LABEL_CHASSIS_MANUFACTURER: &str = "chassis.manufacturer";
pub const LABEL_CHASSIS_SERIAL_NUMBER: &str = "chassis.serial-number";
pub const LABEL_CHASSIS_MODEL: &str = "chassis.model";

// Next, rack location info labels, which identifies where the rack
// physically lives.
pub const LABEL_LOCATION_REGION: &str = "location.region";
pub const LABEL_LOCATION_DATACENTER: &str = "location.datacenter";
pub const LABEL_LOCATION_ROOM: &str = "location.room";
pub const LABEL_LOCATION_POSITION: &str = "location.position";

#[derive(Debug, Clone)]
pub struct Rack {
    pub id: RackId,
    pub rack_profile_id: Option<RackProfileId>,
    pub config: RackConfig,
    pub controller_state: Versioned<RackState>,
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
    pub firmware_upgrade_job: Option<FirmwareUpgradeJob>,
    pub nvos_update_job: Option<NvosUpdateJob>,
    pub health_reports: HealthReportSources,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    pub metadata: Metadata,
    pub version: ConfigVersion,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirmwareUpgradeJob {
    pub job_id: Option<String>,
    #[serde(default)]
    pub firmware_id: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub batch_job_ids: Vec<String>,
    #[serde(default)]
    pub machines: Vec<FirmwareUpgradeDeviceStatus>,
    #[serde(default)]
    pub switches: Vec<FirmwareUpgradeDeviceStatus>,
    #[serde(default)]
    pub power_shelves: Vec<FirmwareUpgradeDeviceStatus>,
}

impl FirmwareUpgradeJob {
    pub fn all_devices(&self) -> impl Iterator<Item = &FirmwareUpgradeDeviceStatus> {
        self.machines
            .iter()
            .chain(self.switches.iter())
            .chain(self.power_shelves.iter())
    }

    pub fn all_devices_mut(&mut self) -> impl Iterator<Item = &mut FirmwareUpgradeDeviceStatus> {
        self.machines
            .iter_mut()
            .chain(self.switches.iter_mut())
            .chain(self.power_shelves.iter_mut())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NvosUpdateJob {
    pub job_id: Option<String>,
    pub firmware_id: String,
    pub image_filename: String,
    pub local_file_path: String,
    pub version: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub switches: Vec<NvosUpdateSwitchStatus>,
}

impl NvosUpdateJob {
    pub fn all_switches(&self) -> impl Iterator<Item = &NvosUpdateSwitchStatus> {
        self.switches.iter()
    }

    pub fn all_switches_mut(&mut self) -> impl Iterator<Item = &mut NvosUpdateSwitchStatus> {
        self.switches.iter_mut()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedNvosArtifact {
    pub firmware_id: String,
    pub image_filename: String,
    pub local_file_path: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvosUpdateSwitchStatus {
    #[serde(default)]
    pub node_id: String,
    pub mac: String,
    pub bmc_ip: String,
    pub nvos_ip: String,
    pub status: String,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Per-device input passed to RMS when starting a firmware upgrade.
/// TODO to be replaced with RMS protobuf message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareUpgradeDeviceInfo {
    pub node_id: String,
    pub mac: String,
    pub bmc_ip: String,
    pub bmc_username: String,
    pub bmc_password: String,
    pub os_mac: Option<String>,
    pub os_ip: Option<String>,
    pub os_username: Option<String>,
    pub os_password: Option<String>,
    pub os_hostname: Option<String>,
}

/// Per-device status tracked inside `FirmwareUpgradeJob`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareUpgradeDeviceStatus {
    #[serde(default)]
    pub node_id: String,
    pub mac: String,
    pub bmc_ip: String,
    pub status: String,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub parent_job_id: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Per-device firmware upgrade status set by the rack state machine during a
/// rack-level firmware upgrade. Used on both machines and switches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RackFirmwareUpgradeStatus {
    pub task_id: String,
    pub status: RackFirmwareUpgradeState,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl RackFirmwareUpgradeStatus {
    /// Returns true if the firmware upgrade is still in progress
    /// (i.e. `ended_at` has not been set yet).
    pub fn is_in_progress(&self) -> bool {
        self.ended_at.is_none()
    }

    /// Returns true if the firmware upgrade finished successfully or failed.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            RackFirmwareUpgradeState::Completed | RackFirmwareUpgradeState::Failed { .. }
        )
    }

    /// Returns true when this status belongs to the active rack-upgrade cycle.
    pub fn is_current_for(&self, requested_at: DateTime<Utc>) -> bool {
        self.ended_at.is_some_and(|ts| ts >= requested_at)
            || self.started_at.is_some_and(|ts| ts >= requested_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RackFirmwareUpgradeState {
    Started,
    InProgress,
    Completed,
    Failed { cause: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchNvosUpdateStatus {
    pub task_id: String,
    pub firmware_id: String,
    pub image_filename: String,
    pub status: SwitchNvosUpdateState,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl SwitchNvosUpdateStatus {
    pub fn is_in_progress(&self) -> bool {
        self.ended_at.is_none()
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            SwitchNvosUpdateState::Completed | SwitchNvosUpdateState::Failed { .. }
        )
    }

    pub fn is_current_for(&self, requested_at: DateTime<Utc>) -> bool {
        self.ended_at.is_some_and(|ts| ts >= requested_at)
            || self.started_at.is_some_and(|ts| ts >= requested_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SwitchNvosUpdateState {
    Started,
    InProgress,
    Completed,
    Failed { cause: String },
}

#[derive(Clone, Debug, Default)]
pub struct RackSearchFilter {
    pub label: Option<crate::metadata::LabelFilter>,
}

pub fn derive_rack_aggregate_health(sources: &HealthReportSources) -> health_report::HealthReport {
    if let Some(replace) = &sources.replace {
        return replace.clone();
    }
    let mut output = health_report::HealthReport::empty("rack-aggregate-health".to_string());
    for report in sources.merges.values() {
        output.merge(report);
    }
    output.observed_at = Some(chrono::Utc::now());
    output
}

impl<'r> FromRow<'r, PgRow> for Rack {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let config: sqlx::types::Json<RackConfig> = row.try_get("config")?;
        let controller_state: sqlx::types::Json<RackState> = row.try_get("controller_state")?;
        let controller_state_outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
            row.try_get("controller_state_outcome").ok();
        let health_reports: HealthReportSources = row
            .try_get::<sqlx::types::Json<HealthReportSources>, _>("health_reports")
            .map(|j| j.0)
            .unwrap_or_default();
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;
        let metadata = Metadata {
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            labels: labels.0,
        };
        let firmware_upgrade_job: Option<FirmwareUpgradeJob> = row
            .try_get::<Option<sqlx::types::Json<FirmwareUpgradeJob>>, _>("firmware_upgrade_job")
            .ok()
            .flatten()
            .map(|j| j.0);
        let nvos_update_job: Option<NvosUpdateJob> = row
            .try_get::<Option<sqlx::types::Json<NvosUpdateJob>>, _>("nvos_update_job")
            .ok()
            .flatten()
            .map(|j| j.0);
        Ok(Rack {
            id: row.try_get("id")?,
            rack_profile_id: row.try_get("rack_profile_id")?,
            config: config.0,
            controller_state: Versioned {
                value: controller_state.0,
                version: row.try_get("controller_state_version")?,
            },
            controller_state_outcome: controller_state_outcome.map(|o| o.0),
            firmware_upgrade_job,
            nvos_update_job,
            health_reports,
            created: row.try_get("created")?,
            updated: row.try_get("updated")?,
            deleted: row.try_get("deleted")?,
            metadata,
            version: row.try_get("version")?,
        })
    }
}

// ============================================================================
// RACK STATES
// ============================================================================

/// State of a Rack as tracked by the controller.
///
/// The rack progresses through discovery and maintenance phases, then enters
/// validation where partitions (groups of nodes) are validated by an external
/// service (RVS).
///
/// ## State Flow
///
/// ```text
/// Created -> Discovering -> Maintenance -> Validating -> Ready
///                ^               |              |          |  |
///                |               v              v          |  |
///                |            Error <------  Error         |  |
///                |                                         |  |
///                +--- topology_changed --------------------+  |
///                                                             |
///           Maintenance <--- reprovision_requested -----------+
/// ```
///
/// ### Maintenance Sub-states
///
/// ```text
/// FirmwareUpgrade -> NVOSUpdate -> ConfigureNmxCluster -> Completed -> Validating(Pending)
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RackState {
    /// Rack has been created in Carbide.
    /// Created when ExpectedMachine/Switch/PS references this rack.
    #[default]
    #[serde(alias = "expected", alias = "unknown")]
    Created,

    /// Discovery in progress - waiting for all expected devices to appear
    /// and reach ManagedHostState::Ready.
    Discovering,

    /// Rack is in the validation phase. The sub-state tracks progress from
    /// waiting for RVS through partition-level pass/fail to a final verdict.
    ///
    /// The active RVS run ID is stored inside each non-`Pending` substate of
    /// `rack_validation`. It is set on the `Pending -> InProgress` transition
    /// when Carbide first observes an `rv.run-id` label on a rack machine.
    #[serde(alias = "validation")]
    Validating {
        #[serde(alias = "rack_validation")]
        validating_state: RackValidationState,
    },

    /// Rack is fully validated and ready for production workloads.
    Ready,

    /// Rack is undergoing maintenance (firmware upgrade, power sequencing, etc.).
    /// Maintenance happens after discovery and before validation.
    Maintenance {
        #[serde(alias = "rack_maintenance")]
        maintenance_state: RackMaintenanceState,
    },

    /// There is error in the Rack; Rack can not be used if it's in error.
    Error { cause: String },

    /// Rack is in the process of deleting.
    Deleting,
}

/// Sub-states of rack maintenance.
///
/// The rack enters maintenance after discovery (all devices found, all machines
/// ready) and exits into `Validation(Pending)` once maintenance is complete,
/// at which point the validation flow takes over.
///
/// ## Sub-state Flow
///
/// ```text
/// FirmwareUpgrade -> NVOSUpdate -> ConfigureNmxCluster -> Completed -> Validation(Pending)
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RackMaintenanceState {
    FirmwareUpgrade {
        rack_firmware_upgrade: FirmwareUpgradeState,
    },
    NVOSUpdate {
        nvos_update: NvosUpdateState,
    },
    ConfigureNmxCluster {
        configure_nmx_cluster: ConfigureNmxClusterState,
    },
    PowerSequence {
        rack_power: RackPowerState,
    },
    Completed,
}

impl Display for RackMaintenanceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RackMaintenanceState::FirmwareUpgrade {
                rack_firmware_upgrade,
            } => {
                write!(f, "FirmwareUpgrade({})", rack_firmware_upgrade)
            }
            RackMaintenanceState::NVOSUpdate { nvos_update } => {
                write!(f, "NVOSUpdate({})", nvos_update)
            }
            RackMaintenanceState::ConfigureNmxCluster {
                configure_nmx_cluster,
            } => {
                write!(f, "ConfigureNmxCluster({})", configure_nmx_cluster)
            }
            RackMaintenanceState::PowerSequence { rack_power } => {
                write!(f, "PowerSequence({})", rack_power)
            }
            RackMaintenanceState::Completed => write!(f, "Completed"),
        }
    }
}

/// Sub-states of `RackMaintenanceState::ConfigureNmxCluster`.
///
/// `Start` submits the asynchronous RMS ScaleUpFabricManager workflow.
/// `WaitForScaleUpFabricManagerJob` polls the submitted job; after it
/// completes, NICo reads the observed fabric status, validates the
/// RMS-selected primary, persists it with the per-switch Fabric Manager
/// status, and advances to the next requested maintenance activity.
///
/// The remaining variants name sub-states of a workflow this version does not
/// run. A `controller_state` row can still hold one, so they are decoded to
/// keep that row from failing the batch queries that load every rack.
/// Maintenance cannot resume from them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfigureNmxClusterState {
    Start,

    WaitForScaleUpFabricManagerJob {
        /// RMS job identifier returned by submission.
        job_id: String,
    },

    /// Retired sub-state. The braces make this a struct variant, which absorbs
    /// the payload the retired workflow wrote; a unit variant would reject it.
    ConfigureCertificates {},

    /// Retired sub-state.
    DisableScaleUpFabricState,

    /// Retired sub-state.
    ConfigureScaleUpFabricManager,

    /// Retired sub-state.
    WaitForFabricStatus,
}

impl Display for ConfigureNmxClusterState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigureNmxClusterState::Start => write!(f, "Start"),
            ConfigureNmxClusterState::WaitForScaleUpFabricManagerJob { job_id } => {
                write!(f, "WaitForScaleUpFabricManagerJob({job_id})")
            }
            ConfigureNmxClusterState::ConfigureCertificates {} => {
                write!(f, "ConfigureCertificates")
            }
            ConfigureNmxClusterState::DisableScaleUpFabricState => {
                write!(f, "DisableScaleUpFabricState")
            }
            ConfigureNmxClusterState::ConfigureScaleUpFabricManager => {
                write!(f, "ConfigureScaleUpFabricManager")
            }
            ConfigureNmxClusterState::WaitForFabricStatus => write!(f, "WaitForFabricStatus"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareUpgradeState {
    Start,
    WaitForComplete,
}

impl Display for FirmwareUpgradeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FirmwareUpgradeState::Start => write!(f, "Start"),
            FirmwareUpgradeState::WaitForComplete => write!(f, "WaitForComplete"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NvosUpdateState {
    Start,
    WaitForComplete,
}

impl Display for NvosUpdateState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NvosUpdateState::Start => write!(f, "Start"),
            NvosUpdateState::WaitForComplete => write!(f, "WaitForComplete"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RackPowerState {
    PoweringOn,
    PoweringOff,
    PowerReset,
}

impl Display for RackPowerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RackPowerState::PoweringOn => write!(f, "PoweringOn"),
            RackPowerState::PoweringOff => write!(f, "PoweringOff"),
            RackPowerState::PowerReset => write!(f, "PowerReset"),
        }
    }
}

/// Sub-states of rack validation.
///
/// The rack enters validation after maintenance completes (starting in
/// `Pending`). RVS drives transitions by writing instance metadata labels
/// that BMMC polls and aggregates into partition-level summaries.
///
/// All non-`Pending` substates carry the `run_id` of the active RVS run.
/// The run ID is set when the first `rv.run-id` label is observed on a
/// rack machine (the `Pending -> InProgress` transition); all subsequent
/// substates inherit it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RackValidationState {
    /// All nodes discovered and all machines have reached
    /// ManagedHostState::Ready. Waiting for RVS to begin partition
    /// validation.
    ///
    /// TODO[#416]: The responsibility of gating production instance allocation
    /// should live in the node/tray-level state machine, not the rack SM.
    /// The proposed mechanism is to force health overrides for each
    /// node that transitioning into READY state, essentially make
    /// nodes "unhealthy". This way no instance can be allocated
    /// for the tenant. RVS, however, will be able to force the
    /// instance via supplying "allow_unhealthy" flag while creating
    /// instances.
    Pending,

    /// At least one partition has started validation, but none have
    /// completed (neither passed nor failed yet).
    InProgress { run_id: String },

    /// At least one partition has passed validation, and no partitions
    /// have failed. Waiting for remaining partitions to complete.
    Partial { run_id: String },

    /// At least one partition has failed validation.
    /// Can recover to Partial if failed partitions are re-validated.
    FailedPartial { run_id: String },

    /// All partitions have passed validation successfully.
    /// Rack is ready to transition to the Ready state.
    Validated { run_id: String },

    /// All partitions have failed validation.
    /// Requires intervention before the rack can be used.
    Failed { run_id: String },
}

impl RackValidationState {
    /// Returns the active RVS run ID, or `None` for the `Pending` substate.
    pub fn run_id(&self) -> Option<&str> {
        match self {
            RackValidationState::InProgress { run_id }
            | RackValidationState::Partial { run_id }
            | RackValidationState::FailedPartial { run_id }
            | RackValidationState::Validated { run_id }
            | RackValidationState::Failed { run_id } => Some(run_id),
            RackValidationState::Pending => None,
        }
    }
}

impl Display for RackValidationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RackValidationState::Pending => write!(f, "Pending"),
            RackValidationState::InProgress { .. } => write!(f, "InProgress"),
            RackValidationState::Partial { .. } => write!(f, "Partial"),
            RackValidationState::FailedPartial { .. } => write!(f, "FailedPartial"),
            RackValidationState::Validated { .. } => write!(f, "Validated"),
            RackValidationState::Failed { .. } => write!(f, "Failed"),
        }
    }
}

impl Display for RackState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RackState::Created => write!(f, "Created"),
            RackState::Discovering => write!(f, "Discovering"),
            RackState::Validating { validating_state } => {
                write!(f, "Validating({})", validating_state)
            }
            RackState::Ready => write!(f, "Ready"),
            RackState::Maintenance { maintenance_state } => {
                write!(f, "Maintenance({})", maintenance_state)
            }
            RackState::Error { cause } => write!(f, "Error({})", cause),
            RackState::Deleting => write!(f, "Deleting"),
        }
    }
}

/// Machine metadata labels set by RVS to communicate validation state.
pub enum MachineRvLabels {
    /// Partition ID grouping nodes into validation partitions.
    PartitionId,
    /// Run correlation ID -- used to filter stale labels from prior runs.
    RunId,
    /// Per-node validation status.
    State,
    /// Failure description (only when status is `fail`).
    FailDesc,
}

impl MachineRvLabels {
    pub fn as_str(&self) -> &'static str {
        match self {
            MachineRvLabels::PartitionId => "rv.part-id",
            MachineRvLabels::RunId => "rv.run-id",
            MachineRvLabels::State => "rv.st",
            MachineRvLabels::FailDesc => "rv.fail-desc",
        }
    }
}

/// Individual maintenance activities that can be performed during on-demand
/// rack maintenance. When the activities list on [`MaintenanceScope`] is
/// empty, all activities are performed.
///
/// Activity-specific configuration is carried inline on the variant
/// (e.g. `FirmwareUpgrade` holds the optional target firmware version).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaintenanceActivity {
    FirmwareUpgrade {
        /// SOT JSON for RMS ApplyFirmwareObject.
        /// `None` is only valid for implicit all-activity maintenance skips.
        #[serde(default)]
        firmware_version: Option<String>,
        /// Firmware components to update (e.g. "BMC", "CPLD", "BIOS").
        /// Empty means all components.
        #[serde(default)]
        components: Vec<String>,
        #[serde(default)]
        force_update: bool,
    },
    NvosUpdate {
        /// Ephemeral SOT JSON used with RMS ApplySwitchSystemImage.
        /// The access token is stored separately as a maintenance credential.
        config_json: String,
    },
    ConfigureNmxCluster,
    PowerSequence,
    /// Per-device power control, dispatched by the rack state controller to
    /// the listed devices on its next tick. Framed out here for the component
    /// manager routing path; the rack state handler side is a follow-up.
    PowerControl {
        action: PowerAction,
    },
}

impl MaintenanceActivity {
    /// Returns `true` if two activities are the same kind, ignoring
    /// any per-activity configuration (e.g. firmware version).
    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl std::fmt::Display for MaintenanceActivity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaintenanceActivity::FirmwareUpgrade { .. } => write!(f, "FirmwareUpgrade"),
            MaintenanceActivity::NvosUpdate { .. } => write!(f, "NvosUpdate"),
            MaintenanceActivity::ConfigureNmxCluster => write!(f, "ConfigureNmxCluster"),
            MaintenanceActivity::PowerSequence => write!(f, "PowerSequence"),
            MaintenanceActivity::PowerControl { .. } => write!(f, "PowerControl"),
        }
    }
}

/// Specifies which devices in the rack should be included in an on-demand
/// maintenance cycle. When all three device-id lists are empty, the full rack
/// is maintained.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct MaintenanceScope {
    #[serde(default)]
    pub machine_ids: Vec<HostMachineId>,
    #[serde(default)]
    pub switch_ids: Vec<SwitchId>,
    #[serde(default)]
    pub power_shelf_ids: Vec<PowerShelfId>,
    /// Which maintenance activities to perform. Empty means all activities.
    #[serde(default)]
    pub activities: Vec<MaintenanceActivity>,
}

impl MaintenanceScope {
    /// Returns `true` when no specific devices were selected, meaning the
    /// maintenance applies to every device in the rack.
    pub fn is_full_rack(&self) -> bool {
        self.machine_ids.is_empty() && self.switch_ids.is_empty() && self.power_shelf_ids.is_empty()
    }

    pub fn should_run(&self, activity: &MaintenanceActivity) -> bool {
        self.activities.is_empty() || self.activities.iter().any(|a| a.same_kind(activity))
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RackConfig {
    /// When set, the Ready state handler will transition back to Maintenance
    /// to re-provision the rack to a new version.
    #[serde(default)]
    pub reprovision_requested: bool,

    /// When set, the Ready state handler will transition back to Discovering
    /// because a tray was replaced (rack topology change).
    #[serde(default)]
    pub topology_changed: bool,

    /// On-demand maintenance request. When `Some`, the rack state handler
    /// (in Ready or Error) transitions the rack to Maintenance. The scope
    /// selects full-rack vs partial-rack and which activities to run.
    #[serde(default)]
    pub maintenance_requested: Option<MaintenanceScope>,

    /// Operator request to terminate the active rack maintenance cycle. The rack
    /// state controller consumes this flag, performs device cleanup, and
    /// transitions the rack to [`RackState::Error`].
    #[serde(default)]
    pub maintenance_termination_requested: bool,
}

/// Reason a rack will not accept a new on-demand maintenance request.
/// See `Rack::check_accepts_maintenance`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RackMaintenanceRejection {
    /// The rack is not in `Ready` or `Error`. Carries the current state so
    /// callers can report exactly what state they saw.
    NotReadyOrError(RackState),
    /// A maintenance request is already pending on this rack.
    AlreadyPending,
}

impl Display for RackMaintenanceRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RackMaintenanceRejection::NotReadyOrError(state) => write!(
                f,
                "rack is not in Ready or Error state (current: {state:?}); \
                 maintenance can only be requested from those states",
            ),
            RackMaintenanceRejection::AlreadyPending => {
                write!(f, "rack already has a pending maintenance request")
            }
        }
    }
}

impl Rack {
    /// Tells us if this rack will accept a new on-demand maintenance requests
    /// right now. Used by every caller that writes to `RackConfig::maintenance_requested`
    /// (e.g. the on-demand-maintenance gRPC handler + and the Component Manager
    /// state controller wrappers). This gives us a way to gate maintenance requests
    /// making their way into the backing `maintenance_requested` data in `RackConfig`.
    pub fn check_accepts_maintenance(&self) -> Result<(), RackMaintenanceRejection> {
        if !matches!(
            *self.controller_state,
            RackState::Ready | RackState::Error { .. }
        ) {
            return Err(RackMaintenanceRejection::NotReadyOrError(
                self.controller_state.value.clone(),
            ));
        }
        if self.config.maintenance_requested.is_some() {
            return Err(RackMaintenanceRejection::AlreadyPending);
        }
        Ok(())
    }
}

// ============================================================================
// SLA & CONVERSIONS
// ============================================================================

pub fn state_sla(state: &RackState, state_version: &ConfigVersion) -> StateSla {
    let _time_in_state = chrono::Utc::now()
        .signed_duration_since(state_version.timestamp())
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(60 * 60 * 24));

    // TODO[#416]: Define SLAs for validation and maintenance states
    match state {
        RackState::Created => StateSla::no_sla(),
        RackState::Discovering => StateSla::no_sla(),
        RackState::Validating { .. } => StateSla::no_sla(),
        RackState::Ready => StateSla::no_sla(),
        RackState::Maintenance { .. } => StateSla::no_sla(),
        RackState::Error { .. } => StateSla::no_sla(),
        RackState::Deleting => StateSla::no_sla(),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
    use carbide_uuid::power_shelf::{PowerShelfIdSource, PowerShelfType};
    use carbide_uuid::switch::{SwitchIdSource, SwitchType};

    use super::*;

    // ConfigureNmxClusterState wire compatibility

    /// Pins the shapes a `controller_state` row can hold for a rack that was
    /// mid-maintenance under the retired synchronous workflow. Decoding these
    /// keeps one such row from failing the batch queries that load every rack.
    #[test]
    fn configure_nmx_cluster_state_decodes_retired_sub_states() {
        for (encoded, expected) in [
            (
                serde_json::json!("DisableScaleUpFabricState"),
                ConfigureNmxClusterState::DisableScaleUpFabricState,
            ),
            (
                serde_json::json!("ConfigureScaleUpFabricManager"),
                ConfigureNmxClusterState::ConfigureScaleUpFabricManager,
            ),
            (
                serde_json::json!("WaitForFabricStatus"),
                ConfigureNmxClusterState::WaitForFabricStatus,
            ),
            (
                serde_json::json!({"ConfigureCertificates": {"configure_certificate": "Start"}}),
                ConfigureNmxClusterState::ConfigureCertificates {},
            ),
            (
                serde_json::json!({
                    "ConfigureCertificates": {
                        "configure_certificate": {
                            "WaitForComplete": {"jobs": [{"switch_id": "s", "job_id": "j"}]}
                        }
                    }
                }),
                ConfigureNmxClusterState::ConfigureCertificates {},
            ),
        ] {
            let decoded: ConfigureNmxClusterState = serde_json::from_value(encoded.clone())
                .unwrap_or_else(|error| panic!("{encoded} should decode: {error}"));

            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn rack_state_decodes_retired_nmx_sub_state_without_failing_the_row() {
        let persisted = serde_json::json!({
            "state": "maintenance",
            "maintenance_state": {
                "ConfigureNmxCluster": {"configure_nmx_cluster": "WaitForFabricStatus"}
            }
        });

        let state: RackState =
            serde_json::from_value(persisted).expect("persisted rack state should decode");

        assert_eq!(
            state,
            RackState::Maintenance {
                maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
                    configure_nmx_cluster: ConfigureNmxClusterState::WaitForFabricStatus,
                },
            }
        );
    }

    // ── MaintenanceScope ────────────────────────────────────────────────

    #[test]
    fn is_full_rack_when_all_lists_empty() {
        let scope = MaintenanceScope::default();
        assert!(scope.is_full_rack());
    }

    #[test]
    fn is_not_full_rack_with_machines() {
        let scope = MaintenanceScope {
            machine_ids: vec![
                MachineId::new(MachineIdSource::Tpm, [0; 32], MachineType::Host)
                    .try_into()
                    .unwrap(),
            ],
            ..Default::default()
        };
        assert!(!scope.is_full_rack());
    }

    #[test]
    fn is_not_full_rack_with_switches() {
        let scope = MaintenanceScope {
            switch_ids: vec![SwitchId::new(
                SwitchIdSource::Tpm,
                [0; 32],
                SwitchType::NvLink,
            )],
            ..Default::default()
        };
        assert!(!scope.is_full_rack());
    }

    #[test]
    fn is_not_full_rack_with_power_shelves() {
        let scope = MaintenanceScope {
            power_shelf_ids: vec![PowerShelfId::new(
                PowerShelfIdSource::Tpm,
                [0; 32],
                PowerShelfType::Rack,
            )],
            ..Default::default()
        };
        assert!(!scope.is_full_rack());
    }

    #[test]
    fn should_run_all_when_activities_empty() {
        let scope = MaintenanceScope::default();
        assert!(scope.should_run(&MaintenanceActivity::FirmwareUpgrade {
            firmware_version: None,
            components: vec![],
            force_update: false,
        }));
        assert!(scope.should_run(&MaintenanceActivity::NvosUpdate {
            config_json: String::new(),
        }));
        assert!(scope.should_run(&MaintenanceActivity::ConfigureNmxCluster));
        assert!(scope.should_run(&MaintenanceActivity::PowerSequence));
    }

    #[test]
    fn should_run_only_selected_activity() {
        let scope = MaintenanceScope {
            activities: vec![MaintenanceActivity::FirmwareUpgrade {
                firmware_version: Some("v2.0".into()),
                components: vec![],
                force_update: false,
            }],
            ..Default::default()
        };
        assert!(scope.should_run(&MaintenanceActivity::FirmwareUpgrade {
            firmware_version: None,
            components: vec![],
            force_update: false,
        }));
        assert!(!scope.should_run(&MaintenanceActivity::NvosUpdate {
            config_json: String::new(),
        }));
        assert!(!scope.should_run(&MaintenanceActivity::ConfigureNmxCluster));
        assert!(!scope.should_run(&MaintenanceActivity::PowerSequence));
    }

    #[test]
    fn should_run_multiple_selected_activities() {
        let scope = MaintenanceScope {
            activities: vec![
                MaintenanceActivity::FirmwareUpgrade {
                    firmware_version: None,
                    components: vec![],
                    force_update: false,
                },
                MaintenanceActivity::NvosUpdate {
                    config_json: r#"{"Id":"fw-nvos"}"#.into(),
                },
                MaintenanceActivity::PowerSequence,
            ],
            ..Default::default()
        };
        assert!(scope.should_run(&MaintenanceActivity::FirmwareUpgrade {
            firmware_version: Some("v1.0".into()),
            components: vec![],
            force_update: false,
        }));
        assert!(!scope.should_run(&MaintenanceActivity::ConfigureNmxCluster));
        assert!(scope.should_run(&MaintenanceActivity::NvosUpdate {
            config_json: String::new(),
        }));
        assert!(scope.should_run(&MaintenanceActivity::PowerSequence));
    }

    // ── MaintenanceActivity ─────────────────────────────────────────────

    #[test]
    fn same_kind_matches_regardless_of_config() {
        let a = MaintenanceActivity::FirmwareUpgrade {
            firmware_version: Some("v1".into()),
            components: vec!["BMC".into()],
            force_update: false,
        };
        let b = MaintenanceActivity::FirmwareUpgrade {
            firmware_version: None,
            components: vec![],
            force_update: false,
        };
        assert!(a.same_kind(&b));

        let a = MaintenanceActivity::NvosUpdate {
            config_json: r#"{"Id":"fw-a"}"#.into(),
        };
        let b = MaintenanceActivity::NvosUpdate {
            config_json: String::new(),
        };
        assert!(a.same_kind(&b));
    }

    #[test]
    fn same_kind_does_not_match_different_variants() {
        let a = MaintenanceActivity::FirmwareUpgrade {
            firmware_version: None,
            components: vec![],
            force_update: false,
        };
        let b = MaintenanceActivity::ConfigureNmxCluster;
        assert!(!a.same_kind(&b));
    }

    #[test]
    fn maintenance_activity_display() {
        assert_eq!(
            MaintenanceActivity::FirmwareUpgrade {
                firmware_version: None,
                components: vec![],
                force_update: false,
            }
            .to_string(),
            "FirmwareUpgrade"
        );
        assert_eq!(
            MaintenanceActivity::ConfigureNmxCluster.to_string(),
            "ConfigureNmxCluster"
        );
        assert_eq!(
            MaintenanceActivity::NvosUpdate {
                config_json: String::new(),
            }
            .to_string(),
            "NvosUpdate"
        );
        assert_eq!(
            MaintenanceActivity::PowerSequence.to_string(),
            "PowerSequence"
        );
    }

    #[test]
    fn rack_config_defaults_missing_maintenance_termination_request_to_false() {
        let config: RackConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.maintenance_termination_requested);

        let config: RackConfig =
            serde_json::from_str(r#"{"maintenance_termination_requested":true}"#).unwrap();
        assert!(config.maintenance_termination_requested);
    }

    // ── Rack::check_accepts_maintenance ─────────────────────────────────

    fn test_rack(state: RackState, maintenance_requested: Option<MaintenanceScope>) -> Rack {
        Rack {
            id: RackId::default(),
            rack_profile_id: None,
            config: RackConfig {
                maintenance_requested,
                ..Default::default()
            },
            controller_state: Versioned::new(state, ConfigVersion::initial()),
            controller_state_outcome: None,
            firmware_upgrade_job: None,
            nvos_update_job: None,
            health_reports: Default::default(),
            created: Utc::now(),
            updated: Utc::now(),
            deleted: None,
            metadata: Metadata::default(),
            version: ConfigVersion::initial(),
        }
    }

    // check_accepts_maintenance: Ready/Error (with no pending request) accept;
    // every other state is rejected as NotReadyOrError, and a state that already
    // has a pending request is rejected as AlreadyPending. The originals only
    // asserted the rejection variant (via matches!), so we compare the error's
    // discriminant rather than its full value. The input is the (state, pending)
    // pair handed to `test_rack`.
    #[test]
    fn check_accepts_maintenance_cases() {
        // Discriminant sentinels for the two rejection variants.
        let not_ready_or_error = std::mem::discriminant(
            &RackMaintenanceRejection::NotReadyOrError(RackState::Created),
        );
        let already_pending = std::mem::discriminant(&RackMaintenanceRejection::AlreadyPending);

        scenarios!(
            run = |(state, maintenance_requested)| {
                test_rack(state, maintenance_requested)
                    .check_accepts_maintenance()
                    .map_err(|e| std::mem::discriminant(&e))
            };
            "accepts in ready state" {
                (RackState::Ready, None) => Yields(()),
            }

            "accepts in error state" {
                (
                    RackState::Error {
                        cause: "something broke".into(),
                    },
                    None,
                ) => Yields(()),
            }

            "rejects in created state" {
                (RackState::Created, None) => FailsWith(not_ready_or_error),
            }

            "rejects in discovering state" {
                (RackState::Discovering, None) => FailsWith(not_ready_or_error),
            }

            "rejects in maintenance state" {
                (
                    RackState::Maintenance {
                        maintenance_state: RackMaintenanceState::Completed,
                    },
                    None,
                ) => FailsWith(not_ready_or_error),
            }

            "rejects when already pending" {
                (RackState::Ready, Some(MaintenanceScope::default())) => FailsWith(already_pending),
            }
        );
    }

    // ── RackMaintenanceRejection display ────────────────────────────────

    #[test]
    fn rejection_not_ready_or_error_display() {
        let rejection = RackMaintenanceRejection::NotReadyOrError(RackState::Discovering);
        let msg = rejection.to_string();
        assert!(msg.contains("not in Ready or Error state"));
    }

    #[test]
    fn rejection_already_pending_display() {
        let rejection = RackMaintenanceRejection::AlreadyPending;
        let msg = rejection.to_string();
        assert!(msg.contains("already has a pending maintenance request"));
    }
}
