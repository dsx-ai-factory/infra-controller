// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use model::component_manager::{
    ComputeTrayComponent, ConfigureSwitchCertificateState, FirmwareState, NvSwitchComponent,
    PowerAction, PowerShelfComponent,
};

use crate::compute_tray_manager::{
    Backend, ComputeTrayEndpoint, ComputeTrayFirmwareUpdateStatus, ComputeTrayManager,
    ComputeTrayResult,
};
use crate::error::ComponentManagerError;
use crate::nv_switch_manager::{
    ConfigureSwitchCertificateJobStatus, NvSwitchManager, SwitchComponentResult, SwitchEndpoint,
    SwitchFactoryResetJobStatus, SwitchFirmwareUpdateStatus, SwitchPasswordRotationState,
    SwitchPowerStateResult, SwitchSlotAndTrayResult,
};
use crate::power_shelf_manager::{
    PowerShelfComponentResult, PowerShelfEndpoint, PowerShelfFirmwareUpdateStatus,
    PowerShelfFirmwareVersions, PowerShelfManager, PowerShelfPowerStateResult,
};
use crate::types::FirmwareUpdateOptions;

/// Configurable switch backend used by component-manager and controller tests.
#[derive(Debug, Clone, Default)]
pub struct MockNvSwitchManager {
    certificate_job_status: Option<ConfigureSwitchCertificateJobStatus>,
    password_rotation_enabled: bool,
    password_rotation_start_result: Option<MockPasswordRotationStartResult>,
    password_rotation_job_status_result: Option<MockPasswordRotationJobStatusResult>,
    expected_password_rotation_password: Option<String>,
    factory_reset_job_status_responses: Option<Arc<Mutex<VecDeque<FactoryResetJobStatusResult>>>>,
    factory_reset_job_status_calls: Arc<Mutex<Vec<String>>>,
}

impl MockNvSwitchManager {
    /// Returns a mock configured with a certificate job status.
    pub fn with_certificate_job_status(
        mut self,
        status: ConfigureSwitchCertificateJobStatus,
    ) -> Self {
        self.certificate_job_status = Some(status);
        self
    }

    /// Enables the mock's password-rotation capability.
    pub fn with_password_rotation_enabled(mut self) -> Self {
        self.password_rotation_enabled = true;
        self
    }

    /// Makes password-rotation submission return `job_id`.
    pub fn with_password_rotation_job(mut self, job_id: impl Into<String>) -> Self {
        self.password_rotation_start_result =
            Some(MockPasswordRotationStartResult::Job(job_id.into()));

        self
    }

    /// Makes password-rotation submission report an ambiguous outcome.
    pub fn with_password_rotation_outcome_unknown(mut self, message: impl Into<String>) -> Self {
        self.password_rotation_start_result = Some(
            MockPasswordRotationStartResult::OutcomeUnknown(message.into()),
        );

        self
    }

    /// Sets the status returned when a password-rotation job is polled.
    pub fn with_password_rotation_job_status(
        mut self,
        status: SwitchPasswordRotationState,
    ) -> Self {
        self.password_rotation_job_status_result =
            Some(MockPasswordRotationJobStatusResult::Status(status));

        self
    }

    /// Makes password-rotation polling fail without observing the job.
    pub fn with_password_rotation_job_status_unavailable(
        mut self,
        message: impl Into<String>,
    ) -> Self {
        self.password_rotation_job_status_result = Some(
            MockPasswordRotationJobStatusResult::Unavailable(message.into()),
        );

        self
    }

    /// Requires submissions to carry the supplied target password.
    pub fn with_expected_password_rotation_password(mut self, password: impl Into<String>) -> Self {
        self.expected_password_rotation_password = Some(password.into());
        self
    }

    /// Queues factory-reset job-status results in polling order.
    pub fn with_factory_reset_job_status_responses(
        mut self,
        responses: impl IntoIterator<Item = FactoryResetJobStatusResult>,
    ) -> Self {
        self.factory_reset_job_status_responses =
            Some(Arc::new(Mutex::new(responses.into_iter().collect())));

        self
    }

