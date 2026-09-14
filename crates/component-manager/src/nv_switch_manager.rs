// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::fmt::Debug;
use std::net::IpAddr;

use carbide_secrets::credentials::Credentials;
use mac_address::MacAddress;
use model::component_manager::{
    ConfigureSwitchCertificateState, FirmwareState, NvSwitchComponent, PowerAction,
};
use model::rack_type::RackHardwareTopology;
use model::switch::FabricManagerStatus;

use crate::error::ComponentManagerError;
use crate::types::FirmwareUpdateOptions;

/// Selects which `NvSwitchManager` backend is used
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Nsm,
    #[default]
    Rms,
    Mock,
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nsm => f.write_str("nsm"),
            Self::Rms => f.write_str("rms"),
            Self::Mock => f.write_str("mock"),
        }
    }
}

/// Backend observation of a switch OS password-rotation job.
///
/// This describes the backend job, not the credential state observed on the
/// switch. In particular, `NotFound` does not prove that the password mutation
/// was never accepted or did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchPasswordRotationState {
    /// The backend cannot currently resolve the job. This does not establish
    /// the mutation outcome; callers must retain or fail the staged target
    /// according to their recovery policy.
    NotFound,

    /// The backend returned a job state that cannot be classified.
    Unknown,

    /// The backend accepted the job and it has not reached a terminal state.
    Pending,

    /// The backend reports that the password mutation completed successfully.
    /// Callers may still need to promote and verify the staged credential.
    Completed,

    /// The backend reports that the job terminated unsuccessfully.
    Failed,
}

/// Physical network identifiers for an NV-Switch, used to register with and
/// operate against the configured backend service.
#[derive(Debug, Clone)]
pub struct SwitchEndpoint {
    pub bmc_ip: IpAddr,
    pub bmc_mac: MacAddress,
    pub nvos_ip: IpAddr,
    pub nvos_mac: MacAddress,
    pub bmc_credentials: Credentials,
    pub nvos_credentials: Credentials,
    /// Fully qualified NVOS hostname from `machine_interfaces` (TLS SNI).
    pub nvos_host_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SwitchComponentResult {
    pub bmc_mac: MacAddress,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SwitchFirmwareUpdateStatus {
    pub bmc_mac: MacAddress,
    pub state: FirmwareState,
    pub target_version: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SwitchSlotAndTrayResult {
    pub bmc_mac: MacAddress,
    pub slot_number: Option<i32>,
    pub tray_index: Option<i32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SwitchPowerStateResult {
    pub bmc_mac: MacAddress,
    pub power_state: Option<String>,
    pub error: Option<String>,
}

impl crate::component_common::ComponentPowerStateResult for SwitchPowerStateResult {
    fn power_state(&self) -> Option<&str> {
        self.power_state.as_deref()
    }

    fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigureSwitchCertificateJobStatus {
    pub state: ConfigureSwitchCertificateState,
    pub error: Option<String>,
}

/// Backend observation of a ScaleUp Fabric Manager configuration job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleUpFabricManagerJobStatus {
    /// The job remains queued or running.
    Pending { description: String },

    /// The desired fabric configuration completed successfully.
    Completed,

    /// The job terminated unsuccessfully.
    Failed { error: Option<String> },

    /// The backend returned an execution state that cannot be classified.
    Unknown { execution_state: i32 },
}

/// Application-level result reported by a ScaleUp Fabric status operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleUpFabricResponseStatus {
    /// The backend completed the operation successfully.
    Success,

    /// The backend completed the operation with a failure result.
    Failure,

    /// The backend returned an unspecified or unrecognized result code.
    Unknown(i32),
}

/// Observed ScaleUp Fabric state for one switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleUpFabricSwitchStatus {
    /// Backend node identifier for the switch.
    pub node_id: String,

    /// Whether the ScaleUp Fabric cluster is enabled on this switch.
    pub enabled: bool,

    /// Backend-provided inspection failure details.
    pub error_message: String,
}

/// Result of inspecting the ScaleUp Fabric configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleUpFabricStatus {
    /// Application-level result of the inspection.
    pub status: ScaleUpFabricResponseStatus,

    /// Per-switch observations, absent when the backend returned no fabric status.
    pub switches: Option<Vec<ScaleUpFabricSwitchStatus>>,

    /// Backend-provided operation failure details.
    pub error_message: String,
}

/// Result of reading per-switch Fabric Manager service status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleUpFabricServiceStatuses {
    /// Application-level result of the batch operation.
    pub status: ScaleUpFabricResponseStatus,

