/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! ComponentIntegrity discovery and signed-measurement polling.

use bmc_platform::{EvidenceProgress, OpCx, OperationReference, PlatformError};
use nv_redfish::Error as RedfishError;
use nv_redfish::component_integrity::{ComponentIntegrity, SpdmGetSignedMeasurementsResponse};
use nv_redfish::core::{AsyncTask, Bmc};
use nv_redfish::task_service::AsyncActionResult;

/// Integrity resource discovery and signed-measurement polling.
pub(super) trait RedfishAttestationExt<B: Bmc> {
    /// All advertised ComponentIntegrity resources.
    async fn integrity_components(&self) -> Result<Vec<ComponentIntegrity<B>>, PlatformError>;

    /// Finds an integrity resource by id; a missing id is an invalid response.
    async fn integrity_component(
        &self,
        component_id: &str,
    ) -> Result<ComponentIntegrity<B>, PlatformError>;

    /// Runs one polling step.
    async fn poll_signed_measurements(
        &self,
        result: AsyncActionResult<SpdmGetSignedMeasurementsResponse>,
    ) -> Result<EvidenceProgress, PlatformError>;
}

impl<B: Bmc> RedfishAttestationExt<B> for OpCx<'_, B> {
    async fn integrity_components(&self) -> Result<Vec<ComponentIntegrity<B>>, PlatformError> {
        self.service_root()
            .component_integrity()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .members()
            .await
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn integrity_component(
        &self,
        component_id: &str,
    ) -> Result<ComponentIntegrity<B>, PlatformError> {
        self.integrity_components()
            .await?
            .into_iter()
            .find(|component| component.raw().id == component_id)
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: format!("ComponentIntegrity resource {component_id} was not found"),
            })
    }

    async fn poll_signed_measurements(
        &self,
        mut result: AsyncActionResult<SpdmGetSignedMeasurementsResponse>,
    ) -> Result<EvidenceProgress, PlatformError> {
        match result.poll_result(self.bmc()).await {
            Ok(Some(measurements)) => Ok(EvidenceProgress::Ready(measurements)),
            Ok(None) => result
                .pending_task()
                .map(|task| EvidenceProgress::Pending(pending(task)))
                .ok_or_else(|| PlatformError::InvalidResponse {
                    message: "signed-measurements request reported neither a result nor a task"
                        .to_string(),
                }),
            Err(RedfishError::TaskFailed { state, messages }) => {
                Ok(EvidenceProgress::Failed { state, messages })
            }
            Err(error) => Err(self.map_redfish_error(error)),
        }
    }
}

pub(super) fn pending(task: &AsyncTask) -> OperationReference {
    OperationReference::RedfishTask {
        uri: task.location.0.clone(),
        retry_after_seconds: task.retry_after.map(|delay| delay.as_secs()),
        task: task.task.clone(),
    }
}