    /// Returns the factory-reset parent job IDs requested by callers.
    pub fn factory_reset_job_status_calls(&self) -> Vec<String> {
        self.factory_reset_job_status_calls
            .lock()
            .expect("factory-reset job-status calls lock poisoned")
            .clone()
    }
}

/// Result returned by one configured factory-reset job-status poll.
pub type FactoryResetJobStatusResult = Result<SwitchFactoryResetJobStatus, ComponentManagerError>;

#[derive(Debug, Clone)]
enum MockPasswordRotationStartResult {
    Job(String),
    OutcomeUnknown(String),
}

#[derive(Debug, Clone)]
enum MockPasswordRotationJobStatusResult {
    Status(SwitchPasswordRotationState),
    Unavailable(String),
}

#[async_trait::async_trait]
impl NvSwitchManager for MockNvSwitchManager {
    fn name(&self) -> &str {
        "mock-nsm"
    }

    fn supports_password_rotation(&self) -> bool {
        self.password_rotation_enabled
    }

    async fn power_control(
        &self,
        endpoints: &[SwitchEndpoint],
        _action: PowerAction,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| SwitchComponentResult {
                bmc_mac: ep.bmc_mac,
                success: true,
                error: None,
            })
            .collect())
    }

    async fn queue_firmware_updates(
        &self,
        endpoints: &[SwitchEndpoint],
        _bundle_version: &str,
        _components: &[NvSwitchComponent],
        _options: &FirmwareUpdateOptions,
    ) -> Result<Vec<SwitchComponentResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| SwitchComponentResult {
                bmc_mac: ep.bmc_mac,
                success: true,
                error: None,
            })
            .collect())
    }

    async fn get_firmware_status(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchFirmwareUpdateStatus>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| SwitchFirmwareUpdateStatus {
                bmc_mac: ep.bmc_mac,
                state: FirmwareState::Completed,
                target_version: "mock-1.0.0".into(),
                error: None,
            })
            .collect())
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        Ok(vec!["mock-1.0.0".into(), "mock-2.0.0".into()])
    }

    async fn get_slot_and_tray(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchSlotAndTrayResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| SwitchSlotAndTrayResult {
                bmc_mac: ep.bmc_mac,
                slot_number: None,
                tray_index: None,
                error: None,
            })
            .collect())
    }

    async fn get_power_state(
        &self,
        endpoints: &[SwitchEndpoint],
    ) -> Result<Vec<SwitchPowerStateResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| SwitchPowerStateResult {
                bmc_mac: ep.bmc_mac,
                power_state: None,
                error: None,
            })
            .collect())
    }

    async fn configure_switch_certificate(
        &self,
        _endpoint: &SwitchEndpoint,
        _domain_name: Option<&str>,
        _services: Option<&[i32]>,
    ) -> Result<String, ComponentManagerError> {
        Ok("mock-switch-cert-job".to_string())
    }

    async fn get_configure_switch_certificate_job_status(
        &self,
        _job_id: &str,
    ) -> Result<ConfigureSwitchCertificateJobStatus, ComponentManagerError> {
        Ok(self
            .certificate_job_status
            .clone()
            .unwrap_or(ConfigureSwitchCertificateJobStatus {
                state: ConfigureSwitchCertificateState::Completed,
                error: None,
            }))
    }

    async fn get_switch_factory_reset_job_status(
        &self,
        job_id: &str,
    ) -> Result<SwitchFactoryResetJobStatus, ComponentManagerError> {
        self.factory_reset_job_status_calls
            .lock()
            .expect("factory-reset job-status calls lock poisoned")
            .push(job_id.to_string());

        let Some(responses) = &self.factory_reset_job_status_responses else {
            return Err(ComponentManagerError::Unsupported(
                "mock switch factory-reset job status is disabled".to_string(),
            ));
        };

        responses
            .lock()
            .expect("factory-reset job-status responses lock poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                Err(ComponentManagerError::Internal(
                    "no mock switch factory-reset job-status response queued".to_string(),
                ))
            })
    }

    async fn ensure_password_rotation(
        &self,
        _endpoint: &SwitchEndpoint,
        next_password: &str,
    ) -> Result<String, ComponentManagerError> {
        if !self.password_rotation_enabled {
            return Err(ComponentManagerError::Unsupported(
                "mock switch password rotation is disabled".to_string(),
            ));
        }

        if let Some(expected) = &self.expected_password_rotation_password
            && next_password != expected
        {
            return Err(ComponentManagerError::RejectedBeforeDispatch(
                "mock switch password rotation received unexpected target password".to_string(),
            ));
        }

        match self.password_rotation_start_result.as_ref() {
            Some(MockPasswordRotationStartResult::Job(job_id)) => Ok(job_id.clone()),
            Some(MockPasswordRotationStartResult::OutcomeUnknown(message)) => Err(
                ComponentManagerError::OperationOutcomeUnknown(message.clone()),
            ),
            None => Ok("mock-switch-password-job".to_string()),
        }
    }

    async fn get_password_rotation_job_status(
        &self,
        _job_id: &str,
    ) -> Result<SwitchPasswordRotationState, ComponentManagerError> {
        match self.password_rotation_job_status_result.as_ref() {
            Some(MockPasswordRotationJobStatusResult::Status(status)) => Ok(*status),
            Some(MockPasswordRotationJobStatusResult::Unavailable(message)) => {
                Err(ComponentManagerError::Unavailable(message.clone()))
            }
            None => Ok(SwitchPasswordRotationState::Completed),
        }
    }
}