    /// Normalized per-switch observations keyed by backend node identifier.
    pub service_statuses: HashMap<String, FabricManagerStatus>,
}

/// Aggregate lifecycle state for a switch factory-reset job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchFactoryResetState {
    /// The parent job or at least one target switch has not completed.
    Pending,

    /// The parent job and every target switch completed successfully.
    Completed,

    /// The parent job or at least one target switch failed.
    Failed,
}

/// Backend observation of a switch factory-reset job and its target switches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchFactoryResetJobStatus {
    /// Aggregate state across the parent operation and every target switch.
    pub state: SwitchFactoryResetState,

    /// Backend-supplied failure details when [`SwitchFactoryResetState::Failed`] is returned.
    /// This is `None` for pending and successful jobs.
    pub error: Option<String>,
}

/// Backend trait for NV-Switch management operations.
///
/// Implementations receive physical endpoint information (BMC + NVOS IPs/MACs)
/// and handle registration with the backend service internally. The
/// service-generated UUID is used for the actual operation and never exposed
/// to the caller; results are keyed by `bmc_mac`.
///
/// Password rotation is split into capability discovery, submission, and
/// observation. This keeps backend-specific job handling here while leaving
/// convergence persistence to the orchestration layer.
#[async_trait::async_trait]
pub trait NvSwitchManager: Send + Sync + Debug + 'static {
    fn name(&self) -> &str;

    /// Reports whether this backend supports OS password rotation.
    ///
    /// `false` means callers must not submit rotation work. `true` means an
    /// unchanged request from the current password to the target password is
    /// safe to repeat: a backend must either apply the target or recognize that
    /// the target is already active. The default keeps existing backends
    /// disabled until they implement this contract.
    fn supports_password_rotation(&self) -> bool {
        false
    }

    fn supports_firmware_object_json(&self) -> bool {
        false
    }

    async fn power_control(
        &self,
        endpoints: &[SwitchEndpoint],
        action: PowerAction,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError>;

    async fn queue_firmware_updates(
        &self,
        endpoints: &[SwitchEndpoint],
        bundle_version: &str,
        components: &[NvSwitchComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError>;

    /// Returns status for firmware jobs known to the backend.
    ///
    /// An endpoint may be omitted when the backend does not track a firmware job for it.
    async fn get_firmware_status(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchFirmwareUpdateStatus>, ComponentManagerError>;

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError>;

    async fn get_slot_and_tray(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchSlotAndTrayResult>, ComponentManagerError>;

    async fn get_power_state(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchPowerStateResult>, ComponentManagerError>;
    async fn configure_switch_certificate(
        &self,
        endpoint: &SwitchEndpoint,
        domain_name: Option<&str>,
        services: Option<&[i32]>,
    ) -> Result<String, ComponentManagerError>;

    async fn get_configure_switch_certificate_job_status(
        &self,
        job_id: &str,
    ) -> Result<ConfigureSwitchCertificateJobStatus, ComponentManagerError>;

    /// Submits asynchronous switch factory-default resets for `endpoints`.
    ///
    /// This destructive operation wipes switch operating-system configuration, reboots
    /// each switch, and completes after the default login is reachable again. It
    /// preserves the factory default password. `tls_server_domain` is the TLS server
    /// domain shared by all targets. When it is absent, the selected backend uses its
    /// configured default. A successful submission returns a non-empty, opaque job ID
    /// that can be passed to [`Self::get_switch_factory_reset_job_status`]. Submission
    /// does not wait for the switches to reboot or become reachable.
    ///
    /// `endpoints` must contain at least one switch. Backends that support this
    /// operation return [`ComponentManagerError::InvalidArgument`] for an empty slice
    /// without dispatching work.
    ///
    /// If dispatch may have reached the backend but no job ID is available, the
    /// implementation returns [`ComponentManagerError::OperationOutcomeUnknown`].
    /// Callers must not automatically resubmit this destructive operation. The default
    /// implementation returns [`ComponentManagerError::Unsupported`].
    async fn batch_reset_switch_factory_default(
        &self,
        _endpoints: &[SwitchEndpoint],
        _tls_server_domain: Option<&str>,
    ) -> Result<String, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "switch factory reset is not supported by the {} backend",
            self.name()
        )))
    }

    /// Returns the aggregate state for a submitted switch factory-reset job.
    ///
    /// [`SwitchFactoryResetState::Completed`] requires the parent operation and every
    /// target switch to complete. [`SwitchFactoryResetState::Failed`] is also terminal
    /// and carries backend failure details in [`SwitchFactoryResetJobStatus::error`].
    /// A missing or unrecognized job observation cannot establish whether the reset
    /// ran and returns [`ComponentManagerError::OperationOutcomeUnknown`]. Other
    /// observation errors also do not establish that resubmission is safe. The default
    /// implementation returns [`ComponentManagerError::Unsupported`].
    async fn get_switch_factory_reset_job_status(
        &self,
        _job_id: &str,
    ) -> Result<SwitchFactoryResetJobStatus, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "switch factory-reset job status is not supported by the {} backend",
            self.name()
        )))
    }

    /// Submits the desired rack-level ScaleUp Fabric Manager configuration.
    ///
    /// A successful submission returns a non-empty, opaque job ID. If the backend may
    /// have accepted the request but does not return a durable job ID, the
    /// implementation returns [`ComponentManagerError::OperationOutcomeUnknown`]. The
    /// default implementation returns [`ComponentManagerError::Unsupported`].
    async fn configure_scale_up_fabric_manager(
        &self,
        _endpoints: &[SwitchEndpoint],
        _topology: RackHardwareTopology,
    ) -> Result<String, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "scale-up fabric manager configuration is not supported by the {} backend",
            self.name()
        )))
    }

    /// Returns the latest observation for a ScaleUp Fabric Manager job.
    ///
    /// `None` means the backend cannot find the requested job. The default
    /// implementation returns [`ComponentManagerError::Unsupported`].
    async fn get_scale_up_fabric_manager_job_status(
        &self,
        _job_id: &str,
    ) -> Result<Option<ScaleUpFabricManagerJobStatus>, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "scale-up fabric manager job status is not supported by the {} backend",
            self.name()
        )))
    }

    /// Reads the primary and enabled state observed for the submitted fabric.
    ///
    /// The default implementation returns [`ComponentManagerError::Unsupported`].
    async fn get_scale_up_fabric_status(
        &self,
        _endpoints: &[SwitchEndpoint],
    ) -> Result<ScaleUpFabricStatus, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "scale-up fabric status is not supported by the {} backend",
            self.name()
        )))
    }

    /// Reads per-switch Fabric Manager service status for persistence by NICo.
    ///
    /// The default implementation returns [`ComponentManagerError::Unsupported`].
    async fn batch_get_scale_up_fabric_service_status(
        &self,
        _endpoints: &[SwitchEndpoint],
    ) -> Result<ScaleUpFabricServiceStatuses, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "scale-up fabric manager service status is not supported by the {} backend",
            self.name()
        )))
    }

    /// Converges the endpoint from its current NVOS credential to
    /// `next_password`.
    ///
    /// Implementations must make repeated identical requests safe after a
    /// controller or backend restart, including overlapping requests through
    /// different backend replicas. If the endpoint authenticates with the current
    /// password, apply the target. If it authenticates with the target, recognize
    /// convergence without applying another password change.
    ///
    /// The returned job ID can be passed to
    /// [`Self::get_password_rotation_job_status`] for an early completion
    /// observation. It is opaque and transient: callers must not derive
    /// credential state from it. Implementations must not log `next_password`
    /// or otherwise expose it outside the backend request.
    ///
    /// If dispatch may have reached the backend but no job ID is available, the
    /// implementation returns [`ComponentManagerError::OperationOutcomeUnknown`].
    /// Callers retain the exact staged current-to-target transition and retry it
    /// later. [`ComponentManagerError::RejectedBeforeDispatch`] is reserved for
    /// rejection that proves no mutation or job was accepted. The default
    /// implementation returns
    /// [`ComponentManagerError::Unsupported`].
    async fn ensure_password_rotation(
        &self,
        _endpoint: &SwitchEndpoint,
        _next_password: &str,
    ) -> Result<String, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "switch password rotation is not supported by the {} backend",
            self.name()
        )))
    }

    /// Returns the latest backend observation for a submitted rotation job.
    ///
    /// A job that cannot be resolved is returned as
    /// [`SwitchPasswordRotationState::NotFound`], not as an error. An error means
    /// no job observation was obtained and does not imply a terminal job state.
    /// The default implementation returns [`ComponentManagerError::Unsupported`].
    async fn get_password_rotation_job_status(
        &self,
        _job_id: &str,
    ) -> Result<SwitchPasswordRotationState, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "switch password rotation is not supported by the {} backend",
            self.name()
        )))
    }
}