#[derive(Debug, Default)]
pub struct MockPowerShelfManager;

#[async_trait::async_trait]
impl PowerShelfManager for MockPowerShelfManager {
    fn name(&self) -> &str {
        "mock-psm"
    }

    async fn power_control(
        &self,
        endpoints: &[PowerShelfEndpoint],
        _action: PowerAction,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| PowerShelfComponentResult {
                pmc_mac: ep.pmc_mac,
                success: true,
                error: None,
            })
            .collect())
    }

    async fn update_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
        _target_version: &str,
        _components: &[PowerShelfComponent],
        _options: &FirmwareUpdateOptions,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| PowerShelfComponentResult {
                pmc_mac: ep.pmc_mac,
                success: true,
                error: None,
            })
            .collect())
    }

    async fn get_firmware_status(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareUpdateStatus>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| PowerShelfFirmwareUpdateStatus {
                pmc_mac: ep.pmc_mac,
                state: FirmwareState::Completed,
                target_version: "mock-1.0.0".into(),
                error: None,
            })
            .collect())
    }

    async fn list_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareVersions>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| PowerShelfFirmwareVersions {
                pmc_mac: ep.pmc_mac,
                versions: vec!["mock-1.0.0".into(), "mock-2.0.0".into()],
                error: None,
            })
            .collect())
    }

    async fn get_power_state(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfPowerStateResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| PowerShelfPowerStateResult {
                pmc_mac: ep.pmc_mac,
                power_state: None,
                error: None,
            })
            .collect())
    }
}

#[derive(Debug, Default)]
pub struct MockComputeTrayManager;

#[async_trait::async_trait]
impl ComputeTrayManager for MockComputeTrayManager {
    fn name(&self) -> &str {
        "mock-ctm"
    }

    fn backend(&self) -> Backend {
        Backend::Mock
    }

    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        _action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| ComputeTrayResult {
                bmc_ip: ep.bmc_ip,
                bmc_mac: ep.bmc_mac,
                success: true,
                error: None,
            })
            .collect())
    }

    async fn update_firmware(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        _target_version: &str,
        _components: &[ComputeTrayComponent],
        _options: &FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| ComputeTrayResult {
                bmc_ip: ep.bmc_ip,
                bmc_mac: ep.bmc_mac,
                success: true,
                error: None,
            })
            .collect())
    }

    async fn get_firmware_status(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError> {
        Ok(endpoints
            .iter()
            .map(|ep| ComputeTrayFirmwareUpdateStatus {
                bmc_ip: ep.bmc_ip,
                bmc_mac: ep.bmc_mac,
                state: FirmwareState::Completed,
                target_version: "mock-1.0.0".into(),
                error: None,
            })
            .collect())
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        Ok(vec!["mock-1.0.0".into(), "mock-2.0.0".into()])
    }
}
